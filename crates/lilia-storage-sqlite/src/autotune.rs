//! Storage side of the auto-tuning engine: records workload statistics, persists them, and
//! applies the planner's recommendations (chunked index builds and index drops).

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use lilia_core::{
    plan_autotune, AutotuneCollector, AutotuneOptions, AutotuneReport, CollectionKind, CycleReport,
    ErrorCode, IndexLookups, IndexOrigin, IndexPhase, IndexRef, IndexUsageView, JsonQueryReport,
    LiliaError, QueryFingerprint, QueryObservation, QueryStats, Recommendation, Result,
    SqlObservation, SqlQueryReport, StorageTier, TrackedIndex, ValueCodec,
};
use lilia_sqlite_native::{NativeConnection, NativeTransaction};

use crate::catalog::CollectionCatalog;
use crate::engine::Resources;
use crate::json::{decode_stored_doc, index_keys, FindTrace};
use crate::lifecycle::Lifecycle;
use crate::sharding::qualified;
use crate::storage::{now_ms, validate_name};

const DOCS_TABLE: &str = "_lilia_json";
const ENTRIES_TABLE: &str = "_lilia_json_index_entries";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn from_i64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Borrowed handles an indexing operation needs. The background worker upgrades its weak
/// references into one of these for the duration of a cycle.
#[derive(Debug)]
pub(crate) struct IndexEnv<'a> {
    pub(crate) resources: &'a Resources,
    pub(crate) lifecycle: &'a Lifecycle,
    pub(crate) catalog: &'a CollectionCatalog,
    pub(crate) codec: &'a dyn ValueCodec,
    pub(crate) dict_registry: &'a lilia_core::DictionaryRegistry,
}

impl IndexEnv<'_> {
    fn schemas(&self) -> Vec<Option<String>> {
        crate::timeseries::data_schemas(self.resources)
    }

    fn is_disk_json(&self, space: &str) -> bool {
        self.catalog.tier_for(CollectionKind::Json, space) == StorageTier::Disk
    }
}

#[derive(Debug, Clone)]
struct Definition {
    space: String,
    name: String,
    expression: String,
    filter: Option<String>,
}

#[derive(Debug, Clone)]
struct IndexRow {
    space: String,
    name: String,
    origin: IndexOrigin,
    phase: IndexPhase,
    first_seen_ms: i64,
    last_used_ms: Option<i64>,
    lookups: u64,
    changed_ms: i64,
}

impl IndexRow {
    fn key(&self) -> (String, String) {
        (self.space.clone(), self.name.clone())
    }
}

#[derive(Debug, Default)]
struct RowChanges {
    upserts: Vec<IndexRow>,
    deletes: Vec<(String, String)>,
}

fn read_definitions(connection: &NativeConnection) -> Result<Vec<Definition>> {
    let mut statement = connection.prepare(
        "SELECT space, index_name, json_path, filter_predicate FROM _lilia_json_indexes;",
    )?;
    let mut definitions = Vec::new();
    while statement.step()? {
        let filter = (!statement.column_is_null(3))
            .then(|| statement.column_text(3).to_owned())
            .filter(|text| !text.is_empty());
        definitions.push(Definition {
            space: statement.column_text(0).to_owned(),
            name: statement.column_text(1).to_owned(),
            expression: statement.column_text(2).to_owned(),
            filter,
        });
    }
    Ok(definitions)
}

fn read_rows(connection: &NativeConnection) -> Result<Vec<IndexRow>> {
    let mut statement = connection.prepare(
        "SELECT space, index_name, origin, phase, first_seen_ms, last_used_ms, lookups, changed_ms \
         FROM _lilia_autotune_indexes;",
    )?;
    let mut rows = Vec::new();
    while statement.step()? {
        let (Some(origin), Some(phase)) = (
            IndexOrigin::parse(statement.column_text(2)),
            IndexPhase::parse(statement.column_text(3)),
        ) else {
            continue;
        };
        rows.push(IndexRow {
            space: statement.column_text(0).to_owned(),
            name: statement.column_text(1).to_owned(),
            origin,
            phase,
            first_seen_ms: statement.column_int64(4),
            last_used_ms: (!statement.column_is_null(5)).then(|| statement.column_int64(5)),
            lookups: from_i64(statement.column_int64(6)),
            changed_ms: statement.column_int64(7),
        });
    }
    Ok(rows)
}

