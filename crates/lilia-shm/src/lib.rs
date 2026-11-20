#![allow(unsafe_code)]

//! # LiliaDB Shared Memory Sub-Engine (`lilia-shm`)
//!
//! Sub-50-nanosecond multi-process zero-copy shared memory engine for macOS, Linux,
//! Android, iOS (with App Extensions / Widgets / Live Activities), and Windows.
//!
//! ## Key Capabilities
//! - **Sub-50 Nanosecond Latency**: Direct memory-mapped access bypassing kernel IPC,
//!   MessagePack serialization, and socket buffers.
//! - **Lock-Free Concurrency**: Atomic Seqlocks with Acquire/Release memory ordering and
//!   inter-process epoch coordination (`crossbeam-epoch` concepts).
//! - **Zero-Copy Views**: Data is read directly as borrowed slices (`&'shm [u8]`) and native
//!   binary JSON views (`BinaryValueRef`) without allocating on the heap.
//! - **Cross-Platform**: Supports POSIX shared memory (`/dev/shm`), file-backed shared mappings
//!   for iOS App Groups (`containerURL(forSecurityApplicationGroupIdentifier:)`) and Android sandboxes.

pub mod arena;
pub mod engine;
pub mod error;
pub mod header;
pub mod page_pool;
pub mod participant;
pub mod reader;
pub mod segment;
pub mod slot;
pub mod slot_map;

pub use arena::ShmArena;
pub use engine::{ShmEngine, ShmOptions, ShmStats, ShmZeroCopyJson};
pub use error::{Result, ShmError};
pub use header::{
    ShmHeader, DEFAULT_NUM_PAGES, DEFAULT_PAGE_SIZE, DEFAULT_SHM_SIZE, DEFAULT_SLOT_CAPACITY,
    SHM_MAGIC, SHM_VERSION,
};
pub use page_pool::{ShmPageDesc, ShmPagePool, ShmPageRef};
pub use participant::{ParticipantHandle, ParticipantSlot, ParticipantTable};
pub use reader::ShmReader;
pub use segment::{ShmBackend, ShmSegment};
pub use slot::{shm_hash, space_hash_16, ShmDataKind, ShmSlot, ShmZeroCopyEntry};
pub use slot_map::LockFreeSlotMap;
