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

Clustering defaults match the original fancykk batch file: `3PC`, classic unmasked
clustering, initial cluster counts 20–30, seed 1, and a 25,000-spike fitting target.
Recording concurrency is chosen automatically. PCA uses every spike. Clustering fits a
stratified subset for large recordings and assigns **all** spikes using the
fitted model. Its historical floor rule can fit up to 49,999 spikes; nine
million spikes use exactly 25,000 fitting spikes. Full-record assignment is
streamed without allocating a full spikes-by-clusters array.

## Shared queue

Use the persistent per-user queue when jobs arrive through separate Explorer
invocations:

```sh
ltxk enqueue recording.1 recording.2 --console
ltxk status
ltxk status --json
ltxk resume --console
```

`--console` opens a live terminal dashboard, with queue totals and one row per
recording. Rows show the processing stage, current trial, iteration, signal
cluster count, and elapsed time. Select a row with the arrow keys to see its
full path and local start time. Elapsed time starts when a worker picks up the
file, resets if an interrupted job is retried, and freezes when it finishes.
Press **L** to open the selected file's log in a text editor, or **O** to open
the logs folder. Trial/iteration counts describe the current fit; they do not
predict completion time. The dashboard remains open until **Enter** or **Q**
after processing finishes, including when a recording fails.

There is one active console per queue. Subsequent submissions join that queue
and return immediately; progress appears in the existing console. A console
can also follow an already running background coordinator. When processing
finishes, new submissions can start a fresh console while the old summary
stays open. Closing an active window or pressing **Ctrl+C** interrupts its
workers; the next launch recovers unfinished jobs. Closing a console that is
following a separate background coordinator leaves that coordinator running.

Without an interactive terminal, `--console` prints plain progress and job
logs and exits on completion. `enqueue` without `--console` or `--background`
participates in the queue with plain foreground output. Use `--background`
explicitly when detached processing is wanted.
One coordinator enforces the worker limit across separate submissions.
By default, `auto` uses about 75% of the available CPU threads, leaving at
least one unused by the worker pool when multiple CPUs are available, with a
maximum of eight simultaneous files.
The OS still schedules threads normally; CPU cores are not pinned or reserved.
For example, eight available CPUs select six workers; four select three.
If CPU detection is unavailable, the fallback is one worker.

Use `--jobs 4` or a numeric `jobs` setting for a fixed limit (1–16), or
`--jobs auto` / `"jobs": "auto"` for automatic selection. Omitting the setting
also selects auto. The dashboard shows the chosen worker maximum. The limit
stays fixed for the active coordinator; later submissions do not change it.
`resume` reads the same configuration when `--jobs` is omitted.

The default nalgebra backend mostly uses one CPU thread per file. A native
BLAS/LAPACK provider can use additional threads during PCA. Autosizing is a CPU
budget, not a live RAM/load controller; reduce `--jobs` when you want more
headroom or are limited by the recording/scratch disk's throughput.
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

## Replace the old fancykk launcher on Windows

Download the Windows ZIP from [Releases](https://github.com/ChenNeuroscience/ltx-klusta/releases)
and extract it. It includes `ltxk.exe`, `runkk.bat`, and `open-logs.bat`.

1. Replace the old `runkk.bat` with the supplied file and put `ltxk.exe` in the
   same folder. The new batch file replaces the whole old script, including
   its calls to prekk and KlustaKwik and its temporary-file/queue management.
2. If your existing context-menu entry points to that batch-file path, keep
   using it. Remove any custom shortcut's minimized launch setting to keep
   the console visible. No new context-menu installation is needed.
3. Drop recordings onto **runkk.bat**, or select the existing right-click
   action. A visible console processes the shared queue. Double-click
   **runkk.bat** without files to reopen/resume it. **open-logs.bat** opens the
   default queue's log folder independently of the console.

Move any customized batch settings into `ltxk.json` beside the executable
(see the example below):

| Old batch setting | New JSON setting |
| --- | --- |
| `prekk_flags` (e.g. `3PC`) | `features` |
| Initial cluster range | `cluster.min_clusters`, `cluster.max_clusters` |
| Target fitting spikes | `cluster.target_spikes` |
| Concurrent processes | `jobs` |

The defaults already match the original fancykk workflow. Old KlustaKwik
command-line arguments are not accepted by `ltxk`; use the documented settings.
Dropping files directly onto `ltxk.exe` also works, but each such invocation
runs independently. Use the batch launcher for the shared queue/dashboard.

A custom registry command can call the executable directly:
`"C:\path\to\ltxk.exe" enqueue --console "%1"`.
The portable launcher uses the existing classic Windows menu.

## Preview the launcher on macOS

From a source checkout, build the binary and create a local Finder droplet:

```sh
cargo build --release --locked
python3 scripts/create_macos_preview.py --exe target/release/ltxk --output "../ltx-klusta Preview.app"
```

Drop numeric recording files onto the app, or double-click it to choose files
or reopen the queue. It opens the same dashboard in Terminal, without adding
a Finder menu. macOS may ask to allow the app to control Terminal on first use.
The app refers to the binary's absolute path, so rebuilding that binary updates
the preview; moving it requires creating a new app. Choose a new output path
when regenerating the app. Existing `.clu.N` outputs are skipped.

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
  "jobs": "auto"
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

Clustering feeds computed PCA/other features directly into `f32` storage, without
writing temporary `.fet` text. Feature sets up to 64 MiB per active recording
stay in RAM; larger ones use a binary cache in the private scratch folder.
The standalone `features` export retains its text format. Direct `f32` storage
retains more precision than the former six-significant-digit round-trip and
can change cluster assignments or the selected model. Pin the build when
reproducing older results.

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
differences can affect later clustering decisions. Pin the build, numerical
backend and seed when reproducibility matters.

## Windows 11 context menu

The old registry command appears under **Show more options**. A command in the
first Windows 11 menu requires an `IExplorerCommand` COM extension and package
identity, normally supplied by an optional signed sparse MSIX package. This is
an installed integration component; the portable clustering executable remains
independent. See [Microsoft's integration guide](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/integrate-packaged-app-with-file-explorer).

This branch provides the safe multi-file queue command that an extension can
invoke, but does not yet install a modern Explorer extension. Such an extension
should filter numeric recording extensions, enumerate the selection, and call
`ltxk enqueue --console` with individual paths and a visible console, without a command shell.
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
python3 tests/check_console.py --exe target/release/ltxk
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
