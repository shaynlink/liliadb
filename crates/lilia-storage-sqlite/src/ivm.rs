use lilia_core::{ErrorCode, LiliaError, MaterializedViewInfo, Result};
use lilia_sqlite_native::NativeConnection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateKind {
    CountAll,
    CountCol(String),
    Sum(String),
    Avg(String),
    Min(String),
    Max(String),
}

#[derive(Debug, Clone)]
pub struct AggregateSpec {
    pub alias: String,
    pub kind: AggregateKind,
}

#[derive(Debug, Clone)]
pub struct ParsedIvmQuery {
    pub source_table: String,
    pub group_by: Vec<String>,
    pub aggregates: Vec<AggregateSpec>,
    pub where_clause: Option<String>,
}

/// # Errors
/// Propagates errors from the operations it performs.
pub fn ensure_ivm_catalog(conn: &NativeConnection) -> Result<()> {
    conn.exec(
        "CREATE TABLE IF NOT EXISTS _lilia_ivm_views (
            name TEXT PRIMARY KEY,
            source_table TEXT NOT NULL,
            query TEXT NOT NULL,
            storage_table TEXT NOT NULL,
            group_by TEXT,
            created_at_ms INTEGER NOT NULL
        );",
    )
}

/// # Errors
/// Returns `InvalidInput` errors detected here, and propagates errors from the
/// operations it performs.
#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
pub fn create_materialized_view(conn: &NativeConnection, name: &str, query: &str) -> Result<()> {
    ensure_ivm_catalog(conn)?;

    let clean_name = name.trim().trim_matches('"').trim_matches('`');
    if clean_name.is_empty()
        || !clean_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid materialized view name '{name}'"),
            false,
        ));
    }

    let parsed = parse_ivm_query(query)?;
    let storage_table = format!("_lilia_ivm_{clean_name}");
    let trg_insert = format!("_lilia_ivm_trg_{clean_name}_ai");
    let trg_delete = format!("_lilia_ivm_trg_{clean_name}_ad");
    let trg_update = format!("_lilia_ivm_trg_{clean_name}_au");

    let is_grouped = !parsed.group_by.is_empty();

    // 1. Generate CREATE TABLE for storage table
    let create_table_sql = if is_grouped {
        let mut col_defs = Vec::new();
        for g in &parsed.group_by {
            col_defs.push(format!("\"{g}\" TEXT NOT NULL"));
        }
        for agg in &parsed.aggregates {
            match &agg.kind {
                AggregateKind::CountAll | AggregateKind::CountCol(_) => {
                    col_defs.push(format!("\"{}\" INTEGER NOT NULL DEFAULT 0", agg.alias));
                }
                AggregateKind::Sum(_) | AggregateKind::Min(_) | AggregateKind::Max(_) => {
                    col_defs.push(format!("\"{}\" REAL NOT NULL DEFAULT 0", agg.alias));
                }
                AggregateKind::Avg(_) => {
                    col_defs.push(format!("\"_sum_{}\" REAL NOT NULL DEFAULT 0", agg.alias));
                    col_defs.push(format!(
                        "\"_count_{}\" INTEGER NOT NULL DEFAULT 0",
                        agg.alias
                    ));
                }
            }
        }
        let pk_cols = parsed
            .group_by
            .iter()
            .map(|g| format!("\"{g}\""))
            .collect::<Vec<_>>()
            .join(", ");
        col_defs.push(format!("PRIMARY KEY ({pk_cols})"));
        format!(
            "CREATE TABLE IF NOT EXISTS \"{storage_table}\" ({});",
            col_defs.join(", ")
        )
    } else {
        let mut col_defs = vec![
            "\"_singleton\" INTEGER PRIMARY KEY CHECK (\"_singleton\" = 1) DEFAULT 1".to_string(),
        ];
        for agg in &parsed.aggregates {
            match &agg.kind {
                AggregateKind::CountAll | AggregateKind::CountCol(_) => {
                    col_defs.push(format!("\"{}\" INTEGER NOT NULL DEFAULT 0", agg.alias));
                }
                AggregateKind::Sum(_) | AggregateKind::Min(_) | AggregateKind::Max(_) => {
                    col_defs.push(format!("\"{}\" REAL NOT NULL DEFAULT 0", agg.alias));
                }
                AggregateKind::Avg(_) => {
                    col_defs.push(format!("\"_sum_{}\" REAL NOT NULL DEFAULT 0", agg.alias));
                    col_defs.push(format!(
                        "\"_count_{}\" INTEGER NOT NULL DEFAULT 0",
                        agg.alias
                    ));
                }
            }
        }
        format!(
            "CREATE TABLE IF NOT EXISTS \"{storage_table}\" ({});",
            col_defs.join(", ")
        )
    };

    // 2. Generate CREATE VIEW for user-facing view
    let create_view_sql = {
        let mut select_items = Vec::new();
        for g in &parsed.group_by {
            select_items.push(format!("\"{g}\""));
        }
        for agg in &parsed.aggregates {
            match &agg.kind {
                AggregateKind::CountAll
                | AggregateKind::CountCol(_)
                | AggregateKind::Sum(_)
                | AggregateKind::Min(_)
                | AggregateKind::Max(_) => {
                    select_items.push(format!("\"{}\"", agg.alias));
                }
                AggregateKind::Avg(_) => {
                    select_items.push(format!(
                        "CASE WHEN \"_count_{0}\" > 0 THEN \"_sum_{0}\" / CAST(\"_count_{0}\" AS REAL) ELSE NULL END AS \"{0}\"",
                        agg.alias
                    ));
                }
            }
        }
        format!(
            "CREATE VIEW IF NOT EXISTS \"{clean_name}\" AS SELECT {} FROM \"{storage_table}\";",
            select_items.join(", ")
        )
    };

    // 3. Triggers for $O(1)$ IVM maintenance
    let (insert_trigger, delete_trigger, update_trigger) =
        generate_triggers(clean_name, &storage_table, &parsed);

    // Initial populate query
    let initial_populate_sql = generate_initial_populate_sql(&storage_table, &parsed);

    // Execute in a transaction
    conn.exec("BEGIN IMMEDIATE;")?;

    let setup_result = (|| -> Result<()> {
        conn.exec(&format!("DROP TRIGGER IF EXISTS \"{trg_insert}\";"))?;
        conn.exec(&format!("DROP TRIGGER IF EXISTS \"{trg_delete}\";"))?;
        conn.exec(&format!("DROP TRIGGER IF EXISTS \"{trg_update}\";"))?;
        conn.exec(&format!("DROP VIEW IF EXISTS \"{clean_name}\";"))?;
        conn.exec(&format!("DROP TABLE IF EXISTS \"{storage_table}\";"))?;

        conn.exec(&create_table_sql)?;
        if !is_grouped {
            conn.exec(&format!(
                "INSERT OR IGNORE INTO \"{storage_table}\" (\"_singleton\") VALUES (1);"
            ))?;
        }

        conn.exec(&initial_populate_sql)?;
        conn.exec(&create_view_sql)?;

        conn.exec(&insert_trigger)?;
        conn.exec(&delete_trigger)?;
        conn.exec(&update_trigger)?;

        let now_ms = crate::storage::now_ms();
        let group_by_str = if is_grouped {
            Some(parsed.group_by.join(", "))
        } else {
            None
        };

        let mut ins_cat = conn.prepare(
            "INSERT OR REPLACE INTO _lilia_ivm_views (name, source_table, query, storage_table, group_by, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6);"
        )?;
        ins_cat.bind_text_static(1, clean_name)?;
        ins_cat.bind_text_static(2, &parsed.source_table)?;
        ins_cat.bind_text_static(3, query)?;
        ins_cat.bind_text_static(4, &storage_table)?;
        if let Some(gb) = &group_by_str {
            ins_cat.bind_text_static(5, gb)?;
        } else {
            ins_cat.bind_null(5)?;
        }
        ins_cat.bind_int64(6, now_ms)?;
        ins_cat.step()?;

        Ok(())
    })();

    if setup_result.is_ok() {
        conn.exec("COMMIT;")?;
        Ok(())
    } else {
        let _ = conn.exec("ROLLBACK;");
        setup_result
    }
}

