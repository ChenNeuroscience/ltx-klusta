//! Clustering, ported from KlustaKwik (unmasked mode): classification EM over
//! a mixture of full-covariance Gaussians plus one uniform noise component,
//! with BIC-penalised cluster deletion and recursive split proposals.
//!
//! Each initial cluster count is tried once on a stratified subset of spikes;
//! the fitted model then labels every spike, and the trial with the lowest
//! penalised score on the full recording wins.
use crate::dump::{Dump, Point};
use crate::feature_store::{Data, FeatureStore};
use crate::progress::{Stage, Update};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub const MAX_CLUSTERS: usize = 100; // Includes the permanent noise component.
const MAX_DIMS: usize = 64; // Matches the feature store's column limit.

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    pub min_clusters: usize,
    pub max_clusters: usize,
    pub target_spikes: usize,
    pub seed: u32,
    pub max_iterations: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_clusters: 20,
            max_clusters: 30,
            target_spikes: 25_000,
            seed: 1,
            max_iterations: 10_000,
        }
    }
}

impl Options {
    pub fn validate(&self) -> Result<()> {
        if self.min_clusters < 2
            || self.min_clusters > self.max_clusters
            || self.max_clusters >= MAX_CLUSTERS
        {
            return Err(Error::Invalid(
                "Initial cluster counts must satisfy 2 <= min <= max < 100 (including noise)",
            ));
        }
        if !(1..=25_000).contains(&self.target_spikes) {
            return Err(Error::Invalid("target-spikes must be between 1 and 25000"));
        }
        if !(1..=100_000).contains(&self.max_iterations) {
            return Err(Error::Invalid(
                "max-iterations must be between 1 and 100000",
            ));
        }
        Ok(())
    }
}

/// The MSVC `rand()` linear congruential generator, implemented here so a
/// seed gives the same results on every platform.
struct Random(u32);
impl Random {
    fn inclusive(&mut self, low: usize, high: usize) -> usize {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        low + ((self.0 >> 16) & 0x7fff) as usize % (high - low + 1)
    }
}

/// BIC penalty for a model with `alive` components, including noise.
fn penalty(points: usize, dims: usize, alive: usize) -> f64 {
    let parameters = (dims * (dims + 1) / 2 + dims + 1) * (alive - 1);
    parameters as f64 * (points as f64).ln() / 2.0
}

/// The number of features per spike.
///
/// The common sizes (3 PCs on 1–4 channels) are compile-time constants via
/// [`Fixed`], so every per-feature loop has constant bounds: the compiler
/// unrolls them, keeps small arrays in registers and drops bounds checks.
/// Plain `usize` is the run-time fallback for any other size. All of the
/// clustering code is generic over this, and [`run`] picks the type once.
trait Dims: Copy + Send + Sync {
    fn n(self) -> usize;
}
#[derive(Clone, Copy)]
struct Fixed<const D: usize>;
impl<const D: usize> Dims for Fixed<D> {
    #[inline(always)]
    fn n(self) -> usize {
        D
    }
}
impl Dims for usize {
    #[inline(always)]
    fn n(self) -> usize {
        self
    }
}

/// `for $i in 0..$n { $body }`, with the first 16 iterations written out.
///
/// LLVM fully unrolls only innermost loops, so in a triangular loop nest the
/// inner loop's length stays variable. Writing out the outer iterations fixes
/// it: when `$n` is a compile-time constant the guards fold away and each
/// inner loop has a constant length. Iterations past 16 run as a normal loop.
macro_rules! unrolled_for {
    ($i:ident in 0..$n:expr => $body:block) => {
        unrolled_for!(@ $i, $n, $body, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15);
        for $i in 16..$n $body
    };
    (@ $i:ident, $n:expr, $body:block, $($k:literal)*) => {
        $(if $k < $n {
            let $i = $k;
            $body
        })*
    };
}

/// Row-major Cholesky factor of the upper triangle of a covariance matrix.
fn cholesky(cov: &[f64], dims: usize) -> Option<Vec<f64>> {
    let mut lower = vec![0.0; dims * dims];
    for i in 0..dims {
        for j in i..dims {
            let mut value = cov[i * dims + j];
            for k in (0..i).rev() {
                value -= lower[i * dims + k] * lower[j * dims + k];
            }
            if i == j {
                if value <= 0.0 || !value.is_finite() {
                    return None;
                }
                lower[i * dims + i] = value.sqrt();
            } else {
                lower[j * dims + i] = value / lower[i * dims + i];
            }
        }
    }
    Some(lower)
}

/// Spikes scored together by full E-steps; see [`Gaussian::score_lanes`].
const LANES: usize = 4;

/// One cluster's likelihood, prepared once per model update.
/// A spike's score is its negative log-likelihood: smaller is a better match.
#[derive(Clone)]
struct Gaussian<D> {
    dims: D,
    id: usize,
    mean: Vec<f64>,
    /// Cholesky factor `L` of the covariance, row-major.
    lower: Vec<f64>,
    /// `1 / L[i,i]`, so each solve multiplies instead of dividing.
    inverse_diagonal: Vec<f64>,
    /// `log det(L) - log(weight) + D/2 log(2π)`.
    offset: f64,
}
impl<D: Dims> Gaussian<D> {
    fn new(dims: D, id: usize, mean: &[f64], cov: &[f64], weight: f64) -> Option<Self> {
        let d = dims.n();
        let lower = cholesky(cov, d)?;
        // KlustaKwik evaluates these log terms in single precision.
        let log_root_det: f64 = (0..d)
            .map(|i| f64::from(lower[i * d + i].ln() as f32))
            .sum();
        let offset = log_root_det - weight.ln()
            + f64::from((2.0 * std::f64::consts::PI).ln() as f32) * d as f64 / 2.0;
        let inverse_diagonal = (0..d).map(|i| 1.0 / lower[i * d + i]).collect();
        Some(Self {
            dims,
            id,
            mean: mean.to_vec(),
            lower,
            inverse_diagonal,
            offset,
        })
    }

