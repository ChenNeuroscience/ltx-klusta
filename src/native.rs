//! Optional LP64 BLAS/LAPACK loading. No numerical library is linked at startup.
use crate::{Error, Result, SAMPLES};
use libloading::Library;
use nalgebra::DMatrix;
use std::ffi::{c_char, CStr};
use std::path::{Path, PathBuf};

type Syrk = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const i32,
    *const i32,
    *const f64,
    *const f64,
    *const i32,
    *const f64,
    *mut f64,
    *const i32,
);
type Syevd = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const i32,
    *mut f64,
    *const i32,
    *mut f64,
    *mut f64,
    *const i32,
    *mut i32,
    *const i32,
    *mut i32,
);
type Syev = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const i32,
    *mut f64,
    *const i32,
    *mut f64,
    *mut f64,
    *const i32,
    *mut i32,
);

pub(crate) struct NativeLapack {
    // Keep every library alive for the entire lifetime of its function pointers.
    _libraries: Vec<Library>,
    pub(crate) path: PathBuf,
    syrk: Syrk,
    syevd: Syevd,
    syev: Syev,
}

impl NativeLapack {
    pub(crate) fn discover(explicit: Option<&Path>) -> Result<Self> {
        if let Some(path) = explicit {
            return Self::load(path);
        }
        for path in candidates() {
            if let Ok(native) = Self::load(&path) {
                return Ok(native);
            }
        }
        Err(Error::Backend("No compatible LP64 BLAS/LAPACK library found. Use --lapack-library PATH or --backend nalgebra.".into()))
    }

    fn load(path: &Path) -> Result<Self> {
        let library = open(path)?;
        check_integer_abi(&library)?;
        // SAFETY: the symbols are the standard Fortran LP64 routines with the
        // signatures above. Their handles are retained in this struct.
        let (syevd, syev, mut syrk) = unsafe {
            (
                symbol::<Syevd>(&library, &[b"dsyevd_\0", b"dsyevd\0", b"DSYEVD\0"]),
                symbol::<Syev>(&library, &[b"dsyev_\0", b"dsyev\0", b"DSYEV\0"]),
                symbol::<Syrk>(&library, &[b"dsyrk_\0", b"dsyrk\0", b"DSYRK\0"]),
            )
        };
        let (syevd, syev) = match (syevd, syev) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                return Err(Error::Backend(format!(
                    "{} does not export LP64 DSYEVD and DSYEV",
                    path.display()
                )))
            }
        };
        let mut libraries = vec![library];
        // Windows GetProcAddress does not search a DLL's dependencies. Support
        // a separate adjacent BLAS DLL as well as all-in-one OpenBLAS providers.
        if syrk.is_none() {
            if let Some(parent) = path.parent() {
                for name in ["blas_win64_MT.dll", "libblas.dll", "blas.dll"] {
                    if let Ok(blas) = open(&parent.join(name)) {
                        check_integer_abi(&blas)?;
                        // SAFETY: same LP64 signature and retained-handle rule.
                        syrk = unsafe {
                            symbol::<Syrk>(&blas, &[b"dsyrk_\0", b"dsyrk\0", b"DSYRK\0"])
                        };
                        if syrk.is_some() {
                            libraries.push(blas);
                            break;
                        }
                    }
                }
            }
        }
        let syrk = syrk.ok_or_else(|| {
            Error::Backend(format!(
                "{} has no accessible LP64 DSYRK routine",
                path.display()
            ))
        })?;
        Ok(Self {
            _libraries: libraries,
            path: path.to_owned(),
            syrk,
            syevd,
            syev,
        })
    }

    pub(crate) fn accumulate(&self, data: &[f64], count: usize, moments: &mut DMatrix<f64>) {
        let (n, k) = (SAMPLES as i32, count as i32);
        // SAFETY: data is 50xcount, moments is 50x50, both column-major;
        // count is bounded by the validated processing block size.
        unsafe {
            (self.syrk)(
                b"L".as_ptr().cast(),
                b"N".as_ptr().cast(),
                &n,
                &k,
                &1.0,
                data.as_ptr(),
                &n,
                &1.0,
                moments.as_mut_ptr(),
                &n,
            );
        }
    }

    pub(crate) fn eigen(&self, mut matrix: DMatrix<f64>) -> Result<(Vec<f64>, DMatrix<f64>)> {
        let original = matrix.clone();
        let n = SAMPLES as i32;
        let mut values = vec![0.0; SAMPLES];
        let mut work = vec![0.0; 2 * (1 + 6 * SAMPLES + 2 * SAMPLES * SAMPLES)];
        let mut iwork = vec![0; 3 * (3 + 5 * SAMPLES)];
        let (lwork, liwork) = (work.len() as i32, iwork.len() as i32);
        let mut info = 0;
        // SAFETY: A is 50x50; W and both workspaces meet DSYEVD's LP64 sizes.
        unsafe {
            (self.syevd)(
                b"V".as_ptr().cast(),
                b"U".as_ptr().cast(),
                &n,
                matrix.as_mut_ptr(),
                &n,
                values.as_mut_ptr(),
                work.as_mut_ptr(),
                &lwork,
                iwork.as_mut_ptr(),
                &liwork,
                &mut info,
            );
        }
        // A failed divide-and-conquer solve has already overwritten A. Retry
        // QR on the original covariance, not the partially modified matrix.
        if info > 0 {
            return self.qr(original);
        }
        if info < 0 {
            return Err(Error::Lapack {
                routine: "DSYEVD",
                info,
            });
        }
        Ok((values, matrix))
    }

    pub(crate) fn qr(&self, mut matrix: DMatrix<f64>) -> Result<(Vec<f64>, DMatrix<f64>)> {
        let n = SAMPLES as i32;
        let mut values = vec![0.0; SAMPLES];
        let mut work = vec![0.0; 3 * (3 * SAMPLES - 1)];
        let lwork = work.len() as i32;
        let mut info = 0;
        // SAFETY: A is 50x50, W has 50 entries, WORK exceeds 3*N-1.
        unsafe {
            (self.syev)(
                b"V".as_ptr().cast(),
                b"U".as_ptr().cast(),
                &n,
                matrix.as_mut_ptr(),
                &n,
                values.as_mut_ptr(),
                work.as_mut_ptr(),
                &lwork,
                &mut info,
            );
        }
        if info != 0 {
            return Err(Error::Lapack {
                routine: "DSYEV",
                info,
            });
        }
        Ok((values, matrix))
    }
}