/// # Errors
/// Propagates errors from the operations it performs.
pub fn drop_materialized_view(conn: &NativeConnection, name: &str) -> Result<()> {
    ensure_ivm_catalog(conn)?;
    let clean_name = name.trim().trim_matches('"').trim_matches('`');

    let storage_table = format!("_lilia_ivm_{clean_name}");
    let trg_insert = format!("_lilia_ivm_trg_{clean_name}_ai");
    let trg_delete = format!("_lilia_ivm_trg_{clean_name}_ad");
    let trg_update = format!("_lilia_ivm_trg_{clean_name}_au");

    conn.exec("BEGIN IMMEDIATE;")?;
    let _ = conn.exec(&format!("DROP TRIGGER IF EXISTS \"{trg_insert}\";"));
    let _ = conn.exec(&format!("DROP TRIGGER IF EXISTS \"{trg_delete}\";"));
    let _ = conn.exec(&format!("DROP TRIGGER IF EXISTS \"{trg_update}\";"));
    let _ = conn.exec(&format!("DROP VIEW IF EXISTS \"{clean_name}\";"));
    let _ = conn.exec(&format!("DROP TABLE IF EXISTS \"{storage_table}\";"));

    if let Ok(mut del_cat) = conn.prepare("DELETE FROM _lilia_ivm_views WHERE name = ?1;") {
        let _ = del_cat.bind_text_static(1, clean_name);
        let _ = del_cat.step();
    }

    conn.exec("COMMIT;")?;
    Ok(())
}