    /// Score `L` spikes at once: `|z|²/2 + offset`, where `L z = spike - mean`.
    ///
    /// Each lane does exactly the arithmetic of a one-spike solve, in the same
    /// order. Lanes are independent, so the compiler can use SIMD across
    /// spikes without reordering any spike's own operations.
    #[inline(always)]
    fn score_lanes<const L: usize>(&self, points: [&[f64]; L]) -> [f64; L] {
        let d = self.dims.n();
        let points = points.map(|p| &p[..d]);
        let (mean, lower, inverse) = (
            &self.mean[..d],
            &self.lower[..d * d],
            &self.inverse_diagonal[..d],
        );
        let mut z = [[0.0; L]; MAX_DIMS];
        // Forward substitution, one row of L at a time. Unrolling the rows
        // gives each row's inner loop a constant length.
        unrolled_for!(i in 0..d => {
            let mut value: [f64; L] = std::array::from_fn(|b| points[b][i] - mean[i]);
            for (j, &l) in lower[i * d..i * d + i].iter().enumerate() {
                for b in 0..L {
                    value[b] -= l * z[j][b];
                }
            }
            z[i] = value.map(|v| v * inverse[i]);
        });
        let mut norm = [-0.0; L]; // -0.0 is the identity Iterator::sum starts from.
        for zi in &z[..d] {
            for b in 0..L {
                norm[b] += zi[b] * zi[b];
            }
        }
        norm.map(|n| n / 2.0 + self.offset)
    }

    fn score(&self, point: &[f64]) -> f64 {
        self.score_lanes([point])[0]
    }

    fn score_all(&self, data: &Data, output: &mut [f64]) {
        let d = self.dims.n();
        let batched = output.len() / LANES * LANES;
        let (output, rest_output) = output.split_at_mut(batched);
        let (values, rest_values) = data.values.split_at(batched * d);
        for (scores, block) in output
            .chunks_exact_mut(LANES)
            .zip(values.chunks_exact(LANES * d))
        {
            // Re-slicing tells the compiler the block's length, which a
            // ChunksExact item does not; without it this loop is far slower.
            let block = &block[..LANES * d];
            let points = std::array::from_fn(|b| &block[b * d..(b + 1) * d]);
            scores.copy_from_slice(&self.score_lanes::<LANES>(points));
        }
        for (score, point) in rest_output.iter_mut().zip(rest_values.chunks_exact(d)) {
            *score = self.score(point);
        }
    }
}

/// A fitted mixture, used to label spikes outside the fitting subset.
struct Model<D> {
    noise_score: f64,
    gaussians: Vec<Gaussian<D>>,
}
impl<D: Dims> Model<D> {
    fn best(&self, point: &[f64]) -> (usize, f64) {
        let mut best = (0, self.noise_score);
        for gaussian in &self.gaussians {
            let score = gaussian.score(point);
            if score < best.1 {
                best = (gaussian.id, score);
            }
        }
        best
    }
}

/// `sum += sign * (point - mean)`, `scatter += sign * (point - mean)(point - mean)ᵀ`
/// over the upper triangle.
#[inline(always)]
fn add_centered(
    dims: impl Dims,
    point: &[f64],
    mean: &[f64],
    mut sum: Option<&mut [f64]>,
    scatter: &mut [f64],
    sign: f64,
) {
    let d = dims.n();
    let (point, mean, scatter) = (&point[..d], &mean[..d], &mut scatter[..d * d]);
    for i in 0..d {
        let delta = point[i] - mean[i];
        if let Some(sum) = &mut sum {
            sum[i] += sign * delta;
        }
        for j in i..d {
            scatter[i * d + j] += sign * delta * (point[j] - mean[j]);
        }
    }
}

/// Cluster statistics as of the last M-step, kept so the next M-step can
/// update only the spikes that changed cluster. Separate from `old_class`,
/// which belongs to the quick E-step. Each CEM run owns its own cache.
#[derive(Default)]
struct Moments {
    class: Vec<usize>,
    counts: Vec<usize>,
    /// Centered sums of products, `Σ(x-μ)(x-μ)ᵀ`, upper triangle.
    scatter: Vec<f64>,
    valid: Vec<bool>,
    changes: Vec<(usize, usize, usize)>,
    age: usize,
}

/// One CEM fit's working state. Component 0 is uniform noise.
struct State<D> {
    dims: D,
    data: Arc<Data>,
    class: Vec<usize>,
    old_class: Vec<usize>,
    second: Vec<usize>,
    alive: Vec<bool>,
    weight: Vec<f64>,
    mean: Vec<f64>,
    /// Upper triangles of each component's covariance.
    cov: Vec<f64>,
    /// Cluster-major: the E-step streams one cluster's column at a time.
    /// Grown on demand; old columns are kept for the quick E-step.
    scores: Vec<f64>,
}

impl<D: Dims> State<D> {
    fn new(dims: D, data: Arc<Data>) -> Self {
        let (n, d) = (data.points, dims.n());
        debug_assert_eq!(d, data.dims);
        Self {
            dims,
            data,
            class: vec![0; n],
            old_class: vec![0; n],
            second: vec![0; n],
            alive: vec![false; MAX_CLUSTERS],
            weight: vec![0.; MAX_CLUSTERS],
            mean: vec![0.; MAX_CLUSTERS * d],
            cov: vec![0.; MAX_CLUSTERS * d * d],
            scores: Vec::new(),
        }
    }

    fn ensure_score_columns(&mut self, columns: usize) {
        let length = columns * self.data.points;
        if length > self.scores.len() {
            self.scores.resize(length, f64::INFINITY);
        }
    }

    /// Live component IDs, in ascending order, starting with noise.
    fn ids(&self) -> Vec<usize> {
        (0..MAX_CLUSTERS)
            .filter(|&c| c == 0 || self.alive[c])
            .collect()
    }

    fn initialize(&mut self, starting: usize, random: &mut Random) {
        self.ensure_score_columns(starting);
        for class in &mut self.class {
            *class = random.inclusive(1, starting - 1);
        }
        for (c, alive) in self.alive.iter_mut().enumerate() {
            *alive = c < starting;
        }
    }

