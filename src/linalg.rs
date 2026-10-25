use crate::{Error, Result, SAMPLES};
use nalgebra::{linalg::SymmetricEigen, DMatrix};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    #[cfg_attr(not(feature = "lapack"), default)]
    Nalgebra,
    #[cfg(feature = "lapack")]
    #[default]
    Lapack,
}

impl Backend {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "nalgebra" => Ok(Self::Nalgebra),
            #[cfg(feature = "lapack")]
            "lapack" => Ok(Self::Lapack),
            #[cfg(not(feature = "lapack"))]
            "lapack" => Err(Error::Invalid(
                "This executable has no LAPACK support; use the LAPACK build",
            )),
            _ => Err(Error::Invalid("Backend must be nalgebra or lapack")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Nalgebra => "nalgebra",
            #[cfg(feature = "lapack")]
            Self::Lapack => "lapack",
        }
    }

    // All matrices are column-major. Only the lower triangle is accumulated.
    pub(crate) fn accumulate(self, data: &[f64], count: usize, moments: &mut DMatrix<f64>) {
        debug_assert_eq!(data.len(), SAMPLES * count);
        match self {
            Self::Nalgebra => {
                let x = nalgebra::DMatrixView::from_slice(data, SAMPLES, count);
                let product = x * x.transpose();
                for column in 0..SAMPLES {
                    for row in column..SAMPLES {
                        moments[(row, column)] += product[(row, column)];
                    }
                }
            }
            #[cfg(feature = "lapack")]
            Self::Lapack => {
                // SAFETY: both arrays are sized for the stated column-major shapes;
                // count is bounded by MAX_BLOCK_SPIKES and the ABI uses 32-bit ints.
                unsafe {
                    blas::dsyrk(
                        b'L',
                        b'N',
                        SAMPLES as i32,
                        count as i32,
                        1.0,
                        data,
                        SAMPLES as i32,
                        1.0,
                        moments.as_mut_slice(),
                        SAMPLES as i32,
                    );
                }
            }
        }
    }

    pub(crate) fn eigen(self, covariance: DMatrix<f64>) -> Result<(Vec<f64>, DMatrix<f64>)> {
        match self {
            Self::Nalgebra => {
                let eigen = SymmetricEigen::try_new(covariance, f64::EPSILON, 100_000)
                    .ok_or(Error::Invalid("PCA eigensolver did not converge"))?;
                Ok((eigen.eigenvalues.as_slice().to_vec(), eigen.eigenvectors))
            }
            #[cfg(feature = "lapack")]
            Self::Lapack => lapack_eigen(covariance),
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(feature = "lapack")]
fn lapack_eigen(mut matrix: DMatrix<f64>) -> Result<(Vec<f64>, DMatrix<f64>)> {
    let n = SAMPLES as i32;
    let mut values = vec![0.0; SAMPLES];
    // DSYEVD with JOBZ=V: LWORK >= 1+6*N+2*N*N and LIWORK >= 3+5*N.
    let mut work = vec![0.0; 1 + 6 * SAMPLES + 2 * SAMPLES * SAMPLES];
    let mut iwork = vec![0; 3 + 5 * SAMPLES];
    let (lwork, liwork) = (work.len() as i32, iwork.len() as i32);
    let mut info = 0;
    // SAFETY: the 50x50 matrix, eigenvalues, and documented workspaces above
    // have the required sizes. Bundled Windows LAPACK and Accelerate use LP64.
    unsafe {
        lapack::dsyevd(
            b'V',
            b'L',
            n,
            matrix.as_mut_slice(),
            n,
            &mut values,
            &mut work,
            lwork,
            &mut iwork,
            liwork,
            &mut info,
        );
    }
    if info != 0 {
        return Err(Error::Lapack(info));
    }
    Ok((values, matrix))
}
