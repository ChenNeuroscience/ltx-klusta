# How ltx-klusta clusters a recording

ltx-klusta performs PCA on each tetrode channel's waveforms, then clusters
the resulting features with a custom-optimised Rust port derived directly from
[KlustaKwik](https://github.com/klusta-team/klustakwik) (its unmasked mode).
Credit for KlustaKwik goes to Kenneth D. Harris and its contributors.

## 1. Features

For each of the four channels:

1. Read every spike's 50 samples and accumulate their mean and covariance.
   A channel whose samples never change is ignored.
2. Find the covariance's eigenvectors (principal components), largest
   variance first. Each is given a fixed sign: its largest-magnitude loading
   is positive.
3. For each spike, the `k`th PC feature is the dot product of the spike's
   mean-subtracted waveform with the `k`th component.

`A` is a channel's peak-to-trough amplitude and `VT` the value at the sample
with the largest variance. Columns are ordered as all channels' PCs, then all
amplitudes, then all VTs. Features are stored as 32-bit floats, then each
column is scaled to the range 0–1 over the whole recording. Constant columns
are dropped.

The recording is read in blocks, so memory use does not grow with its length.
The sums involved are exact, so the block size does not change the result.

## 2. The model

Each spike is a point in feature space: with `3PC` on four active channels, a
point has 12 coordinates. The model is a mixture of:

- **Gaussian clusters**, each with a weight, mean and full covariance (an
  ellipsoid that can be tilted), and
- **one noise cluster**, spread uniformly over the unit box.

A spike's score for a cluster is its negative log-likelihood, so smaller
means a better fit. For a Gaussian with covariance `C = L Lᵀ` (Cholesky
factor `L`):

```
solve L z = spike - mean
score = |z|²/2 + Σ log L[i,i] - log(weight) + D/2 log(2π)
```

The model as a whole is scored by summing each spike's score for its own
cluster, plus a BIC penalty for the number of parameters:

```
penalty = (D(D+1)/2 + D + 1) × (clusters - 1) × log(spikes) / 2
```

## 3. Fitting

Fitting uses classification EM: each spike belongs to exactly one cluster.
Starting from a random assignment, it repeats:

1. **Fit (M-step).** Compute each cluster's weight, mean and unbiased
   covariance from its spikes. A cluster with no more spikes than dimensions,
   or with a singular covariance, is removed.
2. **Score (E-step).** Score every spike against every cluster. To save time,
   most iterations skip spikes that stayed put and were far from a cluster
   last time, reusing their old score. Every 20th iteration, and whenever
   many spikes moved or the total got worse, everything is re-scored.
3. **Assign (C-step).** Move each spike to its best-scoring cluster, and
   remember its second-best. Ties go to the lower cluster number.
4. **Delete.** If moving one cluster's spikes to their second choices costs
   less than the penalty that removing it saves, delete it.
5. **Split.** Periodically, for each cluster, fit one and two clusters to its
   spikes alone. If two fit better, and splitting improves the whole model's
   penalised score, accept the split.

Fitting ends when an iteration that re-scored everything moved no spikes and
accepted no split. There can be at most 100 clusters, including noise.

When few spikes change cluster, the M-step updates each affected cluster's
totals using only the spikes that moved, rather than recomputing from all of
its members. The totals are kept centred on the cluster mean, which avoids
the rounding error of subtracting large sums of squares, and are rebuilt from
scratch every 20 updates.

## 4. Trials and the final labels

The model is fitted once for each starting cluster count from
`min_clusters` to `max_clusters` (20–30 by default, including noise). Each
trial fits a fresh random subset:

```
stride  = max(1, floor(spikes / target_spikes))
subset  = one random spike from each complete block of `stride` spikes
```

So recordings with fewer than 50,000 spikes (by default) are fitted in full,
and larger ones are fitted to between 25,000 and 50,000 spikes. Each trial's
model then labels **every** spike and is scored on the whole recording. The
trial with the lowest penalised score is written as the `.clu` file, with
noise as cluster 1 and the other non-empty clusters numbered from 2.

Random numbers come from a fixed linear congruential generator (the MSVC
`rand()` constants), so a given seed, version and recording always produce
the same result on every platform.

`--dump` can save the data at each stage; see the README.

## Validation

Unit tests check the likelihood against analytic cases, the covariance and
incremental updates against directly computed moments, the noise cluster,
deletion, splitting and the random sequence. Integration tests cluster
synthetic recordings with known, well-separated populations, check PCA against
analytic cases, and check that every spike is labelled, including on one
million spikes. Before adopting a new version for analysis, review its output
on representative recordings.
