// GUI-subsystem launcher: submit quietly, then restore the existing console or
// create one through the user's default console host (including Windows Terminal).
#include "native.h"
#include <UIAutomation.h>
#include <sstream>

namespace {
using namespace ltxk;

std::string read_file(const std::wstring& path, DWORD maximum = 8192) {
    Handle file(CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                           nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
    if (!file.valid()) return {};
    std::string text(maximum, '\0');
    DWORD read = 0;
    if (!ReadFile(file.value, text.data(), maximum, &read, nullptr)) return {};
    text.resize(read);
    return text;
}

// Rust File::lock also uses LockFileEx; one overlapping byte proves the lease.
bool console_owned(const std::wstring& root) {
    Handle file(CreateFileW((root + L"\\console.lock").c_str(), GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr));
    check(file.valid(), L"Cannot check the current console.");
    OVERLAPPED offset{};
    if (LockFileEx(file.value, LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &offset)) {
        UnlockFileEx(file.value, 0, 1, 0, &offset);
        return false;
    }
    check(GetLastError() == ERROR_LOCK_VIOLATION, L"Cannot check the current console lease.");
    return true;
}

bool select_terminal_tab(HWND window, const std::wstring& title) {
    // GetConsoleWindow's root owner is the visible host on current Windows.
    // Select the matching Terminal tab as well as restoring the host window.
    ComPtr<IUIAutomation> automation;
    if (FAILED(CoCreateInstance(CLSID_CUIAutomation, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&automation)))) return false;
    ComPtr<IUIAutomationElement> host;
    if (FAILED(automation->ElementFromHandle(window, &host))) return false;
    VARIANT value; VariantInit(&value); value.vt = VT_I4; value.lVal = UIA_TabItemControlTypeId;
    ComPtr<IUIAutomationCondition> condition;
    if (FAILED(automation->CreatePropertyCondition(UIA_ControlTypePropertyId, value, &condition))) return false;
    ComPtr<IUIAutomationElementArray> tabs;
    if (FAILED(host->FindAll(TreeScope_Descendants, condition.Get(), &tabs))) return false;
    int count = 0; tabs->get_Length(&count);
    if (!count) return true; // A classic console host has no tabs to select.
    ComPtr<IUIAutomationElement> exact, fallback;
    int exact_count = 0, fallback_count = 0;
    for (int i = 0; i < count; ++i) {
        ComPtr<IUIAutomationElement> tab;
        if (FAILED(tabs->GetElement(i, &tab))) continue;
        BSTR name = nullptr;
        if (FAILED(tab->get_CurrentName(&name))) continue;
        const std::wstring label(name ? name : L"");
        SysFreeString(name);
        if (!title.empty() && label == title) { exact = tab; ++exact_count; }
        if (label == L"ltx-klusta" || label.find(L"ltx-klusta:") == 0) { fallback = tab; ++fallback_count; }
    }
    auto target = exact_count == 1 ? exact : (exact_count == 0 && fallback_count == 1 ? fallback : nullptr);
    if (!target) return false;
    ComPtr<IUIAutomationSelectionItemPattern> pattern;
    if (FAILED(target->GetCurrentPatternAs(UIA_SelectionItemPatternId, IID_PPV_ARGS(&pattern))) || FAILED(pattern->Select())) return false;
    target->SetFocus();
    return true;
}

enum class Existing { absent, starting, visible, external };
Existing activate_existing(const std::wstring& root) {
    if (!console_owned(root)) return Existing::absent;
    std::istringstream owner(read_file(root + L"\\console.owner", 128));
    DWORD pid = 0;
    unsigned long long created = 0;
    if (!(owner >> pid >> created) || !pid || !created) return Existing::starting;
    Handle process(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, FALSE, pid));
    if (!process.valid() || WaitForSingleObject(process.value, 0) != WAIT_TIMEOUT) return Existing::starting;
    FILETIME start{}, exit{}, kernel{}, user{};
    if (!GetProcessTimes(process.value, &start, &exit, &kernel, &user) ||
        created != (static_cast<unsigned long long>(start.dwHighDateTime) << 32 | start.dwLowDateTime))
        return Existing::starting;
    if (!AttachConsole(pid)) return Existing::external;
    const auto console = GetConsoleWindow();
    auto window = GetAncestor(console, GA_ROOTOWNER);
    if (!window || !IsWindowVisible(window)) window = console;
    std::vector<wchar_t> title(32768);
    GetConsoleTitleW(title.data(), static_cast<DWORD>(title.size()));
    FreeConsole();
    if (!window || !IsWindowVisible(window)) return Existing::external;
    ShowWindowAsync(window, SW_RESTORE);
    const auto selected = select_terminal_tab(window, title.data());
    if (!SetForegroundWindow(window)) {
        FLASHWINFO flash{sizeof(flash), window, FLASHW_TRAY, 3, 0};
        FlashWindowEx(&flash);
    }
    return selected ? Existing::visible : Existing::external;
}

