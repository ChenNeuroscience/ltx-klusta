# Rust prekk (experimental branch)

This version preserves the four positional arguments and feature ordering of C++
prekk, while reading the tetrode recording twice in fixed-size blocks. The first
pass accumulates channel statistics and 50 × 50 covariance matrices; the second
projects waveforms and writes `.fet.N` rows directly to disk. RAM use is bounded
by block size, rather than the total spike count. The default is 4,096 spikes per
block. All input waveforms are signed 8-bit samples and calculations use `f64`.

## CLI

The command line uses `clap`, including generated help/version output, validated
options, spelling suggestions, and shell completion generation:

```sh
prekk recording.1 temporary/recording temporary/count.txt 3PC
prekk recording.1 temporary/recording temporary/count.txt 3PC,A,VT --block-spikes 8192
prekk --help
prekk --version
prekk --completions powershell
prekk --completions zsh > _prekk
```

The count and feature output directories must already exist. Input/output paths
may contain Unicode or spaces. Features may be specified in any order; output
always contains each active channel's PCs, followed by all amplitudes, then all
VT values, matching the original program. Text uses six significant digits in
scientific notation, which KlustaKwik accepts. Constant channels are omitted.
Counts remain limited to 2,147,483,647 for compatibility; nine million is below
this guard and does not require allocating an array of that length.

## Numerical builds and Windows DLLs

There are two builds, both using the same streaming reader and feature writer:

| Build | Covariance/PCA | Runtime numerical libraries |
| --- | --- | --- |
| `nalgebra` (default Cargo build) | Pure Rust matrix multiplication and symmetric eigensolver | None |
| `lapack` (`--features lapack`) | BLAS `DSYRK` and LAPACK `DSYEVD`, with `DSYEV` on convergence failure | Windows: the two bundled x64 DLLs; macOS: system Accelerate |

Choose the `nalgebra` Windows package for an executable without external
numerical DLLs. Rust's Windows C runtime is linked statically as well; Windows
system DLLs remain ordinary OS dependencies. The LAPACK executable defaults to
LAPACK but can select `--backend nalgebra` for comparison. It still needs the
DLLs to start, because those imports are resolved by Windows before argument
parsing. A pure Rust build cannot select `--backend lapack`.

The repository's `.lib` files are **import libraries**, not static copies of
BLAS/LAPACK. For the LAPACK Windows build, keep `prekk.exe`,
`blas_win64_MT.dll`, and `lapack_win64_MT.dll` together. The build packages them
automatically. True static LAPACK linking would require a different library
build; enabling Rust's static C runtime does not embed these DLLs.

On macOS, `--features lapack` links Apple's Accelerate framework, already supplied
by the OS. Linux LAPACK builds link system `liblapack` and `libblas` and require
their development packages at build time and runtime libraries at execution.

## Build and check

With a current stable Rust toolchain:

```sh
cargo build --release --locked
cargo test --locked
cargo build --release --locked --features lapack
cargo test --locked --features lapack
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
```

The executable is `target/release/prekk` (`prekk.exe` on Windows). Building one
variant after the other replaces that output; use `CARGO_TARGET_DIR` for separate
build directories if you want both locally. `Cargo.lock` fixes dependency versions.

C++ sources, bundled Armadillo, and CMake are retained as a numerical reference.
To compare the Rust result with a compiled C++ executable:

```sh
python3 tests/check_features.py --exe target/release/prekk --baseline PATH_TO_CPP --block-spikes 7
python3 tests/check_rust.py --exe target/release/prekk --baseline PATH_TO_CPP
```

`Build Rust` in GitHub Actions builds and **executes** both variants on Windows
x64 and macOS Apple Silicon. Each job compiles C++ on the same runner and compares
features, tests the CLI, chunk boundaries, rank-deficient/repeated-eigenvalue PCA,
Unicode paths, and input/output alias protection. Windows packages run outside
the checkout with a restricted DLL search PATH, so tests cannot accidentally
find numerical DLLs left in the source tree. CI also checks PE imports and file
checksums. Artifacts are named `prekk-rust-PLATFORM-BACKEND`.

For a macOS Actions artifact, run `chmod +x prekk` after extracting it, because
GitHub resets artifact file permissions. Published release ZIPs restore the
executable permission automatically.

Routine CI uses small synthetic recordings. The **large_recording** checkbox in
**Run workflow** opts into the full 9-million-spike test on both Windows builds,
including a 256 MiB peak-working-set ceiling. It is off by default. The same test
can be run locally with `python3 tests/check_large.py --exe PATH --baseline CPP`;
`--spikes 100000` selects a smaller benchmark (a multiple of 250).

After this branch is merged, successful pushes or manual builds of main publish commit-tagged
releases containing both Windows packages and both Apple Silicon packages, with
ZIP checksums. PRs and manual branch builds produce test artifacts only. The current C++ release on main
remains available while this branch is evaluated.

## Numerical compatibility

Eigenvectors are sorted by decreasing eigenvalue and given the original sign
convention (largest absolute loading positive). For distinct eigenvalues, the
test suite compares feature values with the C++ implementation. Repeated
eigenvalues admit rotations of their eigenvectors, so those tests compare the
retained PCA subspace instead of requiring an arbitrary vector basis to match.
This is not a guarantee of byte-identical clustering: floating-point differences
and valid rotations can still affect a downstream algorithm. Compare a known
recording before adopting the experimental build for analysis.

The LAPACK build preserves Armadillo's fallback from divide-and-conquer `DSYEVD`
to QR-based `DSYEV` when the first solver cannot converge. The original covariance
is restored for that retry, because both routines overwrite their input matrix.
