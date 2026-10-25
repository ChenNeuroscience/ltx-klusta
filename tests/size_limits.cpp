#include "size_checks.h"
#include <iostream>

int main() {
    static_assert(sizeof(void*) == 8, "Tests must run in a 64-bit process");
    const struct { std::int64_t spikes; std::size_t bytes; } cases[] = {
        {2, 432},
        {9000000, 1944000000ULL},
        {9942053, 2147483448ULL},
        {9942054, 2147483664ULL},
        {10000000, 2160000000ULL},
        {20000000, 4320000000ULL},
    };
    for (const auto& item : cases) {
        if (prekk::checked_buffer_size(item.spikes) != item.bytes) {
            std::cerr << "Incorrect payload size for " << item.spikes << " spikes\n";
            return 1;
        }
    }
    for (const auto spikes : {std::int64_t(-1), std::int64_t(0), std::int64_t(1),
                             std::int64_t(2147483648LL)}) {
        try {
            prekk::checked_buffer_size(spikes);
            std::cerr << "Accepted unsupported count: " << spikes << '\n';
            return 1;
        } catch (const std::logic_error&) {}
    }
    return 0;
}
