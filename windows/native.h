#pragma once
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <shellapi.h>
#include <shlobj.h>
#include <shlwapi.h>
#include <wrl/client.h>
#include <filesystem>
#include <string>
#include <vector>
#include "shell_util.h"

namespace ltxk {
using Microsoft::WRL::ComPtr;
struct Failure { std::wstring message; };
inline void check(bool ok, const wchar_t* message) {
    if (ok) return;
    const auto code = GetLastError();
    wchar_t* detail = nullptr;
    FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
                   FORMAT_MESSAGE_IGNORE_INSERTS, nullptr, code, 0,
                   reinterpret_cast<wchar_t*>(&detail), 0, nullptr);
    std::wstring text(message);
    if (detail) { text += L"\n"; text += detail; LocalFree(detail); }
    throw Failure{text};
}
inline void hcheck(HRESULT result, const wchar_t* message) {
    if (FAILED(result)) { SetLastError(static_cast<DWORD>(result)); check(false, message); }
}
struct Handle {
    HANDLE value = INVALID_HANDLE_VALUE;
    Handle() = default;
    explicit Handle(HANDLE handle) : value(handle) {}
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
    ~Handle() { if (valid()) CloseHandle(value); }
    bool valid() const { return value && value != INVALID_HANDLE_VALUE; }
};
inline std::wstring directory(HMODULE module = nullptr) {
    std::vector<wchar_t> buffer(32768);
    const DWORD length = GetModuleFileNameW(module, buffer.data(), static_cast<DWORD>(buffer.size()));
    check(length && length < buffer.size(), L"Cannot locate the ltxk installation.");
    const std::wstring path(buffer.data(), length);
    return path.substr(0, path.find_last_of(L'\\'));
}
inline std::wstring queue_root() {
    // Match Queue::default_path, including environment overrides in tests.
    std::vector<wchar_t> local(32768);
    const auto length = GetEnvironmentVariableW(L"LOCALAPPDATA", local.data(), static_cast<DWORD>(local.size()));
    check(length && length < local.size(), L"LOCALAPPDATA is not set.");
    std::wstring root = std::wstring(local.data(), length) + L"\\ltxk\\queue";
    const int result = SHCreateDirectoryExW(nullptr, root.c_str(), nullptr);
    SetLastError(static_cast<DWORD>(result));
    check(result == ERROR_SUCCESS || result == ERROR_ALREADY_EXISTS || result == ERROR_FILE_EXISTS,
          L"Cannot create the ltxk queue folder.");
    return root;
}
inline std::string utf8(std::wstring_view value) {
    const int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                       static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
    check(size > 0, L"The selected path cannot be encoded as Unicode.");
    std::string result(size, '\0');
    check(WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
                             result.data(), size, nullptr, nullptr) == size, L"Cannot encode the selected path.");
    return result;
}
inline void write_file(const std::wstring& path, std::string_view text, DWORD disposition = CREATE_NEW) {
    Handle file(CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr, disposition, FILE_ATTRIBUTE_NORMAL, nullptr));
    check(file.valid(), L"Cannot write the Explorer selection.");
    DWORD written = 0;
    check(text.size() <= MAXDWORD && WriteFile(file.value, text.data(), static_cast<DWORD>(text.size()),
          &written, nullptr) && written == text.size(), L"Cannot save the Explorer selection.");
}
inline void error_box(const std::wstring& text) {
    MessageBoxW(nullptr, text.c_str(), L"ltx-klusta", MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
}
}
