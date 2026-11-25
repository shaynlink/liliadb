//! How a transaction's buffered writes change what it reads.

use bytes::Bytes;

use crate::batch::Write;
use crate::error::Result;
use crate::keys::{self, validate_name};
use crate::record::{decode_kv, encode_doc, encode_kv, now_ms};
use crate::txn::View;

/// Records written by one write, and an update of the overlay.
pub type Changes = Vec<(Bytes, Option<Bytes>)>;

/// The records `write` changes in `view`, and whether it deletes a live record.
///
/// # Errors
///
/// Invalid names; engine errors of the reads a write needs.
pub fn changes(write: &Write, view: &View<'_>) -> Result<(Changes, bool)> {
    match write {
        Write::KvPut {
            namespace,
            key,
            value,
            expires_at_ms,
            ..
        } => {
            validate_name("namespace", namespace)?;
            Ok((
                vec![(
                    keys::kv(namespace, key),
                    Some(encode_kv(value, *expires_at_ms)),
                )],
                false,
            ))
        }
        Write::KvDelete { namespace, key, .. } => {
            validate_name("namespace", namespace)?;
            let record_key = keys::kv(namespace, key);
            let live = match view.peek(&record_key)? {
                Some(item) => !decode_kv(&item.value)?.expired(now_ms()),
                None => false,
            };
            Ok((vec![(record_key, None)], live))
        }
        Write::JsonPut { space, id, doc, .. } => {
            validate_name("space", space)?;
            Ok((vec![(keys::doc(space, id), Some(encode_doc(doc)))], false))
        }
        Write::JsonDelete { space, id, .. } => {
            validate_name("space", space)?;
            let record_key = keys::doc(space, id);
            let live = view.peek(&record_key)?.is_some();
            Ok((vec![(record_key, None)], live))
        }
        Write::Graph(write) => crate::graph::overlay(write, view),
        Write::Fts(write) => crate::fts::overlay(write, view),
        Write::Vector(write) => crate::vectors::overlay(write, view),
        Write::TimeSeries(write) => crate::timeseries::overlay(write, view),
        Write::Geo(write) => crate::geo::overlay(write, view),
        Write::Crdt(write) => crate::crdt::overlay(write, view),
        Write::Bitemporal(write) => crate::bitemporal::overlay(write, view),
    }
}
