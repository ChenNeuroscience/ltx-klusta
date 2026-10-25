# prekk

Stream Axona tetrode voltage recordings into KlustaKwik feature files using Rust.
Each recording is read twice in fixed-size blocks, so RAM use depends on the
block size rather than the total spike count.

There is **one executable per platform**, with no required numerical DLLs.
Windows downloads contain `prekk.exe`; Apple Silicon downloads contain `prekk`.
Both support pure Rust calculations and optional LAPACK loaded at runtime.

## Usage

The existing four positional arguments remain compatible with `runkk.bat`:

```sh
prekk recording.1 temporary/recording temporary/count.txt 3PC
prekk recording.1 temporary/recording temporary/count.txt 3PC,A,VT --block-spikes 8192
prekk --help
prekk --version
prekk --completions powershell
prekk --completions zsh > _prekk
```

Create the output directories first. Paths may contain Unicode and spaces.
The output is `OUTPUT_BASE.fet.N`, where N is the input file's numeric extension.
The count file contains only the number of spikes.

Features are `0PC`–`9PC`, amplitude `A`, and voltage at the sample of maximum
variance `VT`. Select at least one feature. Output order is all active channels'
PCs, then all amplitudes, then all VT values, irrespective of argument order.
Constant channels are omitted. Feature values use six significant digits in
scientific notation.

## Numerical backend

| Option | Behaviour |
| --- | --- |
| `--backend nalgebra` | Default. Pure Rust; no numerical library is loaded. |
| `--backend auto` | Use an available compatible LAPACK provider, otherwise nalgebra. |
| `--backend lapack` | Require LAPACK; report an error if no compatible provider is found. |
| `--lapack-library PATH` | Load a specific provider; implies LAPACK unless a backend is specified. |

For example:

```sh
prekk recording.1 temporary/recording temporary/count.txt 3PC --backend auto
prekk recording.1 temporary/recording temporary/count.txt 3PC --lapack-library /path/to/library
```

An explicit library path must work, including with `--backend auto`. It cannot
be combined with `--backend nalgebra`. The selected backend and provider path
are printed to standard error for reproducibility.

On macOS, automatic discovery includes the system Accelerate framework.
On Windows it searches beside the executable and on PATH for compatible
OpenBLAS or LAPACK providers. Existing `lapack_win64_MT.dll` and
`blas_win64_MT.dll` pairs are also recognized, but are neither included nor
needed for the default backend. Linux discovery includes system OpenBLAS and
LAPACK shared libraries. `PREKK_LAPACK_LIBRARY` supplies a preferred provider path.

Providers must expose the Fortran **LP64** interface (32-bit integer arguments).
Known ILP64 OpenBLAS providers are rejected. Keep any provider's own dependent
libraries beside it. LAPACK support uses BLAS `DSYRK` to accumulate covariance,
then `DSYEVD` for PCA, with a `DSYEV` retry on convergence failure. The original
covariance is restored before retrying.

## Matrix size and batching

Each channel's waveform has 50 signed 8-bit time samples. Consequently, its PCA
covariance matrix is **50 × 50**; this dimension comes from the waveform data.

The throughput tuning parameter is `--block-spikes`, not the covariance size.
The default 4,096-spike block supplies a 50 × 4,096 matrix of doubles per channel
(1.56 MiB) to the covariance calculation. Larger blocks are not automatically
faster: matrix shape, cache behaviour, backend, and file/output costs all matter.
The default is a starting point, not a claim of optimality.

Run the optional kernel benchmark locally to examine block sizes:

```sh
cargo test --release covariance_block_benchmark -- --ignored --nocapture
```

It prints throughput for 512, 2,048, 4,096, 8,192, and 16,384 spikes per block,
using nalgebra and any discovered LAPACK provider. It excludes recording I/O
and text formatting. Routine CI does not run this benchmark.

The reader expects four channels per spike, each containing a 4-byte timestamp
and 50 signed samples. Counts must be at least two and at most 2,147,483,647.
Nine million spikes are below this guard; the reader allocates no array of that
length.

## Build and test

With a current stable Rust toolchain:

```sh
cargo build --release --locked
cargo test --release --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
python3 tests/check_features.py --exe target/release/prekk
python3 tests/check_rust.py --exe target/release/prekk
```

On Windows the executable has the `.exe` suffix. No LAPACK installation is
needed to build or run the default backend, and Windows uses a static C runtime.
`Cargo.lock` fixes dependency versions.

The `Build` workflow builds and executes the same program on Windows x64 and
macOS Apple Silicon. Tests cover mathematical feature properties, covariance
products, eigenvector residuals/orthogonality, rank-deficient and tied-eigenvalue
PCA, chunk boundaries, CLI/completions, Unicode paths, output safety, and runtime
provider selection. Windows CI downloads a checksum-pinned LP64 OpenBLAS provider
only for testing; it is never included in the package. There is no separate
native build and no startup BLAS/LAPACK import.

The optional **large_recording** checkbox in **Run workflow** runs the full
9-million-spike Windows test with a 256 MiB peak-working-set ceiling. It is off
by default. A smaller local check is:

```sh
python3 tests/check_large.py --exe target/release/prekk --spikes 100000
```

## Downloads and releases

Successful main builds publish [GitHub releases](https://github.com/ChenNeuroscience/prekk/releases)
tagged with the short source commit ID. Each release contains one ZIP per
platform plus ZIP checksums. Packages include the executable, this README, build
metadata, dependency notices, and file checksums; no numerical DLLs are bundled.

PR and manual branch runs provide test artifacts. For a macOS Actions artifact,
run `chmod +x prekk` after extraction because GitHub resets artifact permissions.
Published release ZIPs restore the executable permission automatically.

Eigenvectors are sorted by decreasing eigenvalue with their largest absolute
loading positive. Repeated eigenvalues can yield different valid bases, and
floating-point results can differ across backends. Pin the backend when numerical
reproducibility matters for an analysis.