    /// Fit every component from its members: weight, mean, unbiased covariance.
    /// Components with no more members than dimensions are removed.
    fn m_step(&mut self, cache: Option<&mut Moments>) {
        let (n, d) = (self.data.points, self.dims.n());
        let mut counts = [0_usize; MAX_CLUSTERS];
        self.mean.fill(0.);
        self.cov.fill(0.);
        for &class in &self.class {
            counts[class] += 1;
        }
        for (c, &count) in counts.iter().enumerate().skip(1) {
            if count <= d {
                self.alive[c] = false;
            }
        }
        let ids = self.ids();
        for &c in &ids {
            self.weight[c] = (counts[c] + usize::from(c == 0)) as f64 / (n + 1) as f64;
        }
        for (p, &c) in self.class.iter().enumerate() {
            for (sum, &value) in self.mean[c * d..(c + 1) * d]
                .iter_mut()
                .zip(self.data.row(p))
            {
                *sum += value;
            }
        }
        for &c in &ids {
            if counts[c] > 0 {
                for value in &mut self.mean[c * d..(c + 1) * d] {
                    *value /= counts[c] as f64;
                }
            }
        }
        for (p, &c) in self.class.iter().enumerate() {
            if c == 0 || !self.alive[c] {
                continue;
            }
            let mean = &self.mean[c * d..(c + 1) * d];
            let cov = &mut self.cov[c * d * d..(c + 1) * d * d];
            add_centered(self.dims, self.data.row(p), mean, None, cov, 1.);
        }
        if let Some(cache) = cache {
            cache.class.clone_from(&self.class);
            cache.counts.clear();
            cache.counts.extend_from_slice(&counts);
            // Split fits have only one or two components; keep only the
            // prefix with members, not all 100 slots.
            let columns = counts.iter().rposition(|&n| n > 0).unwrap_or(0) + 1;
            cache.scatter.clear();
            cache
                .scatter
                .extend_from_slice(&self.cov[..columns * d * d]);
            cache.valid.clone_from(&self.alive);
            cache.age = 0;
        }
        for &c in ids.iter().skip(1) {
            for value in &mut self.cov[c * d * d..(c + 1) * d * d] {
                *value /= (counts[c] - 1) as f64;
            }
        }
    }

    /// The same fit as [`Self::m_step`], updating the cached statistics with
    /// only the spikes that changed cluster when few did. For a cluster with
    /// count `n`, centre `μ` and scatter `S`, with entering/leaving spikes
    /// summed about the *old* centre as `Δs = Σ±(x-μ)`, `ΔQ = Σ±(x-μ)(x-μ)ᵀ`:
    ///
    /// ```text
    /// μ' = μ + Δs/n'      S' = S + ΔQ - Δs Δsᵀ/n'      cov' = S'/(n'-1)
    /// ```
    ///
    /// Falls back to a full fit after large changes, a revived component, a
    /// failed Cholesky check, or every 20 updates, to bound rounding drift.
    fn m_step_incremental(&mut self, cache: &mut Moments) {
        let (n, d) = (self.data.points, self.dims.n());
        if cache.class.is_empty() || cache.age >= 19 {
            self.m_step(Some(cache));
            return;
        }
        cache.changes.clear();
        for (p, (&old, &new)) in cache.class.iter().zip(&self.class).enumerate() {
            if old != new {
                cache.changes.push((p, old, new));
                if cache.changes.len() > n / 8 || (new != 0 && self.alive[new] && !cache.valid[new])
                {
                    self.m_step(Some(cache));
                    return;
                }
            }
        }
        cache.age += 1;
        if cache.changes.is_empty() {
            return;
        }
        let mut delta_sum = vec![0.; MAX_CLUSTERS * d];
        let mut affected = [false; MAX_CLUSTERS];
        for &(p, old, new) in &cache.changes {
            cache.counts[old] -= 1;
            cache.counts[new] += 1;
            for (c, sign) in [(old, -1.), (new, 1.)] {
                affected[c] = true;
                if c == 0 || !cache.valid[c] {
                    continue;
                }
                add_centered(
                    self.dims,
                    self.data.row(p),
                    &self.mean[c * d..(c + 1) * d],
                    Some(&mut delta_sum[c * d..(c + 1) * d]),
                    &mut cache.scatter[c * d * d..(c + 1) * d * d],
                    sign,
                );
            }
        }
        for c in 1..MAX_CLUSTERS {
            if cache.counts[c] <= d {
                self.alive[c] = false;
            }
            if !self.alive[c] || !affected[c] {
                continue;
            }
            let count = cache.counts[c] as f64;
            let sum = &delta_sum[c * d..(c + 1) * d];
            let mean = &mut self.mean[c * d..(c + 1) * d];
            let scatter = &mut cache.scatter[c * d * d..(c + 1) * d * d];
            let cov = &mut self.cov[c * d * d..(c + 1) * d * d];
            for i in 0..d {
                mean[i] += sum[i] / count;
                for j in i..d {
                    scatter[i * d + j] -= sum[i] * sum[j] / count;
                    cov[i * d + j] = scatter[i * d + j] / (count - 1.);
                }
            }
            if cholesky(cov, d).is_none() {
                self.m_step(Some(cache));
                return;
            }
        }
        for c in self.ids() {
            self.weight[c] = (cache.counts[c] + usize::from(c == 0)) as f64 / (n + 1) as f64;
        }
        cache.class.clone_from(&self.class);
        cache.valid.clone_from(&self.alive);
    }

    fn gaussian(&self, c: usize) -> Option<Gaussian<D>> {
        let d = self.dims.n();
        Gaussian::new(
            self.dims,
            c,
            &self.mean[c * d..(c + 1) * d],
            &self.cov[c * d * d..(c + 1) * d * d],
            self.weight[c],
        )
    }

    fn noise_score(&self) -> f64 {
        f64::from((-self.weight[0].ln()) as f32)
    }

    /// Score spikes against every component. A quick (non-full) step skips
    /// spikes that kept their cluster and were far from this one last time,
    /// leaving their old score in place.
    fn e_step(&mut self, full: bool) {
        let n = self.data.points;
        let ids = self.ids();
        self.ensure_score_columns(ids.last().unwrap() + 1);
        let noise = self.noise_score();
        self.scores[..n].fill(noise);
        for c in ids.into_iter().skip(1) {
            let Some(gaussian) = self.gaussian(c) else {
                // Singular covariance: remove the component immediately.
                self.alive[c] = false;
                continue;
            };
            if full {
                gaussian.score_all(&self.data, &mut self.scores[c * n..(c + 1) * n]);
                continue;
            }
            for p in 0..n {
                if self.class[p] == self.old_class[p]
                    && self.scores[c * n + p] - self.scores[self.class[p] * n + p] > 1000_f64.ln()
                {
                    continue;
                }
                self.scores[c * n + p] = gaussian.score(self.data.row(p));
            }
        }
    }

