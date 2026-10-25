#pragma once

#include <cstddef>
#include <cstdint>
#include <ios>
#include <limits>
#include <stdexcept>

namespace prekk {

// Each Axona spike has four channels of a 4-byte timestamp and 50 samples.
constexpr std::size_t bytes_per_spike = 4 * (4 + 50);

inline std::size_t checked_buffer_size(std::int64_t spikes) {
    if (spikes < 2)
        throw std::invalid_argument("num_spikes must be at least 2");
    // The bundled BLAS uses 32-bit dimensions even in an x64 process.
    if (spikes > (std::numeric_limits<int>::max)())
        throw std::length_error("num_spikes exceeds the BLAS dimension limit (2147483647)");
    const auto count = static_cast<std::uint64_t>(spikes);
    if (count > (std::numeric_limits<std::size_t>::max)() / bytes_per_spike ||
        count > static_cast<std::uint64_t>((std::numeric_limits<std::streamsize>::max)()) / bytes_per_spike)
        throw std::length_error("Spike data exceeds the supported buffer/read size");
    return static_cast<std::size_t>(count) * bytes_per_spike;
}

} // namespace prekk
