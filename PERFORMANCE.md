# Performance work

The optimisations preserve the model, global normalization,
Windows random generator, sampling rule, trial schedule and convergence rules.
They do not reduce the fitting target or initial-count trials. Score storage
changes preserve the arithmetic exactly. The common Gaussian scoring kernels
use cached reciprocal diagonals, which can change floating-point rounding and,
on sensitive data, later decisions. This retains the same mathematical solve.

Direct-f32 feature extraction also intentionally removes the preceding decimal
rounding step. This retains more feature precision, but can change the search
trajectory and final model; see the separate results below.

## Implemented

- **Feed features directly into f32 storage.** Remove decimal formatting,
  temporary `.fet` writes and reparsing from clustering. Keep up to 64 MiB of
  features per job in RAM; stream larger sets to a binary cache. Both paths use
  identical f32 values and full-record normalization. The standalone `features`
  command continues to export six-significant-digit text. This intentionally
  removes the old decimal rounding from clustering and can change results.
- **Retain buffered feature reads.** Subset selection previously used an
  absolute seek before every selected spike. `BufReader::seek` discards its
  buffer, even if the next row is already there. Relative seeks retain nearby
  rows and avoid repeatedly reading overlapping megabyte blocks. Random-access,
  repeated, backward, and buffer-boundary reads remain supported.
- **Specialise common dimensions.** Gaussian likelihood and covariance updates
  use fixed-size kernels for 3 and 12 features, the usual one-channel and
  four-channel `3PC` cases. This lets the compiler unroll loops and remove bounds
  checks. Other dimensions use the generic implementation. The operations and
  summation order are retained apart from the reciprocal multiplication below.
  No fast-math flags, reduced precision, or additional native libraries are used.
- **Reuse iteration storage.** CEM reuses its previous-assignment buffer rather
  than allocating it on every iteration. Means use bounded slices that allow
  vectorisation across independent feature columns.
- **Stream cluster score columns.** The E-step visits one Gaussian at a time.
  Its scores now occupy consecutive doubles, rather than one entry every 800
  bytes in a table with 100 slots per spike. Columns are allocated on demand;
  a local one/two-cluster split fit needs only two/three logical columns,
  including noise. Previously computed columns are retained after deletion,
  preserving the quick E-step's reuse of stale scores. Classification still
  visits component IDs in ascending order, and all reductions retain spike order.
- **Cache reciprocal Cholesky diagonals.** For the common 3/12-feature kernels,
  compute `1 / L[i,i]` once per Gaussian update and multiply by it in each
  triangular solve. The mean, Cholesky factor, weight and log normalization
  are still recomputed whenever the cluster changes. This uses full-precision
  division to construct the reciprocal, with no hardware reciprocal estimate.
  Feature normalization and covariance estimation keep their existing divisions.

Numerical checks cover analytic correlated Gaussians, close agreement of the
reciprocal kernels with the division-based generic solve, covariance agreement
with independently computed moments, and complete clustering through both PCA backends. Local
before/after benchmarks additionally compare every `.clu` byte and all reported
trial scores on representative recordings. Input recordings remain local and
are not included in the repository or CI.

## Where the time goes

A six-second stack sample during a real 48,517-spike, 12-feature fit attributed
about 65% of worker samples to Gaussian scoring and another 16% to its E-step
caller. Covariance/mean updates accounted for about 7%. These are sampled
proportions for that part of one recording, rather than universal percentages.

Each Gaussian evaluation solves `L z = x - mean`, then computes
`dot(z,z)/2 + offset`. `offset` already contains the log determinant, cluster
weight and normalizing constant; there is no logarithm per spike/cluster pair.
The triangular solve has dependent multiply/subtract steps and one division
per dimension, making cached reciprocals useful here.

Changing the score layout reduces wasted cache-line traffic and removes the
100-column allocation from small split fits. Sequential columns also suit
hardware prefetching. No architecture-specific prefetch hints are enabled;
they need evidence of a remaining memory bottleneck on each target CPU.

## Validation of the score changes

Before the direct-f32 feature change, the storage-only prototype produced byte-identical labels and identical printed
trial scores on three real recordings. With reciprocal scoring enabled, nine
recordings across three sessions (48,517–704,080 spikes) retained byte-identical
labels. Development-only traces also matched assignment hashes, full/quick
steps, split/delete decisions and RNG state throughout the fits. Small score
rounding differences were observed. This is evidence for those recordings,
not a promise of identical clustering for every recording.