fn upsert_row(tx: &NativeTransaction<'_>, row: &IndexRow) -> Result<()> {
    let mut statement = tx.prepare(
        "INSERT INTO _lilia_autotune_indexes(space, index_name, origin, phase, first_seen_ms, last_used_ms, lookups, changed_ms) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(space, index_name) DO UPDATE SET origin = excluded.origin, phase = excluded.phase, \
         first_seen_ms = excluded.first_seen_ms, last_used_ms = excluded.last_used_ms, \
         lookups = excluded.lookups, changed_ms = excluded.changed_ms;",
    )?;
    statement.bind_text_static(1, &row.space)?;
    statement.bind_text_static(2, &row.name)?;
    statement.bind_text_static(3, row.origin.as_str())?;
    statement.bind_text_static(4, row.phase.as_str())?;
    statement.bind_int64(5, row.first_seen_ms)?;
    match row.last_used_ms {
        Some(at) => statement.bind_int64(6, at)?,
        None => statement.bind_null(6)?,
    }
    statement.bind_int64(7, to_i64(row.lookups))?;
    statement.bind_int64(8, row.changed_ms)?;
    statement.step()?;
    Ok(())
}

fn delete_row(tx: &NativeTransaction<'_>, space: &str, name: &str) -> Result<()> {
    let mut statement =
        tx.prepare("DELETE FROM _lilia_autotune_indexes WHERE space = ?1 AND index_name = ?2;")?;
    statement.bind_text_static(1, space)?;
    statement.bind_text_static(2, name)?;
    statement.step()?;
    Ok(())
}

fn upsert_query_stats(
    tx: &NativeTransaction<'_>,
    fingerprint: &QueryFingerprint,
    stats: &QueryStats,
) -> Result<()> {
    let fields = serde_json::to_string(&fingerprint.fields)
        .map_err(|error| LiliaError::new(ErrorCode::Storage, error.to_string(), false))?;
    let mut statement = tx.prepare(
        "INSERT INTO _lilia_autotune_query_stats(space, fields, executions, scans, slow_scans, \
         scan_rows_examined, rows_examined, rows_returned, total_duration_us, max_duration_us, \
         first_seen_ms, last_seen_ms) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT(space, fields) DO UPDATE SET executions = excluded.executions, scans = excluded.scans, \
         slow_scans = excluded.slow_scans, scan_rows_examined = excluded.scan_rows_examined, \
         rows_examined = excluded.rows_examined, rows_returned = excluded.rows_returned, \
         total_duration_us = excluded.total_duration_us, max_duration_us = excluded.max_duration_us, \
         first_seen_ms = excluded.first_seen_ms, last_seen_ms = excluded.last_seen_ms;",
    )?;
    statement.bind_text_static(1, &fingerprint.space)?;
    statement.bind_text_static(2, &fields)?;
    for (column, value) in [
        (3, stats.executions),
        (4, stats.scans),
        (5, stats.slow_scans),
        (6, stats.scan_rows_examined),
        (7, stats.rows_examined),
        (8, stats.rows_returned),
        (9, stats.total_duration_us),
        (10, stats.max_duration_us),
    ] {
        statement.bind_int64(column, to_i64(value))?;
    }
    statement.bind_int64(11, stats.first_seen_ms)?;
    statement.bind_int64(12, stats.last_seen_ms)?;
    statement.step()?;
    Ok(())
}

fn update_lookups(
    tx: &NativeTransaction<'_>,
    space: &str,
    name: &str,
    lookups: IndexLookups,
) -> Result<()> {
    let mut statement = tx.prepare(
        "UPDATE _lilia_autotune_indexes SET lookups = ?3, last_used_ms = ?4 \
         WHERE space = ?1 AND index_name = ?2;",
    )?;
    statement.bind_text_static(1, space)?;
    statement.bind_text_static(2, name)?;
    statement.bind_int64(3, to_i64(lookups.lookups))?;
    statement.bind_int64(4, lookups.last_used_ms)?;
    statement.step()?;
    Ok(())
}

/// Auto-tuning state shared by the database handle and its background worker.
#[derive(Debug)]
pub(crate) struct AutotuneEngine {
    options: AutotuneOptions,
    collector: AutotuneCollector,
    /// Indexes being backfilled: their definition exists but `json_find` must not use them yet.
    building: Mutex<HashSet<(String, String)>>,
    /// Serializes tuning cycles.
    cycle_guard: Mutex<()>,
    last_cycle: Mutex<Option<CycleReport>>,
}

