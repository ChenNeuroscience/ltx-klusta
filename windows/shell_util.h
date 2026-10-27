#pragma once
#include <algorithm>
#include <string>
#include <string_view>

namespace ltxk {
// Windows passes one selection to Invoke. Hide the verb for mixed selections
// and for Axona sidecars that also happen to end in a tetrode number.
inline bool recording_name(std::wstring_view path) {
    const auto slash = path.find_last_of(L"/\\");
    const auto name = path.substr(slash == path.npos ? 0 : slash + 1);
    const auto dot = name.find_last_of(L'.');
    if (dot == name.npos || dot == 0 || dot + 1 == name.size()) return false;
    for (const auto c : name.substr(dot + 1)) if (c < L'0' || c > L'9') return false;
    auto stem = name.substr(0, dot);
    const auto previous = stem.find_last_of(L'.');
    if (previous == stem.npos) return true;
    std::wstring kind(stem.substr(previous + 1));
    std::transform(kind.begin(), kind.end(), kind.begin(), [](wchar_t c) {
        return static_cast<wchar_t>(c >= L'A' && c <= L'Z' ? c + (L'a' - L'A') : c);
    });
    return kind != L"clu" && kind != L"fet" && kind != L"res" && kind != L"spk";
}

// Quote one argv element using the Windows CRT rules, without a shell.
inline std::wstring quote(std::wstring_view arg) {
    std::wstring result = L"\"";
    size_t slashes = 0;
    for (const auto c : arg) {
        if (c == L'\\') { ++slashes; continue; }
        result.append(slashes * (c == L'"' ? 2 : 1), L'\\');
        slashes = 0;
        if (c == L'"') result += L'\\';
        result += c;
    }
    result.append(slashes * 2, L'\\');
    return result + L'"';
}

inline std::string json_string(std::string_view value) {
    std::string out = "\"";
    constexpr char hex[] = "0123456789abcdef";
    for (const unsigned char c : value) {
        if (c == '"' || c == '\\') { out += '\\'; out += static_cast<char>(c); }
        else if (c < 32) { out += "\\u00"; out += hex[c >> 4]; out += hex[c & 15]; }
        else out += static_cast<char>(c);
    }
    return out + '"';
}
}
