use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use lilia_core::{
    CollectionKind, CollectionSpec, Durability, ErrorCode, EvictionPolicy, LiliaError, Result,
    StorageTier,
};
use lilia_sqlite_native::NativeConnection;

use crate::storage::now_ms;

/// Concurrent registry resolving storage tiers, TTLs, and eviction policies per collection.
#[derive(Debug)]
pub struct CollectionCatalog {
    specs: RwLock<HashMap<(CollectionKind, String), CollectionSpec>>,
    ephemeral_tables: RwLock<HashSet<String>>,
    schema_epoch: AtomicU64,
}

impl Default for CollectionCatalog {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl CollectionCatalog {
    #[must_use]
    pub fn new(initial: Vec<CollectionSpec>) -> Self {
        let mut specs = HashMap::with_capacity(initial.len());
        for spec in initial {
            specs.insert((spec.kind, spec.name.clone()), spec);
        }
        Self {
            specs: RwLock::new(specs),
            ephemeral_tables: RwLock::new(HashSet::new()),
            schema_epoch: AtomicU64::new(1),
        }
    }

    pub fn set_spec(&self, spec: CollectionSpec) {
        let mut guard = self
            .specs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert((spec.kind, spec.name.clone()), spec);
        self.schema_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn remove_spec(&self, kind: CollectionKind, name: &str) -> Option<CollectionSpec> {
        let mut guard = self
            .specs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = guard.remove(&(kind, name.to_owned()));
        if removed.is_some() {
            self.schema_epoch.fetch_add(1, Ordering::AcqRel);
        }
        removed
    }

    #[must_use]
    pub fn spec_for(&self, kind: CollectionKind, name: &str) -> CollectionSpec {
        let guard = self
            .specs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .get(&(kind, name.to_owned()))
            .cloned()
            .unwrap_or_else(|| CollectionSpec::new(kind, name, StorageTier::Disk))
    }

    #[must_use]
    pub fn tier_for(&self, kind: CollectionKind, name: &str) -> StorageTier {
        let guard = self
            .specs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .get(&(kind, name.to_owned()))
            .map_or(StorageTier::Disk, |spec| spec.tier)
    }

    #[must_use]
    pub fn list_specs(&self) -> Vec<CollectionSpec> {
        let guard = self
            .specs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out: Vec<CollectionSpec> = guard.values().cloned().collect();
        out.sort_by(|a, b| (a.kind.as_str(), &a.name).cmp(&(b.kind.as_str(), &b.name)));
        out
    }

    pub fn register_ephemeral_table(&self, name: impl Into<String>) {
        let mut guard = self
            .ephemeral_tables
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.insert(name.into()) {
            self.schema_epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[must_use]
    pub fn list_ephemeral_tables(&self) -> Vec<String> {
        let guard = self
            .ephemeral_tables
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out: Vec<String> = guard.iter().cloned().collect();
        out.sort();
        out
    }

    #[must_use]
    pub fn schema_epoch(&self) -> u64 {
        self.schema_epoch.load(Ordering::Acquire)
    }

    /// Loads persisted collection specifications from `main._lilia_catalog` if present.
    pub(crate) fn load_from_connection(&self, conn: &NativeConnection) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT kind, name, tier, ttl_ms, eviction_json, durability, compartment, fle_policy_json, check_expr, auto_embed_json \
             FROM _lilia_catalog;",
        )?;
        let mut loaded = Vec::new();
        while stmt.step()? {
            let kind_str = stmt.column_text(0).to_owned();
            let name = stmt.column_text(1).to_owned();
            let tier_str = stmt.column_text(2).to_owned();
            let ttl_ms = if stmt.column_is_null(3) {
                None
            } else {
                u64::try_from(stmt.column_int64(3)).ok()
            };
            let eviction_str = stmt.column_text(4).to_owned();
            let durability_str = if stmt.column_is_null(5) {
                None
            } else {
                Some(stmt.column_text(5).to_owned())
            };
            let compartment = if stmt.column_count() > 6 && !stmt.column_is_null(6) {
                Some(stmt.column_text(6).to_owned())
            } else {
                None
            };
            let fle_policy: Option<lilia_core::FlePolicy> =
                if stmt.column_count() > 7 && !stmt.column_is_null(7) {
                    serde_json::from_str(stmt.column_text(7)).ok()
                } else {
                    None
                };

            let check = if stmt.column_count() > 8 && !stmt.column_is_null(8) {
                Some(stmt.column_text(8).to_owned())
            } else {
                None
            };
            let auto_embed: Option<lilia_core::AutoEmbedConfig> =
                if stmt.column_count() > 9 && !stmt.column_is_null(9) {
                    serde_json::from_str(stmt.column_text(9)).ok()
                } else {
                    None
                };

            let Some(kind) = CollectionKind::parse(&kind_str) else {
                continue;
            };
            let Some(tier) = StorageTier::parse(&tier_str) else {
                continue;
            };
            let eviction: EvictionPolicy =
                serde_json::from_str(&eviction_str).unwrap_or(EvictionPolicy::None);
            let durability: Option<Durability> =
                durability_str.and_then(|s| serde_json::from_str(&s).ok());
            loaded.push(CollectionSpec {
                kind,
                name,
                tier,
                ttl_ms,
                eviction,
                durability,
                compartment,
                fle_policy,
                check,
                auto_embed,
            });
        }
        drop(stmt);

        if !loaded.is_empty() {
            let mut guard = self
                .specs
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for spec in loaded {
                // Explicit options configured on DatabaseOptions take precedence over persisted
                // rules, except for auto-embed: it is only ever changed through
                // `JsonStore::set_auto_embed`, so the persisted value always wins.
                match guard.entry((spec.kind, spec.name.clone())) {
                    Entry::Occupied(mut explicit) => {
                        explicit.get_mut().auto_embed = spec.auto_embed;
                    }
                    Entry::Vacant(slot) => {
                        slot.insert(spec);
                    }
                }
            }
        }
        Ok(())
    }

    /// Persists a collection specification into `main._lilia_catalog`.
    pub(crate) fn persist_spec(conn: &NativeConnection, spec: &CollectionSpec) -> Result<()> {
        let eviction_json = serde_json::to_string(&spec.eviction)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        let durability_json = spec
            .durability
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        let fle_policy_json = spec
            .fle_policy
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        let auto_embed_json = spec
            .auto_embed
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        let mut stmt = conn.prepare(
            "INSERT INTO _lilia_catalog(kind, name, tier, ttl_ms, eviction_json, durability, compartment, fle_policy_json, check_expr, auto_embed_json, updated_at_ms) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(kind, name) DO UPDATE SET \
               tier = excluded.tier, \
               ttl_ms = excluded.ttl_ms, \
               eviction_json = excluded.eviction_json, \
               durability = excluded.durability, \
               compartment = excluded.compartment, \
               fle_policy_json = excluded.fle_policy_json, \
               check_expr = excluded.check_expr, \
               auto_embed_json = excluded.auto_embed_json, \
               updated_at_ms = excluded.updated_at_ms;",
        )?;
        stmt.bind_text_static(1, spec.kind.as_str())?;
        stmt.bind_text_static(2, &spec.name)?;
        stmt.bind_text_static(3, spec.tier.as_str())?;
        match spec.ttl_ms {
            Some(ttl) => stmt.bind_int64(4, i64::try_from(ttl).unwrap_or(i64::MAX))?,
            None => stmt.bind_null(4)?,
        }
        stmt.bind_text_static(5, &eviction_json)?;
        match durability_json.as_deref() {
            Some(d) => stmt.bind_text_static(6, d)?,
            None => stmt.bind_null(6)?,
        }
        match spec.compartment.as_deref() {
            Some(c) => stmt.bind_text_static(7, c)?,
            None => stmt.bind_null(7)?,
        }
        match fle_policy_json.as_deref() {
            Some(p) => stmt.bind_text_static(8, p)?,
            None => stmt.bind_null(8)?,
        }
        match spec.check.as_deref() {
            Some(c) => stmt.bind_text_static(9, c)?,
            None => stmt.bind_null(9)?,
        }
        match auto_embed_json.as_deref() {
            Some(config) => stmt.bind_text_static(10, config)?,
            None => stmt.bind_null(10)?,
        }
        stmt.bind_int64(11, now_ms())?;
        stmt.step()?;
        Ok(())
    }

    /// Deletes a persisted collection specification from `main._lilia_catalog`.
    pub(crate) fn delete_persisted_spec(
        conn: &NativeConnection,
        kind: CollectionKind,
        name: &str,
    ) -> Result<()> {
        let mut stmt =
            conn.prepare("DELETE FROM _lilia_catalog WHERE kind = ?1 AND name = ?2;")?;
        stmt.bind_text_static(1, kind.as_str())?;
        stmt.bind_text_static(2, name)?;
        stmt.step()?;
        Ok(())
    }
}
