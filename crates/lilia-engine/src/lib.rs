//! `LiliaDB` native storage engine: an ordered key-value store built for durable write
//! throughput.
//!
//! - **Writes** are grouped: concurrent commits share one journal write and one sync
//!   ([`Durability`] decides which sync), then land in a lock-free memtable. A batch is atomic
//!   and may carry version expectations ([`WriteBatch::expect`]): the version of a key is the
//!   sequence number of the write that produced it.
//! - **Reads** never block writes: they see an immutable snapshot of the memtables and sorted
//!   runs, and [`Snapshot`]s pin a sequence number for repeatable reads and ranges.
//! - **Sorted runs** are flushed and compacted in the background, at background priority, into
//!   leveled files of checksummed 16 KiB blocks with Bloom filters and a shared block cache.
//! - **Recovery** replays the journals written since the last flush, up to the first damaged
//!   record, so a torn write never surfaces partially.
//!
//! All I/O goes through [`lilia_io::Vfs`], so the engine runs on the native file system or on a
//! simulated one that models power loss.

mod batch;
mod coding;
mod commit;
mod compaction;
mod db;
mod error;
mod files;
mod iter;
mod journal;
mod key;
mod memtable;
mod options;
mod table;
mod version;

pub use batch::{Expectation, Op, WriteBatch};
pub use db::{Db, Entry, RangeIter, Snapshot, Stats, Versioned};
pub use error::{Error, Result};
pub use key::MAX_SEQUENCE;
pub use options::{Durability, Options, LEVELS};