    /// Assign each spike its best component and remember the runner-up.
    /// Ties go to the lower component ID.
    fn c_step(&mut self, noise: bool) -> bool {
        let n = self.data.points;
        let candidates: Vec<_> = self
            .ids()
            .into_iter()
            .skip(usize::from(!noise))
            .map(|c| (c, &self.scores[c * n..(c + 1) * n]))
            .collect();
        if candidates.is_empty() {
            return false;
        }
        for p in 0..n {
            self.old_class[p] = self.class[p];
            let (mut best, mut second) = ((0, f64::INFINITY), (0, f64::INFINITY));
            for &(c, scores) in &candidates {
                if scores[p] < best.1 {
                    second = best;
                    best = (c, scores[p]);
                } else if scores[p] < second.1 {
                    second = (c, scores[p]);
                }
            }
            self.class[p] = best.0;
            self.second[p] = second.0;
        }
        true
    }

    /// Delete the component whose spikes lose least by moving to their
    /// runner-up, if that loss is smaller than the BIC penalty saved.
    fn delete(&mut self) -> bool {
        let ids = self.ids();
        if ids.len() <= 1 {
            return false;
        }
        let mut losses = [0.; MAX_CLUSTERS];
        let n = self.data.points;
        for (p, &c) in self.class.iter().enumerate() {
            losses[c] += self.scores[self.second[p] * n + p] - self.scores[c * n + p];
        }
        let candidate = *ids
            .iter()
            .skip(1)
            .min_by(|&&a, &&b| losses[a].total_cmp(&losses[b]))
            .unwrap();
        if losses[candidate] >= penalty(n, self.dims.n(), 2) {
            return false;
        }
        self.alive[candidate] = false;
        for (p, class) in self.class.iter_mut().enumerate() {
            if *class == candidate {
                *class = self.second[p];
            }
        }
        true
    }

    /// Total score of the current assignments plus the BIC penalty.
    fn score(&self) -> f64 {
        let mut score = penalty(self.data.points, self.dims.n(), self.ids().len());
        for (p, &c) in self.class.iter().enumerate() {
            if c > 0 && !self.alive[c] {
                return f64::INFINITY;
            }
            score += self.scores[c * self.data.points + p];
        }
        score
    }

    /// Run classification EM to convergence. `recurse` enables deletion and
    /// split proposals; `noise` lets spikes be assigned to the noise component.
    fn cem(
        &mut self,
        options: &Options,
        random: &mut Random,
        recurse: bool,
        noise: bool,
        report: &mut dyn FnMut(usize, usize, bool) -> Result<()>,
    ) -> Result<f64> {
        let mut score = f64::INFINITY;
        let mut full = true;
        let mut previous = vec![0; self.class.len()];
        let mut moments = Moments::default();
        for iteration in 1..=options.max_iterations {
            previous.copy_from_slice(&self.class);
            self.m_step_incremental(&mut moments);
            self.e_step(full);
            if !self.c_step(noise) {
                return Ok(f64::INFINITY);
            }
            if recurse {
                self.delete();
            }
            let changed = previous
                .iter()
                .zip(&self.class)
                .filter(|(a, b)| a != b)
                .count();
            let old_score = score;
            score = self.score();
            let last_full = full;
            full = changed as f64 > 0.05 * self.data.points as f64
                || changed == 0
                || iteration % 20 == 0
                || score > old_score;
            let split_due = iteration == 20
                || (iteration > 20 && (iteration - 20) % 40 == 39)
                || (changed == 0 && last_full);
            let clusters = |state: &Self| state.alive.iter().skip(1).filter(|&&v| v).count();
            report(iteration, clusters(self), false)?;
            let split = if recurse && split_due {
                report(iteration, clusters(self), true)?;
                let split = self.try_splits(options, random)?;
                report(iteration, clusters(self), false)?;
                split
            } else {
                false
            };
            if changed == 0 && last_full && !split {
                return Ok(score);
            }
        }
        Err(Error::Message(format!(
            "Clustering did not converge within {} iterations",
            options.max_iterations
        )))
    }

    /// For each cluster, fit one and two clusters to its spikes alone. Where
    /// two fit better, accept the split if it improves the whole model's
    /// score relative to the score at the start of this pass.
    fn try_splits(&mut self, options: &Options, random: &mut Random) -> Result<bool> {
        let ids = self.ids();
        if ids.len() >= MAX_CLUSTERS - 1 {
            return Ok(false);
        }
        let baseline = self.score();
        let mut did_split = false;
        for c in ids.into_iter().skip(1) {
            let indices: Vec<_> = self
                .class
                .iter()
                .enumerate()
                .filter_map(|(p, &label)| (label == c).then_some(p))
                .collect();
            if indices.is_empty() {
                continue;
            }
            let Some(unused) = (1..MAX_CLUSTERS).find(|&c| !self.alive[c]) else {
                break;
            };
            let mut local = State::new(self.dims, Arc::new(self.data.subset(&indices)));
            local.initialize(2, random);
            let unsplit = local.cem(options, random, false, false, &mut |_, _, _| Ok(()))?;
            local.initialize(3, random);
            let split = local.cem(options, random, false, false, &mut |_, _, _| Ok(()))?;
            if split >= unsplit || !split.is_finite() || local.ids().len() < 3 {
                continue;
            }
            let mut proposal = State::new(self.dims, Arc::clone(&self.data));
            proposal.class.clone_from(&self.class);
            for (p, &source) in indices.iter().enumerate() {
                proposal.class[source] = if local.class[p] == 1 { c } else { unused };
            }
            for &class in &proposal.class {
                proposal.alive[class] = true;
            }
            proposal.m_step(None);
            proposal.e_step(true);
            // Score the proposed assignments as they are, without a C-step.
            if proposal.score() < baseline {
                self.ensure_score_columns(proposal.scores.len() / self.data.points);
                self.class = proposal.class;
                self.alive = proposal.alive;
                did_split = true;
            }
        }
        Ok(did_split)
    }

