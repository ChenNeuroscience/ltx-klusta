# Classic clustering in fancykk

The supported workflow is the one in the original `fancykk/runkk.bat`:

1. Extract `3PC` features from each active Axona tetrode channel.
2. Use classic, unmasked KlustaKwik (`UseDistributional 0`). Try each initial
   cluster count from 20 through 30 once, with random seed 1.
3. Set the sampling stride to `max(1, floor(N / 25000))`. Fit `floor(N / stride)`
   spikes, choosing one random spike from each complete interval of that stride.
4. When the stride exceeds 1, evaluate the fitted model over **every** spike.
   Select the trial with the lowest penalized score on the full recording.
5. Write `recording.clu.N` beside `recording.N`. Existing outputs are skipped
   unless `--force` is given. Label 1 is always reserved for uniform noise.

This preserves the historical floor rule: recordings with fewer than 50,000
spikes use the full data, and the fitting count is not always exactly 25,000.
For nine million spikes, the stride is 360 and the fitting count is 25,000.
The default fitting count is always below 50,000, regardless of recording length.

PCA uses the **whole recording**, before this clustering subset is drawn. Its
block size affects memory and throughput, not the definition of the PCA model.
Clustering's fitting subset is a separate, deliberate approximation already
used by the batch file. Each initial-count trial draws a fresh subset, as in
the original program.

## Model and optimization

The model uses full-covariance Gaussian components and one uniform component
over the features' normalized unit box. It uses classification EM (hard
assignments), unbiased covariance estimates, a single noise pseudo-count,
and the classic BIC penalty:

```
parameters = (D*(D+1)/2 + D + 1) * (alive_components - 1)
penalty = parameters * log(N) / 2
```

Components with at most `D` fitting points or singular covariance are removed.
The optimization retains quick E-steps, periodic full steps, one-at-a-time
BIC deletion using runner-up assignments, and recursive two-way split proposals.
Split comparisons retain the classic pass-start baseline. There is room for
future algorithm improvements, but they should be evaluated separately.
The cap is 100 components including noise, matching the supplied Windows binary.
The classic `PriorPoint` setting only affected distributional mode and is absent.

Numerical compatibility details include six-significant-digit feature text,
the original reader's conversion to `f32` followed by `f64` arithmetic, global
min/max normalization, ascending component IDs for ties, and the Windows CRT
random sequence. BLAS/LAPACK selection affects feature extraction; clustering's
small covariance/Cholesky calculations use Rust on every platform.

There are explicit corrections at invalid or degenerate inputs: constant
feature columns are omitted instead of dividing by zero; singular components
are excluded immediately instead of leaving stale likelihoods eligible for
assignment; non-convergence is reported as an error. An entirely constant
feature set produces only noise. Failures preserve an existing output file.

## Memory and temporary storage

Only the fitting set and its component scores are held in memory. The
recording's features and trial labels use private disk files, and full-record
assignment runs in fixed-size blocks. There is no `N * clusters` allocation
for the full recording. Scratch directories are unique, including when two
source folders contain identically named recordings. Files are removed on
completion or failure; the queue clears interrupted work before retrying it.

Feature extraction currently writes temporary `.fet` text, converts it to a
compact `f32` cache, then removes the text before fitting. Large recordings
therefore need temporary disk space: a 12-column cache for nine million spikes
is 432 MB, and the temporary text is larger. `--work-dir` selects another disk.
Each initial-count trial still classifies the entire recording; bounded RAM
does not eliminate that computation.

## Verification and scientific validation

Automated checks cover covariance/likelihood equations, the noise component,
BIC deletion, split search, Windows RNG values, large-count sampling arithmetic,
and synthetic recordings with known separated populations. Integration tests
exercise both full fitting and subset assignment, all-spike output, reproducible
block sizes, Unicode paths, atomic replacement, and concurrent queue recovery.
Windows and macOS run these checks with pure Rust and optional runtime LAPACK.
Routine CI does not generate nine million spikes.

A development-only check against historical upstream source, with the Windows
RNG substituted on macOS, produced identical labels on two synthetic recordings
(1,200 spikes with full fitting and 1,201 spikes with stride 4). No legacy source
or comparison harness is included in this repository or its CI. This check
does not identify the exact source of the customized June 2014 Windows binary
or establish identical results on real recordings. Review representative
recordings before adopting the new clustering implementation for analysis;
label numbers and results can also vary across floating-point backends.

## Algorithm provenance

The reference is the classic path in
[klusta-team/klustakwik, commit 9ca1d21](https://github.com/klusta-team/klustakwik/tree/9ca1d21fcbda61198e3a194bf9fa16bbae3d84ed)
(June 2014), preceding the supplied executable's build date. Credit for
KlustaKwik goes to Kenneth D. Harris and its contributors, including the
subsequent work of Dan Goodman and Shabnam Kadir. The masked/distributional
algorithm, mask-based initialization, start files, diagnostic data dumps,
and other unused modes are outside this implementation's scope.
