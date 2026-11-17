#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;

use lilia_sqlite_native::StmtSlot;

use crate::storage::{normalize_limit, now_ms, parse_version, validate_name};
use crate::Database;
use lilia_core::{CollectionKind, KvEntry, Result, StorageTier};

impl Database {
    pub fn kv_get(&self, namespace: &str, key: &[u8]) -> Result<Option<KvEntry>> {
        validate_name("namespace", namespace)?;
        let now = now_ms();
        match self.catalog.tier_for(CollectionKind::Kv, namespace) {
            StorageTier::Memory => {
                let Some(snap) = self.mem_engine.kv.get(namespace, key, now, true) else {
                    return Ok(None);
                };
                let value = self.codec.decode(&snap.value)?;
                return Ok(Some(KvEntry {
                    namespace: namespace.to_owned(),
                    key: key.to_vec(),
                    value,
                    version: snap.version,
                    expires_at_ms: snap.expires_at_ms,
                }));
            }
            StorageTier::Buffered => {
                if let Some(snap) = self.ring_core.overlay.kv.get(namespace, key, now, false) {
                    return match snap.value.state {
                        None => Ok(None),
                        Some((encoded, seq, expires_at_ms)) => {
                            if expires_at_ms.is_some_and(|exp| exp <= now) {
                                return Ok(None);
                            }
                            let value = self.codec.decode(&encoded)?;
                            Ok(Some(KvEntry {
                                namespace: namespace.to_owned(),
                                key: key.to_vec(),
                                value,
                                version: seq.max(1),
                                expires_at_ms,
                            }))
                        }
                    };
                }
            }
            StorageTier::Disk => {}
        }

        let mut connection = self.read_lock()?;
        let stmt = connection.slot(StmtSlot::KvGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, namespace)?;
        stmt.bind_blob_static(2, key)?;
        stmt.bind_int64(3, now)?;
        let entry = if stmt.step()? {
            let raw_val = stmt.column_blob(0);
            let version = parse_version(stmt.column_int64(1))?;
            let expires_at_ms = if stmt.column_is_null(2) {
                None
            } else {
                Some(stmt.column_int64(2))
            };
            let value = if lilia_core::cas::is_kv_pointer(raw_val) {
                if let Some((_, hash, _)) = lilia_core::cas::decode_kv_pointer(raw_val) {
                    self.cas.get(&hash)?.ok_or_else(|| {
                        lilia_core::LiliaError::new(
                            lilia_core::ErrorCode::Storage,
                            format!("missing CAS blob file for hash {hash}"),
                            false,
                        )
                    })?
                } else {
                    self.codec.decode(raw_val)?
                }
            } else {
                self.codec.decode(raw_val)?
            };
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
        let now = now_ms();
        let norm_limit = usize::try_from(normalize_limit(limit)).unwrap_or(usize::MAX);
        let tier = self.catalog.tier_for(CollectionKind::Kv, namespace);

        if tier == StorageTier::Memory {
            let snaps = self
                .mem_engine
                .kv
                .scan(Some(namespace), after, norm_limit, now);
            let mut entries = Vec::with_capacity(snaps.len());
            for (_, key, snap) in snaps {
                let value = self.codec.decode(&snap.value)?;
                entries.push(KvEntry {
                    namespace: namespace.to_owned(),
                    key,
                    value,
                    version: snap.version,
                    expires_at_ms: snap.expires_at_ms,
                });
            }
            return Ok(entries);
        }

        let overlay_items = if tier == StorageTier::Buffered {
            self.ring_core
                .overlay
                .kv
                .scan(Some(namespace), after, usize::MAX, now)
        } else {
            Vec::new()
        };

        let fetch_limit = if overlay_items.is_empty() {
            normalize_limit(limit)
        } else {
            normalize_limit(limit).saturating_add(i64::try_from(overlay_items.len()).unwrap_or(0))
        };

        let mut merged: BTreeMap<Vec<u8>, Option<KvEntry>> = BTreeMap::new();
        {
            let mut connection = self.read_lock()?;
            let stmt = connection.slot(StmtSlot::KvScan)?;
            stmt.reset()?;
            stmt.bind_text_static(1, namespace)?;
            stmt.bind_blob_static(2, after.unwrap_or_default())?;
            stmt.bind_int64(3, now)?;
            stmt.bind_int64(4, fetch_limit)?;
            while stmt.step()? {
                let key = stmt.column_blob(0).to_vec();
                let raw_val = stmt.column_blob(1);
                let version = parse_version(stmt.column_int64(2))?;
                let expires_at_ms = if stmt.column_is_null(3) {
                    None
                } else {
                    Some(stmt.column_int64(3))
                };
                let value = if lilia_core::cas::is_kv_pointer(raw_val) {
                if let Some((_, hash, _)) = lilia_core::cas::decode_kv_pointer(raw_val) {
                    self.cas.get(&hash)?.ok_or_else(|| {
                        lilia_core::LiliaError::new(
                            lilia_core::ErrorCode::Storage,
                            format!("missing CAS blob file for hash {hash}"),
                            false,
                        )
                    })?
                } else {
                    self.codec.decode(raw_val)?
                }
            } else {
                self.codec.decode(raw_val)?
            };
                merged.insert(
                    key.clone(),
                    Some(KvEntry {
                        namespace: namespace.to_owned(),
                        key,
                        value,
                        version,
                        expires_at_ms,
                    }),
                );
            }
            stmt.reset()?;
        }

        if !overlay_items.is_empty() {
            for (_, key, snap) in overlay_items {
                if after.is_some_and(|a| key.as_slice() <= a) {
                    continue;
                }
                match snap.value.state {
                    None => {
                        merged.insert(key, None);
                    }
                    Some((raw_val, seq, expires_at_ms)) => {
                        if expires_at_ms.is_some_and(|exp| exp <= now) {
                            merged.insert(key, None);
                        } else {
                            let value = self.codec.decode(&raw_val)?;
                            merged.insert(
                                key.clone(),
                                Some(KvEntry {
                                    namespace: namespace.to_owned(),
                                    key,
                                    value,
                                    version: seq.max(1),
                                    expires_at_ms,
                                }),
                            );
                        }
                    }
                }
            }
        }

        Ok(merged
            .into_values()
            .flatten()
            .take(norm_limit)
            .collect())
    }

    /// Retrieve the physical filesystem path to the CAS blob for a KV entry, if stored in CAS.
    pub fn kv_get_blob_path(&self, namespace: &str, key: &[u8]) -> Result<Option<std::path::PathBuf>> {
        validate_name("namespace", namespace)?;
        let mut connection = self.read_lock()?;
        let stmt = connection.slot(StmtSlot::KvGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, namespace)?;
        stmt.bind_blob_static(2, key)?;
        stmt.bind_int64(3, now_ms())?;
        let res = if stmt.step()? {
            let raw_val = stmt.column_blob(0);
            if lilia_core::cas::is_kv_pointer(raw_val) {
                if let Some((_, hash, _)) = lilia_core::cas::decode_kv_pointer(raw_val) {
                    let path = self.cas.path_for_hash(&hash);
                    if path.is_file() {
                        Some(path)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        stmt.reset()?;
        Ok(res)
    }

    /// Open a read-only stream to the CAS blob for a KV entry, without allocating the payload in memory.
    pub fn kv_get_blob_reader(&self, namespace: &str, key: &[u8]) -> Result<Option<std::fs::File>> {
        if let Some(path) = self.kv_get_blob_path(namespace, key)? {
            let file = std::fs::File::open(path).map_err(|e| {
                lilia_core::LiliaError::new(
                    lilia_core::ErrorCode::Io,
                    format!("failed to open blob file: {e}"),
                    false,
                )
            })?;
            Ok(Some(file))
        } else {
            Ok(None)
        }
    }
}