    fn model(&self) -> Model<D> {
        Model {
            noise_score: self.noise_score(),
            gaussians: self
                .ids()
                .into_iter()
                .skip(1)
                .filter_map(|c| self.gaussian(c))
                .collect(),
        }
    }

    /// Weights, means and full covariances indexed by component ID (0 is
    /// noise). Means and covariances are NaN where there is no Gaussian.
    fn dump_model(&self, dump: &Dump, trial: usize) -> Result<()> {
        let d = self.dims.n();
        let k = self.ids().last().unwrap() + 1;
        let fitted = |c: usize| c > 0 && self.alive[c];
        let name = |part: &str| format!("trial_{trial}_{part}");
        dump.array(
            Point::Models,
            &name("weights"),
            &[k],
            (0..k).map(|c| {
                if c == 0 || fitted(c) {
                    self.weight[c]
                } else {
                    0.
                }
            }),
        )?;
        dump.array(
            Point::Models,
            &name("means"),
            &[k, d],
            (0..k * d).map(|i| {
                if fitted(i / d) {
                    self.mean[i]
                } else {
                    f64::NAN
                }
            }),
        )?;
        dump.array(
            Point::Models,
            &name("covariances"),
            &[k, d, d],
            (0..k * d * d).map(|index| {
                let (c, i, j) = (index / (d * d), index / d % d, index % d);
                let upper = c * d * d + i.min(j) * d + i.max(j);
                if fitted(c) {
                    self.cov[upper]
                } else {
                    f64::NAN
                }
            }),
        )
    }
}

pub struct Output {
    /// One byte per spike: the winning trial's component IDs.
    pub labels: PathBuf,
    pub counts: [usize; MAX_CLUSTERS],
    pub score: f64,
    pub fitting_spikes: usize,
    pub stride: usize,
}

/// The fitting subset: one random spike from each complete interval of
/// `stride = max(1, floor(N / target))`, giving `floor(N / stride)` spikes.
fn sampling_plan(points: usize, target: usize) -> (usize, usize) {
    let stride = (points / target).max(1);
    (stride, points / stride)
}

pub(crate) fn run(
    store: &FeatureStore,
    options: &Options,
    work: &Path,
    log: &mut dyn Write,
    notify: &mut dyn FnMut(Update) -> Result<()>,
    dump: &Dump,
) -> Result<Output> {
    match store.dims() {
        3 => run_with(Fixed::<3>, store, options, work, log, notify, dump),
        6 => run_with(Fixed::<6>, store, options, work, log, notify, dump),
        9 => run_with(Fixed::<9>, store, options, work, log, notify, dump),
        12 => run_with(Fixed::<12>, store, options, work, log, notify, dump),
        d => run_with(d, store, options, work, log, notify, dump),
    }
}