Validation used private copies of recordings and separate instrumented builds.
The application and CI do not contain recordings or the validation trace code.

For the score-storage/reciprocal change, before direct-f32 extraction: three-run
medians on the local macOS ARM64 machine, with nalgebra, seed 1,
default fitting settings and one recording at a time:

| Spikes | Total before | Total after | Peak RSS before | Peak RSS after |
| ---: | ---: | ---: | ---: | ---: |
| 48,517 | 24.65 s | 17.68 s | 133 MiB | 61 MiB |
| 91,242 | 9.84 s | 8.59 s | 98 MiB | 47 MiB |
| 704,080 | 12.15 s | 11.31 s | 87 MiB | 32 MiB |

These include PCA, feature-cache creation, fitting, all-spike assignment and
output. Build order alternated between repetitions. The largest recording
spends more time in the unchanged PCA and I/O work. These measurements compare
the preceding Rust build with this implementation; they are not Windows timings
or comparisons against historical C++ KlustaKwik.
Peak RSS uses per-process OS resource accounting, collected separately from
the portable timing helper below.

## Direct-f32 feature measurements

Three alternating repetitions per build on macOS ARM64, using nalgebra and
one file at a time:

| Spikes | Feature preparation before | Feature preparation after | Total before | Total after |
| ---: | ---: | ---: | ---: | ---: |
| 48,517 | 0.186 s | 0.097 s | 17.15 s | 15.51 s |
| 91,242 | 0.350 s | 0.175 s | 8.18 s | 8.01 s |
| 704,080 | 2.690 s | 1.370 s | 10.59 s | 9.09 s |

Feature preparation includes PCA/projection and cache finalization. Total
timings also include changed CEM search trajectories, so those total savings
cannot all be attributed to I/O removal. These inputs use the RAM path.

Across nine real recordings, adjusted Rand agreement with the preceding text
round-trip ranged from 0.715 to 1.0; seven were at least 0.95 and two were
byte-identical. Two files selected one fewer signal cluster. The most sensitive
file retained 21 groups but had agreement 0.715, so this change affects more
than arbitrary label numbering on some data. These are comparisons between
two outputs, not ground-truth accuracy measurements.

On that sensitive file, the original and refactored standalone text feature
exports were byte-identical, isolating the representation change after PCA.
Synthetic population tests still pass. RAM and disk paths are checked against
one another exactly, including subset/global bounds, backward/repeated reads,
partial blocks and feature-column order. Nine-million-spike disk selection is
tested by sizing metadata without generating a large recording in CI.

## Measure a recording

Developer timing output can be enabled without changing the algorithm:

```sh
LTXK_PROFILE=1 ltxk cluster recording.1
```

This logs feature extraction/cache times and each trial's subset-loading,
fitting, and assignment times. On PowerShell, set `$env:LTXK_PROFILE = '1'`
before invoking `ltxk`. Queue jobs write the same timing information to their
job logs. Normal runs omit it.

For repeatable comparisons, use private copies of recordings, the same seed,
backend and options, release builds, and sequential runs. Report medians and
ranges across multiple runs. Cached filesystem data and other workloads affect
timings, and macOS measurements do not establish Windows throughput.

To compare two compatible `ltxk` builds from a source checkout:

```sh
python3 scripts/benchmark_cluster.py --baseline /path/to/old/ltxk --candidate target/release/ltxk --recordings /path/to/recording.1 /path/to/recording.2 --runs 3 --output comparison.json
```

The helper makes private copies, pins the default fancykk settings and nalgebra,
alternates build order, and runs one recording at a time. It records total and
phase timings, trial scores, label hashes, raw label changes and adjusted Rand
index (which allows label IDs to differ). Existing recording outputs remain
untouched. Input copies and generated labels are removed on completion; the
JSON report remains local unless you choose to share it. Timings include file
I/O and can vary with caching and other workloads.

## Further candidates

- Batched likelihood/triangular solves could improve SIMD or BLAS throughput.
  They need tests of numerical error and complete clustering results because
  backend reduction order can differ.
- Full-record assignment is independent per spike and could run in parallel.
  Preserve output order and the sequential full-score reduction so that trial
  selection remains reproducible. Coordinate its worker budget with the
  existing recording-level queue to avoid oversubscribing the machine.
- SIMD across independent spikes could shorten Gaussian evaluation while
  preserving each spike's reduction order. Register pressure, scattered feature
  loads and quick-step masks need measurement on both ARM64 and x64.

These are candidates for measurement, rather than enabled algorithm changes.
