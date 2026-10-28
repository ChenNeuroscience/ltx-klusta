// The shell DLL only inspects names and hands a request file to a separate
// process. Queue I/O and clustering never run on Explorer's menu/UI path.
#include "native.h"
#include <atomic>
#include <new>

namespace {
// Keep this CLSID in sync with AppxManifest.xml.in.
const CLSID command_id = {0xb0ea0498, 0x10ee, 0x49f3, {0xa6, 0x31, 0x36, 0x13, 0xb6, 0x98, 0x96, 0x5c}};
std::atomic<long> references{0};
HMODULE module_handle = nullptr;

std::vector<std::wstring> selection(IShellItemArray* items) {
    if (!items) return {};
    DWORD count = 0;
    if (FAILED(items->GetCount(&count)) || !count) return {};
    std::vector<std::wstring> paths;
    for (DWORD i = 0; i < count; ++i) {
        ltxk::ComPtr<IShellItem> item;
        if (FAILED(items->GetItemAt(i, &item))) return {};
        SFGAOF attributes = 0;
        if (FAILED(item->GetAttributes(SFGAO_FILESYSTEM | SFGAO_FOLDER, &attributes)) ||
            !(attributes & SFGAO_FILESYSTEM) || (attributes & SFGAO_FOLDER)) return {};
        PWSTR path = nullptr;
        if (FAILED(item->GetDisplayName(SIGDN_FILESYSPATH, &path))) return {};
        std::wstring name(path);
        CoTaskMemFree(path);
        if (!ltxk::recording_name(name)) return {};
        paths.push_back(std::move(name));
    }
    return paths;
}

class Command final : public IExplorerCommand {
    std::atomic<ULONG> refs{1};
public:
    Command() { ++references; }
    ~Command() { --references; }
    IFACEMETHODIMP QueryInterface(REFIID iid, void** object) override {
        if (!object) return E_POINTER;
        *object = nullptr;
        if (iid == IID_IUnknown || iid == IID_IExplorerCommand) {
            *object = static_cast<IExplorerCommand*>(this); AddRef(); return S_OK;
        }
        return E_NOINTERFACE;
    }
    IFACEMETHODIMP_(ULONG) AddRef() override { return ++refs; }
    IFACEMETHODIMP_(ULONG) Release() override { const auto n = --refs; if (!n) delete this; return n; }
    IFACEMETHODIMP GetTitle(IShellItemArray*, PWSTR* title) override {
        return SHStrDupW(L"Run ltxk", title);
    }
    IFACEMETHODIMP GetIcon(IShellItemArray*, PWSTR* icon) override {
        try { return SHStrDupW((ltxk::directory(module_handle) + L"\\ltxk-launch.exe,0").c_str(), icon); }
        catch (...) { return E_FAIL; }
    }
    IFACEMETHODIMP GetToolTip(IShellItemArray*, PWSTR* tip) override {
        return SHStrDupW(L"Add recordings to the ltx-klusta queue and show progress", tip);
    }
    IFACEMETHODIMP GetCanonicalName(GUID* id) override { if (!id) return E_POINTER; *id = command_id; return S_OK; }
    IFACEMETHODIMP GetState(IShellItemArray* items, BOOL ok_to_be_slow, EXPCMDSTATE* state) override {
        if (!state) return E_POINTER;
        *state = ECS_HIDDEN;
        DWORD count = 0;
        if (!ok_to_be_slow && items && SUCCEEDED(items->GetCount(&count)) && count > 64)
            return E_PENDING; // Let Explorer retry large selections off its UI path.
        try { if (!selection(items).empty()) *state = ECS_ENABLED; return S_OK; }
        catch (...) { return E_FAIL; }
    }
    IFACEMETHODIMP GetFlags(EXPCMDFLAGS* flags) override {
        if (!flags) return E_POINTER;
        *flags = ECF_DEFAULT; return S_OK;
    }
    IFACEMETHODIMP EnumSubCommands(IEnumExplorerCommand** commands) override {
        if (commands) *commands = nullptr;
        return E_NOTIMPL;
    }
    IFACEMETHODIMP Invoke(IShellItemArray* items, IBindCtx*) override {
        std::wstring request;
        try {
            const auto paths = selection(items);
            if (paths.empty()) return E_INVALIDARG;
            const auto root = ltxk::queue_root() + L"\\requests";
            const int made = SHCreateDirectoryExW(nullptr, root.c_str(), nullptr);
            SetLastError(static_cast<DWORD>(made));
            ltxk::check(made == ERROR_SUCCESS || made == ERROR_ALREADY_EXISTS, L"Cannot create the selection folder.");
            GUID guid;
            ltxk::hcheck(CoCreateGuid(&guid), L"Cannot identify the selection.");
            wchar_t id[40];
            StringFromGUID2(guid, id, ARRAYSIZE(id));
            request = root + L"\\" + id + L".json";
            std::string json = "[";
            for (const auto& path : paths) {
                if (json.size() > 1) json += ',';
                json += ltxk::json_string(ltxk::utf8(path));
            }
            json += ']';
            ltxk::write_file(request, json);
            const auto dir = ltxk::directory(module_handle);
            const auto launcher = dir + L"\\ltxk-launch.exe";
            auto args = ltxk::quote(launcher) + L" --request " + ltxk::quote(request);
            STARTUPINFOW startup{};
            startup.cb = sizeof(startup);
            PROCESS_INFORMATION process{};
            ltxk::check(CreateProcessW(launcher.c_str(), args.data(), nullptr, nullptr, FALSE, 0,
                         nullptr, dir.c_str(), &startup, &process), L"Cannot start ltxk. Run Install Explorer integration.cmd again.");
            AllowSetForegroundWindow(process.dwProcessId);
            CloseHandle(process.hThread); CloseHandle(process.hProcess);
            return S_OK;
        } catch (const ltxk::Failure& error) {
            if (!request.empty()) DeleteFileW(request.c_str());
            ltxk::error_box(error.message); return E_FAIL;
        } catch (...) {
            if (!request.empty()) DeleteFileW(request.c_str());
            return E_OUTOFMEMORY;
        }
    }
};

class Factory final : public IClassFactory {
    std::atomic<ULONG> refs{1};
public:
    Factory() { ++references; }
    ~Factory() { --references; }
    IFACEMETHODIMP QueryInterface(REFIID iid, void** object) override {
        if (!object) return E_POINTER;
        *object = nullptr;
        if (iid == IID_IUnknown || iid == IID_IClassFactory) { *object = this; AddRef(); return S_OK; }
        return E_NOINTERFACE;
    }
    IFACEMETHODIMP_(ULONG) AddRef() override { return ++refs; }
    IFACEMETHODIMP_(ULONG) Release() override { const auto n = --refs; if (!n) delete this; return n; }
    IFACEMETHODIMP CreateInstance(IUnknown* outer, REFIID iid, void** object) override {
        if (outer) return CLASS_E_NOAGGREGATION;
        auto command = new (std::nothrow) Command;
        if (!command) return E_OUTOFMEMORY;
        const auto result = command->QueryInterface(iid, object); command->Release(); return result;
    }
    IFACEMETHODIMP LockServer(BOOL lock) override { if (lock) ++references; else --references; return S_OK; }
};
}

BOOL WINAPI DllMain(HINSTANCE module, DWORD reason, void*) {
    if (reason == DLL_PROCESS_ATTACH) module_handle = module;
    return TRUE;
}
extern "C" HRESULT __stdcall DllCanUnloadNow() { return references.load() ? S_FALSE : S_OK; }
extern "C" HRESULT __stdcall DllGetClassObject(REFCLSID clsid, REFIID iid, void** object) {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (clsid != command_id) return CLASS_E_CLASSNOTAVAILABLE;
    auto factory = new (std::nothrow) Factory;
    if (!factory) return E_OUTOFMEMORY;
    const auto result = factory->QueryInterface(iid, object); factory->Release(); return result;
}