/// # Errors
/// Returns `NotFound` errors detected here, and propagates errors from the
/// operations it performs.
pub fn refresh_materialized_view(conn: &NativeConnection, name: &str) -> Result<()> {
    ensure_ivm_catalog(conn)?;
    let clean_name = name.trim().trim_matches('"').trim_matches('`');

    let mut stmt = conn.prepare("SELECT query FROM _lilia_ivm_views WHERE name = ?1;")?;
    stmt.bind_text_static(1, clean_name)?;
    if !stmt.step()? {
        return Err(LiliaError::new(
            ErrorCode::NotFound,
            format!("materialized view '{clean_name}' does not exist"),
            false,
        ));
    }
    let query = stmt.column_text(0).to_string();
    drop(stmt);

    create_materialized_view(conn, clean_name, &query)
}

/// # Errors
/// Propagates errors from the operations it performs.
pub fn list_materialized_views(conn: &NativeConnection) -> Result<Vec<MaterializedViewInfo>> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT name, source_table, query, storage_table, group_by, created_at_ms \
         FROM _lilia_ivm_views ORDER BY name ASC;",
    ) else {
        return Ok(Vec::new());
    };

    let mut list = Vec::new();
    while stmt.step()? {
        let name = stmt.column_text(0).to_string();
        let source_table = stmt.column_text(1).to_string();
        let query = stmt.column_text(2).to_string();
        let storage_table = stmt.column_text(3).to_string();
        let group_by = if stmt.column_type(4) == lilia_sqlite_native::ffi::SQLITE_NULL {
            None
        } else {
            Some(stmt.column_text(4).to_string())
        };
        let created_at_ms = stmt.column_int64(5);

        list.push(MaterializedViewInfo {
            name,
            source_table,
            query,
            storage_table,
            group_by,
            created_at_ms,
        });
    }

    Ok(list)
}

