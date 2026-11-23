//! The protocol of the `LiliaDB` daemon and of the bindings that share its operations: a
//! [`Request`] carries one [`Operation`], a [`Response`] its [`ResponseValue`] or the error.
//! A connection opens with the preface of its [`Codec`]; its frames are size-prefixed `rkyv`
//! archives ([`read_frame`], [`write_frame`]), or for JavaScript clients `MessagePack` requests
//! and responses in JS frames ([`to_js_frame`]). [`execute`]
//! runs an operation on a database, so every binding gives an operation the same meaning;
//! [`open`] and [`close`] open a database from the options the bindings take and close it.

mod codec;
mod execute;
mod frame;
mod lifecycle;
pub mod names;
mod operation;
mod response;

use lilia_core::LiliaError;
use serde::{Deserialize, Serialize};
pub use serde_bytes::ByteBuf;

pub use codec::{execute_js, from_msgpack, to_js_frame, to_msgpack, Codec, RequestEncoding};
pub use execute::{execute, GEO_COLLECTION};
pub use frame::{read_archived_frame, read_frame, write_archived_frame, write_frame};
#[cfg(any(unix, windows))]
pub use lifecycle::open;
pub use lifecycle::{close, OpenOptions, SharedDatabase};
pub use operation::Operation;
pub use response::{ResponseValue, StoredVector};

pub const PROTOCOL_MAJOR: u16 = 2;
pub const PROTOCOL_MINOR: u16 = 0;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv)]
pub struct Request {
    pub id: String,
    pub token: String,
    /// Optional relative admission budget, starting after the full frame is decoded.
    /// Includes waiting for a daemon blocking worker, but does not cancel running storage work.
    pub deadline_ms: Option<u64>,
    pub operation: Operation,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv)]
pub struct Response {
    pub id: String,
    pub result: std::result::Result<ResponseValue, LiliaError>,
}