impl AutotuneEngine {
    pub(crate) fn new(options: AutotuneOptions) -> Result<Self> {
        options.validate()?;
        Ok(Self {
            collector: AutotuneCollector::new(
                options.max_tracked_queries,
                options.slow_query_threshold_us,
            ),
            options,
            building: Mutex::new(HashSet::new()),
            cycle_guard: Mutex::new(()),
            last_cycle: Mutex::new(None),
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn enabled(&self) -> bool {
        self.options.enabled
    }

    /// Whether `find` executions on `space` are worth recording: only disk-tier collections have
    /// real secondary indexes.
    pub(crate) fn records(&self, catalog: &CollectionCatalog, space: &str) -> bool {
        self.options.enabled && catalog.tier_for(CollectionKind::Json, space) == StorageTier::Disk
    }

    pub(crate) fn record_find(
        &self,
        space: &str,
        query: &serde_json::Map<String, serde_json::Value>,
        trace: &FindTrace,
        returned: usize,
        elapsed: Duration,
    ) {
        let at_ms = now_ms();
        let fingerprint = QueryFingerprint::new(space, query.keys().map(String::as_str));
        self.collector.record_query(
            fingerprint,
            &QueryObservation {
                used_index: trace.index_used.is_some(),
                rows_examined: trace.rows_examined,
                rows_returned: returned as u64,
                duration_us: micros(elapsed),
                at_ms,
            },
        );
        if let Some(index) = &trace.index_used {
            self.collector.record_index_lookup(space, index, at_ms);
        }
    }

    pub(crate) fn record_index_use(&self, space: &str, index: &str) {
        if self.options.enabled {
            self.collector.record_index_lookup(space, index, now_ms());
        }
    }

    pub(crate) fn record_sql(&self, sql: &str, elapsed: Duration, full_scan_steps: u64) {
        if self.options.enabled {
            self.collector.record_sql(
                lilia_core::normalize_sql(sql),
                &SqlObservation {
                    duration_us: micros(elapsed),
                    full_scan_steps,
                    at_ms: now_ms(),
                },
            );
        }
    }

    pub(crate) fn is_building(&self, space: &str, index: &str) -> bool {
        let building = lock(&self.building);
        !building.is_empty() && building.contains(&(space.to_owned(), index.to_owned()))
    }

    /// Cleans up builds interrupted by a crash, then restores persisted statistics.
    ///
    /// An interrupted build leaves a partially filled index; it is removed here, before any
    /// query can use it, and will be proposed again if the workload still needs it.
    pub(crate) fn initialize(&self, env: &IndexEnv<'_>) -> Result<()> {
        let interrupted: Vec<IndexRow> = {
            let reader = env.resources.read_lock(env.lifecycle)?;
            read_rows(&reader)?
                .into_iter()
                .filter(|row| row.phase == IndexPhase::Building)
                .collect()
        };
        if !interrupted.is_empty() {
            let mut writer = env.resources.lock_writer(env.lifecycle, None)?;
            let tx = writer.transaction()?;
            for row in &interrupted {
                remove_definition(&tx, &env.schemas(), &row.space, &row.name)?;
                delete_row(&tx, &row.space, &row.name)?;
            }
            tx.commit()?;
        }
        if self.options.enabled {
            self.seed(env)?;
        }
        Ok(())
    }

    fn seed(&self, env: &IndexEnv<'_>) -> Result<()> {
        let reader = env.resources.read_lock(env.lifecycle)?;
        let mut statement = reader.prepare(
            "SELECT space, fields, executions, scans, slow_scans, scan_rows_examined, rows_examined, \
             rows_returned, total_duration_us, max_duration_us, first_seen_ms, last_seen_ms \
             FROM _lilia_autotune_query_stats ORDER BY last_seen_ms DESC LIMIT ?1;",
        )?;
        statement.bind_int64(1, to_i64(self.options.max_tracked_queries as u64))?;
        while statement.step()? {
            let Ok(fields) = serde_json::from_str::<Vec<String>>(statement.column_text(1)) else {
                continue;
            };
            self.collector.seed_query(
                QueryFingerprint {
                    space: statement.column_text(0).to_owned(),
                    fields,
                },
                QueryStats {
                    executions: from_i64(statement.column_int64(2)),
                    scans: from_i64(statement.column_int64(3)),
                    slow_scans: from_i64(statement.column_int64(4)),
                    scan_rows_examined: from_i64(statement.column_int64(5)),
                    rows_examined: from_i64(statement.column_int64(6)),
                    rows_returned: from_i64(statement.column_int64(7)),
                    total_duration_us: from_i64(statement.column_int64(8)),
                    max_duration_us: from_i64(statement.column_int64(9)),
                    first_seen_ms: statement.column_int64(10),
                    last_seen_ms: statement.column_int64(11),
                },
            );
        }
        drop(statement);
        for row in read_rows(&reader)? {
            if let Some(last_used_ms) = row.last_used_ms {
                self.collector.seed_index_lookups(
                    &row.space,
                    &row.name,
                    IndexLookups {
                        lookups: row.lookups,
                        last_used_ms,
                    },
                );
            }
        }
        Ok(())
    }

    /// Joins live index definitions with the tuner's bookkeeping. Pure: returns the bookkeeping
    /// changes needed instead of writing them.
    fn merge(
        &self,
        definitions: &[Definition],
        rows: &[IndexRow],
        now: i64,
    ) -> (Vec<TrackedIndex>, RowChanges) {
        let building = lock(&self.building).clone();
        let usage = self.collector.index_lookup_snapshot();
        let rows_by_key: HashMap<(String, String), &IndexRow> =
            rows.iter().map(|row| (row.key(), row)).collect();
        let live: HashSet<(String, String)> = definitions
            .iter()
            .map(|definition| (definition.space.clone(), definition.name.clone()))
            .collect();
        let cooldown = to_i64(self.options.recreate_cooldown_ms);

        let mut changes = RowChanges::default();
        let mut tracked = Vec::with_capacity(definitions.len());

        for definition in definitions {
            let key = (definition.space.clone(), definition.name.clone());
            let row = match rows_by_key.get(&key) {
                Some(row) if row.phase != IndexPhase::Dropped => (*row).clone(),
                _ => {
                    let adopted = IndexRow {
                        space: definition.space.clone(),
                        name: definition.name.clone(),
                        origin: IndexOrigin::Manual,
                        phase: IndexPhase::Ready,
                        first_seen_ms: now,
                        last_used_ms: None,
                        lookups: 0,
                        changed_ms: now,
                    };
                    changes.upserts.push(adopted.clone());
                    adopted
                }
            };
            let observed = usage.get(&key);
            tracked.push(TrackedIndex {
                space: row.space.clone(),
                index_name: row.name.clone(),
                expression: definition.expression.clone(),
                partial: definition.filter.is_some(),
                origin: row.origin,
                phase: row.phase,
                first_seen_ms: row.first_seen_ms,
                last_used_ms: observed
                    .map(|lookups| lookups.last_used_ms)
                    .into_iter()
                    .chain(row.last_used_ms)
                    .max(),
                lookups: observed
                    .map_or(0, |lookups| lookups.lookups)
                    .max(row.lookups),
                changed_ms: row.changed_ms,
            });
        }

        for row in rows {
            let key = row.key();
            if live.contains(&key) {
                continue;
            }
            let obsolete = match row.phase {
                IndexPhase::Ready => true,
                IndexPhase::Building => !building.contains(&key),
                IndexPhase::Dropped => now.saturating_sub(row.changed_ms) >= cooldown,
            };
            if obsolete {
                changes.deletes.push(key);
            } else if row.phase == IndexPhase::Dropped {
                tracked.push(TrackedIndex {
                    space: row.space.clone(),
                    index_name: row.name.clone(),
                    expression: String::new(),
                    partial: false,
                    origin: row.origin,
                    phase: IndexPhase::Dropped,
                    first_seen_ms: row.first_seen_ms,
                    last_used_ms: row.last_used_ms,
                    lookups: row.lookups,
                    changed_ms: row.changed_ms,
                });
            }
        }
        (tracked, changes)
    }

    /// Writes the bookkeeping changes and every statistic changed since the last flush.
    fn persist(&self, env: &IndexEnv<'_>, changes: &RowChanges, now: i64) -> Result<()> {
        let queries = self.collector.drain_dirty_queries();
        let lookups = self.collector.drain_dirty_index_lookups();
        if queries.is_empty()
            && lookups.is_empty()
            && changes.upserts.is_empty()
            && changes.deletes.is_empty()
        {
            return Ok(());
        }
        let written = (|| -> Result<()> {
            let mut writer = env.resources.lock_writer(env.lifecycle, None)?;
            let tx = writer.transaction()?;
            for (space, name) in &changes.deletes {
                delete_row(&tx, space, name)?;
                self.collector.forget_index(space, name);
            }
            for row in &changes.upserts {
                upsert_row(&tx, row)?;
            }
            for (fingerprint, stats) in &queries {
                upsert_query_stats(&tx, fingerprint, stats)?;
            }
            for (space, name, usage) in &lookups {
                update_lookups(&tx, space, name, *usage)?;
            }
            let mut purge =
                tx.prepare("DELETE FROM _lilia_autotune_query_stats WHERE last_seen_ms < ?1;")?;
            purge.bind_int64(1, now.saturating_sub(to_i64(self.options.prune_idle_ms)))?;
            purge.step()?;
            drop(purge);
            tx.commit()
        })();
        if written.is_err() {
            self.collector.mark_all_dirty();
        }
        written
    }

    /// Persists statistics now; used when the database is closing.
    pub(crate) fn flush(&self, env: &IndexEnv<'_>) -> Result<()> {
        if !self.options.enabled {
            return Ok(());
        }
        self.persist(env, &RowChanges::default(), now_ms())
    }

    /// Runs one tuning cycle: persist statistics, plan, then apply what the options allow.
    pub(crate) fn run_cycle(&self, env: &IndexEnv<'_>, now: i64) -> Result<CycleReport> {
        if !self.options.enabled {
            return Err(LiliaError::new(
                ErrorCode::Unsupported,
                "autotune is disabled for this database",
                false,
            ));
        }
        let Ok(_cycle) = self.cycle_guard.try_lock() else {
            return Err(LiliaError::new(
                ErrorCode::Busy,
                "an autotune cycle is already running",
                true,
            ));
        };

        let (definitions, rows) = snapshot_indexes(env)?;
        let (tracked, changes) = self.merge(&definitions, &rows, now);
        self.persist(env, &changes, now)?;

        let queries = self.collector.query_snapshot();
        let mut report = CycleReport {
            at_ms: now,
            ..CycleReport::default()
        };
        for recommendation in plan_autotune(&self.options, &queries, &tracked, now) {
            match &recommendation {
                Recommendation::CreateIndex {
                    space,
                    index_name,
                    expression,
                    fields,
                    ..
                } if self.options.auto_create => {
                    match self.build_index(env, space, index_name, expression, now) {
                        Ok(()) => {
                            self.collector.clear_scan_evidence(&QueryFingerprint {
                                space: space.clone(),
                                fields: fields.clone(),
                            });
                            report.created.push(IndexRef {
                                space: space.clone(),
                                index_name: index_name.clone(),
                            });
                        }
                        Err(error) => report
                            .failures
                            .push(format!("create {space}/{index_name}: {error}")),
                    }
                }
                Recommendation::DropIndex {
                    space, index_name, ..
                } if self.options.auto_prune => {
                    match self.drop_index(env, space, index_name, now) {
                        Ok(()) => report.dropped.push(IndexRef {
                            space: space.clone(),
                            index_name: index_name.clone(),
                        }),
                        Err(error) => report
                            .failures
                            .push(format!("drop {space}/{index_name}: {error}")),
                    }
                }
                _ => report.pending.push(recommendation),
            }
        }
        if let Err(error) = self.persist(env, &RowChanges::default(), now) {
            report.failures.push(format!("persist statistics: {error}"));
        }
        *lock(&self.last_cycle) = Some(report.clone());
        Ok(report)
    }

    /// Builds `index_name` over `space` without blocking writers for the whole backfill.
    ///
    /// The definition is created first so concurrent writes keep the index current; the backfill
    /// then runs in short writer transactions, and `json_find` ignores the index until it is ready.
    fn build_index(
        &self,
        env: &IndexEnv<'_>,
        space: &str,
        index_name: &str,
        expression: &str,
        now: i64,
    ) -> Result<()> {
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;
        if !env.is_disk_json(space) {
            return Err(LiliaError::new(
                ErrorCode::Unsupported,
                format!("collection '{space}' is not stored on disk"),
                false,
            ));
        }
        let compiled = lilia_core::parse_expr(expression)?;
        let key = (space.to_owned(), index_name.to_owned());
        lock(&self.building).insert(key.clone());

        let built = self.backfill(env, space, index_name, expression, &compiled, now);
        if built.is_err() {
            // Best effort: when the database is closing, the interrupted build is removed at the
            // next open instead.
            let _ = self.remove_index(env, space, index_name);
        }
        lock(&self.building).remove(&key);
        built
    }

    fn backfill(
        &self,
        env: &IndexEnv<'_>,
        space: &str,
        index_name: &str,
        expression: &str,
        compiled: &lilia_core::Expr,
        now: i64,
    ) -> Result<()> {
        let schemas = env.schemas();
        register_building_index(env, &schemas, space, index_name, expression, now)?;

        let chunk = i64::try_from(self.options.backfill_chunk_docs).unwrap_or(i64::MAX);
        for schema in &schemas {
            let docs = qualified(schema.as_deref(), DOCS_TABLE);
            let entries = qualified(schema.as_deref(), ENTRIES_TABLE);
            let backfill = Backfill {
                env,
                docs,
                entries,
                space,
                index_name,
                compiled,
                chunk,
            };
            let mut cursor = String::new();
            while let Some(next) = backfill.next_chunk(&cursor)? {
                cursor = next;
            }
        }

        let mut writer = env.resources.lock_writer(env.lifecycle, None)?;
        let tx = writer.transaction()?;
        let mut ready = tx.prepare(
            "UPDATE _lilia_autotune_indexes SET phase = 'ready', changed_ms = ?3 WHERE space = ?1 AND index_name = ?2;",
        )?;
        ready.bind_text_static(1, space)?;
        ready.bind_text_static(2, index_name)?;
        ready.bind_int64(3, now_ms())?;
        ready.step()?;
        drop(ready);
        tx.commit()
    }

    /// Removes an index definition and its entries, and forgets its bookkeeping.
    fn remove_index(&self, env: &IndexEnv<'_>, space: &str, index_name: &str) -> Result<bool> {
        let mut writer = env.resources.lock_writer(env.lifecycle, None)?;
        let tx = writer.transaction()?;
        let removed = remove_definition(&tx, &env.schemas(), space, index_name)?;
        delete_row(&tx, space, index_name)?;
        tx.commit()?;
        self.collector.forget_index(space, index_name);
        Ok(removed)
    }

    fn drop_index(
        &self,
        env: &IndexEnv<'_>,
        space: &str,
        index_name: &str,
        now: i64,
    ) -> Result<()> {
        let mut writer = env.resources.lock_writer(env.lifecycle, None)?;
        let tx = writer.transaction()?;
        remove_definition(&tx, &env.schemas(), space, index_name)?;
        let previous = read_rows(tx.conn())?
            .into_iter()
            .find(|row| row.space == space && row.name == index_name);
        upsert_row(
            &tx,
            &IndexRow {
                space: space.to_owned(),
                name: index_name.to_owned(),
                origin: previous
                    .as_ref()
                    .map_or(IndexOrigin::Manual, |row| row.origin),
                phase: IndexPhase::Dropped,
                first_seen_ms: previous.as_ref().map_or(now, |row| row.first_seen_ms),
                last_used_ms: previous.as_ref().and_then(|row| row.last_used_ms),
                lookups: previous.as_ref().map_or(0, |row| row.lookups),
                changed_ms: now,
            },
        )?;
        tx.commit()?;
        self.collector.forget_index(space, index_name);
        Ok(())
    }

    /// Builds a read-only report of what was observed and what the planner would do.
    pub(crate) fn report(&self, env: &IndexEnv<'_>, now: i64) -> Result<AutotuneReport> {
        let mut report = AutotuneReport {
            enabled: self.options.enabled,
            generated_at_ms: now,
            json_queries: Vec::new(),
            sql_queries: Vec::new(),
            indexes: Vec::new(),
            index_table_bytes: 0,
            recommendations: Vec::new(),
            last_cycle: lock(&self.last_cycle).clone(),
        };
        if !self.options.enabled {
            return Ok(report);
        }

        let (definitions, rows) = snapshot_indexes(env)?;
        let (tracked, _) = self.merge(&definitions, &rows, now);
        let queries = self.collector.query_snapshot();
        report.recommendations = plan_autotune(&self.options, &queries, &tracked, now);
        report.json_queries = queries
            .into_iter()
            .map(|(fingerprint, stats)| JsonQueryReport {
                space: fingerprint.space,
                fields: fingerprint.fields,
                stats,
            })
            .collect();
        report.sql_queries = self
            .collector
            .sql_snapshot()
            .into_iter()
            .map(|(sql, stats)| SqlQueryReport { sql, stats })
            .collect();

        let (footprints, table_bytes) = read_footprints(env)?;
        report.index_table_bytes = table_bytes;

        let idle_window = to_i64(self.options.prune_idle_ms);
        report.indexes = tracked
            .into_iter()
            .filter(|index| index.phase != IndexPhase::Dropped)
            .map(|index| {
                let (entries, approx_bytes) = footprints
                    .get(&(index.space.clone(), index.index_name.clone()))
                    .copied()
                    .unwrap_or_default();
                let reference = index
                    .last_used_ms
                    .unwrap_or(i64::MIN)
                    .max(index.first_seen_ms);
                IndexUsageView {
                    zombie: index.phase == IndexPhase::Ready
                        && now.saturating_sub(reference) >= idle_window,
                    space: index.space,
                    index_name: index.index_name,
                    expression: index.expression,
                    origin: index.origin,
                    phase: index.phase,
                    entries,
                    approx_bytes,
                    lookups: index.lookups,
                    last_used_ms: index.last_used_ms,
                    first_seen_ms: index.first_seen_ms,
                }
            })
            .collect();
        report
            .indexes
            .sort_by(|a, b| (&a.space, &a.index_name).cmp(&(&b.space, &b.index_name)));
        Ok(report)
    }

    /// Forgets every recorded statistic, in memory and on disk. Index lookup counters restart
    /// and the idle window of every index begins now.
    pub(crate) fn reset(&self, env: &IndexEnv<'_>, now: i64) -> Result<()> {
        let mut writer = env.resources.lock_writer(env.lifecycle, None)?;
        let tx = writer.transaction()?;
        tx.conn().exec("DELETE FROM _lilia_autotune_query_stats;")?;
        let mut restart = tx.prepare(
            "UPDATE _lilia_autotune_indexes SET lookups = 0, last_used_ms = NULL, first_seen_ms = ?1 \
             WHERE phase != 'dropped';",
        )?;
        restart.bind_int64(1, now)?;
        restart.step()?;
        drop(restart);
        tx.commit()?;
        self.collector.reset();
        Ok(())
    }
}

fn snapshot_indexes(env: &IndexEnv<'_>) -> Result<(Vec<Definition>, Vec<IndexRow>)> {
    let reader = env.resources.read_lock(env.lifecycle)?;
    let definitions = read_definitions(&reader)?
        .into_iter()
        .filter(|definition| env.is_disk_json(&definition.space))
        .collect();
    Ok((definitions, read_rows(&reader)?))
}

/// Creates the index definition and its `building` bookkeeping row, clearing stale entries.
fn register_building_index(
    env: &IndexEnv<'_>,
    schemas: &[Option<String>],
    space: &str,
    index_name: &str,
    expression: &str,
    now: i64,
) -> Result<()> {
    let mut writer = env.resources.lock_writer(env.lifecycle, None)?;
    let tx = writer.transaction()?;
    let mut upsert = tx.prepare(
        "INSERT INTO _lilia_json_indexes(space, index_name, json_path, filter_predicate) VALUES(?1, ?2, ?3, NULL) \
         ON CONFLICT(space, index_name) DO UPDATE SET json_path = excluded.json_path, filter_predicate = NULL;",
    )?;
    upsert.bind_text_static(1, space)?;
    upsert.bind_text_static(2, index_name)?;
    upsert.bind_text_static(3, expression)?;
    upsert.step()?;
    drop(upsert);
    for schema in schemas {
        let entries = qualified(schema.as_deref(), ENTRIES_TABLE);
        let mut clear = tx.prepare(&format!(
            "DELETE FROM {entries} WHERE space = ?1 AND index_name = ?2;"
        ))?;
        clear.bind_text_static(1, space)?;
        clear.bind_text_static(2, index_name)?;
        clear.step()?;
    }
    upsert_row(
        &tx,
        &IndexRow {
            space: space.to_owned(),
            name: index_name.to_owned(),
            origin: IndexOrigin::Auto,
            phase: IndexPhase::Building,
            first_seen_ms: now,
            last_used_ms: None,
            lookups: 0,
            changed_ms: now,
        },
    )?;
    tx.commit()
}

/// One schema's share of an index backfill, processed in bounded writer transactions.
#[derive(Debug)]
struct Backfill<'a> {
    env: &'a IndexEnv<'a>,
    docs: String,
    entries: String,
    space: &'a str,
    index_name: &'a str,
    compiled: &'a lilia_core::Expr,
    chunk: i64,
}

impl Backfill<'_> {
    /// Indexes the documents after `cursor` (at most `chunk`) in one writer transaction.
    /// Returns the cursor of the next chunk, or `None` once the schema is exhausted.
    fn next_chunk(&self, cursor: &str) -> Result<Option<String>> {
        let (docs, entries) = (&self.docs, &self.entries);
        let mut writer = self.env.resources.lock_writer(self.env.lifecycle, None)?;
        let tx = writer.transaction()?;
        let mut select = tx.prepare(&format!(
            "SELECT id, value, format, dict_id FROM {docs} WHERE space = ?1 AND id > ?2 ORDER BY id LIMIT ?3;"
        ))?;
        select.bind_text_static(1, self.space)?;
        select.bind_text_static(2, cursor)?;
        select.bind_int64(3, self.chunk)?;
        let mut batch = Vec::new();
        while select.step()? {
            let doc = decode_stored_doc(
                &select,
                1,
                select.column_int64(2),
                select.column_int64(3),
                self.space,
                self.env.codec,
                self.env.dict_registry,
                None,
            )?;
            batch.push((select.column_text(0).to_owned(), doc));
        }
        drop(select);

        let mut insert = tx.prepare(&format!(
            "INSERT OR IGNORE INTO {entries}(space, index_name, idx_value, doc_id) VALUES(?1, ?2, ?3, ?4);"
        ))?;
        for (doc_id, doc) in &batch {
            for key in index_keys(self.compiled, None, doc) {
                insert.reset()?;
                insert.bind_text_static(1, self.space)?;
                insert.bind_text_static(2, self.index_name)?;
                insert.bind_text_static(3, &key)?;
                insert.bind_text_static(4, doc_id)?;
                insert.step()?;
            }
        }
        drop(insert);
        tx.commit()?;

        let full = i64::try_from(batch.len()).unwrap_or(i64::MAX) >= self.chunk;
        Ok(batch.pop().filter(|_| full).map(|(last_id, _)| last_id))
    }
}

/// Entry count and approximate bytes, keyed by `(space, index_name)`.
type Footprints = HashMap<(String, String), (u64, u64)>;

/// Entries and approximate bytes per index across every data schema, plus the payload bytes of
/// the shared index table as reported by `dbstat`.
fn read_footprints(env: &IndexEnv<'_>) -> Result<(Footprints, u64)> {
    let reader = env.resources.read_lock(env.lifecycle)?;
    let mut footprints = Footprints::new();
    for schema in env.schemas() {
        let entries = qualified(schema.as_deref(), ENTRIES_TABLE);
        let mut statement = reader.prepare(&format!(
            "SELECT space, index_name, count(*), COALESCE(sum(length(idx_value) + length(doc_id)), 0) \
             FROM {entries} GROUP BY space, index_name;"
        ))?;
        while statement.step()? {
            let slot = footprints
                .entry((
                    statement.column_text(0).to_owned(),
                    statement.column_text(1).to_owned(),
                ))
                .or_default();
            slot.0 += from_i64(statement.column_int64(2));
            slot.1 += from_i64(statement.column_int64(3));
        }
    }
    // `dbstat` is an optional virtual table: its absence only blanks this figure.
    let table_bytes = reader
        .prepare("SELECT COALESCE(sum(payload), 0) FROM dbstat WHERE name = '_lilia_json_index_entries';")
        .ok()
        .and_then(|mut statement| {
            statement
                .step()
                .ok()
                .filter(|has_row| *has_row)
                .map(|_| from_i64(statement.column_int64(0)))
        })
        .unwrap_or(0);
    Ok((footprints, table_bytes))
}

/// Deletes an index definition and its entries in every data schema. Returns whether a
/// definition existed.
fn remove_definition(
    tx: &NativeTransaction<'_>,
    schemas: &[Option<String>],
    space: &str,
    index_name: &str,
) -> Result<bool> {
    let mut definition =
        tx.prepare("DELETE FROM _lilia_json_indexes WHERE space = ?1 AND index_name = ?2;")?;
    definition.bind_text_static(1, space)?;
    definition.bind_text_static(2, index_name)?;
    definition.step()?;
    drop(definition);
    let removed = tx.conn().changes() > 0;
    for schema in schemas {
        let entries = qualified(schema.as_deref(), ENTRIES_TABLE);
        let mut statement = tx.prepare(&format!(
            "DELETE FROM {entries} WHERE space = ?1 AND index_name = ?2;"
        ))?;
        statement.bind_text_static(1, space)?;
        statement.bind_text_static(2, index_name)?;
        statement.step()?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, DatabaseOptions};
    use lilia_core::BatchOperation;
    use serde_json::json;

    fn find_paid(db: &Database) -> usize {
        let query = json!({ "status": "paid" });
        db.json_find("orders", query.as_object().unwrap(), None, 100)
            .unwrap()
            .len()
    }

    #[test]
    fn indexes_under_construction_are_invisible_to_find() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(DatabaseOptions::durable(dir.path().join("db.lilia"))).unwrap();
        let batch: Vec<BatchOperation> = (0..8)
            .map(|i| BatchOperation::JsonPut {
                space: "orders".into(),
                id: format!("order_{i}"),
                value: json!({ "status": if i % 2 == 0 { "paid" } else { "pending" } }),
                if_version: None,
            })
            .collect();
        db.batch(&batch).unwrap();
        db.create_json_index("orders", "idx_status", "$.status")
            .unwrap();
        assert_eq!(find_paid(&db), 4);

        // Simulate a half-built index: definition present, entries missing.
        db.sql_query("DELETE FROM _lilia_json_index_entries", &[])
            .unwrap();
        assert_eq!(
            find_paid(&db),
            0,
            "an empty index answers from its (empty) entries"
        );

        lock(&db.autotune.building).insert(("orders".to_owned(), "idx_status".to_owned()));
        assert!(db.autotune.is_building("orders", "idx_status"));
        assert_eq!(
            find_paid(&db),
            4,
            "while building, find falls back to a scan"
        );

        lock(&db.autotune.building).clear();
        assert!(!db.autotune.is_building("orders", "idx_status"));
    }
}
