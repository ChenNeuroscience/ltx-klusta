<img src="assets/ltxk.svg" width="96" height="96" alt="Three blue, green and red clusters separated by dividing lines">

# ltx-klusta

ltx-klusta clusters the spikes in tetrode recordings for review in your usual
cluster-cutting software. Give it a recording such as
`session.1` and it writes `session.clu.1` next to it.

It runs on Windows and on Apple Silicon Macs. There is nothing else to install.

## Install

**Windows x64 — recommended: install the right-click command**

1. Download the `windows` zip from the
   [latest release](https://github.com/ChenNeuroscience/ltx-klusta/releases/latest).
2. Right-click it and choose **Extract All…**. Open the extracted folder.
3. Double-click **Install Explorer integration.cmd**. If Windows shows a
   download warning, choose **More info → Run anyway**.
4. Approve the administrator prompt to trust the bundled ltxk signing
   certificate. Setup installs ltx-klusta for your Windows account and adds
   **Run ltxk** to File Explorer's right-click menu.

Select one or more recordings, such as `session.1` and `session.2`, right-click,
and choose **Run ltxk**. They join the shared queue and progress opens in your
default terminal. An existing ltxk console is reused, including after its
previous recordings have finished. Open **ltx-klusta** from **Start** to see
the queue again.

The command appears directly in the Windows 11 menu. If it does not appear
after setup, restart File Explorer or sign out and back in. Windows 10 version
2004 or newer uses the classic menu. Outputs such as `session.clu.1` are
excluded; select recordings only.

Signing is self-signed and free: no Microsoft account, payment, or Microsoft
review is needed. Certificate trust applies to this computer; an administrator
may need to help on a managed lab PC. To update, download the latest Windows
zip and run its installer again. A release with a new signing certificate asks
for trust again. Setup carries your settings forward; close the old console
before updating.

To remove the menu command and Start shortcut, run **Uninstall Explorer
integration.cmd**. Your recordings, results, queue, and settings are kept.
[Windows setup details](windows/README.md) also cover removing certificate trust.

**Portable alternative:** extract the zip and drag recordings onto `ltxk.exe`.
This needs no administrator access. For a portable **Send to → ltxk** command,
press **Windows + R**, enter `shell:sendto`, and create a shortcut to `ltxk.exe`
in that folder.

**Mac**

1. Download the `macos` zip from the
   [latest release](https://github.com/ChenNeuroscience/ltx-klusta/releases/latest)
   and double-click it to unzip.
2. Drag **ltx-klusta.app** into your **Applications** folder.
3. The first time you open it, macOS may refuse because the app is not from
   the App Store. Open **System Settings → Privacy & Security**, scroll down
   and click **Open Anyway**. When asked, allow it to control Terminal, which
   is where progress is shown.

## Cluster recordings

On Windows, select one or more recordings, right-click, and choose **Run ltxk**.
You can also drag them onto `ltxk.exe` (Windows) or **ltx-klusta.app** (Mac).
Recording files end in the tetrode number: `session.1`, `session.2` and so on.
A window opens showing each recording's progress.

- Results are saved beside each recording: `session.1` gives `session.clu.1`.
- You can drop more recordings at any time. They join the same queue, and
  several recordings are processed at once.
- Recordings that already have a `.clu` file are skipped, so it is safe to
  drop a whole folder's worth again.
- Keep the window open until everything is done, then press **Enter** to close
  it. If you close it early, unfinished recordings continue the next time you
  open ltx-klusta.
- A completed progress window keeps watching for new recordings until you
  close it, so later selections join that same window.
- To see the queue again, choose **ltx-klusta** from Start or double-click
  `ltxk.exe`, or open the Mac app and
  choose **Open Queue**.

In the progress window, use the arrow keys to select a recording. **L** opens
its log, which explains any failure, and **O** opens the folder of all logs.

A typical recording takes from a few seconds to a few minutes.

## Settings

The defaults suit most recordings. To change them, copy `ltxk.example.json`
to `ltxk.json` in the same folder as `ltxk.exe`, and edit it. With the Windows
installer, right-click **ltx-klusta** in Start, choose **Open file location**,
then right-click its shortcut and choose **Open file location** again to reach
the installed files. Setup also prints this folder when it finishes. On a Mac,
right-click **ltx-klusta.app**, choose **Show Package Contents**, and put
`ltxk.json` in **Contents → Resources**.

| Setting | Default | Meaning |
| --- | --- | --- |
| `features` | `3PC` | What to measure on each channel: `0PC`–`9PC` principal components, `A` amplitude, `VT` voltage at the most variable sample, e.g. `3PC,A`. |
| `cluster.min_clusters`, `cluster.max_clusters` | 20, 30 | The range of starting cluster counts to try (including noise). Each is tried once and the best result is kept. |
| `cluster.target_spikes` | 25000 | How many spikes to fit the model to. Every spike is still labelled. |
| `cluster.seed` | 1 | Random seed. The same recording, settings and version always give the same result. |
| `jobs` | `"auto"` | How many recordings to process at once (1–16). `auto` uses about three quarters of the CPU. |

Unknown or invalid settings are reported rather than ignored. A recording
keeps the settings it was queued with.

## How it works

For each channel that carries signal, ltx-klusta finds the principal
components of the spike waveforms (PCA over every spike), and measures each
spike against them. It then clusters those features with a custom-optimised
Rust port derived directly from KlustaKwik: a mixture of Gaussian clusters
plus a noise cluster, fitted by classification EM, with clusters split and
merged to balance fit against complexity (BIC). See
[CLUSTERING.md](CLUSTERING.md) for the details.

Recordings use the Axona tetrode format: a text header containing
`num_spikes`, then `data_start`, then for each spike four channels of a 4-byte
timestamp and 50 signed 8-bit samples.

## Command line

Everything above is also available from a terminal. On a Mac, either use
`ltx-klusta.app/Contents/Resources/ltxk` or the separate `ltxk` file in the
zip (if macOS blocks it, run `xattr -d com.apple.quarantine ltxk` once).

```sh
ltxk session.1 session.2            # queue recordings and show progress
ltxk                                # show the queue
ltxk cluster session.1 --force      # the same, with options; --force replaces .clu files
ltxk cluster session.1 --no-queue   # process here only, without the shared queue
ltxk cluster session.1 --background # queue and return immediately
ltxk status                         # list jobs and their logs (--json for scripts)
ltxk cluster --help                 # every option
```

Each setting in the table above has a matching option, such as
`--features 3PC,A` or `--min-clusters 10`, and `--config FILE` loads a
different settings file. `--work-dir DIR` puts temporary files on another
disk. Large recordings keep their features in a temporary file rather than in
memory, so millions of spikes need little RAM. Without a terminal, progress is
printed as plain text.

The queue is shared by every ltx-klusta window and command for your user
account, and survives crashes: an interrupted recording starts again from
the beginning the next time the queue runs. It is stored in
`%LOCALAPPDATA%\ltxk\queue` on Windows and
`~/Library/Application Support/ltxk/queue` on a Mac (`--queue-dir` changes it).

### Saving intermediate data

`--dump` saves data from inside the pipeline as NumPy `.npy` files, in a
`session.dump.1` folder beside the recording (or under `--dump-dir`).
`--dump-only` stops once the requested data is written, without writing a
`.clu` file, which skips clustering entirely if only `pca` or `features`
are requested.

```sh
ltxk cluster session.1 --no-queue --dump features --dump-only
ltxk cluster session.1 --dump pca,features,models,labels
```

| Point | Files | Contents |
| --- | --- | --- |
| `pca` | `pca_channels`, `pca_means`, `pca_components` | Active channel numbers; each channel's mean waveform (channels × 50); principal components (channels × PCs × 50). |
| `features` | `features`, `feature_min`, `feature_range` | Every spike's features as clustered (spikes × columns, 32-bit float), ordered as all channels' PCs, then all amplitudes, then all VTs. Clustering uses `(value - min) / range`, ignoring constant columns. |
| `subsets` | `trial_N_subset` | Spike indices fitted in the trial starting with N clusters. |
| `models` | `trial_N_weights`, `trial_N_means`, `trial_N_covariances` | The trial's fitted clusters, indexed by cluster ID, in normalized feature space. ID 0 is noise; unused IDs are NaN. |
| `labels` | `trial_N_labels` | Every spike's cluster ID in that trial. The `.clu` file is the best trial, renumbered from 1 (noise). |

Load them with `numpy.load("features.npy")` in Python, or `readNPY` from
[npy-matlab](https://github.com/kwikteam/npy-matlab) in MATLAB.

## Developing

ltx-klusta is a single Rust crate. Build and test with a current stable Rust
toolchain and Python 3:

```sh
cargo build --release
cargo test --release
cargo fmt --check && cargo clippy --all-targets -- -D warnings
python3 tests/check_cli.py --exe target/release/ltxk       # CLI and dumps
python3 tests/check_features.py --exe target/release/ltxk  # PCA and feature maths
python3 tests/check_cluster.py --exe target/release/ltxk   # clustering and queue
python3 tests/check_console.py --exe target/release/ltxk   # progress display
python3 tests/check_shell.py --exe target/release/ltxk     # Explorer batch submissions
python3 tests/check_large.py --exe target/release/ltxk     # 1M spikes, bounded memory
```

`tests/check_large.py --spikes 9000000` exercises a nine-million-spike
recording; it is not part of CI.

| Source | Purpose |
| --- | --- |
| `src/main.rs` | Command line |
| `src/pipeline.rs` | One recording end to end: settings, stages, `.clu` output |
| `src/extract.rs` | Reading recordings, PCA and features |
| `src/feature_store.rs` | Features held in memory, or in a temporary file when large |
| `src/cluster.rs` | Clustering |
| `src/dump.rs` | `--dump` and the `.npy` writer |
| `src/queue.rs`, `src/queue_progress.rs`, `src/dashboard.rs`, `src/progress.rs` | Shared queue and progress display |

The `Build` GitHub workflow lints, tests and packages Windows x64 and macOS
Apple Silicon builds on every push and pull request
(`scripts/package_rust.py`, which on macOS also builds the app with
`scripts/create_macos_app.py`, and on Windows builds and self-signs the native
Explorer extension with `scripts/build_windows.py`). Windows builds need the
Visual Studio C++ tools and Windows SDK; users need neither. See
[Windows integration development](windows/README.md) for signing-key reuse and
the Windows 11 manual checks. Each push to `main` publishes a release tagged
with the commit.

[PERFORMANCE.md](PERFORMANCE.md) describes how the clustering is kept fast
and how to compare two builds on your own recordings with
`scripts/benchmark_cluster.py`. `LTXK_PROFILE=1` adds stage timings to the log.
