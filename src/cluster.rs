//! Classic (unmasked) KlustaKwik CEM: full covariance, uniform noise,
//! BIC deletion, recursive split proposals, and the ltxk subset policy.
//! Algorithm reference: klusta-team/klustakwik at 9ca1d21 (June 2014).
use crate::feature_store::{Data, FeatureStore};
use crate::progress::{Stage, Update};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub const MAX_CLUSTERS: usize = 100; // Includes the permanent uniform-noise component.

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

/// Explicit Windows CRT generator, so results do not depend on the host's rand().
struct Random(u32);
impl Random {
    fn inclusive(&mut self, low: usize, high: usize) -> usize {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        low + ((self.0 >> 16) & 0x7fff) as usize % (high - low + 1)
    }
}

fn penalty(points: usize, dims: usize, alive: usize) -> f64 {
    let parameters = (dims * (dims + 1) / 2 + dims + 1) * (alive - 1);
    parameters as f64 * (points as f64).ln() / 2.0
}

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

#[derive(Clone)]
struct Gaussian {
    id: usize,
    mean: Vec<f64>,
    lower: Vec<f64>,
    inverse_diagonal: Vec<f64>,
    offset: f64,
}
impl Gaussian {
    fn new(id: usize, mean: &[f64], cov: &[f64], weight: f64) -> Option<Self> {
        let dims = mean.len();
        let lower = cholesky(cov, dims)?;
        // Retain the historical casts to float in the log-normalizing terms.
        let log_root_det: f64 = (0..dims)
            .map(|i| f64::from(lower[i * dims + i].ln() as f32))
            .sum();
        let offset = log_root_det - weight.ln()
            + f64::from((2.0 * std::f64::consts::PI).ln() as f32) * dims as f64 / 2.0;
        let inverse_diagonal = (0..dims).map(|i| 1.0 / lower[i * dims + i]).collect();
        Some(Self {
            id,
            mean: mean.to_vec(),
            lower,
            inverse_diagonal,
            offset,
        })
    }

    fn score(&self, point: &[f64], root: &mut [f64]) -> f64 {
        match self.mean.len() {
            3 => self.score_fixed::<3>(point, root),
            12 => self.score_fixed::<12>(point, root),
            _ => self.score_dynamic(point, root),
        }
    }

    // Common 3PC dimensions let LLVM remove loop bounds and unroll the solve.
    // Cached reciprocal diagonals replace repeated divisions. All reductions
    // retain their order, but each reciprocal multiplication can round differently.
    fn score_fixed<const D: usize>(&self, point: &[f64], root: &mut [f64]) -> f64 {
        let mean: &[f64; D] = self.mean.as_slice().try_into().unwrap();
        let lower: &[[f64; D]; D] = self.lower.as_chunks::<D>().0.try_into().unwrap();
        let inverse_diagonal: &[f64; D] = self.inverse_diagonal.as_slice().try_into().unwrap();
        let point: &[f64; D] = point.try_into().unwrap();
        let root: &mut [f64; D] = root.try_into().unwrap();
        for i in 0..D {
            let mut value = point[i] - mean[i];
            for (j, &previous) in root[..i].iter().enumerate() {
                value -= lower[i][j] * previous;
            }
            root[i] = value * inverse_diagonal[i];
        }
        root.iter().map(|x| x * x).sum::<f64>() / 2.0 + self.offset
    }

    fn score_dynamic(&self, point: &[f64], root: &mut [f64]) -> f64 {
        let dims = self.mean.len();
        for i in 0..dims {
            let mut value = point[i] - self.mean[i];
            for (j, &previous) in root[..i].iter().enumerate() {
                value -= self.lower[i * dims + j] * previous;
            }
            root[i] = value / self.lower[i * dims + i];
        }
        root.iter().map(|x| x * x).sum::<f64>() / 2.0 + self.offset
    }
}

struct Model {
    noise_score: f64,
    gaussians: Vec<Gaussian>,
}
impl Model {
    fn best(&self, point: &[f64], root: &mut [f64]) -> (usize, f64) {
        let mut best = (0, self.noise_score);
        for gaussian in &self.gaussians {
            let score = gaussian.score(point, root);
            if score < best.1 {
                best = (gaussian.id, score);
            }
        }
        best
    }
}

struct State {
    data: Arc<Data>,
    class: Vec<usize>,
    old_class: Vec<usize>,
    second: Vec<usize>,
    alive: Vec<bool>,
    weight: Vec<f64>,
    mean: Vec<f64>,
    cov: Vec<f64>,
    // Cluster-major columns: the E-step streams one cluster's scores at a time.
    // Grow on demand and retain old columns for the historical quick E-step.
    scores: Vec<f64>,
}