fn generate_initial_populate_sql(storage_table: &str, parsed: &ParsedIvmQuery) -> String {
    let is_grouped = !parsed.group_by.is_empty();

    let mut target_cols = Vec::new();
    let mut select_exprs = Vec::new();

    if is_grouped {
        for g in &parsed.group_by {
            target_cols.push(format!("\"{g}\""));
            select_exprs.push(format!("\"{g}\""));
        }
    } else {
        target_cols.push("\"_singleton\"".to_string());
        select_exprs.push("1".to_string());
    }

    for agg in &parsed.aggregates {
        match &agg.kind {
            AggregateKind::CountAll => {
                target_cols.push(format!("\"{}\"", agg.alias));
                select_exprs.push("COUNT(*)".to_string());
            }
            AggregateKind::CountCol(col) => {
                target_cols.push(format!("\"{}\"", agg.alias));
                select_exprs.push(format!("COUNT(\"{col}\")"));
            }
            AggregateKind::Sum(col) => {
                target_cols.push(format!("\"{}\"", agg.alias));
                select_exprs.push(format!("COALESCE(SUM(\"{col}\"), 0)"));
            }
            AggregateKind::Min(col) => {
                target_cols.push(format!("\"{}\"", agg.alias));
                select_exprs.push(format!("MIN(\"{col}\")"));
            }
            AggregateKind::Max(col) => {
                target_cols.push(format!("\"{}\"", agg.alias));
                select_exprs.push(format!("MAX(\"{col}\")"));
            }
            AggregateKind::Avg(col) => {
                target_cols.push(format!("\"_sum_{}\"", agg.alias));
                select_exprs.push(format!("COALESCE(SUM(\"{col}\"), 0)"));
                target_cols.push(format!("\"_count_{}\"", agg.alias));
                select_exprs.push(format!("COUNT(\"{col}\")"));
            }
        }
    }

    let where_part = parsed
        .where_clause
        .as_ref()
        .map_or_default(|w| format!("WHERE {w}"));

    let group_part = if is_grouped {
        format!(
            "GROUP BY {}",
            parsed
                .group_by
                .iter()
                .map(|g| format!("\"{g}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };

    format!(
        "INSERT OR REPLACE INTO \"{storage_table}\" ({}) SELECT {} FROM \"{}\" {where_part} {group_part};",
        target_cols.join(", "),
        select_exprs.join(", "),
        parsed.source_table
    )
}

#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
fn generate_triggers(
    view_name: &str,
    storage_table: &str,
    parsed: &ParsedIvmQuery,
) -> (String, String, String) {
    let is_grouped = !parsed.group_by.is_empty();
    let source = &parsed.source_table;

    if is_grouped {
        // Grouped triggers
        let pk_cols = parsed
            .group_by
            .iter()
            .map(|g| format!("\"{g}\""))
            .collect::<Vec<_>>()
            .join(", ");

        let _pk_vals_new = parsed
            .group_by
            .iter()
            .map(|g| format!("NEW.\"{g}\""))
            .collect::<Vec<_>>()
            .join(", ");

        // Build INSERT trigger
        let mut insert_target_cols = parsed
            .group_by
            .iter()
            .map(|g| format!("\"{g}\""))
            .collect::<Vec<_>>();
        let mut insert_vals = parsed
            .group_by
            .iter()
            .map(|g| format!("NEW.\"{g}\""))
            .collect::<Vec<_>>();
        let mut upsert_sets = Vec::new();

        for agg in &parsed.aggregates {
            match &agg.kind {
                AggregateKind::CountAll => {
                    insert_target_cols.push(format!("\"{}\"", agg.alias));
                    insert_vals.push("1".to_string());
                    upsert_sets.push(format!(
                        "\"{}\" = \"{storage_table}\".\"{}\" + 1",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::CountCol(col) => {
                    insert_target_cols.push(format!("\"{}\"", agg.alias));
                    insert_vals.push(format!(
                        "CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END"
                    ));
                    upsert_sets.push(format!(
                        "\"{}\" = \"{storage_table}\".\"{}\" + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Sum(col) => {
                    insert_target_cols.push(format!("\"{}\"", agg.alias));
                    insert_vals.push(format!("COALESCE(NEW.\"{col}\", 0)"));
                    upsert_sets.push(format!(
                        "\"{}\" = \"{storage_table}\".\"{}\" + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Avg(col) => {
                    insert_target_cols.push(format!("\"_sum_{}\"", agg.alias));
                    insert_vals.push(format!("COALESCE(NEW.\"{col}\", 0)"));
                    upsert_sets.push(format!(
                        "\"_sum_{}\" = \"{storage_table}\".\"_sum_{}\" + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));

                    insert_target_cols.push(format!("\"_count_{}\"", agg.alias));
                    insert_vals.push(format!(
                        "CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END"
                    ));
                    upsert_sets.push(format!(
                        "\"_count_{}\" = \"{storage_table}\".\"_count_{}\" + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Min(_) | AggregateKind::Max(_) => {}
            }
        }

        let insert_trigger = format!(
            "CREATE TRIGGER IF NOT EXISTS \"_lilia_ivm_trg_{view_name}_ai\" AFTER INSERT ON \"{source}\" \
             FOR EACH ROW BEGIN \
                 INSERT INTO \"{storage_table}\" ({}) VALUES ({}) \
                 ON CONFLICT ({pk_cols}) DO UPDATE SET {}; \
             END;",
            insert_target_cols.join(", "),
            insert_vals.join(", "),
            upsert_sets.join(", ")
        );

        // Build DELETE trigger
        let mut delete_updates = Vec::new();
        let where_old_match = parsed
            .group_by
            .iter()
            .map(|g| format!("\"{g}\" = OLD.\"{g}\""))
            .collect::<Vec<_>>()
            .join(" AND ");

        let count_col_name = parsed
            .aggregates
            .iter()
            .find_map(|agg| {
                if matches!(
                    agg.kind,
                    AggregateKind::CountAll | AggregateKind::CountCol(_)
                ) {
                    Some(agg.alias.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                parsed
                    .aggregates
                    .first()
                    .map_or_else(|| "count".into(), |a| a.alias.clone())
            });

        for agg in &parsed.aggregates {
            match &agg.kind {
                AggregateKind::CountAll => {
                    delete_updates.push(format!("\"{}\" = \"{}\" - 1", agg.alias, agg.alias));
                }
                AggregateKind::CountCol(col) => {
                    delete_updates.push(format!(
                        "\"{}\" = \"{}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Sum(col) => {
                    delete_updates.push(format!(
                        "\"{}\" = \"{}\" - COALESCE(OLD.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Avg(col) => {
                    delete_updates.push(format!(
                        "\"_sum_{}\" = \"_sum_{}\" - COALESCE(OLD.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                    delete_updates.push(format!(
                        "\"_count_{}\" = \"_count_{}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Min(_) | AggregateKind::Max(_) => {}
            }
        }

        let delete_trigger = format!(
            "CREATE TRIGGER IF NOT EXISTS \"_lilia_ivm_trg_{view_name}_ad\" AFTER DELETE ON \"{source}\" \
             FOR EACH ROW BEGIN \
                 UPDATE \"{storage_table}\" SET {} WHERE {where_old_match}; \
                 DELETE FROM \"{storage_table}\" WHERE {where_old_match} AND \"{count_col_name}\" <= 0; \
             END;",
            delete_updates.join(", ")
        );

        // Build UPDATE trigger (handles both intra-group update and inter-group transition)
        let same_group_cond = parsed
            .group_by
            .iter()
            .map(|g| format!("OLD.\"{g}\" = NEW.\"{g}\""))
            .collect::<Vec<_>>()
            .join(" AND ");

        let diff_group_cond = parsed
            .group_by
            .iter()
            .map(|g| format!("OLD.\"{g}\" != NEW.\"{g}\""))
            .collect::<Vec<_>>()
            .join(" OR ");

        let mut intra_updates = Vec::new();
        for agg in &parsed.aggregates {
            match &agg.kind {
                AggregateKind::Sum(col) => {
                    intra_updates.push(format!(
                        "\"{}\" = \"{}\" - COALESCE(OLD.\"{col}\", 0) + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Avg(col) => {
                    intra_updates.push(format!(
                        "\"_sum_{}\" = \"_sum_{}\" - COALESCE(OLD.\"{col}\", 0) + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                    intra_updates.push(format!(
                        "\"_count_{0}\" = \"_count_{0}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias
                    ));
                }
                AggregateKind::CountCol(col) => {
                    intra_updates.push(format!(
                        "\"{}\" = \"{}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::CountAll | AggregateKind::Min(_) | AggregateKind::Max(_) => {}
            }
        }

        let intra_update_stmt = if intra_updates.is_empty() {
            String::new()
        } else {
            let where_new_match = parsed
                .group_by
                .iter()
                .map(|g| format!("\"{g}\" = NEW.\"{g}\""))
                .collect::<Vec<_>>()
                .join(" AND ");
            format!("UPDATE \"{storage_table}\" SET {} WHERE {where_new_match} AND ({same_group_cond});", intra_updates.join(", "))
        };

        let update_trigger = format!(
            "CREATE TRIGGER IF NOT EXISTS \"_lilia_ivm_trg_{view_name}_au\" AFTER UPDATE ON \"{source}\" \
             FOR EACH ROW BEGIN \
                 {intra_update_stmt} \
                 UPDATE \"{storage_table}\" SET {} WHERE {where_old_match} AND ({diff_group_cond}); \
                 DELETE FROM \"{storage_table}\" WHERE {where_old_match} AND ({diff_group_cond}) AND \"{count_col_name}\" <= 0; \
                 INSERT INTO \"{storage_table}\" ({}) SELECT {} WHERE ({diff_group_cond}) \
                 ON CONFLICT ({pk_cols}) DO UPDATE SET {}; \
             END;",
            delete_updates.join(", "),
            insert_target_cols.join(", "),
            insert_vals.join(", "),
            upsert_sets.join(", ")
        );

        (insert_trigger, delete_trigger, update_trigger)
    } else {
        // Global singleton triggers
        let mut insert_updates = Vec::new();
        let mut delete_updates = Vec::new();
        let mut update_updates = Vec::new();

        for agg in &parsed.aggregates {
            match &agg.kind {
                AggregateKind::CountAll => {
                    insert_updates.push(format!("\"{}\" = \"{}\" + 1", agg.alias, agg.alias));
                    delete_updates.push(format!("\"{}\" = \"{}\" - 1", agg.alias, agg.alias));
                }
                AggregateKind::CountCol(col) => {
                    insert_updates.push(format!(
                        "\"{}\" = \"{}\" + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                    delete_updates.push(format!(
                        "\"{}\" = \"{}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                    update_updates.push(format!(
                        "\"{}\" = \"{}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Sum(col) => {
                    insert_updates.push(format!(
                        "\"{}\" = \"{}\" + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                    delete_updates.push(format!(
                        "\"{}\" = \"{}\" - COALESCE(OLD.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                    update_updates.push(format!(
                        "\"{}\" = \"{}\" - COALESCE(OLD.\"{col}\", 0) + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                }
                AggregateKind::Avg(col) => {
                    insert_updates.push(format!(
                        "\"_sum_{}\" = \"_sum_{}\" + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                    insert_updates.push(format!(
                        "\"_count_{}\" = \"_count_{}\" + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));

                    delete_updates.push(format!(
                        "\"_sum_{}\" = \"_sum_{}\" - COALESCE(OLD.\"{col}\", 0)",
                        agg.alias, agg.alias
                    ));
                    delete_updates.push(format!(
                        "\"_count_{}\" = \"_count_{}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias, agg.alias
                    ));

                    update_updates.push(format!(
                        "\"_sum_{0}\" = \"_sum_{0}\" - COALESCE(OLD.\"{col}\", 0) + COALESCE(NEW.\"{col}\", 0)",
                        agg.alias
                    ));
                    update_updates.push(format!(
                        "\"_count_{0}\" = \"_count_{0}\" - CASE WHEN OLD.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END + CASE WHEN NEW.\"{col}\" IS NOT NULL THEN 1 ELSE 0 END",
                        agg.alias
                    ));
                }
                AggregateKind::Min(_) | AggregateKind::Max(_) => {}
            }
        }

        let insert_trigger = format!(
            "CREATE TRIGGER IF NOT EXISTS \"_lilia_ivm_trg_{view_name}_ai\" AFTER INSERT ON \"{source}\" \
             FOR EACH ROW BEGIN \
                 UPDATE \"{storage_table}\" SET {} WHERE \"_singleton\" = 1; \
             END;",
            insert_updates.join(", ")
        );

        let delete_trigger = format!(
            "CREATE TRIGGER IF NOT EXISTS \"_lilia_ivm_trg_{view_name}_ad\" AFTER DELETE ON \"{source}\" \
             FOR EACH ROW BEGIN \
                 UPDATE \"{storage_table}\" SET {} WHERE \"_singleton\" = 1; \
             END;",
            delete_updates.join(", ")
        );

        let update_trigger = if update_updates.is_empty() {
            String::new()
        } else {
            format!(
                "CREATE TRIGGER IF NOT EXISTS \"_lilia_ivm_trg_{view_name}_au\" AFTER UPDATE ON \"{source}\" \
                 FOR EACH ROW BEGIN \
                     UPDATE \"{storage_table}\" SET {} WHERE \"_singleton\" = 1; \
                 END;",
                update_updates.join(", ")
            )
        };

        (insert_trigger, delete_trigger, update_trigger)
    }
}

/// # Errors
/// Returns `InvalidInput` errors detected here, and propagates errors from the
/// operations it performs.
#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
pub fn parse_ivm_query(query: &str) -> Result<ParsedIvmQuery> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();

    let select_pos = lower.find("select ").ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "materialized view query must be a SELECT statement",
            false,
        )
    })?;

    let from_pos = lower.find(" from ").ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "materialized view query must have a FROM clause",
            false,
        )
    })?;

    let select_list_str = trimmed[select_pos + 7..from_pos].trim();
    let after_from = &trimmed[from_pos + 6..];

    let group_by_pos = after_from.to_ascii_lowercase().find(" group by ");
    let where_pos = after_from.to_ascii_lowercase().find(" where ");

    let (source_table, where_clause, group_by_cols) = match (where_pos, group_by_pos) {
        (Some(wp), Some(gp)) => {
            let table = after_from[..wp].trim();
            let where_str = after_from[wp + 7..gp].trim();
            let group_str = after_from[gp + 10..].trim();
            (
                table,
                Some(where_str.to_string()),
                parse_ident_list(group_str),
            )
        }
        (Some(wp), None) => {
            let table = after_from[..wp].trim();
            let where_str = after_from[wp + 7..].trim();
            (table, Some(where_str.to_string()), Vec::new())
        }
        (None, Some(gp)) => {
            let table = after_from[..gp].trim();
            let group_str = after_from[gp + 10..].trim();
            (table, None, parse_ident_list(group_str))
        }
        (None, None) => {
            let table = after_from.split_whitespace().next().unwrap_or(after_from);
            (table, None, Vec::new())
        }
    };

    let source_table_clean = source_table.trim_matches('"').trim_matches('`').to_string();

    let mut aggregates = Vec::new();
    let items = split_sql_items(select_list_str);

    for item in items {
        let item_trim = item.trim();
        let item_lower = item_trim.to_ascii_lowercase();

        let (expr_part, alias) = if let Some(as_idx) = item_lower.rfind(" as ") {
            let expr = item_trim[..as_idx].trim();
            let al = item_trim[as_idx + 4..]
                .trim()
                .trim_matches('"')
                .trim_matches('`');
            (expr, al.to_string())
        } else {
            let last_word = item_trim.split_whitespace().last().unwrap_or(item_trim);
            (
                item_trim,
                last_word.trim_matches('"').trim_matches('`').to_string(),
            )
        };

        let expr_lower = expr_part.to_ascii_lowercase();
        if expr_lower.starts_with("count(") && expr_lower.ends_with(')') {
            let inside = expr_part[6..expr_part.len() - 1].trim();
            if inside == "*" || inside == "1" {
                aggregates.push(AggregateSpec {
                    alias,
                    kind: AggregateKind::CountAll,
                });
            } else {
                aggregates.push(AggregateSpec {
                    alias,
                    kind: AggregateKind::CountCol(
                        inside.trim_matches('"').trim_matches('`').to_string(),
                    ),
                });
            }
        } else if expr_lower.starts_with("sum(") && expr_lower.ends_with(')') {
            let inside = expr_part[4..expr_part.len() - 1].trim();
            aggregates.push(AggregateSpec {
                alias,
                kind: AggregateKind::Sum(inside.trim_matches('"').trim_matches('`').to_string()),
            });
        } else if expr_lower.starts_with("avg(") && expr_lower.ends_with(')') {
            let inside = expr_part[4..expr_part.len() - 1].trim();
            aggregates.push(AggregateSpec {
                alias,
                kind: AggregateKind::Avg(inside.trim_matches('"').trim_matches('`').to_string()),
            });
        } else if expr_lower.starts_with("min(") && expr_lower.ends_with(')') {
            let inside = expr_part[4..expr_part.len() - 1].trim();
            aggregates.push(AggregateSpec {
                alias,
                kind: AggregateKind::Min(inside.trim_matches('"').trim_matches('`').to_string()),
            });
        } else if expr_lower.starts_with("max(") && expr_lower.ends_with(')') {
            let inside = expr_part[4..expr_part.len() - 1].trim();
            aggregates.push(AggregateSpec {
                alias,
                kind: AggregateKind::Max(inside.trim_matches('"').trim_matches('`').to_string()),
            });
        } else {
            // It's a grouping column or passthrough
        }
    }

    if aggregates.is_empty() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "materialized view query must contain at least one aggregate function (COUNT, SUM, AVG, MIN, MAX)",
            false,
        ));
    }

    Ok(ParsedIvmQuery {
        source_table: source_table_clean,
        group_by: group_by_cols,
        aggregates,
        where_clause,
    })
}

fn split_sql_items(input: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth: usize = 0;

    for ch in input.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => {
                items.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        items.push(current.trim().to_string());
    }
    items
}

fn parse_ident_list(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('`').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
