#[cfg(not(target_arch = "wasm32"))]
pub mod archive;
pub mod layout;
pub mod views;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use lilia_core::sharding::{ShardInfo, ShardStatus, ShardingConfig};
use lilia_core::{BatchOperation, ErrorCode, LiliaError, Result};
use lilia_sqlite_native::{NativeConnection, NativeTransaction, StmtSlot};

pub use layout::ShardLayout;

/// Qualifies `table` with the attached database `schema`, or returns it
/// unchanged when no schema is set (non-sharded databases).
#[must_use]
pub(crate) fn qualified(schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(schema) => format!("{schema}.{table}"),
        None => table.to_string(),
    }
}

/// Central coordinator for local temporal sharding, attachment, rollover, and cold storage.
#[derive(Debug)]
pub struct ShardManager {
    pub config: ShardingConfig,
    pub layout: ShardLayout,
    active_shard_id: Arc<RwLock<String>>,
    shards: Arc<RwLock<HashMap<String, ShardInfo>>>,
}

impl ShardManager {
    /// Creates a new `ShardManager` for the given base path and configuration.
    ///
    /// # Errors
    /// Returns `Storage` errors detected here, and propagates errors from the
    /// operations it performs.
    pub fn new(base_path: &Path, config: ShardingConfig) -> Result<Self> {
        let layout = ShardLayout::resolve(base_path, &config);
        layout.ensure_dirs().map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("failed to create shards directory: {e}"),
                false,
            )
        })?;

        Ok(Self {
            config,
            layout,
            active_shard_id: Arc::new(RwLock::new(String::new())),
            shards: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Loads the shard catalog from the coordinator database and syncs with on-disk files.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub fn load_from_connection(&self, connection: &NativeConnection) -> Result<()> {
        let mut loaded = HashMap::new();

        // 1. Read existing records from _lilia_shards
        if let Ok(mut stmt) = connection.prepare(
            "SELECT shard_id, period_start_ms, period_end_ms, file_name, status, \
                    is_compressed, uncompressed_bytes, compressed_bytes, checksum, \
                    created_at_ms, archived_at_ms \
             FROM _lilia_shards \
             ORDER BY period_start_ms ASC;",
        ) {
            while stmt.step()? {
                let shard_id = stmt.column_text(0).to_string();
                let period_start_ms = stmt.column_int64(1);
                let period_end_ms = stmt.column_int64(2);
                let file_name = stmt.column_text(3).to_string();
                let status_str = stmt.column_text(4);
                let status = ShardStatus::parse(status_str).unwrap_or(ShardStatus::Warm);
                let is_compressed = stmt.column_int64(5) != 0;
                let uncompressed_bytes = u64::try_from(stmt.column_int64(6)).unwrap_or_default();
                let compressed_bytes = u64::try_from(stmt.column_int64(7)).unwrap_or_default();
                let checksum = if stmt.column_is_null(8) {
                    None
                } else {
                    Some(stmt.column_text(8).to_string())
                };
                let created_at_ms = stmt.column_int64(9);
                let archived_at_ms = if stmt.column_is_null(10) {
                    None
                } else {
                    Some(stmt.column_int64(10))
                };

                let file_path = self.layout.shards_dir.join(&file_name);

                loaded.insert(
                    shard_id.clone(),
                    ShardInfo {
                        shard_id,
                        status,
                        period_start_ms,
                        period_end_ms,
                        file_path,
                        is_compressed,
                        uncompressed_bytes,
                        compressed_bytes,
                        checksum,
                        created_at_ms,
                        archived_at_ms,
                    },
                );
            }
        }

        // 2. Discover files in shards_dir
        if let Ok(entries) = std::fs::read_dir(&self.layout.shards_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if path.extension().is_some_and(|ext| ext == "lilia") {
                        let id = name.trim_end_matches(".lilia");
                        if !loaded.contains_key(id) {
                            if let Ok((start, end)) = self.config.granularity.parse_period_range(id)
                            {
                                let meta = std::fs::metadata(&path).ok();
                                let size = meta.map_or(0, |m| m.len());
                                loaded.insert(
                                    id.to_string(),
                                    ShardInfo {
                                        shard_id: id.to_string(),
                                        status: ShardStatus::Warm,
                                        period_start_ms: start,
                                        period_end_ms: end,
                                        file_path: path.clone(),
                                        is_compressed: false,
                                        uncompressed_bytes: size,
                                        compressed_bytes: 0,
                                        checksum: None,
                                        created_at_ms: crate::storage::now_ms(),
                                        archived_at_ms: None,
                                    },
                                );
                            }
                        }
                    } else if name.ends_with(".lilia.zst") {
                        let id = name.trim_end_matches(".lilia.zst");
                        if !loaded.contains_key(id) {
                            if let Ok((start, end)) = self.config.granularity.parse_period_range(id)
                            {
                                let meta = std::fs::metadata(&path).ok();
                                let size = meta.map_or(0, |m| m.len());
                                loaded.insert(
                                    id.to_string(),
                                    ShardInfo {
                                        shard_id: id.to_string(),
                                        status: ShardStatus::Cold,
                                        period_start_ms: start,
                                        period_end_ms: end,
                                        file_path: path.clone(),
                                        is_compressed: true,
                                        uncompressed_bytes: 0,
                                        compressed_bytes: size,
                                        checksum: None,
                                        created_at_ms: crate::storage::now_ms(),
                                        archived_at_ms: Some(crate::storage::now_ms()),
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }

        // 3. Ensure an active shard exists for current time
        let now = crate::storage::now_ms();
        let current_period = self.config.granularity.format_period(now);
        let mut active_id = String::new();

        if let Some(existing) = loaded.get_mut(&current_period) {
            if !existing.is_compressed {
                existing.status = ShardStatus::Active;
                active_id.clone_from(&current_period);
            }
        }

        if active_id.is_empty() {
            let (start, end) = self
                .config
                .granularity
                .parse_period_range(&current_period)?;
            let file_name = ShardLayout::shard_file_name(&current_period);
            let file_path = self.layout.shards_dir.join(&file_name);
            let info = ShardInfo {
                shard_id: current_period.clone(),
                status: ShardStatus::Active,
                period_start_ms: start,
                period_end_ms: end,
                file_path,
                is_compressed: false,
                uncompressed_bytes: 0,
                compressed_bytes: 0,
                checksum: None,
                created_at_ms: now,
                archived_at_ms: None,
            };
            loaded.insert(current_period.clone(), info);
            active_id = current_period;
        }

        // 4. Persist updated shards to _lilia_shards
        for info in loaded.values() {
            self.persist_shard_record(connection, info)?;
        }

        *self
            .active_shard_id
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = active_id;
        *self
            .shards
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = loaded;

        Ok(())
    }

    /// Persists or updates a shard record in `_lilia_shards`.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn persist_shard_record(
        &self,
        connection: &NativeConnection,
        info: &ShardInfo,
    ) -> Result<()> {
        let file_name = info
            .file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let checksum_sql = match &info.checksum {
            Some(cs) => format!("'{cs}'"),
            None => "NULL".to_string(),
        };
        let archived_sql = match info.archived_at_ms {
            Some(ts) => ts.to_string(),
            None => "NULL".to_string(),
        };

        let sql = format!(
            "INSERT INTO _lilia_shards(shard_id, period_start_ms, period_end_ms, file_name, \
                                       status, is_compressed, uncompressed_bytes, compressed_bytes, \
                                       checksum, created_at_ms, archived_at_ms) \
             VALUES('{id}', {start}, {end}, '{fname}', '{status}', {comp}, {ubytes}, {cbytes}, \
                    {checksum_sql}, {created}, {archived_sql}) \
             ON CONFLICT(shard_id) DO UPDATE SET \
               status = '{status}', \
               file_name = '{fname}', \
               is_compressed = {comp}, \
               uncompressed_bytes = {ubytes}, \
               compressed_bytes = {cbytes}, \
               checksum = {checksum_sql}, \
               archived_at_ms = {archived_sql};",
            id = info.shard_id,
            start = info.period_start_ms,
            end = info.period_end_ms,
            fname = file_name,
            status = info.status.as_str(),
            comp = i32::from(info.is_compressed),
            ubytes = info.uncompressed_bytes,
            cbytes = info.compressed_bytes,
            created = info.created_at_ms,
        );

        connection.exec(&sql)
    }

    /// Returns list of all known shards.
    #[must_use]
    pub fn list_shards(&self) -> Vec<ShardInfo> {
        let mut list: Vec<ShardInfo> = self
            .shards
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        list.sort_by_key(|s| s.period_start_ms);
        list
    }

    /// Returns the catalogue entry for `shard_id`.
    ///
    /// # Errors
    /// Returns `NotFound` errors.
    pub fn get_shard_info(&self, shard_id: &str) -> Result<ShardInfo> {
        self.shards
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(shard_id)
            .cloned()
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::NotFound,
                    format!("shard '{shard_id}' not found"),
                    false,
                )
            })
    }

    /// Returns information about the current active writing shard.
    ///
    /// # Errors
    /// Returns `NotFound` errors.
    pub fn active_shard(&self) -> Result<ShardInfo> {
        let active_id = self
            .active_shard_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.shards
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&active_id)
            .cloned()
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::NotFound,
                    format!("active shard '{active_id}' not found"),
                    false,
                )
            })
    }

    /// Returns list of SQLite aliases (`shard_YYYY_MM`) for all currently attached shards (Active and Warm).
    #[must_use]
    pub fn attached_aliases(&self) -> Vec<String> {
        let mut aliases = Vec::new();
        let guard = self
            .shards
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for info in guard.values() {
            if info.status.is_attached() && !info.is_compressed {
                aliases.push(ShardLayout::shard_alias(&info.shard_id));
            }
        }
        aliases.sort();
        aliases
    }

    /// Returns list of attached `(alias, file_path)` pairs.
    #[must_use]
    pub fn attached_shard_paths(&self) -> Vec<(String, PathBuf)> {
        let mut pairs = Vec::new();
        let guard = self
            .shards
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for info in guard.values() {
            if info.status.is_attached() && !info.is_compressed {
                pairs.push((
                    ShardLayout::shard_alias(&info.shard_id),
                    info.file_path.clone(),
                ));
            }
        }
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    /// Resolves the appropriate attached shard alias for writing an operation with the given timestamp.
    /// If timestamp is within an attached warm shard, returns its alias (Option A).
    /// Otherwise, returns the active shard alias.
    #[must_use]
    pub fn resolve_target_shard_alias(&self, timestamp_ms: Option<i64>) -> String {
        let active_id = self
            .active_shard_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        if let Some(ts) = timestamp_ms {
            let period = self.config.granularity.format_period(ts);
            let guard = self
                .shards
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(info) = guard.get(&period) {
                if info.status.is_attached() && !info.is_compressed {
                    return ShardLayout::shard_alias(&info.shard_id);
                }
            }
        }

        ShardLayout::shard_alias(&active_id)
    }

    /// Every schema holding sharded data, newest shard first, ending with the
    /// coordinator (`main`), which keeps data written before sharding was enabled.
    #[must_use]
    pub fn data_schemas(&self) -> Vec<String> {
        let mut schemas: Vec<String> = self.attached_aliases().into_iter().rev().collect();
        schemas.push("main".to_string());
        schemas
    }

    /// Schemas that may hold time-series chunks overlapping `[start_ms, end_ms]`:
    /// attached shards whose period overlaps the range, the active shard and the
    /// coordinator (both receive chunks that fall outside any shard period).
    #[must_use]
    pub fn schemas_overlapping(&self, start_ms: i64, end_ms: i64) -> Vec<String> {
        let guard = self
            .shards
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut schemas: Vec<String> = guard
            .values()
            .filter(|info| {
                info.status.is_attached()
                    && !info.is_compressed
                    && (info.status == ShardStatus::Active
                        || (info.period_start_ms <= end_ms && info.period_end_ms > start_ms))
            })
            .map(|info| ShardLayout::shard_alias(&info.shard_id))
            .collect();
        schemas.sort();
        schemas.push("main".to_string());
        schemas
    }

    /// Whether `now_ms` has moved past the end of the active shard's period.
    #[must_use]
    pub fn rollover_due(&self, now_ms: i64) -> bool {
        self.active_shard()
            .map_or(true, |active| now_ms >= active.period_end_ms)
    }

    /// Picks the schema a batch operation must run against.
    ///
    /// A key/document that already exists is mutated in the schema it lives in,
    /// so records never duplicate across shards. New records go to the active
    /// shard. Operations on non-sharded models need no routing (`None`).
    pub(crate) fn route_operation(
        &self,
        tx: &mut NativeTransaction<'_>,
        operation: &BatchOperation,
    ) -> Result<Option<String>> {
        let located = match operation {
            BatchOperation::KvSet { namespace, key, .. }
            | BatchOperation::KvDelete { namespace, key, .. } => {
                let stmt = tx.slot(StmtSlot::KvLocate)?;
                stmt.reset()?;
                stmt.bind_text_static(1, namespace)?;
                stmt.bind_blob_static(2, key)?;
                let found = stmt.step()?.then(|| stmt.column_text(0).to_string());
                stmt.reset()?;
                found
            }
            BatchOperation::JsonPut { space, id, .. }
            | BatchOperation::JsonPutRaw { space, id, .. }
            | BatchOperation::JsonPutHlc { space, id, .. }
            | BatchOperation::JsonDelete { space, id, .. } => {
                let stmt = tx.slot(StmtSlot::JsonLocate)?;
                stmt.reset()?;
                stmt.bind_text_static(1, space)?;
                stmt.bind_text_static(2, id)?;
                let found = stmt.step()?.then(|| stmt.column_text(0).to_string());
                stmt.reset()?;
                found
            }
            _ => return Ok(None),
        };
        Ok(Some(
            located.unwrap_or_else(|| self.resolve_target_shard_alias(None)),
        ))
    }

    /// Rollover to a new active shard for `now_ms`.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn rollover(&self, connection: &NativeConnection, now_ms: i64) -> Result<ShardInfo> {
        let new_period = self.config.granularity.format_period(now_ms);
        let active_id = self
            .active_shard_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        if new_period == active_id && !active_id.is_empty() {
            // Already active on this period
            return self.active_shard();
        }

        let mut shards = self
            .shards
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Transition previous active shard to Warm
        if let Some(old) = shards.get_mut(&active_id) {
            old.status = ShardStatus::Warm;
            self.persist_shard_record(connection, old)?;
        }

        // Create or activate new shard
        let new_info = if let Some(existing) = shards.get_mut(&new_period) {
            existing.status = ShardStatus::Active;
            existing.clone()
        } else {
            let (start, end) = self.config.granularity.parse_period_range(&new_period)?;
            let file_name = ShardLayout::shard_file_name(&new_period);
            let file_path = self.layout.shards_dir.join(&file_name);
            let info = ShardInfo {
                shard_id: new_period.clone(),
                status: ShardStatus::Active,
                period_start_ms: start,
                period_end_ms: end,
                file_path,
                is_compressed: false,
                uncompressed_bytes: 0,
                compressed_bytes: 0,
                checksum: None,
                created_at_ms: now_ms,
                archived_at_ms: None,
            };
            shards.insert(new_period.clone(), info.clone());
            info
        };

        self.persist_shard_record(connection, &new_info)?;
        *self
            .active_shard_id
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = new_period;

        Ok(new_info)
    }

    /// Updates the in-memory and persisted status of a shard.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn update_shard_info(&self, connection: &NativeConnection, info: ShardInfo) -> Result<()> {
        let mut guard = self
            .shards
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.persist_shard_record(connection, &info)?;
        guard.insert(info.shard_id.clone(), info);
        Ok(())
    }

    /// Removes a shard from tracking (e.g. when purged).
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn remove_shard(&self, connection: &NativeConnection, shard_id: &str) -> Result<()> {
        let mut guard = self
            .shards
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        connection.exec(&format!(
            "DELETE FROM _lilia_shards WHERE shard_id = '{shard_id}';"
        ))?;
        guard.remove(shard_id);
        Ok(())
    }
}
