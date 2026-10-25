# ltx-klusta

LTX stands for **legacy tetrode x**. The command is **`ltxk`**.

Rust feature extraction and classic KlustaKwik clustering for legacy tetrode
recordings. This combines the old `prekk.exe`, `KlustaKwik.exe`, and `runkk.bat`
workflow into one executable: `ltxk.exe` on Windows x64 or `ltxk` on macOS
Apple Silicon. No numerical DLLs are required. LAPACK is an optional runtime
provider for PCA.

The clustering port is ready for evaluation on representative recordings.
See [CLUSTERING.md](CLUSTERING.md) for the exact supported algorithm, numerical
compatibility, memory/storage requirements, and validation performed so far.
See [PERFORMANCE.md](PERFORMANCE.md) for profiling and optimisations that retain
the current calculations.

## Cluster recordings

```sh
ltxk cluster recording.1
ltxk cluster recording.1 recording.2 --backend auto
ltxk cluster recording.1 --force
ltxk --help
ltxk cluster --help
ltxk --completions powershell
```

Drag recordings onto the Windows executable or a shortcut to it to run the
same clustering operation with default settings. Paths may contain spaces,
Unicode, and punctuation. Results are written beside each input:
`recording.1` becomes `recording.clu.1`. Existing results are skipped unless
`--force` is given. A failed job leaves any previous output intact; new output
is published atomically after it is complete.

Defaults match the original fancykk batch file: `3PC`, classic unmasked
clustering, initial cluster counts 20–30, seed 1, a 25,000-spike fitting target,
and four simultaneous recordings. PCA uses every spike. Clustering fits a
stratified subset for large recordings and assigns **all** spikes using the
fitted model. Its historical floor rule can fit up to 49,999 spikes; nine
million spikes use exactly 25,000 fitting spikes. Full-record assignment is
streamed without allocating a full spikes-by-clusters array.

## Shared queue

Use the persistent per-user queue when jobs arrive through separate Explorer
invocations:

```sh
ltxk enqueue recording.1 recording.2 --background
ltxk status
ltxk status --json
ltxk resume
```

`enqueue` without `--background` participates in the queue in the foreground.
One coordinator processes at most four jobs at once by default, even when
several processes submit recordings. Its `--jobs` value sets the active limit.
Duplicate pending/running recordings share one job. Completed jobs can be
submitted again; existing `.clu.N` files are still skipped by default.

Each job has a durable pending/running/done/failed record and a diagnostic log.
A failed recording frees its worker slot and does not block subsequent jobs.
After a coordinator is interrupted, `resume` or another `enqueue` recovers its
running jobs and clears their old private scratch files before retrying them.
Queue history and logs remain available through `status`.

The default queue is `%LOCALAPPDATA%\ltxk\queue` on Windows and
`~/Library/Application Support/ltxk/queue` on macOS. `--queue-dir PATH`
overrides it on `enqueue`, `status`, and `resume`.

## Configuration

Copy `ltxk.example.json` to `ltxk.json` beside the executable to customize
the same settings previously edited at the top of the batch file:

```json
{
  "features": "3PC",
  "cluster": {
    "min_clusters": 20,
    "max_clusters": 30,
    "target_spikes": 25000,
    "seed": 1,
    "max_iterations": 10000
  },
  "backend": "nalgebra",
  "jobs": 4
}
```

Missing settings use defaults. Unknown or invalid settings are reported.
`--config PATH` selects another JSON file; explicit command-line options
override it. Configuration paths such as `work_dir` and `lapack_library` are
resolved relative to the configuration file. Queued jobs retain the settings
with which they were submitted.

`--work-dir PATH` chooses an existing scratch folder, useful when a large
recording needs temporary storage on another disk. Ordinary command-line jobs
use private temporary directories, and queued jobs use private directories
under the queue folder. The fitting target may be reduced with
`--target-spikes`; this changes the clustering approximation. Waveform
`--block-spikes` changes PCA memory/throughput while preserving its mathematics.

## Extract features only

The former prekk operation is available as a subcommand:

```sh
ltxk features recording.1 temporary/recording temporary/count.txt 3PC
ltxk features recording.1 temporary/recording temporary/count.txt 3PC,A,VT --backend auto
```

Create the output directories first. This writes `OUTPUT_BASE.fet.N` and a
count file. Features are `0PC`–`9PC`, amplitude `A`, and voltage at the sample
of maximum variance `VT`. Select at least one feature. Column order is all
active channels' PCs, then all amplitudes, then all VT values. Constant channels
are omitted. Values have six significant digits in scientific notation.