fn open(path: &Path) -> Result<Library> {
    // SAFETY: providers are installed numerical libraries or explicitly chosen
    // by the user. Windows dependencies are searched beside the chosen library
    // and in default OS directories, without adding a process-wide search path.
    let loaded = unsafe {
        #[cfg(windows)]
        {
            let absolute = std::fs::canonicalize(path)?;
            libloading::os::windows::Library::load_with_flags(absolute, 0x100 | 0x1000)
                .map(Library::from)
        }
        #[cfg(not(windows))]
        {
            Library::new(path)
        }
    };
    loaded.map_err(|error| Error::Backend(format!("Cannot load {}: {error}", path.display())))
}

unsafe fn symbol<T: Copy>(library: &Library, names: &[&[u8]]) -> Option<T> {
    for name in names {
        // SAFETY: caller supplies the matching function-pointer type. The
        // owning NativeLapack retains the library after copying the pointer.
        if let Ok(value) = unsafe { library.get::<T>(*name) } {
            return Some(*value);
        }
    }
    None
}

fn check_integer_abi(library: &Library) -> Result<()> {
    type Config = unsafe extern "C" fn() -> *const c_char;
    // SAFETY: OpenBLAS documents this no-argument configuration-string API.
    if let Some(config) = unsafe {
        symbol::<Config>(
            library,
            &[b"openblas_get_config\0", b"openblas_get_config_\0"],
        )
    } {
        let pointer = unsafe { config() };
        if !pointer.is_null() {
            let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
            if text.contains("USE64BITINT") {
                return Err(Error::Backend(
                    "ILP64 OpenBLAS is incompatible; select an LP64 (32-bit integer) provider"
                        .into(),
                ));
            }
        }
    }
    Ok(())
}

fn candidates() -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Some(path) = std::env::var_os("PREKK_LAPACK_LIBRARY") {
        result.push(path.into());
    }
    #[cfg(windows)]
    {
        let mut directories = Vec::new();
        if let Some(path) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_owned))
        {
            directories.push(path);
        }
        if let Some(path) = std::env::var_os("PATH") {
            directories.extend(std::env::split_paths(&path));
        }
        for directory in directories {
            for name in [
                "libopenblas.dll",
                "openblas.dll",
                "lapack_win64_MT.dll",
                "lapack.dll",
            ] {
                let path = directory.join(name);
                if path.is_file() {
                    result.push(path);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    result.extend(
        [
            "/System/Library/Frameworks/Accelerate.framework/Accelerate",
            "libopenblas.dylib",
            "/opt/homebrew/opt/openblas/lib/libopenblas.dylib",
            "/usr/local/opt/openblas/lib/libopenblas.dylib",
        ]
        .map(PathBuf::from),
    );
    #[cfg(all(unix, not(target_os = "macos")))]
    result.extend(
        [
            "libopenblas.so.0",
            "libopenblas.so",
            "liblapack.so.3",
            "liblapack.so",
        ]
        .map(PathBuf::from),
    );
    result
}
