#include "shell_util.h"
#include <iostream>
#include <stdexcept>
#include <vector>

void require(bool ok, const char* message) { if (!ok) throw std::runtime_error(message); }
void utility_checks() {
    for (auto path : {L"session.1", L"a.b.123", L"C:\\folder\\space & ! λ.02", L"\\\\server\\share\\wave.12"})
        require(ltxk::recording_name(path), "Recording was hidden");
    for (auto path : {L"session.txt", L"session.1x", L"session.", L".1", L"session.clu.1",
                      L"a.FET.2", L"a.res.3", L"a.spk.4", L"folder.1\\recording"})
        require(!ltxk::recording_name(path), "Sidecar or non-recording was accepted");
    require(ltxk::quote(L"") == L"\"\"", "Empty argument quoting");
    require(ltxk::quote(L"a\\") == L"\"a\\\\\"", "Trailing slash quoting");
    require(ltxk::json_string("C:\\a\"b\n\t") == "\"C:\\\\a\\\"b\\u000a\\u0009\"", "JSON escaping");
}

#ifdef _WIN32
#include "native.h"

ltxk::ComPtr<IShellItemArray> items(const std::vector<std::wstring>& paths) {
    std::vector<PCIDLIST_ABSOLUTE> ids;
    for (const auto& path : paths) {
        PIDLIST_ABSOLUTE id = nullptr;
        ltxk::hcheck(SHParseDisplayName(path.c_str(), nullptr, &id, 0, nullptr), L"Cannot parse a test item.");
        ids.push_back(id);
    }
    ltxk::ComPtr<IShellItemArray> result;
    const auto status = SHCreateShellItemArrayFromIDLists(static_cast<UINT>(ids.size()), ids.data(), &result);
    for (const auto id : ids) CoTaskMemFree(const_cast<ITEMIDLIST*>(id));
    ltxk::hcheck(status, L"Cannot create a test selection.");
    return result;
}

int wmain(int argc, wchar_t** argv) {
    try {
        require(argc == 2, "Pass the shell DLL path");
        utility_checks();
        for (auto value : {L"", L"a b", L"C:\\spaces here\\", L"a\\\"b", L"a & ! % λ", L"x; y"}) {
            auto line = L"program.exe " + ltxk::quote(value);
            int count = 0;
            auto parsed = CommandLineToArgvW(line.c_str(), &count);
            require(parsed && count == 2 && std::wstring(parsed[1]) == value, "Windows argument round trip");
            LocalFree(parsed);
        }
        ltxk::hcheck(CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED), L"Cannot initialise COM.");
        const auto module = LoadLibraryW(argv[1]);
        ltxk::check(module != nullptr, L"Cannot load the shell DLL.");
        auto factory_entry = reinterpret_cast<HRESULT (__stdcall*)(REFCLSID, REFIID, void**)>(GetProcAddress(module, "DllGetClassObject"));
        auto unload = reinterpret_cast<HRESULT (__stdcall*)()>(GetProcAddress(module, "DllCanUnloadNow"));
        require(factory_entry && unload && unload() == S_OK, "Missing DLL exports or leaked references");
        CLSID id;
        ltxk::hcheck(CLSIDFromString(L"{B0EA0498-10EE-49F3-A631-3613B698965C}", &id), L"Invalid command CLSID.");
        ltxk::ComPtr<IClassFactory> factory;
        ltxk::hcheck(factory_entry(id, IID_PPV_ARGS(&factory)), L"Cannot get the command factory.");
        ltxk::ComPtr<IExplorerCommand> command;
        ltxk::hcheck(factory->CreateInstance(nullptr, IID_PPV_ARGS(&command)), L"Cannot create the Explorer command.");
        PWSTR title = nullptr;
        ltxk::hcheck(command->GetTitle(nullptr, &title), L"Cannot get the title.");
        require(std::wstring(title) == L"Run ltxk", "Wrong menu title");
        CoTaskMemFree(title);
        wchar_t temporary[32768];
        ltxk::check(GetTempPathW(ARRAYSIZE(temporary), temporary) != 0, L"Cannot locate temp.");
        GUID unique; CoCreateGuid(&unique);
        wchar_t suffix[40]; StringFromGUID2(unique, suffix, ARRAYSIZE(suffix));
        const auto root = std::filesystem::path(temporary) / suffix;
        std::filesystem::create_directory(root);
        const auto one = (root / L"wave & ! λ.1").wstring();
        const auto two = (root / L"wave.2").wstring();
        const auto sidecar = (root / L"wave.clu.1").wstring();
        const auto other = (root / L"other.txt").wstring();
        for (const auto& name : {one, two, sidecar, other}) ltxk::write_file(name, "test");
        for (const auto& paths : std::vector<std::vector<std::wstring>>{{one}, {one, two}, {one, sidecar}, {one, other}, {root.wstring()}}) {
            auto selected = items(paths);
            EXPCMDSTATE state = ECS_DISABLED;
            ltxk::hcheck(command->GetState(selected.Get(), FALSE, &state), L"Cannot query command eligibility.");
            const bool eligible = paths[0] == one && (paths.size() == 1 || paths[1] == two);
            require(state == (eligible ? ECS_ENABLED : ECS_HIDDEN), "Wrong selection eligibility");
        }
        EXPCMDSTATE state;
        auto large = items(std::vector<std::wstring>(65, one));
        require(command->GetState(large.Get(), FALSE, &state) == E_PENDING, "Large selection blocked the menu UI path");
        ltxk::hcheck(command->GetState(large.Get(), TRUE, &state), L"Cannot query a large selection.");
        require(state == ECS_ENABLED, "Large selection was not enabled on the slow path");
        large.Reset();
        command->GetState(nullptr, FALSE, &state);
        require(state == ECS_HIDDEN, "The command appeared without a selection");
        command.Reset(); factory.Reset();
        require(unload() == S_OK, "COM objects were not released");
        FreeLibrary(module); CoUninitialize();
        std::filesystem::remove_all(root);
        std::cout << "Explorer COM selection, lifetime, Unicode and quoting checks passed\n";
        return 0;
    } catch (const ltxk::Failure& error) { std::wcerr << error.message << L'\n'; }
    catch (const std::exception& error) { std::cerr << error.what() << '\n'; }
    return 1;
}
#else
int main() {
    try { utility_checks(); std::cout << "Shell filename and encoding checks passed\n"; return 0; }
    catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
#endif
