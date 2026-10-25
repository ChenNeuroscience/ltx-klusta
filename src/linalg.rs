use crate::{native::NativeLapack, Error, Result, SAMPLES};
use nalgebra::{linalg::SymmetricEigen, DMatrix};
use std::path::Path;

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// Detect a compatible native provider, otherwise use nalgebra
    Auto,
    /// Use pure Rust numerical routines
    #[default]
    Nalgebra,
    /// Require a compatible LP64 BLAS/LAPACK provider
    Lapack,
}

pub(crate) struct Engine {
    native: Option<NativeLapack>,
}

impl Engine {
    pub(crate) fn resolve(choice: Backend, library: Option<&Path>) -> Result<Self> {
        if choice == Backend::Nalgebra {
            if library.is_some() {
                return Err(Error::Invalid(
                    "A LAPACK library cannot be used with the nalgebra backend",
                ));
            }
            return Ok(Self { native: None });
        }
        match NativeLapack::discover(library) {
            Ok(native) => Ok(Self {
                native: Some(native),
            }),
            Err(_) if choice == Backend::Auto && library.is_none() => Ok(Self { native: None }),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn description(&self) -> String {
        match &self.native {
            Some(native) => format!("lapack ({})", native.path.display()),
            None => "nalgebra".into(),
        }
    }

    pub(crate) fn accumulate(&self, data: &[f64], count: usize, moments: &mut DMatrix<f64>) {
        debug_assert_eq!(data.len(), SAMPLES * count);
        if let Some(native) = &self.native {
            native.accumulate(data, count, moments);
        } else {
            let x = nalgebra::DMatrixView::from_slice(data, SAMPLES, count);
            let product = x * x.transpose();
            for column in 0..SAMPLES {
                for row in column..SAMPLES {
                    moments[(row, column)] += product[(row, column)];
                }
            }
        }
    }

    pub(crate) fn eigen(&self, covariance: DMatrix<f64>) -> Result<(Vec<f64>, DMatrix<f64>)> {
        if let Some(native) = &self.native {
            native.eigen(covariance)
        } else {
            let eigen = SymmetricEigen::try_new(covariance, f64::EPSILON, 100_000)
                .ok_or(Error::Invalid("PCA eigensolver did not converge"))?;
            Ok((eigen.eigenvalues.as_slice().to_vec(), eigen.eigenvectors))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engines() -> Vec<Engine> {
        let mut values = vec![Engine::resolve(Backend::Nalgebra, None).unwrap()];
        let native = Engine::resolve(Backend::Lapack, None);
        if std::env::var_os("FANCYKK_REQUIRE_LAPACK").is_some() {
            assert!(
                native.is_ok(),
                "CI requires LAPACK: {}",
                native.err().map(|e| e.to_string()).unwrap_or_default()
            );
            values.push(Engine::resolve(Backend::Lapack, None).unwrap());
        } else if let Ok(native) = native {
            values.push(native);
        }
        values
    }

    #[test]
    fn accumulated_integer_products_match_direct_sums() {
        let count = 7;
        let data: Vec<_> = (0..count * SAMPLES)
            .map(|i| ((i * 31 % 199) as i32 - 99) as f64)
            .collect();
        for engine in engines() {
            let mut actual = DMatrix::zeros(SAMPLES, SAMPLES);
            engine.accumulate(&data, count, &mut actual);
            engine.accumulate(&data, count, &mut actual);
            for column in 0..SAMPLES {
                for row in column..SAMPLES {
                    let expected = 2.0
                        * (0..count)
                            .map(|i| data[i * SAMPLES + row] * data[i * SAMPLES + column])
                            .sum::<f64>();
                    assert_eq!(actual[(row, column)], expected, "{}", engine.description());
                }
            }
        }
    }

    fn assert_eigenpairs(matrix: &DMatrix<f64>, values: Vec<f64>, vectors: DMatrix<f64>) {
        let residual = matrix * &vectors
            - &vectors * DMatrix::from_diagonal(&nalgebra::DVector::from_vec(values));
        assert!(residual.norm() / matrix.norm() < 1e-12);
        assert!(
            (vectors.transpose() * vectors - DMatrix::identity(SAMPLES, SAMPLES)).norm() < 1e-12
        );
    }

    #[test]
    fn eigenvectors_are_orthonormal_and_reconstruct_a_dense_matrix() {
        let mut state = 943_u64;
        let data: Vec<_> = (0..80 * SAMPLES)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 56) as i8 as f64
            })
            .collect();
        let mut lower = DMatrix::zeros(SAMPLES, SAMPLES);
        engines()[0].accumulate(&data, 80, &mut lower);
        let matrix = DMatrix::from_fn(SAMPLES, SAMPLES, |row, column| {
            lower[(row.max(column), row.min(column))]
        });
        for engine in engines() {
            let (values, vectors) = engine.eigen(matrix.clone()).unwrap();
            assert_eigenpairs(&matrix, values, vectors);
        }
    }

    #[test]
    fn qr_fallback_has_correct_eigenpairs() {
        let matrix = DMatrix::from_fn(SAMPLES, SAMPLES, |row, column| {
            if row == column {
                row as f64 + 1.0
            } else if row.abs_diff(column) == 1 {
                0.3
            } else {
                0.0
            }
        });
        for engine in engines() {
            if let Some(native) = engine.native {
                let (values, vectors) = native.qr(matrix.clone()).unwrap();
                assert_eigenpairs(&matrix, values, vectors);
            }
        }
    }

    #[test]
    fn explicit_missing_library_is_an_error() {
        let missing = Path::new("fancykk-provider-that-does-not-exist");
        assert!(Engine::resolve(Backend::Lapack, Some(missing)).is_err());
        assert!(Engine::resolve(Backend::Auto, Some(missing)).is_err());
        assert!(Engine::resolve(Backend::Nalgebra, Some(missing)).is_err());
    }

    #[test]
    #[ignore = "manual performance measurement; never part of routine CI"]
    fn covariance_block_benchmark() {
        use std::{hint::black_box, time::Instant};
        println!("backend,block_spikes,input_mib,million_spikes_per_second");
        for engine in engines() {
            for block in [512, 2048, 4096, 8192, 16384] {
                let data: Vec<_> = (0..SAMPLES * block)
                    .map(|i| ((i * 31 % 199) as i32 - 99) as f64)
                    .collect();
                let mut moments = DMatrix::zeros(SAMPLES, SAMPLES);
                engine.accumulate(&data, block, &mut moments);
                let iterations = 1_310_720 / block;
                let started = Instant::now();
                for _ in 0..iterations {
                    engine.accumulate(black_box(&data), block, black_box(&mut moments));
                }
                let seconds = started.elapsed().as_secs_f64();
                println!(
                    "{},{},{:.3},{:.3}",
                    engine.description().split_whitespace().next().unwrap(),
                    block,
                    data.len() as f64 * 8.0 / 1048576.0,
                    (iterations * block) as f64 / seconds / 1e6
                );
            }
        }
    }
}
