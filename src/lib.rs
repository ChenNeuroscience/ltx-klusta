//! Spike clustering for tetrode recordings: PCA feature extraction followed by a
//! Rust port of KlustaKwik's classification-EM clustering.

#[cfg(not(target_pointer_width = "64"))]
compile_error!("ltxk requires a 64-bit target");

pub mod cluster;
pub mod concurrency;
mod dashboard;
pub mod dump;
mod extract;
pub mod feature_store;
pub mod pipeline;
pub mod progress;
pub mod queue;
mod queue_progress;
mod windows_console;
pub use extract::Features;
use std::fmt;
use std::io;

pub const SAMPLES: usize = 50;
pub const CHANNELS: usize = 4;
pub const BYTES_PER_SPIKE: usize = CHANNELS * (4 + SAMPLES);
pub const DEFAULT_BLOCK_SPIKES: usize = 4096;
pub const MAX_BLOCK_SPIKES: usize = 65536;
// Sums of signed-byte products remain exactly representable in f64 up to this
// count, independent of block boundaries.
pub const MAX_SPIKES: u64 = i32::MAX as u64;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(&'static str),
    Message(String),
}
impl Error {
    /// Keep the failed operation and path alongside the underlying error.
    pub fn at_path(action: &str, path: &std::path::Path, error: impl fmt::Display) -> Self {
        Self::Message(format!("{action} \"{}\": {error}", path.display()))
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Invalid(message) => f.write_str(message),
            Self::Message(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
