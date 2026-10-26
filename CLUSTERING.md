# Classic clustering in ltx-klusta

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

### Follow one iteration

Each spike is a short feature vector: normally three PCs from each of four
channels, for 12 numbers. A signal cluster is an ellipsoid in that feature
space. Its mean is the center; its covariance describes the spread and
correlations, allowing tilted ellipsoids.

1. **Fit the current groups (M-step).** Count their spikes, recompute each mean
   and unbiased covariance, and set the component weights from their sizes.
2. **Score spikes (E-step).** For each valid signal cluster, factor its
   covariance as `L Lᵀ`. Solve `L z = spike - mean` and evaluate
   `dot(z,z)/2 + sum(log(diag(L))) - log(weight) + D*log(2π)/2`.
   A smaller score is a better match. The uniform noise class has its own score.
   Log terms and the factorization are shared by all spikes in that update.
3. **Assign groups (C-step).** Give each spike to the component with the lowest
   score and remember its runner-up. Assignments are hard: one group per spike.
4. **Consider a deletion.** Estimate the cost of moving a group's spikes to
   their runner-up groups. Delete the best candidate when that cost is smaller
   than the reduction in the BIC complexity penalty.
5. **Periodically try splits.** Fit one and two signal groups to a candidate
   cluster, then evaluate a promising split in the full fitting model. Accept
   it using the classic penalized-score comparison and repeat the main loop.

Convergence requires unchanged assignments after a full scoring step and no
accepted split. Quick steps reuse some old scores under the original rule;
periodic full steps refresh them. Each initial-count trial performs this search
on its own subset. Its fitted model then assigns the full recording when
subsampling is in use, and the best full-record penalized score wins.

The common 3/12-feature scoring kernels cache `1 / L[i,i]` for their triangular
solves. Multiplication by this reciprocal can round differently from division;
it preserves the model and solve equations but can change sensitive decisions.
See [PERFORMANCE.md](PERFORMANCE.md) for measurements and validation.

Clustering casts computed features directly to `f32`, then uses `f64` for
normalization and clustering arithmetic. It retains global min/max bounds,
ascending component IDs for ties and the Windows CRT random generator. The
former six-significant-digit `.fet` round-trip is removed from clustering;
standalone feature export still uses that text format. More precise feature
values can change assignments, subsequent RNG consumption and the selected
model, even with the same initial seed. BLAS/LAPACK selection affects feature extraction; clustering's
small covariance/Cholesky calculations use Rust on every platform.

There are explicit corrections at invalid or degenerate inputs: constant
feature columns are omitted instead of dividing by zero; singular components
are excluded immediately instead of leaving stale likelihoods eligible for
assignment; non-convergence is reported as an error. An entirely constant
feature set produces only noise. Failures preserve an existing output file.

## Memory and temporary storage

The fitting set and its scores are held in memory. Full-record `f32` features
also stay in RAM when their total size is at most 64 MiB per file. Above that
threshold, features stream directly to a little-endian binary cache. A
12-column cache for nine million spikes is 432 MB (about 412 MiB), so it uses
the disk path. The threshold bounds feature RAM independently of recording
length and adds at most 64 MiB per active job to the fitting/PCA working sets.

Both storage paths use the same feature values and full-record normalization
bounds. Fitting reads only selected spikes; full-record assignment streams
all rows in order. There is no full-record spikes-by-clusters allocation.
No temporary `.fet` or count file is created for clustering.

Scratch directories are unique, including when two source folders contain
identically named recordings. Files are removed on completion/failure, and the
queue clears interrupted work before retrying. `--work-dir` selects the scratch
disk. Each initial-count trial still classifies every spike when subsampling;
bounded RAM does not eliminate that computation.

## Verification and scientific validation

Automated checks cover covariance/likelihood equations, the noise component,
BIC deletion, split search, Windows RNG values, large-count sampling arithmetic,
and synthetic recordings with known separated populations. Integration tests
exercise both full fitting and subset assignment, all-spike output, reproducible
block sizes, Unicode paths, atomic replacement, and concurrent queue recovery.
Windows and macOS run these checks with pure Rust and optional runtime LAPACK.
Routine CI does not generate nine million spikes.

Development checks against historical upstream source, with the Windows RNG
substituted on macOS, produced identical labels on two synthetic recordings and
three real recordings (48,517, 91,242 and 704,080 spikes), before removal of the
six-significant-digit round-trip. No legacy source or
C++ comparison harness is included in this repository or its CI. These checks
do not identify the exact source of the customized June 2014 Windows binary
or establish equivalence for every recording. Review representative
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
