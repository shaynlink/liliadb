//! Planning the writes of key-value records and JSON documents, and the secondary-index entries
//! a document carries.

use std::collections::BTreeSet;

use bytes::Bytes;

use super::{Model, Planner};
use crate::error::{invalid, Result};
use crate::guard::Guard;
use crate::keys::{self, validate_name};
use crate::record::{decode_doc, decode_kv, encode_doc, encode_kv};

impl Planner<'_> {
    pub(super) fn kv_put(
        &mut self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        expires: Option<i64>,
        if_version: Option<u64>,
    ) -> Result<()> {
        validate_name("namespace", namespace)?;
        let record_key = keys::kv(namespace, key);
        let guard = Guard::new(self.access, self.catalog.schema(namespace));
        if if_version.is_some() || guard.checks_writes() {
            let old = self
                .current(&record_key)?
                .map(|raw| decode_kv(&raw))
                .transpose()?
                .filter(|r| !r.expired(self.now_ms));
            self.check_version(&record_key, if_version, old.is_some())?;
            guard.check_kv(
                namespace,
                key,
                old.as_ref().map(|r| r.value.as_ref()),
                Some(value),
            )?;
        }
        let record = encode_kv(value, expires);
        let op = self.write_record(
            &record_key,
            Some(record),
            (Model::Kv, namespace, hex::encode(key)),
        );
        self.plan.results.push((Some(op), false));
        Ok(())
    }

    pub(super) fn kv_delete(
        &mut self,
        namespace: &str,
        key: &[u8],
        if_version: Option<u64>,
    ) -> Result<()> {
        validate_name("namespace", namespace)?;
        let record_key = keys::kv(namespace, key);
        let guard = Guard::new(self.access, self.catalog.schema(namespace));
        let Some(raw) = self.current(&record_key)? else {
            self.check_version(&record_key, if_version, false)?;
            self.plan.results.push((None, false));
            return Ok(());
        };
        let live = Some(decode_kv(&raw)?).filter(|r| !r.expired(self.now_ms));
        self.check_version(&record_key, if_version, live.is_some())?;
        if let Some(old) = &live {
            guard.check_kv(namespace, key, Some(&old.value), None)?;
        }
        let op = self.write_record(&record_key, None, (Model::Kv, namespace, hex::encode(key)));
        self.plan.results.push((Some(op), live.is_some()));
        Ok(())
    }

    /// Replaces the index entries of document `id` in `space` from `old` to `new`.
    fn reindex(
        &mut self,
        space: &str,
        id: &str,
        old: Option<&lilia_core::BinaryDoc>,
        new: Option<&lilia_core::BinaryDoc>,
    ) {
        let Some(schema) = self.catalog.schema(space) else {
            return;
        };
        for index in &schema.indexes {
            let before = old.map_or_default(|d| index.values(d));
            let after: BTreeSet<String> = new.map_or_default(|d| index.values(d));
            for value in before.difference(&after) {
                self.op(keys::index_entry(space, index.id, value, id), None);
            }
            // An index being built may not hold the old document's entries yet (the build
            // skips documents changed since its snapshot), so every new entry is written.
            let added: Vec<&String> = if index.ready {
                after.difference(&before).collect()
            } else {
                after.iter().collect()
            };
            for value in added {
                self.op(
                    keys::index_entry(space, index.id, value, id),
                    Some(Bytes::new()),
                );
            }
        }
    }

    pub(super) fn json_put(
        &mut self,
        space: &str,
        id: &str,
        doc: &lilia_core::BinaryDoc,
        if_version: Option<u64>,
    ) -> Result<()> {
        validate_name("space", space)?;
        if id.is_empty() {
            return Err(invalid("document ids must not be empty"));
        }
        let record_key = keys::doc(space, id);
        let schema = self.catalog.schema(space);
        let indexed = schema.as_ref().is_some_and(|s| !s.indexes.is_empty());
        let guard = Guard::new(self.access, schema);
        if if_version.is_some() || guard.checks_writes() || indexed {
            let old = self
                .current(&record_key)?
                .map(|raw| decode_doc(&raw))
                .transpose()?;
            self.check_version(&record_key, if_version, old.is_some())?;
            guard.check_doc(space, old.as_ref(), Some(doc))?;
            self.reindex(space, id, old.as_ref(), Some(doc));
        }
        let op = self.write_record(
            &record_key,
            Some(encode_doc(doc)),
            (Model::Json, space, id.to_owned()),
        );
        self.plan.results.push((Some(op), false));
        Ok(())
    }

    pub(super) fn json_delete(
        &mut self,
        space: &str,
        id: &str,
        if_version: Option<u64>,
    ) -> Result<()> {
        validate_name("space", space)?;
        let record_key = keys::doc(space, id);
        let guard = Guard::new(self.access, self.catalog.schema(space));
        let old = self
            .current(&record_key)?
            .map(|raw| decode_doc(&raw))
            .transpose()?;
        self.check_version(&record_key, if_version, old.is_some())?;
        let Some(old) = old else {
            self.plan.results.push((None, false));
            return Ok(());
        };
        guard.check_doc(space, Some(&old), None)?;
        self.reindex(space, id, Some(&old), None);
        let op = self.write_record(&record_key, None, (Model::Json, space, id.to_owned()));
        self.plan.results.push((Some(op), true));
        Ok(())
    }
}
