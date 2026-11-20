#![allow(clippy::missing_errors_doc)]

use lilia_sqlite_native::StmtSlot;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::storage::{normalize_limit, now_ms, parse_version, validate_name};
use crate::Database;
use lilia_core::{CollectionKind, KvEntry, Result, StorageTier};

impl Database {
    pub(crate) fn is_kv_entry_authorized(
        &self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        action: lilia_core::security::PolicyAction,
    ) -> bool {
        let policies = self.security_registry.policies_for_table(namespace);
        if policies.is_empty() {
            return true;
        }
        let sec_ctx = crate::security::get_active_context();
        for np in policies {
            let applies = match action {
                lilia_core::security::PolicyAction::Select => np.policy.action.applies_to_read(),
                lilia_core::security::PolicyAction::Insert
                | lilia_core::security::PolicyAction::Update
                | lilia_core::security::PolicyAction::Delete => np.policy.action.applies_to_write(),
                _ => true,
            };
            if !applies {
                continue;
            }
            if let Ok(expr) = lilia_core::parse_expr(&np.policy.condition) {
                if !lilia_core::security::eval_security_condition_kv(
                    &expr,
                    namespace,
                    key,
                    value,
                    sec_ctx.as_ref(),
                ) {
                    return false;
                }
            }
        }
        true
    }

    pub fn kv_get(&self, namespace: &str, key: &[u8]) -> Result<Option<KvEntry>> {
        validate_name("namespace", namespace)?;
        // Fast path: L1 Cache Hit (0.00 ms)
        if let Some(cached) = self.cache.get_kv(namespace, key) {
            let expired = cached.expires_at_ms.is_some_and(|exp| exp <= now_ms());
            if !expired {
                if let Some(ref prefetcher) = self.prefetcher {
                    prefetcher.on_access(lilia_core::AccessTarget::kv(namespace, key));
                }
                if !self.is_kv_entry_authorized(
                    namespace,
                    &cached.key,
                    &cached.value,
                    lilia_core::security::PolicyAction::Select,
                ) {
                    return Ok(None);
                }
                return Ok(Some((*cached).clone()));
            }
            self.cache.invalidate_kv(namespace, key);
        }

        let now = now_ms();
        match self.catalog.tier_for(CollectionKind::Kv, namespace) {
            StorageTier::Memory => {
                let Some(snap) = self.mem_engine.kv.get(namespace, key, now, true) else {
                    return Ok(None);
                };
                let value = self.codec.decode(&snap.value)?;
                if !self.is_kv_entry_authorized(
                    namespace,
                    key,
                    &value,
                    lilia_core::security::PolicyAction::Select,
                ) {
                    return Ok(None);
                }
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
                            if !self.is_kv_entry_authorized(
                                namespace,
                                key,
                                &value,
                                lilia_core::security::PolicyAction::Select,
                            ) {
                                return Ok(None);
                            }
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
            let e = Arc::new(KvEntry {
                namespace: namespace.to_owned(),
                key: key.to_vec(),
                value,
                version,
                expires_at_ms,
            });
            self.cache.put_kv(namespace, key, Arc::clone(&e));
            Some((*e).clone())
        } else {
            None
        };
        stmt.reset()?;

        if let Some(ref prefetcher) = self.prefetcher {
            prefetcher.on_access(lilia_core::AccessTarget::kv(namespace, key));
        }

        if let Some(ref e) = entry {
            if !self.is_kv_entry_authorized(
                namespace,
                &e.key,
                &e.value,
                lilia_core::security::PolicyAction::Select,
            ) {
                return Ok(None);
            }
        }
        Ok(entry)
    }

    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
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
                if !self.is_kv_entry_authorized(
                    namespace,
                    &key,
                    &value,
                    lilia_core::security::PolicyAction::Select,
                ) {
                    continue;
                }
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

        let mut out = Vec::with_capacity(norm_limit.min(merged.len()));
        for entry in merged.into_values().flatten() {
            if self.is_kv_entry_authorized(
                namespace,
                &entry.key,
                &entry.value,
                lilia_core::security::PolicyAction::Select,
            ) {
                out.push(entry);
                if out.len() >= norm_limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Retrieve the physical filesystem path to the CAS blob for a KV entry, if stored in CAS.
    pub fn kv_get_blob_path(
        &self,
        namespace: &str,
        key: &[u8],
    ) -> Result<Option<std::path::PathBuf>> {
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

    /// Put an archived typed value into a KV namespace.
    pub fn typed_put<T>(
        &self,
        namespace: &str,
        key: &[u8],
        value: &T,
        expires_at_ms: Option<i64>,
    ) -> Result<u64>
    where
        T: for<'a> rkyv::Serialize<
            rkyv::api::high::HighSerializer<
                rkyv::util::AlignedVec,
                rkyv::ser::allocator::ArenaHandle<'a>,
                rkyv::rancor::Error,
            >,
        >,
    {
        let bytes = lilia_core::archive_to_bytes(value)?;
        let results = self.batch(&[lilia_core::BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: bytes.to_vec(),
            if_version: None,
            expires_at_ms,
        }])?;
        Ok(results.first().and_then(|r| r.version).unwrap_or(1))
    }

    /// Read an archived typed struct directly from KV storage with 0 nanoseconds deserialization.
    pub fn with_archived_kv<T, R, F>(
        &self,
        namespace: &str,
        key: &[u8],
        f: F,
    ) -> Result<Option<R>>
    where
        T: rkyv::Archive,
        T::Archived: for<'a> rkyv::bytecheck::CheckBytes<
            rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>,
        >,
        F: FnOnce(&T::Archived) -> R,
    {
        if let Some(ref shm) = self.shm_engine {
            if let Some(archived) = shm.get_archived::<T>(namespace, key) {
                return Ok(Some(f(archived)));
            }
        }
        let Some(entry) = self.kv_get(namespace, key)? else {
            return Ok(None);
        };
        let archived = lilia_core::access_archived::<T>(&entry.value)?;
        Ok(Some(f(archived)))
    }
}