The old four-argument top-level invocation has moved to `features`; the old
batch file should be replaced with the combined `cluster` or `enqueue` operation.

## Numerical backend

| Option | Behaviour |
| --- | --- |
| `--backend nalgebra` | Default: pure Rust calculations. |
| `--backend auto` | Use a compatible LAPACK provider if available, otherwise nalgebra. |
| `--backend lapack` | Require a compatible provider. |
| `--lapack-library PATH` | Select a provider explicitly; implies LAPACK unless a backend is specified. |

An explicit library path must work, including in `auto` mode. It cannot be
combined with `--backend nalgebra`. The selected provider is logged.
`LTXK_LAPACK_LIBRARY` supplies a preferred discovery path.

macOS discovery includes the system Accelerate framework. Windows searches
beside the executable and on PATH for LP64 OpenBLAS/LAPACK providers, including
existing `lapack_win64_MT.dll` + `blas_win64_MT.dll` pairs. Providers must use
32-bit Fortran integer arguments; known ILP64 OpenBLAS libraries are rejected.
Keep a provider's dependent DLLs beside it. No provider or DLL is bundled.

For each channel, PCA accumulates a global 50 × 50 covariance matrix from
50-sample waveforms, using BLAS `DSYRK` when available. LAPACK uses `DSYEVD`,
with a `DSYEV` retry on non-convergence. Clustering uses Rust full-covariance
calculations on the much smaller feature space.

The default 4,096-spike block supplies a 50 × 4,096 double matrix per channel
(1.56 MiB). Larger blocks are not automatically faster. The optional local
benchmark is:

```sh
cargo test --release covariance_block_benchmark -- --ignored --nocapture
```

Repeated eigenvalues can yield different valid PCA bases, and floating-point
differences can affect later clustering decisions. Pin the numerical backend
and seed when reproducibility matters.

## Windows 11 context menu

The old registry command appears under **Show more options**. A command in the
first Windows 11 menu requires an `IExplorerCommand` COM extension and package
identity, normally supplied by an optional signed sparse MSIX package. This is
an installed integration component; the portable clustering executable remains
independent. See [Microsoft's integration guide](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/integrate-packaged-app-with-file-explorer).

This branch provides the safe multi-file queue command that an extension can
invoke, but does not yet install a modern Explorer extension. Such an extension
should filter numeric recording extensions, enumerate the selection, and call
`ltxk enqueue --background` with individual paths, without a command shell.
It must keep menu construction fast and leave processing to the queue.
GitHub's Windows runner can test its COM and package behaviour, but visual menu
placement still needs a Windows 11 desktop check.

For the existing menu, **Shift+F10** opens the classic menu directly without
clicking Show more options. See [Microsoft's menu guidance](https://blogs.windows.com/blog/2021/07/19/extending-the-context-menu-and-share-dialog-in-windows-11/).

## Build, test, and releases

```sh
cargo build --release --locked
cargo test --release --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
python3 tests/check_features.py --exe target/release/ltxk
python3 tests/check_rust.py --exe target/release/ltxk
python3 tests/check_cluster.py --exe target/release/ltxk
```

Use a current stable Rust toolchain. Windows executables have the `.exe` suffix
and use a static C runtime. No numerical library is needed to build or run.
The input format is four channels per spike, each with a 4-byte timestamp and
50 signed 8-bit samples. Supported counts are 2 through 2,147,483,647; nine
million is below this bound. The count guard is not a guarantee of runtime or
available disk capacity at its upper extreme.

The `Build` workflow builds and tests the packaged executable on Windows x64
and macOS Apple Silicon with both numerical backends. It checks PCA mathematics,
CLI/completions, full clustering and subsets, output safety, shared queue failures
and crash recovery, and execution without installed numerical libraries.
Windows CI downloads a checksum-pinned OpenBLAS provider for tests only.

The optional **large_recording** workflow checkbox runs a nine-million-spike
**feature extraction** memory test on Windows. It is off by default and does
not run the full clustering search. Routine CI uses small synthetic recordings.

Successful main builds publish [GitHub releases](https://github.com/ChenNeuroscience/ltx-klusta/releases)
tagged with the source commit ID. Each platform has one ZIP plus its checksum.
Packages include the executable, documentation, example settings, build
metadata, dependency notices, and file checksums. PR builds provide test
artifacts. For a macOS Actions artifact, run `chmod +x ltxk` after extracting;
release ZIPs preserve the executable permission automatically.