fn run_with<D: Dims>(
    dims: D,
    store: &FeatureStore,
    options: &Options,
    work: &Path,
    log: &mut dyn Write,
    notify: &mut dyn FnMut(Update) -> Result<()>,
    dump: &Dump,
) -> Result<Output> {
    options.validate()?;
    let (stride, fitting_spikes) = sampling_plan(store.points, options.target_spikes);
    notify(Update::Dataset {
        spikes: store.points,
        fitting_spikes,
    })?;
    writeln!(
        log,
        "{} spikes, {} active features; fitting {fitting_spikes} (stride {stride})",
        store.points,
        store.dims()
    )?;
    let mut random = Random(options.seed);
    let mut best: Option<Output> = None;
    let profile = std::env::var_os("LTXK_PROFILE").is_some();
    for starting in options.min_clusters..=options.max_clusters {
        notify(Update::Trial {
            current: starting - options.min_clusters + 1,
            total: options.max_clusters - options.min_clusters + 1,
            initial: starting,
        })?;
        let started = Instant::now();
        let indices: Vec<_> = (0..fitting_spikes)
            .map(|i| {
                if stride == 1 {
                    i
                } else {
                    stride * i + random.inclusive(0, stride - 1)
                }
            })
            .collect();
        dump.array(
            Point::Subsets,
            &format!("trial_{starting}_subset"),
            &[indices.len()],
            indices.iter().map(|&i| i as u32),
        )?;
        let mut state = State::new(dims, Arc::new(store.load_indices(&indices)?));
        let loaded = Instant::now();
        notify(Update::Stage(Stage::Fitting))?;
        let fitted_score = if store.dims() == 0 {
            state.alive[0] = true;
            state.weight[0] = 1.;
            state.class.fill(0);
            0.
        } else {
            state.initialize(starting, &mut random);
            let mut report = |iteration, clusters, splitting| {
                notify(Update::Iteration {
                    iteration,
                    clusters,
                    splitting,
                })
            };
            state.cem(options, &mut random, true, true, &mut report)?
        };
        let fitted = Instant::now();
        state.dump_model(dump, starting)?;

        // Label every spike: the fit's own labels, or the model's best match.
        let model = state.model();
        let candidate = work.join("candidate.labels");
        let mut labels = BufWriter::with_capacity(1 << 20, File::create(&candidate)?);
        let mut counts = [0; MAX_CLUSTERS];
        let mut score = fitted_score;
        notify(Update::Stage(Stage::Assigning))?;
        if stride == 1 {
            for &class in &state.class {
                labels.write_all(&[class as u8])?;
                counts[class] += 1;
            }
        } else {
            score = penalty(store.points, store.dims(), state.ids().len());
            let mut assigned = 0_usize;
            store.for_each(|row| {
                let (class, value) = model.best(row);
                counts[class] += 1;
                score += value;
                labels.write_all(&[class as u8])?;
                assigned += 1;
                if assigned.is_multiple_of(4096) {
                    notify(Update::Assigned { count: assigned })?;
                }
                Ok(())
            })?;
        }
        labels.flush()?;
        drop(labels);
        notify(Update::Assigned {
            count: store.points,
        })?;
        if dump.wants(Point::Labels) {
            let name = format!("trial_{starting}_labels");
            dump.array(
                Point::Labels,
                &name,
                &[store.points],
                std::fs::read(&candidate)?,
            )?;
        }
        if profile {
            writeln!(
                log,
                "Profile trial={starting} load_s={:.6} fit_s={:.6} assign_s={:.6}",
                (loaded - started).as_secs_f64(),
                (fitted - loaded).as_secs_f64(),
                fitted.elapsed().as_secs_f64()
            )?;
        }
        if !score.is_finite() {
            return Err(Error::Invalid("Clustering produced a non-finite score"));
        }
        let clusters = counts.iter().skip(1).filter(|&&n| n > 0).count();
        notify(Update::TrialDone {
            clusters,
            noise: counts[0],
        })?;
        writeln!(
            log,
            "Start {starting}: {clusters} signal clusters, {} noise spikes, full score {score:.8}",
            counts[0]
        )?;
        if best.as_ref().is_none_or(|best| score < best.score) {
            let path = work.join("best.labels");
            std::fs::copy(&candidate, &path)?;
            best = Some(Output {
                labels: path,
                counts,
                score,
                fitting_spikes,
                stride,
            });
        }
    }
    Ok(best.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(dims: usize, values: Vec<f64>) -> Arc<Data> {
        Arc::new(Data {
            dims,
            points: values.len() / dims,
            values,
        })
    }

    /// Deterministic values in [0, 1) for synthetic feature sets.
    fn pattern(points: usize, dims: usize) -> Arc<Data> {
        data(
            dims,
            (0..points * dims)
                .map(|i| ((i * 37 + i / dims * 19) % 101) as f64 / 100.)
                .collect(),
        )
    }

    #[test]
    fn score_columns_grow_without_discarding_quick_step_history() {
        let mut state = State::new(1, data(1, vec![0., 1.]));
        state.ensure_score_columns(2);
        state
            .scores
            .copy_from_slice(&[-0.0, 1.25, f64::INFINITY, -7.]);
        let original: Vec<_> = state.scores.iter().map(|x| x.to_bits()).collect();
        state.ensure_score_columns(4);
        assert_eq!(
            state.scores[..4]
                .iter()
                .map(|x| x.to_bits())
                .collect::<Vec<_>>(),
            original
        );
        assert!(state.scores[4..].iter().all(|&x| x == f64::INFINITY));
        state.ensure_score_columns(1);
        assert_eq!(state.scores.len(), 8);
    }

    #[test]
    fn column_scores_keep_ascending_ties_and_noise_exclusion() {
        let mut state = State::new(1, data(1, vec![0.; 3]));
        state.alive[1..3].fill(true);
        state.ensure_score_columns(3);
        state
            .scores
            .copy_from_slice(&[1., 5., 0., 1., 2., 3., 1., 1., 3.]);
        assert!(state.c_step(true));
        assert_eq!(state.class, [0, 2, 0]);
        assert_eq!(state.second, [1, 1, 1]);
        let expected = ((penalty(3, 1, 3) + 1.) + 1.) + 0.;
        assert_eq!(state.score().to_bits(), expected.to_bits());
        assert!(state.c_step(false));
        assert_eq!(state.class, [1, 2, 1]);
        assert_eq!(state.second, [2, 1, 2]);
    }

    #[test]
    fn subset_counts_cross_large_recordings_without_allocating_them() {
        assert_eq!(sampling_plan(9_000_000, 25_000), (360, 25_000));
        assert_eq!(sampling_plan(49_999, 25_000), (1, 49_999));
        assert_eq!(sampling_plan(50_000, 25_000), (2, 25_000));
        assert_eq!(sampling_plan(1_000_019, 25_000), (40, 25_000));
    }

    #[test]
    fn random_sequence_matches_msvc_rand() {
        let mut random = Random(1);
        assert_eq!(
            (0..5)
                .map(|_| random.inclusive(0, 32767))
                .collect::<Vec<_>>(),
            [41, 18467, 6334, 26500, 19169]
        );
    }

    #[test]
    fn gaussian_score_uses_full_covariance() {
        let gaussian = Gaussian::new(2, 1, &[1., 2.], &[4., 2., 0., 3.], 0.25).unwrap();
        let score = gaussian.score(&[3., 5.]);
        // det=8; delta' inv(cov) delta = 3, including the cross term.
        assert!(
            (score
                - (3. / 2. + 8_f64.ln() / 2. - 0.25_f64.ln() + (2. * std::f64::consts::PI).ln()))
            .abs()
                < 2e-7
        );
        assert!(cholesky(&[1., 1., 0., 1.], 2).is_none());
    }

    /// A one-spike solve written as plainly as possible.
    fn reference_score<D: Dims>(gaussian: &Gaussian<D>, point: &[f64]) -> f64 {
        let d = gaussian.mean.len();
        let mut z = vec![0.; d];
        for i in 0..d {
            let mut value = point[i] - gaussian.mean[i];
            for (j, &previous) in z[..i].iter().enumerate() {
                value -= gaussian.lower[i * d + j] * previous;
            }
            z[i] = value * gaussian.inverse_diagonal[i];
        }
        z.iter().map(|x| x * x).sum::<f64>() / 2. + gaussian.offset
    }

    fn correlated_gaussian<D: Dims>(dims: D) -> (Gaussian<D>, Vec<f64>) {
        let d = dims.n();
        let mut lower = vec![0.; d * d];
        for i in 0..d {
            for j in 0..=i {
                lower[i * d + j] = if i == j {
                    0.25 + i as f64 / 32.
                } else {
                    (i + j) as f64 / 256.
                };
            }
        }
        let mut cov = vec![0.; d * d];
        for i in 0..d {
            for j in i..d {
                cov[i * d + j] = (0..d).map(|k| lower[i * d + k] * lower[j * d + k]).sum();
            }
        }
        (
            Gaussian::new(dims, 1, &vec![0.5; d], &cov, 0.7).unwrap(),
            lower,
        )
    }

    fn check_scores<D: Dims>(dims: D) {
        let d = dims.n();
        let (gaussian, lower) = correlated_gaussian(dims);
        // Analytic: a point mean + L z scores |z|²/2 + offset.
        let z: Vec<f64> = (0..d).map(|i| (i as f64 - 4.) / 7.).collect();
        let point: Vec<f64> = (0..d)
            .map(|i| 0.5 + (0..=i).map(|j| lower[i * d + j] * z[j]).sum::<f64>())
            .collect();
        let expected = z.iter().map(|x| x * x).sum::<f64>() / 2. + gaussian.offset;
        assert!((gaussian.score(&point) - expected).abs() < 1e-9);
        // Batched and single scores match the plain solve bit for bit,
        // including partial batches.
        for n in 1..=9 {
            let data = pattern(n, d);
            let expected: Vec<_> = (0..n)
                .map(|p| reference_score(&gaussian, data.row(p)).to_bits())
                .collect();
            let mut actual = vec![f64::INFINITY; n];
            gaussian.score_all(&data, &mut actual);
            assert_eq!(
                actual.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected
            );
            for (p, &expected) in expected.iter().enumerate() {
                assert_eq!(gaussian.score(data.row(p)).to_bits(), expected);
            }
        }
    }

    #[test]
    fn fixed_and_runtime_dimensions_score_identically() {
        check_scores(Fixed::<3>);
        check_scores(Fixed::<12>);
        check_scores(3_usize);
        check_scores(7_usize);
        check_scores(12_usize);
        let (fixed, _) = correlated_gaussian(Fixed::<12>);
        let (runtime, _) = correlated_gaussian(12_usize);
        let data = pattern(33, 12);
        let (mut a, mut b) = (vec![0.; 33], vec![0.; 33]);
        fixed.score_all(&data, &mut a);
        runtime.score_all(&data, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn full_and_quick_steps_preserve_stale_score_history() {
        for dims in [3, 12] {
            for n in 208..=213 {
                for full in [false, true] {
                    let mut state = State::new(dims, pattern(n, dims));
                    state.class = (0..n).map(|p| [0, 1, 7][p % 3]).collect();
                    state.old_class = state
                        .class
                        .iter()
                        .enumerate()
                        .map(|(p, &c)| if p % 7 == 0 { 2 } else { c })
                        .collect();
                    state.alive[1] = true;
                    state.alive[7] = true;
                    state.weight[0] = 0.5;
                    for c in [1, 7] {
                        state.weight[c] = 0.25;
                        for i in 0..dims {
                            state.mean[c * dims + i] = 0.5;
                            state.cov[c * dims * dims + i * dims + i] = 0.05;
                        }
                    }
                    state.ensure_score_columns(8);
                    state.scores = (0..8 * n)
                        .map(|i| {
                            if i % 17 == 0 {
                                f64::INFINITY
                            } else {
                                (i * 37 % 109) as f64 - 54.
                            }
                        })
                        .collect();
                    let mut expected = state.scores.clone();
                    expected[..n].fill(state.noise_score());
                    for c in state.ids().into_iter().skip(1) {
                        let gaussian = state.gaussian(c).unwrap();
                        for p in 0..n {
                            if !full
                                && state.class[p] == state.old_class[p]
                                && expected[c * n + p] - expected[state.class[p] * n + p]
                                    > 1000_f64.ln()
                            {
                                continue;
                            }
                            expected[c * n + p] = reference_score(&gaussian, state.data.row(p));
                        }
                    }
                    state.e_step(full);
                    assert_eq!(
                        state.scores.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                        expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                    );
                }
            }
        }
    }

    #[test]
    fn reciprocal_diagonals_remain_finite_across_covariance_scales() {
        for variance in [1e-280_f64, 1e-12, 0.25, 1e12, 1e280] {
            let mut covariance = vec![0.; 9];
            for i in 0..3 {
                covariance[i * 3 + i] = variance;
            }
            let gaussian = Gaussian::new(Fixed::<3>, 1, &[0.; 3], &covariance, 0.7).unwrap();
            assert!(gaussian
                .inverse_diagonal
                .iter()
                .all(|x| x.is_finite() && *x > 0.));
            let scale = variance.sqrt();
            // z = (1, 2, 3) exactly, up to rounding of the reciprocal.
            let score = gaussian.score(&[scale, 2. * scale, 3. * scale]);
            let expected = 7. + gaussian.offset;
            assert!((score - expected).abs() <= 16. * f64::EPSILON * (1. + expected.abs()));
        }
    }

    #[test]
    fn uniform_noise_wins_for_outliers_but_not_for_a_gaussian_peak() {
        let gaussian = Gaussian::new(2, 1, &[0.2, 0.2], &[0.0001, 0., 0., 0.0001], 0.9).unwrap();
        let model = Model {
            noise_score: -0.1_f64.ln(),
            gaussians: vec![gaussian],
        };
        assert_eq!(model.best(&[0.2, 0.2]).0, 1);
        assert_eq!(model.best(&[0.9, 0.9]).0, 0);
    }

    #[test]
    fn m_step_unbiased_covariance_and_noise_pseudocount() {
        let mut state = State::new(1, data(1, vec![0., 1., 2., 3.]));
        state.class.fill(1);
        state.alive[1] = true;
        state.m_step(None);
        assert_eq!(state.weight[0], 0.2);
        assert_eq!(state.weight[1], 0.8);
        assert_eq!(state.mean[1], 1.5);
        assert!((state.cov[1] - 5. / 3.).abs() < 1e-14);
        assert!((penalty(4, 1, 2) - 1.5 * 4_f64.ln()).abs() < 1e-14);
    }

    #[test]
    fn covariances_match_global_moments() {
        fn check<D: Dims>(dims: D) {
            let (points, d) = (64, dims.n());
            let data = pattern(points, d);
            let mut state = State::new(dims, Arc::clone(&data));
            state.class.fill(1);
            state.alive[1] = true;
            state.m_step(None);
            for i in 0..d {
                let sum_i: f64 = (0..points).map(|p| data.row(p)[i]).sum();
                assert!((state.mean[d + i] - sum_i / points as f64).abs() < 1e-14);
                for j in i..d {
                    let sum_j: f64 = (0..points).map(|p| data.row(p)[j]).sum();
                    let sum_ij: f64 = (0..points).map(|p| data.row(p)[i] * data.row(p)[j]).sum();
                    let expected = (sum_ij - sum_i * sum_j / points as f64) / (points - 1) as f64;
                    assert!((state.cov[d * d + i * d + j] - expected).abs() < 1e-12);
                }
            }
        }
        check(Fixed::<3>);
        check(Fixed::<12>);
        check(5_usize);
    }

    fn assert_same_moments<D: Dims>(actual: &State<D>, expected: &State<D>, tolerance: f64) {
        assert_eq!(actual.alive, expected.alive);
        let d = actual.dims.n();
        for c in actual.ids() {
            assert_eq!(actual.weight[c], expected.weight[c]);
            if c == 0 {
                continue;
            }
            for i in 0..d {
                assert!((actual.mean[c * d + i] - expected.mean[c * d + i]).abs() < tolerance);
                for j in i..d {
                    let k = c * d * d + i * d + j;
                    assert!(
                        (actual.cov[k] - expected.cov[k]).abs() < tolerance,
                        "cluster {c}, ({i},{j}): {} versus {}",
                        actual.cov[k],
                        expected.cov[k]
                    );
                }
            }
        }
    }

    #[test]
    fn incremental_moments_match_fresh_fits_after_exchanges_and_noise_moves() {
        fn check<D: Dims>(dims: D) {
            let points = 512;
            let data = pattern(points, dims.n());
            let mut updated = State::new(dims, Arc::clone(&data));
            let mut fresh = State::new(dims, data);
            updated.class = (0..points).map(|i| 1 + i % 2).collect();
            updated.alive[1..3].fill(true);
            let mut cache = Moments::default();
            for iteration in 0..100 {
                if iteration > 0 {
                    let p = iteration * 37 % points;
                    updated.class[p] = (updated.class[p] + 1) % 3;
                    // This includes exchanging spikes without changing counts.
                    let q = (p + 1) % points;
                    updated.class[q] = 3 - updated.class[q].max(1);
                }
                fresh.class.clone_from(&updated.class);
                fresh.alive.clone_from(&updated.alive);
                updated.m_step_incremental(&mut cache);
                fresh.m_step(None);
                assert_same_moments(&updated, &fresh, 2e-14);
                assert_eq!(cache.class, updated.class);
                assert!(cache.age < 20);
            }
        }
        check(1_usize);
        check(Fixed::<3>);
        check(7_usize);
        check(Fixed::<12>);
    }

    #[test]
    fn incremental_moments_refresh_after_deletion_revival_and_reinitialization() {
        let points = 200;
        let data = data(
            1,
            (0..points).map(|p| (p * 37 % 101) as f64 / 100.).collect(),
        );
        let mut state = State::new(1, Arc::clone(&data));
        state.class = (0..points).map(|p| 1 + usize::from(p < 5)).collect();
        state.alive[1..3].fill(true);
        let mut cache = Moments::default();
        state.m_step_incremental(&mut cache);
        state.class[..5].fill(1);
        state.alive[2] = false;
        state.m_step_incremental(&mut cache);
        assert!(!cache.valid[2]);
        // A split can reuse a deleted ID, possibly including an old member.
        state.class[..5].fill(2);
        state.alive[2] = true;
        state.m_step_incremental(&mut cache);
        assert_eq!(cache.age, 0);
        let mut fresh = State::new(1, data);
        fresh.class.clone_from(&state.class);
        fresh.alive.clone_from(&state.alive);
        fresh.m_step(None);
        assert_same_moments(&state, &fresh, f64::EPSILON);
        state.initialize(4, &mut Random(1));
        state.m_step_incremental(&mut cache);
        assert_eq!(cache.age, 0);
        assert_eq!(cache.class, state.class);
    }

    #[test]
    fn incremental_downdate_rebuilds_before_discarding_a_singular_cluster() {
        let mut state = State::new(1, data(1, (0..100).map(|p| f64::from(p == 0)).collect()));
        state.class.fill(1);
        state.alive[1] = true;
        let mut cache = Moments::default();
        state.m_step_incremental(&mut cache);
        state.class[0] = 0;
        // Force a small negative downdate remainder; a fresh covariance is zero.
        cache.scatter[1] -= 1e-12;
        state.m_step_incremental(&mut cache);
        assert_eq!(cache.age, 0);
        assert_eq!(state.cov[1], 0.);
        assert!(state.alive[1]);
        state.e_step(true);
        assert!(!state.alive[1]);
    }

    #[test]
    fn incremental_moments_keep_small_variances_around_a_large_center() {
        let points = 256;
        let data = data(
            1,
            (0..points)
                .map(|p| 0.75 + (p * 37 % 101) as f64 * 1e-10)
                .collect(),
        );
        let mut updated = State::new(1, Arc::clone(&data));
        updated.class = (0..points).map(|p| 1 + p % 2).collect();
        updated.alive[1..3].fill(true);
        let mut fresh = State::new(1, data);
        let mut cache = Moments::default();
        for iteration in 0..100 {
            updated.class[iteration] = 3 - updated.class[iteration];
            updated.m_step_incremental(&mut cache);
            fresh.class.clone_from(&updated.class);
            fresh.alive.clone_from(&updated.alive);
            fresh.m_step(None);
            for c in 1..3 {
                assert!((updated.mean[c] - fresh.mean[c]).abs() < 2e-15);
                assert!((updated.cov[c] / fresh.cov[c] - 1.).abs() < 2e-6);
            }
        }
    }

    #[test]
    fn deletion_reassigns_to_runner_up_when_bic_gain_exceeds_loss() {
        let mut state = State::new(1, data(1, vec![0.; 4]));
        state.alive[1] = true;
        state.class.fill(1);
        state.second.fill(0);
        state.ensure_score_columns(2);
        state.scores[..4].fill(0.1);
        state.scores[4..8].fill(0.);
        assert!(state.delete());
        assert_eq!(state.class, [0; 4]);
        assert!(!state.alive[1]);
    }

    #[test]
    fn split_search_recovers_two_well_separated_populations() {
        let data = data(
            1,
            (0..400)
                .map(|i| (i % 2) as f64 * 0.8 + (i * 37 % 101) as f64 / 1000.)
                .collect(),
        );
        let options = Options {
            min_clusters: 2,
            max_clusters: 2,
            ..Options::default()
        };
        // CEM uses random split proposals, so a single proposal can collapse.
        let mut recovered = false;
        for seed in 1..=32 {
            let mut state = State::new(1, Arc::clone(&data));
            let mut random = Random(seed);
            state.initialize(2, &mut random);
            let score = state
                .cem(&options, &mut random, true, true, &mut |_, _, _| Ok(()))
                .unwrap();
            assert!(score.is_finite());
            if state.class[0] != state.class[1] {
                assert!(state
                    .class
                    .iter()
                    .enumerate()
                    .all(|(i, &c)| c == state.class[i % 2]));
                recovered = true;
                break;
            }
        }
        assert!(
            recovered,
            "No split proposal recovered the known populations"
        );
    }
}
