#![allow(clippy::missing_errors_doc)]

use lilia_sqlite_native::StmtSlot;

use crate::storage::{normalize_limit, now_ms, parse_version, validate_name};
use crate::Database;
use lilia_core::{KvEntry, Result};

impl Database {
    pub fn kv_get(&self, namespace: &str, key: &[u8]) -> Result<Option<KvEntry>> {
        validate_name("namespace", namespace)?;
        let mut connection = self.read_lock()?;
        let stmt = connection.slot(StmtSlot::KvGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, namespace)?;
        stmt.bind_blob_static(2, key)?;
        stmt.bind_int64(3, now_ms())?;
        let entry = if stmt.step()? {
            let raw_val = stmt.column_blob(0);
            let version = parse_version(stmt.column_int64(1))?;
            let expires_at_ms = if stmt.column_is_null(2) {
                None
            } else {
                Some(stmt.column_int64(2))
            };
            let value = self.codec.decode(raw_val)?;
            Some(KvEntry {
                namespace: namespace.to_owned(),
                key: key.to_vec(),
                value,
                version,
                expires_at_ms,
            })
        } else {
            None
        };
        stmt.reset()?;
        Ok(entry)
    }

    pub fn kv_scan(
        &self,
        namespace: &str,
        after: Option<&[u8]>,
        limit: u32,
    ) -> Result<Vec<KvEntry>> {
        validate_name("namespace", namespace)?;
        let mut connection = self.read_lock()?;
        let stmt = connection.slot(StmtSlot::KvScan)?;
        stmt.reset()?;
        stmt.bind_text_static(1, namespace)?;
        stmt.bind_blob_static(2, after.unwrap_or_default())?;
        stmt.bind_int64(3, now_ms())?;
        stmt.bind_int64(4, normalize_limit(limit))?;
        let mut entries = Vec::new();
        while stmt.step()? {
            let key = stmt.column_blob(0).to_vec();
            let raw_val = stmt.column_blob(1);
            let version = parse_version(stmt.column_int64(2))?;
            let expires_at_ms = if stmt.column_is_null(3) {
                None
            } else {
                Some(stmt.column_int64(3))
            };
            let value = self.codec.decode(raw_val)?;
            entries.push(KvEntry {
                namespace: namespace.to_owned(),
                key,
                value,
                version,
                expires_at_ms,
            });
        }
        stmt.reset()?;
        Ok(entries)
    }
}
