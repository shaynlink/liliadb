//! Report sections, each a `fn(&Data) -> String` (empty when there is nothing to show).

mod kv;
mod meta;
mod models;

pub use kv::{reads, writes, ycsb};
pub use meta::{engines, environment, failures, method, summary};
pub use models::{analytics, fts, json, sdk, vectors};
