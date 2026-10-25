# Performance work

The optimisations preserve feature precision, global normalization, the Windows
random sequence, selected fitting spikes, trial order, CEM decisions, and the
order of floating-point accumulation. They do not reduce the fitting target,
initial-count trials, or convergence requirements.

## Implemented

- **Retain buffered feature reads.** Subset selection previously used an
  absolute seek before every selected spike. `BufReader::seek` discards its
  buffer, even if the next row is already there. Relative seeks retain nearby
  rows and avoid repeatedly reading overlapping megabyte blocks. Random-access,
  repeated, backward, and buffer-boundary reads remain supported.
- **Specialise common dimensions.** Gaussian likelihood and covariance updates
  use fixed-size kernels for 3 and 12 features, the usual one-channel and
  four-channel `3PC` cases. This lets the compiler unroll loops and remove bounds
  checks. Other dimensions use the generic implementation. The operations and
  summation order are retained; no reciprocal approximations, fast-math flags,
  reduced precision, or additional native libraries are used.
- **Reuse iteration storage.** CEM reuses its previous-assignment buffer rather
  than allocating it on every iteration. Means use bounded slices that allow
  vectorisation across independent feature columns.

Numerical checks cover analytic correlated Gaussians, bit-for-bit agreement of
specialised and generic likelihoods, covariance agreement with independently
computed moments, and complete clustering through both PCA backends. Local
before/after benchmarks additionally compare every `.clu` byte and all reported
trial scores on representative recordings. Input recordings remain local and
are not included in the repository or CI.

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

## Further candidates

- A direct binary feature-cache pipeline could eliminate the temporary `.fet`
  write/read cycle. It must preserve the existing six-significant-digit-to-f32
  conversion, since changing that rounding can alter clustering decisions.
- Batched likelihood/triangular solves could improve SIMD or BLAS throughput.
  They need tests of numerical error and complete clustering results because
  backend reduction order can differ.
- Full-record assignment is independent per spike and could run in parallel.
  Preserve output order and the sequential full-score reduction so that trial
  selection remains reproducible. Coordinate its worker budget with the
  existing recording-level queue to avoid oversubscribing the machine.
- Compact likelihood storage could reduce cache traffic. Quick E-steps,
  runner-up deletion scores and reused likelihoods must retain their semantics.

These are candidates for measurement, rather than enabled algorithm changes.
