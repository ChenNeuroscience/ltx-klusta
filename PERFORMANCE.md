# Performance notes

Almost all of the time goes into clustering, and most of that into scoring
spikes against clusters (the E-step). Reading the recording and PCA take a
few seconds even for millions of spikes. Each recording uses one thread, and
the queue runs several recordings at once.

None of the optimisations below change the model or the order of any
spike's arithmetic, except where noted. Unit tests compare the optimised
scoring bit for bit with a plain reference implementation.

## What makes it fast

**Feature counts known at compile time.** The clustering code is generic over
the number of features (`Dims` in `src/cluster.rs`). The common counts, 3, 6,
9 and 12 (3 PCs on one to four channels), are compile-time constants, so loops
over features have fixed lengths: the compiler unrolls them, keeps small
arrays in registers, and removes bounds checks. Any other count uses the same
code with a run-time length.

**Unrolled triangular solve.** Scoring solves `L z = x - mean` row by row.
LLVM only fully unrolls innermost loops, so the row loop is written out with
the small `unrolled_for!` macro, which gives each row's inner loop a constant
length.

**Four spikes at a time.** Full E-steps score four spikes per call. Each
spike's operations happen in the same order as when scoring one spike, and
the four are independent, so they map onto SIMD instructions without
reassociating floating-point arithmetic. No fast-math or CPU-specific flags
are used.

**Reciprocal diagonal.** `1 / L[i,i]` is computed once per cluster update and
multiplied in each solve, instead of dividing. This can change the last bit
of a score compared with dividing.

**Scores stored per cluster.** Each cluster's scores are stored contiguously,
since the E-step works one cluster at a time, and the storage grows only as
clusters are added.

**Incremental M-steps.** When few spikes change cluster, only their
contributions are added to or removed from the cluster totals (see
CLUSTERING.md). This changes floating-point rounding slightly, not the
equations.

**Features as 32-bit floats.** Up to 64 MiB of features per recording stay in
memory; larger feature sets go to a temporary file, which fitting reads
selectively and labelling reads in order.

Some of these are sensitive to small details of how the code is written; the
comments in `score_lanes` and `score_all` note what to preserve. For example,
iterating with `chunks_exact` without re-slicing each block to its known
length made full E-steps about 75% slower.

## Measurements

Apple M-series CPU, one recording at a time, default settings. Times are user
CPU seconds (best of four runs) on synthetic nine-unit recordings, compared
with the previous hand-unrolled implementation:

| Recording | Features | Before | After |
| --- | ---: | ---: | ---: |
| 60,000 spikes, 4 channels | 12 | 2.09 s | 2.10 s |
| 20,000 spikes, 4 channels | 12 | 1.04 s | 1.05 s |
| 40,000 spikes, 1 channel | 3 | 0.97 s | 1.00 s |
| 20,000 spikes, 4 channels, `3PC,A` | 16 | 6.11 s | 2.28 s |

The 3- and 12-feature outputs were byte-identical to the previous build. The
16-feature case previously used a slower generic path that divided rather
than multiplied by the reciprocal, so its labels can differ slightly.

One million spikes cluster end to end in about 5.5 s with a peak of about
130 MiB (`tests/check_large.py`).

## Measuring

`LTXK_PROFILE=1` adds timings for feature extraction and for each trial's
loading, fitting and labelling to the log:

```sh
LTXK_PROFILE=1 ltxk cluster session.1 --no-queue --force
```

To compare two builds on your own recordings:

```sh
python3 scripts/benchmark_cluster.py --baseline old/ltxk --candidate target/release/ltxk \
    --recordings session.1 session.2 --runs 3 --output comparison.json
```

This works on private copies of the recordings, alternates the two builds,
and reports timings, each trial's score, and how closely the two builds'
labels agree (adjusted Rand index). Use release builds, run one recording at a
time on an otherwise idle machine, and compare medians or minimums over
several runs: other workloads easily change timings by 50% or more.
