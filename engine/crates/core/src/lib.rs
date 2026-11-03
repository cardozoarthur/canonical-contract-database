//! Shared public errors and resource limits. No storage or Foundry dependency.
use std::{fmt, io};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    TooLarge,
    UnsupportedVersion(u16),
    Corruption { offset: u64 },
    IncompleteTail { offset: u64 },
    WriterBusy,
    InvalidInput(&'static str),
    Poisoned,
    Conflict,
    UnknownSchema,
    NotFound,
    Unauthorized,
    InvalidCursor,
    HistoryPruned,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::TooLarge => write!(f, "frame exceeds size limit"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported format version {v}"),
            Self::Corruption { offset } => write!(f, "corruption at byte {offset}"),
            Self::IncompleteTail { offset } => write!(f, "incomplete tail at byte {offset}"),
            Self::WriterBusy => write!(f, "writer already open"),
            Self::InvalidInput(s) => write!(f, "invalid input: {s}"),
            Self::Poisoned => write!(f, "writer requires reopen after I/O failure"),
            Self::Conflict => write!(f, "immutable identity or revision conflict"),
            Self::UnknownSchema => write!(f, "unknown schema reference"),
            Self::NotFound => write!(f, "not found"),
            Self::Unauthorized => write!(f, "unauthorized namespace or subject"),
            Self::HistoryPruned => write!(f, "revision payload was explicitly pruned"),
            Self::InvalidCursor => write!(f, "invalid or expired query cursor"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