void submit(const std::wstring& exe, const std::wstring& request, const std::wstring& root) {
    const auto log_path = root + L"\\launcher.log";
    SECURITY_ATTRIBUTES attributes{sizeof(attributes), nullptr, TRUE};
    Handle log(CreateFileW(log_path.c_str(), GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
                          &attributes, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr));
    Handle input(CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
                            &attributes, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
    check(log.valid() && input.valid(), L"Cannot open the launcher log.");
    auto args = quote(exe) + L" enqueue --request " + quote(request);
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = input.value; startup.hStdOutput = log.value; startup.hStdError = log.value;
    PROCESS_INFORMATION info{};
    check(CreateProcessW(exe.c_str(), args.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW,
                        nullptr, directory().c_str(), &startup, &info), L"Cannot submit the recordings.");
    Handle process(info.hProcess), thread(info.hThread);
    if (WaitForSingleObject(process.value, 30000) != WAIT_OBJECT_0) {
        TerminateProcess(process.value, 1);
        throw Failure{L"Queue submission timed out. See " + log_path};
    }
    DWORD code = 1;
    check(GetExitCodeProcess(process.value, &code), L"Cannot read the queue result.");
    if (code != 0) {
        const auto text = read_file(log_path);
        const int count = MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0);
        std::wstring details(count, L'\0');
        MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), details.data(), count);
        throw Failure{L"The selection could not be queued.\n\n" + details + L"\nLog: " + log_path};
    }
}

int run(const std::wstring& request) {
    const auto root = queue_root();
    // Serialize the submit/activate/start handshake across rapid right-clicks.
    Handle launch(CreateFileW((root + L"\\desktop-launch.lock").c_str(), GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr));
    check(launch.valid(), L"Cannot open the launcher lock.");
    OVERLAPPED offset{};
    check(LockFileEx(launch.value, LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, &offset), L"Cannot lock the launcher.");
    const auto exe = directory() + L"\\ltxk.exe";
    if (!request.empty()) submit(exe, request, root);
    for (int attempt = 0; attempt < 100; ++attempt) {
        const auto existing = activate_existing(root);
        if (existing == Existing::visible) return 0;
        if (existing == Existing::external) {
            std::wstring message = L"The queue is already open in another terminal.\n";
            if (!request.empty()) message += L"Your selected recordings have been queued.\n";
            message += L"Switch to that terminal to see progress.";
            MessageBoxW(nullptr, message.c_str(),
                        L"ltx-klusta", MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND);
            return 0;
        }
        if (existing == Existing::absent) {
            auto args = quote(exe);
            STARTUPINFOW startup{};
            startup.cb = sizeof(startup);
            PROCESS_INFORMATION info{};
            check(CreateProcessW(exe.c_str(), args.data(), nullptr, nullptr, FALSE, CREATE_NEW_CONSOLE,
                                nullptr, directory().c_str(), &startup, &info), L"Cannot open the progress console.");
            Handle process(info.hProcess), thread(info.hThread);
            AllowSetForegroundWindow(info.dwProcessId);
            // Do not release the launch lock until the new process publishes its
            // lease. An early exit leaves the queued jobs available for retry.
            for (int ready = 0; ready < 100; ++ready) {
                if (console_owned(root)) return 0;
                if (WaitForSingleObject(process.value, 100) == WAIT_OBJECT_0)
                    throw Failure{L"The ltxk console closed before it was ready. Open ltxk.exe to inspect the queue."};
            }
            throw Failure{L"The console is taking longer than expected to start. Open ltxk.exe to inspect the queue."};
        }
        Sleep(50);
    }
    throw Failure{L"Another console is starting. Open ltxk.exe to inspect the queue."};
}
}

int WINAPI wWinMain(HINSTANCE, HINSTANCE, PWSTR, int) {
    const auto apartment = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    std::wstring request;
    int result = 1;
    try {
        int count = 0;
        auto args = CommandLineToArgvW(GetCommandLineW(), &count);
        ltxk::check(args != nullptr, L"Cannot read launcher arguments.");
        const bool valid = count == 1 || (count == 3 && std::wstring_view(args[1]) == L"--request");
        if (count == 3 && valid) request = args[2];
        LocalFree(args);
        if (!valid) throw ltxk::Failure{L"Use Run ltxk from a recording's right-click menu, or open ltxk-launch.exe."};
        // Only delete request files created in our own queue's requests folder.
        if (!request.empty()) {
            const auto parent = std::filesystem::path(request).parent_path().lexically_normal();
            if (parent != std::filesystem::path(ltxk::queue_root() + L"\\requests").lexically_normal())
                throw ltxk::Failure{L"The Explorer request is outside the queue's requests folder."};
        }
        result = run(request);
    } catch (const ltxk::Failure& error) { ltxk::error_box(error.message); }
    catch (...) { ltxk::error_box(L"The launcher could not open the queue. Open ltxk.exe to see progress."); }
    if (!request.empty()) {
        // Recheck the parent before cleanup, including on malformed arguments.
        try {
            if (std::filesystem::path(request).parent_path().lexically_normal() ==
                std::filesystem::path(ltxk::queue_root() + L"\\requests").lexically_normal()) DeleteFileW(request.c_str());
        } catch (...) {}
    }
    if (SUCCEEDED(apartment)) CoUninitialize();
    return result;
}