fn accumulate_cov_fixed<const D: usize>(point: &[f64], mean: &[f64], cov: &mut [f64]) {
    let point: &[f64; D] = point.try_into().unwrap();
    let mean: &[f64; D] = mean.try_into().unwrap();
    let cov: &mut [[f64; D]; D] = cov.as_chunks_mut::<D>().0.try_into().unwrap();
    let delta: [f64; D] = std::array::from_fn(|i| point[i] - mean[i]);
    for i in 0..D {
        for j in i..D {
            cov[i][j] += delta[i] * delta[j];
        }
    }
}

impl State {
    fn new(data: Arc<Data>) -> Self {
        let (n, d) = (data.points, data.dims);
        Self {
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

    fn m_step(&mut self) {
        let (n, d) = (self.data.points, self.data.dims);
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
        let mut delta = vec![0.; d];
        for (p, &c) in self.class.iter().enumerate() {
            if c == 0 || !self.alive[c] {
                continue;
            }
            let point = self.data.row(p);
            let mean = &self.mean[c * d..(c + 1) * d];
            let cov = &mut self.cov[c * d * d..(c + 1) * d * d];
            match d {
                3 => accumulate_cov_fixed::<3>(point, mean, cov),
                12 => accumulate_cov_fixed::<12>(point, mean, cov),
                _ => {
                    for ((value, &sample), &center) in delta.iter_mut().zip(point).zip(mean) {
                        *value = sample - center;
                    }
                    for i in 0..d {
                        for j in i..d {
                            cov[i * d + j] += delta[i] * delta[j];
                        }
                    }
                }
            }
        }
        for &c in ids.iter().skip(1) {
            for value in &mut self.cov[c * d * d..(c + 1) * d * d] {
                *value /= (counts[c] - 1) as f64;
            }
        }
    }

    fn gaussian(&self, c: usize) -> Option<Gaussian> {
        let d = self.data.dims;
        Gaussian::new(
            c,
            &self.mean[c * d..(c + 1) * d],
            &self.cov[c * d * d..(c + 1) * d * d],
            self.weight[c],
        )
    }

    fn noise_score(&self) -> f64 {
        f64::from((-self.weight[0].ln()) as f32)
    }

    fn e_step(&mut self, full: bool) {
        let n = self.data.points;
        let ids = self.ids();
        self.ensure_score_columns(ids.last().unwrap() + 1);
        let noise = self.noise_score();
        self.scores[..n].fill(noise);
        let mut root = vec![0.; self.data.dims];
        for c in ids.into_iter().skip(1) {
            let Some(gaussian) = self.gaussian(c) else {
                // Reindex immediately; never assign a point to a singular class
                // using stale likelihoods, as the old implementation could do.
                self.alive[c] = false;
                continue;
            };
            if full {
                for (p, score) in self.scores[c * n..(c + 1) * n].iter_mut().enumerate() {
                    *score = gaussian.score(self.data.row(p), &mut root);
                }
                continue;
            }
            for p in 0..n {
                if self.class[p] == self.old_class[p]
                    && self.scores[c * n + p] - self.scores[self.class[p] * n + p] > 1000_f64.ln()
                {
                    continue;
                }
                self.scores[c * n + p] = gaussian.score(self.data.row(p), &mut root);
            }
        }
    }

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
        for p in 0..self.data.points {
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
        let gain = penalty(self.data.points, self.data.dims, 2);
        if losses[candidate] >= gain {
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

    fn score(&self) -> f64 {
        let mut score = penalty(self.data.points, self.data.dims, self.ids().len());
        for (p, &c) in self.class.iter().enumerate() {
            if c > 0 && !self.alive[c] {
                return f64::INFINITY;
            }
            score += self.scores[c * self.data.points + p];
        }
        score
    }

    fn cem(
        &mut self,
        options: &Options,
        random: &mut Random,
        recurse: bool,
        noise: bool,
    ) -> Result<f64> {
        self.cem_observed(options, random, recurse, noise, None)
    }

    fn cem_observed(
        &mut self,
        options: &Options,
        random: &mut Random,
        recurse: bool,
        noise: bool,
        mut report: Option<&mut dyn FnMut(usize, usize, bool) -> Result<()>>,
    ) -> Result<f64> {
        let mut score = f64::INFINITY;
        let mut full = true;
        let mut previous = vec![0; self.class.len()];
        for iteration in 1..=options.max_iterations {
            previous.copy_from_slice(&self.class);
            self.m_step();
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
            if let Some(report) = report.as_mut() {
                report(
                    iteration,
                    self.alive.iter().skip(1).filter(|&&v| v).count(),
                    false,
                )?;
            }
            let split = if recurse && split_due {
                if let Some(report) = report.as_mut() {
                    report(
                        iteration,
                        self.alive.iter().skip(1).filter(|&&v| v).count(),
                        true,
                    )?;
                }
                let split = self.try_splits(options, random)?;
                if let Some(report) = report.as_mut() {
                    report(
                        iteration,
                        self.alive.iter().skip(1).filter(|&&v| v).count(),
                        false,
                    )?;
                }
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
            let mut local = State::new(Arc::new(self.data.subset(&indices)));
            local.initialize(2, random);
            let unsplit = local.cem(options, random, false, false)?;
            local.initialize(3, random);
            let split = local.cem(options, random, false, false)?;
            if split >= unsplit || !split.is_finite() || local.ids().len() < 3 {
                continue;
            }
            let mut proposal = State::new(Arc::clone(&self.data));
            proposal.class.clone_from(&self.class);
            for (p, &source) in indices.iter().enumerate() {
                proposal.class[source] = if local.class[p] == 1 { c } else { unused };
            }
            for &class in &proposal.class {
                proposal.alive[class] = true;
            }
            proposal.m_step();
            proposal.e_step(true);
            // As in classic KlustaKwik, score the proposed assignments without
            // a C-step, against the score at the beginning of this split pass.
            if proposal.score() < baseline {
                self.ensure_score_columns(proposal.scores.len() / self.data.points);
                self.class = proposal.class;
                self.alive = proposal.alive;
                did_split = true;
            }
        }
        Ok(did_split)
    }

    fn model(&self) -> Model {
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
}

pub struct Output {
    pub labels: PathBuf,
    pub counts: [usize; MAX_CLUSTERS],
    pub score: f64,
    pub fitting_spikes: usize,
    pub stride: usize,
}

fn sampling_plan(points: usize, target: usize) -> (usize, usize) {
    let stride = (points / target).max(1);
    (stride, points / stride)
}

/// Try each initial count once, comparing scores on the full recording. Each
/// trial draws a fresh stratified subset, matching runkk.bat and -Subset.
pub fn run(
    store: &FeatureStore,
    options: &Options,
    work: &Path,
    log: &mut dyn Write,
) -> Result<Output> {
    run_observed(store, options, work, log, &mut |_| Ok(()))
}

pub(crate) fn run_observed(
    store: &FeatureStore,
    options: &Options,
    work: &Path,
    log: &mut dyn Write,
    notify: &mut dyn FnMut(Update) -> Result<()>,
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
    let mut root = vec![0.; store.dims()];
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
        let data = Arc::new(store.load_indices(&indices)?);
        let loaded = Instant::now();
        let mut state = State::new(data);
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
            state.cem_observed(options, &mut random, true, true, Some(&mut report))?
        };
        let fitted = Instant::now();
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
                let (class, value) = model.best(row, &mut root);
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
        notify(Update::Assigned {
            count: store.points,
        })?;
        drop(labels);
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
            // Both files are private scratch files. Copy replaces an earlier
            // best trial on Windows too, without touching the final .clu file.
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

    #[test]
    fn score_columns_grow_without_discarding_quick_step_history() {
        let mut state = State::new(Arc::new(Data {
            dims: 1,
            points: 2,
            values: vec![0., 1.],
        }));
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
        let mut state = State::new(Arc::new(Data {
            dims: 1,
            points: 3,
            values: vec![0.; 3],
        }));
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
    fn windows_random_sequence() {
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
        let gaussian = Gaussian::new(1, &[1., 2.], &[4., 2., 0., 3.], 0.25).unwrap();
        let mut root = [0.; 2];
        let score = gaussian.score(&[3., 5.], &mut root);
        // det=8; delta' inv(cov) delta = 3, including the cross term.
        assert!(
            (score
                - (3. / 2. + 8_f64.ln() / 2. - 0.25_f64.ln() + (2. * std::f64::consts::PI).ln()))
            .abs()
                < 2e-7
        );
        assert!(cholesky(&[1., 1., 0., 1.], 2).is_none());
    }

    #[test]
    fn specialized_gaussians_match_division_reference_and_correlated_systems() {
        let mut random = Random(425);
        for dims in [3, 12] {
            let mean = vec![0.5; dims];
            let mut lower = vec![0.; dims * dims];
            for i in 0..dims {
                for j in 0..=i {
                    lower[i * dims + j] = if i == j {
                        1. + random.inclusive(0, 1000) as f64 / 1000.
                    } else {
                        random.inclusive(0, 1000) as f64 / 10000. - 0.05
                    };
                }
            }
            let mut cov = vec![0.; dims * dims];
            for i in 0..dims {
                for j in i..dims {
                    cov[i * dims + j] = (0..dims)
                        .map(|k| lower[i * dims + k] * lower[j * dims + k])
                        .sum();
                }
            }
            let gaussian = Gaussian::new(1, &mean, &cov, 0.9).unwrap();
            let offset: f64 = (0..dims)
                .map(|i| f64::from(lower[i * dims + i].ln() as f32))
                .sum::<f64>()
                - 0.9_f64.ln()
                + f64::from((2. * std::f64::consts::PI).ln() as f32) * dims as f64 / 2.;
            for _ in 0..64 {
                let z: Vec<_> = (0..dims)
                    .map(|_| random.inclusive(0, 2000) as f64 / 1000. - 1.)
                    .collect();
                let point: Vec<_> = (0..dims)
                    .map(|i| mean[i] + (0..dims).map(|j| lower[i * dims + j] * z[j]).sum::<f64>())
                    .collect();
                let actual = gaussian.score(&point, &mut vec![0.; dims]);
                let fallback = gaussian.score_dynamic(&point, &mut vec![0.; dims]);
                assert!((actual - fallback).abs() <= 16. * f64::EPSILON * (1. + fallback.abs()));
                let expected = z.iter().map(|x| x * x).sum::<f64>() / 2. + offset;
                assert!((actual - expected).abs() < 1e-6);
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
            let gaussian = Gaussian::new(1, &[0.; 3], &covariance, 0.7).unwrap();
            assert!(gaussian
                .inverse_diagonal
                .iter()
                .all(|x| x.is_finite() && *x > 0.));
            let scale = variance.sqrt();
            let point = [scale, 2. * scale, 3. * scale];
            let actual = gaussian.score(&point, &mut [0.; 3]);
            let reference = gaussian.score_dynamic(&point, &mut [0.; 3]);
            assert!((actual - reference).abs() <= 16. * f64::EPSILON * (1. + reference.abs()));
        }
    }

    #[test]
    fn uniform_noise_wins_for_outliers_but_not_for_a_gaussian_peak() {
        let gaussian = Gaussian::new(1, &[0.2, 0.2], &[0.0001, 0., 0., 0.0001], 0.9).unwrap();
        let model = Model {
            noise_score: -0.1_f64.ln(),
            gaussians: vec![gaussian],
        };
        let mut root = [0.; 2];
        assert_eq!(model.best(&[0.2, 0.2], &mut root).0, 1);
        assert_eq!(model.best(&[0.9, 0.9], &mut root).0, 0);
    }

    #[test]
    fn m_step_unbiased_covariance_and_noise_pseudocount() {
        let data = Arc::new(Data {
            dims: 1,
            points: 4,
            values: vec![0., 1., 2., 3.],
        });
        let mut state = State::new(data);
        state.class.fill(1);
        state.alive[1] = true;
        state.m_step();
        assert_eq!(state.weight[0], 0.2);
        assert_eq!(state.weight[1], 0.8);
        assert_eq!(state.mean[1], 1.5);
        assert!((state.cov[1] - 5. / 3.).abs() < 1e-14);
        assert!((penalty(4, 1, 2) - 1.5 * 4_f64.ln()).abs() < 1e-14);
    }

    #[test]
    fn common_dimension_covariances_match_global_moments() {
        for dims in [3, 12] {
            let points = 64;
            let values = (0..points * dims)
                .map(|i| ((i * 37 + i / dims * 19) % 101) as f64 / 100.)
                .collect();
            let data = Arc::new(Data {
                points,
                dims,
                values,
            });
            let mut state = State::new(Arc::clone(&data));
            state.class.fill(1);
            state.alive[1] = true;
            state.m_step();
            for i in 0..dims {
                let sum_i: f64 = (0..points).map(|p| data.row(p)[i]).sum();
                assert!((state.mean[dims + i] - sum_i / points as f64).abs() < 1e-14);
                for j in i..dims {
                    let sum_j: f64 = (0..points).map(|p| data.row(p)[j]).sum();
                    let sum_ij: f64 = (0..points).map(|p| data.row(p)[i] * data.row(p)[j]).sum();
                    let expected = (sum_ij - sum_i * sum_j / points as f64) / (points - 1) as f64;
                    assert!((state.cov[dims * dims + i * dims + j] - expected).abs() < 1e-12);
                }
            }
        }
    }

    #[test]
    fn deletion_reassigns_to_runner_up_when_bic_gain_exceeds_loss() {
        let mut state = State::new(Arc::new(Data {
            dims: 1,
            points: 4,
            values: vec![0.; 4],
        }));
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
        let values: Vec<_> = (0..400)
            .map(|i| (i % 2) as f64 * 0.8 + (i * 37 % 101) as f64 / 1000.)
            .collect();
        let data = Arc::new(Data {
            dims: 1,
            points: values.len(),
            values,
        });
        let options = Options {
            min_clusters: 2,
            max_clusters: 2,
            ..Options::default()
        };
        // CEM uses random split proposals, so a single proposal can collapse.
        let mut recovered = false;
        for seed in 1..=32 {
            let mut state = State::new(Arc::clone(&data));
            let mut random = Random(seed);
            state.initialize(2, &mut random);
            let score = state.cem(&options, &mut random, true, true).unwrap();
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
