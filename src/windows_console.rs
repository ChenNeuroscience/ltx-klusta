//! Publish the owner of a visible console for the native Explorer launcher.
//! The console OS lock is the lease; PID + process creation time reject stale files.

pub(crate) struct Session {
    #[cfg(windows)]
    path: std::path::PathBuf,
}

impl Session {
    pub(crate) fn register(root: &std::path::Path) -> crate::Result<Self> {
        #[cfg(windows)]
        {
            use std::ffi::c_void;
            #[repr(C)]
            #[derive(Default)]
            struct FileTime {
                low: u32,
                high: u32,
            }
            #[link(name = "Kernel32")]
            extern "system" {
                fn GetCurrentProcess() -> *mut c_void;
                fn GetProcessTimes(
                    process: *mut c_void,
                    created: *mut FileTime,
                    exited: *mut FileTime,
                    kernel: *mut FileTime,
                    user: *mut FileTime,
                ) -> i32;
            }
            let mut times: [FileTime; 4] = std::array::from_fn(|_| FileTime::default());
            // GetCurrentProcess returns a pseudo-handle that must not be closed.
            let success = unsafe {
                GetProcessTimes(
                    GetCurrentProcess(),
                    &mut times[0],
                    &mut times[1],
                    &mut times[2],
                    &mut times[3],
                )
            };
            if success == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let created = u64::from(times[0].low) | (u64::from(times[0].high) << 32);
            let path = root.join("console.owner");
            let mut temporary = tempfile::NamedTempFile::new_in(root)?;
            use std::io::Write;
            writeln!(temporary, "{} {created}", std::process::id())?;
            temporary.as_file().sync_all()?;
            temporary.persist(&path).map_err(|error| error.error)?;
            Ok(Self { path })
        }
        #[cfg(not(windows))]
        {
            let _ = root;
            Ok(Self {})
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = std::fs::remove_file(&self.path);
    }
}
