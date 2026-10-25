# prekk

The experimental Rust rewrite is documented in [RUST.md](RUST.md), including
the CLI, optional LAPACK backend, streaming design, and native Windows/macOS CI.
The notes below describe the retained C++ reference and its previous release.

Extract KlustaKwik features from Axona tetrode files: principal components,
amplitude (`A`), and voltage at the sample with maximum variance (`VT`).
Input records contain four channels, each with a 4-byte timestamp and 50 signed
8-bit voltage samples. The existing command line and feature ordering are retained.

## Windows x64 download

Open this repository's [Releases](https://github.com/ChenNeuroscience/prekk/releases)
and download `prekk-windows-x64.zip` from the latest release. Each release is
tagged with the short Git commit ID of its source, with a ZIP checksum provided
as a separate `.sha256` asset. Unzip the download before running.

The workflow builds and tests on pull requests and pushes to `main`. Successful
pushes to `main` publish a release automatically. It also supports **Actions →
Build Windows x64 → Run workflow** after it is on the default branch; a successful
manual run publishes the selected commit. Pull requests do not publish releases.
The `prekk-windows-x64` Actions artifact remains available for testing PR builds.

The download contains `prekk.exe`, `blas_win64_MT.dll`, `lapack_win64_MT.dll`,
this README, and SHA-256 checksums. Keep all three executable/library files
together. Back up the old executable, then copy these three files into your
existing `fancykk` directory. The existing `runkk.bat` command line still applies:

```bat
prekk.exe "C:\data\recording.1" "C:\temp\recording" "C:\temp\spike-count.txt" 3PC
```

This writes `C:\temp\recording.fet.1` and the spike-count file.
The Windows tests execute feature extraction with the packaged x64 libraries;
CI also checks the PE architecture of the executable.

## Memory and spike counts

The target recording size for this build is **up to 9 million spikes**, subject
to available RAM. A 64-bit operating system alone does not remove the address
limit of an old 32-bit `prekk.exe`.

The original calculation `long bufferSize = BYTES_PER_SPIKE*nSpikes` multiplies
two 32-bit `int` values before assigning to `long`. It overflows at 9,942,054
spikes (216 bytes each), even if compiled for x64. Windows `long` is also 32-bit.
This build uses checked 64-bit counts and buffer sizes and explicitly requires
64-bit pointers and Armadillo indices. Regression checks cover 9 million spikes,
the old signed overflow boundary, and payload sizes over 4 GiB without allocating
those payloads.

Nine million spikes have a raw payload of 1,944,000,000 bytes (1.81 GiB). PCA
needs considerably more memory: each 9,000,000 × 50 double matrix alone is
3,600,000,000 bytes (3.35 GiB), and this implementation holds several waveform
and PCA matrices at once. This change removes the 32-bit process ceiling; it
does not make PCA stream data or guarantee that a 9-million-spike run fits a
particular machine. The feature tests use small synthetic recordings; a complete
9-million-spike PCA run is not part of CI.

The numerical libraries still use 32-bit BLAS dimensions. Counts above
2,147,483,647 are explicitly rejected. Invalid headers, short payloads, allocation
failures, and output failures produce an error and a nonzero exit status.

## KlustaKwik

The separate `KlustaKwik.exe` does not need rebuilding just to replace a 32-bit
prekk. The examined `fancykk` distribution already has an x64 KlustaKwik.
Its batch file uses classic mode, 20–30 starting clusters, the executable's
default maximum of 100 possible clusters, and a `Subset` factor derived from
25,000 spikes. That subsampling controls clustering work, not prekk's input size.

Older KlustaKwik source uses 32-bit array indices even in x64 builds. For an
`N × MaxPossibleClusters` array, a signed 32-bit element index reaches its limit
at approximately `2147483647 / MaxPossibleClusters` spikes: about 21.47 million
at 100 clusters. Nine million is below that boundary. Increasing the cluster
limit changes it, and KlustaKwik can still exhaust physical RAM.

Upstream replaced those indices in [September 2014](https://github.com/klusta-team/klustakwik/commit/3208a6fba25de34ef510a87d12f1e4f76fff25e8)
and later added a configurable [`RamLimitGB`](https://github.com/klusta-team/klustakwik/blob/d8b750107fef53996957d774dfa3ec71f2442462/parameters.h).
The examined executable has a June 2014 PE timestamp and no `RamLimitGB` option
in its embedded parameter list; its bundled readme describes a modified upstream
build. Its exact source revision is unknown. Do not assume that current upstream
options apply to that binary. No KlustaKwik fork is required for the current
9-million-spike target based on the identified limits and existing settings.

## Build from source

Windows, with Visual Studio C++ tools and CMake installed:

```bat
cmake -S . -B build -A x64
cmake --build build --config Release --parallel
ctest --test-dir build -C Release --output-on-failure
```

The resulting executable and matching DLLs are in `build/Release`.
Linux requires BLAS/LAPACK development libraries; macOS uses Accelerate:

```sh
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build --parallel
ctest --test-dir build --output-on-failure
```

Tests require Python 3 and use only its standard library. They check feature
ordering, signed samples, inactive channels, PCA centering/orthogonality, amplitude,
VT, filenames containing spaces, malformed inputs, and large-count arithmetic.
`tests/check_features.py --exe PATH --baseline OLD_PATH` additionally compares
feature values with an existing executable on the same platform.
