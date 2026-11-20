use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShmError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Corrupt SHM header: {0}")]
    CorruptHeader(String),

    #[error("Version mismatch: expected {expected}, found {found}")]
    VersionMismatch { expected: u32, found: u32 },

    #[error("SHM capacity exceeded: needed {needed} bytes, available {available}")]
    CapacityExceeded { needed: usize, available: usize },

    #[error("Slot table full: capacity {0}")]
    SlotTableFull(usize),

    #[error("Page index {0} out of bounds (max {1})")]
    PageOutOfBounds(u32, u32),

    #[error("Writer process {0} is dead or unresponsive")]
    WriterDead(u32),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("Lilia error: {0}")]
    Lilia(#[from] lilia_core::LiliaError),
}

pub type Result<T> = std::result::Result<T, ShmError>;
