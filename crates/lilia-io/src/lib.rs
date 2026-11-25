//! Platform layer of `LiliaDB`.
//!
//! Every byte the storage engine reads or writes, and every durability decision it makes, goes
//! through the [`Vfs`] trait of this crate:
//!
//! - [`NativeVfs`]: the operating system's file system, with the strongest sync primitive of each
//!   platform ([`SyncLevel`]);
//! - [`MemVfs`]: an in-memory file system that models what survives a power cut (synced data,
//!   synced directory entries, an optional torn tail of unsynced writes) and can inject I/O
//!   errors, so crash recovery can be tested deterministically;
//! - [`PoolVfs`]: a file system over a fixed pool of storage [`Slot`]s, for platforms whose files
//!   can only be opened asynchronously (the browser's origin private file system).
//!
//! [`HardwareProfile`] describes the machine the engine runs on, and [`qos`] lowers the priority
//! of the engine's own background threads. This crate is the only one of the engine stack that
//! calls the operating system directly; each `unsafe` block states why it is sound.

#![allow(
    clippy::missing_errors_doc,
    reason = "every fallible function of this crate is an I/O operation that returns the operating system's (or the simulated file system's) `io::Error` unchanged; there is nothing to document per function beyond that"
)]

mod clock;
mod hardware;
#[cfg(feature = "ipc")]
pub mod ipc;
mod mem;
#[cfg(any(unix, windows))]
mod native;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod opfs;
pub mod pool;
pub mod qos;
mod vfs;

pub use clock::now_ms;
pub use hardware::HardwareProfile;
pub use mem::{CrashOptions, MemVfs};
#[cfg(any(unix, windows))]
pub use native::NativeVfs;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use opfs::OpfsPool;
pub use pool::{MemSlot, PoolVfs, Slot};
pub use vfs::{File, LockGuard, OpenMode, SyncLevel, Vfs};
