use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lilia_core::{LiveQueryDiff, Result};
use lilia_sqlite_native::{ChangeOp, ChangesetIterator, NativeConnection, RowChange};

/// Callback invoked when a differential live query event occurs.
pub type LiveQueryCallback = Arc<dyn Fn(LiveQueryDiff) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    Like,
}

#[derive(Debug, Clone)]
pub enum ValueExpr {
    Column(String),
    JsonExtract { column: String, path: String },
    Literal(serde_json::Value),
}

#[derive(Debug, Clone)]
pub enum Predicate {
    Binary {
        left: ValueExpr,
        op: BinaryOp,
        right: ValueExpr,
    },
    InList {
        expr: ValueExpr,
        list: Vec<serde_json::Value>,
        negated: bool,
    },
    IsNull {
        expr: ValueExpr,
        negated: bool,
    },
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
    AlwaysTrue,
}

#[derive(Debug, Clone)]
pub struct ParsedLiveQuery {
    pub raw_sql: String,
    pub table: String,
    pub is_json_view: bool,
    pub json_space_filter: Option<String>,
    pub projections: Vec<String>,
    pub predicate: Predicate,
}

pub struct LiveSubscription {
    pub id: u64,
    pub query: ParsedLiveQuery,
    pub callback: LiveQueryCallback,
}

impl std::fmt::Debug for LiveSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSubscription")
            .field("id", &self.id)
            .field("query", &self.query)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct TableSchemaInfo {
    pub columns: Vec<String>,
    pub pk_indices: Vec<usize>,
}

pub struct LiveQueryManager {
    next_id: AtomicU64,
    subscriptions: RwLock<HashMap<u64, LiveSubscription>>,
    table_schemas: RwLock<HashMap<String, TableSchemaInfo>>,
}

impl std::fmt::Debug for LiveQueryManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveQueryManager")
            .field("subscriptions", &self.subscriber_count())
            .finish()
    }
}

impl Default for LiveQueryManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveQueryManager {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            subscriptions: RwLock::new(HashMap::new()),
            table_schemas: RwLock::new(HashMap::new()),
        }
    }

    pub fn has_subscribers(&self) -> bool {
        self.subscriptions
            .read()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscriptions.read().map(|s| s.len()).unwrap_or(0)
    }

    pub fn register(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        callback: LiveQueryCallback,
    ) -> Result<(u64, ParsedLiveQuery)> {
        let parsed = parse_live_query(sql, params)?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let sub = LiveSubscription {
            id,
            query: parsed.clone(),
            callback,
        };

        let mut subs = self
            .subscriptions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.insert(id, sub);

        Ok((id, parsed))
    }

    pub fn unregister(&self, id: u64) {
        let mut subs = self
            .subscriptions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.remove(&id);
    }

    pub fn process_changeset(&self, changeset: &[u8], conn: &NativeConnection) {
        if changeset.is_empty() || !self.has_subscribers() {
            return;
        }

        let Ok(mut iter) = ChangesetIterator::new(changeset) else {
            return;
        };

        let mut row_changes = Vec::new();
        while let Ok(Some(change)) = iter.next_change() {
            row_changes.push(change);
        }

        if row_changes.is_empty() {
            return;
        }

        let subs: Vec<LiveSubscription> = {
            let guard = self
                .subscriptions
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .values()
                .map(|s| LiveSubscription {
                    id: s.id,
                    query: s.query.clone(),
                    callback: Arc::clone(&s.callback),
                })
                .collect()
        };

        for sub in subs {
            let mut added = Vec::new();
            let mut updated = Vec::new();
            let mut removed = Vec::new();

            for change in &row_changes {
                let change_table = &change.table;
                let target_table = &sub.query.table;

                let matches_table = change_table.eq_ignore_ascii_case(target_table)
                    || (sub.query.is_json_view && change_table == "_lilia_json")
                    || (change_table.eq_ignore_ascii_case(&format!("_lilia_ivm_{target_table}")));

                if !matches_table {
                    continue;
                }

                let schema = self.get_or_load_schema(change_table, conn);
                if schema.columns.is_empty() {
                    continue;
                }

                let (old_row, new_row) = match change.op {
                    ChangeOp::Update => self.build_update_rows(change_table, &schema, change, conn),
                    ChangeOp::Insert => {
                        let row = build_row_map(&schema.columns, &change.new_values);
                        (None, row)
                    }
                    ChangeOp::Delete => {
                        let row = build_row_map(&schema.columns, &change.old_values);
                        (row, None)
                    }
                };

                if let Some(expected_space) = &sub.query.json_space_filter {
                    let old_space = old_row
                        .as_ref()
                        .and_then(|r| r.get("space"))
                        .and_then(|v| v.as_str());
                    let new_space = new_row
                        .as_ref()
                        .and_then(|r| r.get("space"))
                        .and_then(|v| v.as_str());
                    let old_matches_space = old_space == Some(expected_space.as_str());
                    let new_matches_space = new_space == Some(expected_space.as_str());

                    if !old_matches_space && !new_matches_space {
                        continue;
                    }
                }

                match change.op {
                    ChangeOp::Insert => {
                        if let Some(nr) = &new_row {
                            if eval_predicate(&sub.query.predicate, nr) {
                                added.push(project_row(nr, &sub.query.projections));
                            }
                        }
                    }
                    ChangeOp::Delete => {
                        if let Some(orr) = &old_row {
                            if eval_predicate(&sub.query.predicate, orr) {
                                removed.push(project_row(orr, &sub.query.projections));
                            }
                        }
                    }
                    ChangeOp::Update => {
                        let old_matches = old_row
                            .as_ref()
                            .is_some_and(|r| eval_predicate(&sub.query.predicate, r));
                        let new_matches = new_row
                            .as_ref()
                            .is_some_and(|r| eval_predicate(&sub.query.predicate, r));

                        match (old_matches, new_matches) {
                            (false, true) => {
                                if let Some(nr) = &new_row {
                                    added.push(project_row(nr, &sub.query.projections));
                                }
                            }
                            (true, true) => {
                                if let Some(nr) = &new_row {
                                    updated.push(project_row(nr, &sub.query.projections));
                                }
                            }
                            (true, false) => {
                                if let Some(orr) = &old_row {
                                    removed.push(project_row(orr, &sub.query.projections));
                                }
                            }
                            (false, false) => {}
                        }
                    }
                }
            }

            if !added.is_empty() || !updated.is_empty() || !removed.is_empty() {
                let diff = LiveQueryDiff {
                    subscription_id: sub.id,
                    added,
                    updated,
                    removed,
                    is_initial: false,
                };
                (sub.callback)(diff);
            }
        }
    }

    fn get_or_load_schema(&self, table: &str, conn: &NativeConnection) -> TableSchemaInfo {
        {
            let guard = self
                .table_schemas
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(info) = guard.get(table) {
                return info.clone();
            }
        }

        let pragma_sql = format!("PRAGMA table_info(\"{table}\");");
        let mut columns = Vec::new();
        let mut pk_indices = Vec::new();
        if let Ok(mut stmt) = conn.prepare(&pragma_sql) {
            let mut idx = 0;
            while let Ok(true) = stmt.step() {
                columns.push(stmt.column_text(1).to_string());
                if stmt.column_int64(5) > 0 {
                    pk_indices.push(idx);
                }
                idx += 1;
            }
        }

        let info = TableSchemaInfo {
            columns,
            pk_indices,
        };

        let mut guard = self
            .table_schemas
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(table.to_string(), info.clone());
        info
    }

    #[allow(clippy::type_complexity)]
    fn build_update_rows(
        &self,
        table: &str,
        schema: &TableSchemaInfo,
        change: &RowChange,
        conn: &NativeConnection,
    ) -> (
        Option<HashMap<String, serde_json::Value>>,
        Option<HashMap<String, serde_json::Value>>,
    ) {
        let has_missing_columns = schema.columns.iter().enumerate().any(|(i, _)| {
            change.old_values.get(i).is_none_or(Option::is_none)
                && change.new_values.get(i).is_none_or(Option::is_none)
        });

        let mut current_row_vals = Vec::new();
        if has_missing_columns && !schema.pk_indices.is_empty() {
            let mut where_clauses = Vec::new();
            let mut pk_values = Vec::new();
            for (pki, &idx) in schema.pk_indices.iter().enumerate() {
                let pk_col = &schema.columns[idx];
                where_clauses.push(format!("\"{pk_col}\" = ?{}", pki + 1));
                let val = change
                    .new_values
                    .get(idx)
                    .and_then(|v| v.as_ref())
                    .or_else(|| change.old_values.get(idx).and_then(|v| v.as_ref()))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                pk_values.push(val);
            }

            let query = format!(
                "SELECT * FROM \"{table}\" WHERE {} LIMIT 1;",
                where_clauses.join(" AND ")
            );

            if let Ok(mut stmt) = conn.prepare(&query) {
                let mut str_cache = Vec::new();
                for (idx, val) in pk_values.iter().enumerate() {
                    let col_idx = (idx + 1) as i32;
                    match val {
                        serde_json::Value::Null => {
                            let _ = stmt.bind_null(col_idx);
                        }
                        serde_json::Value::Bool(b) => {
                            let _ = stmt.bind_int64(col_idx, i64::from(*b));
                        }
                        serde_json::Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                let _ = stmt.bind_int64(col_idx, i);
                            } else if let Some(f) = n.as_f64() {
                                let _ = stmt.bind_double(col_idx, f);
                            } else {
                                str_cache.push(n.to_string());
                                let _ = stmt.bind_text_static(col_idx, str_cache.last().unwrap());
                            }
                        }
                        serde_json::Value::String(s) => {
                            let _ = stmt.bind_text_static(col_idx, s.as_str());
                        }
                        _ => {
                            str_cache.push(val.to_string());
                            let _ = stmt.bind_text_static(col_idx, str_cache.last().unwrap());
                        }
                    }
                }

                if let Ok(true) = stmt.step() {
                    for i in 0..schema.columns.len() {
                        let v = match stmt.column_type(i as i32) {
                            lilia_sqlite_native::ffi::SQLITE_INTEGER => {
                                serde_json::Value::Number(stmt.column_int64(i as i32).into())
                            }
                            lilia_sqlite_native::ffi::SQLITE_FLOAT => {
                                serde_json::Number::from_f64(stmt.column_double(i as i32))
                                    .map_or(serde_json::Value::Null, serde_json::Value::Number)
                            }
                            lilia_sqlite_native::ffi::SQLITE_TEXT => {
                                serde_json::Value::String(stmt.column_text(i as i32).to_string())
                            }
                            lilia_sqlite_native::ffi::SQLITE_BLOB => {
                                let b = stmt.column_blob(i as i32);
                                if let Ok(s) = std::str::from_utf8(b) {
                                    if let Ok(jv) = serde_json::from_str(s) {
                                        jv
                                    } else {
                                        serde_json::Value::String(s.to_string())
                                    }
                                } else {
                                    serde_json::Value::String(hex::encode(b))
                                }
                            }
                            _ => serde_json::Value::Null,
                        };
                        current_row_vals.push(v);
                    }
                }
            }
        }

        let mut old_map = HashMap::with_capacity(schema.columns.len());
        let mut new_map = HashMap::with_capacity(schema.columns.len());

        for (i, col) in schema.columns.iter().enumerate() {
            let fallback = current_row_vals
                .get(i)
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let old_val = match change.old_values.get(i) {
                Some(Some(v)) => v.clone(),
                _ => fallback.clone(),
            };

            let new_val = match change.new_values.get(i) {
                Some(Some(v)) => v.clone(),
                _ => match change.old_values.get(i) {
                    Some(Some(v)) => v.clone(),
                    _ => fallback,
                },
            };

            old_map.insert(col.clone(), old_val.clone());
            old_map.insert(col.to_ascii_lowercase(), old_val);

            new_map.insert(col.clone(), new_val.clone());
            new_map.insert(col.to_ascii_lowercase(), new_val);
        }

        (Some(old_map), Some(new_map))
    }
}

fn build_row_map(
    cols: &[String],
    values: &[Option<serde_json::Value>],
) -> Option<HashMap<String, serde_json::Value>> {
    if values.is_empty() {
        return None;
    }
    let mut map = HashMap::with_capacity(cols.len());
    for (i, col) in cols.iter().enumerate() {
        if let Some(Some(v)) = values.get(i) {
            map.insert(col.clone(), v.clone());
            map.insert(col.to_ascii_lowercase(), v.clone());
        } else {
            map.insert(col.clone(), serde_json::Value::Null);
            map.insert(col.to_ascii_lowercase(), serde_json::Value::Null);
        }
    }
    Some(map)
}

fn project_row(
    row: &HashMap<String, serde_json::Value>,
    projections: &[String],
) -> serde_json::Value {
    if projections.is_empty() || (projections.len() == 1 && projections[0] == "*") {
        let mut obj = serde_json::Map::new();
        for (k, v) in row {
            if !k.chars().any(char::is_uppercase) || !row.contains_key(&k.to_ascii_lowercase()) {
                obj.insert(k.clone(), v.clone());
            }
        }
        return serde_json::Value::Object(obj);
    }

    let mut obj = serde_json::Map::new();
    for col in projections {
        let val = row
            .get(col)
            .or_else(|| row.get(&col.to_ascii_lowercase()))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        obj.insert(col.clone(), val);
    }
    serde_json::Value::Object(obj)
}

fn eval_expr(expr: &ValueExpr, row: &HashMap<String, serde_json::Value>) -> serde_json::Value {
    match expr {
        ValueExpr::Literal(val) => val.clone(),
        ValueExpr::Column(col) => row
            .get(col)
            .or_else(|| row.get(&col.to_ascii_lowercase()))
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        ValueExpr::JsonExtract { column, path } => {
            let root = row
                .get(column)
                .or_else(|| row.get(&column.to_ascii_lowercase()));
            let Some(root) = root else {
                return serde_json::Value::Null;
            };

            let parsed_root: serde_json::Value = match root {
                serde_json::Value::String(s) => {
                    serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
                }
                other => other.clone(),
            };

            extract_json_path(&parsed_root, path)
        }
    }
}

fn extract_json_path(value: &serde_json::Value, path: &str) -> serde_json::Value {
    let clean_path = path.trim();
    let parts: Vec<&str> = clean_path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|p| !p.is_empty())
        .collect();

    let mut curr = value;
    for part in parts {
        match curr {
            serde_json::Value::Object(map) => {
                if let Some(next) = map.get(part) {
                    curr = next;
                } else {
                    return serde_json::Value::Null;
                }
            }
            _ => return serde_json::Value::Null,
        }
    }
    curr.clone()
}

pub fn eval_predicate(pred: &Predicate, row: &HashMap<String, serde_json::Value>) -> bool {
    match pred {
        Predicate::AlwaysTrue => true,
        Predicate::And(preds) => preds.iter().all(|p| eval_predicate(p, row)),
        Predicate::Or(preds) => preds.iter().any(|p| eval_predicate(p, row)),
        Predicate::Not(p) => !eval_predicate(p, row),
        Predicate::IsNull { expr, negated } => {
            let val = eval_expr(expr, row);
            let is_null = val.is_null();
            if *negated {
                !is_null
            } else {
                is_null
            }
        }
        Predicate::InList {
            expr,
            list,
            negated,
        } => {
            let val = eval_expr(expr, row);
            let in_list = list.iter().any(|item| values_equal(&val, item));
            if *negated {
                !in_list
            } else {
                in_list
            }
        }
        Predicate::Binary { left, op, right } => {
            let lv = eval_expr(left, row);
            let rv = eval_expr(right, row);
            eval_binary_op(&lv, *op, &rv)
        }
    }
}

fn eval_binary_op(left: &serde_json::Value, op: BinaryOp, right: &serde_json::Value) -> bool {
    match op {
        BinaryOp::Eq => values_equal(left, right),
        BinaryOp::NotEq => !values_equal(left, right),
        BinaryOp::Lt => compare_values(left, right).is_some_and(|c| c.is_lt()),
        BinaryOp::Lte => compare_values(left, right).is_some_and(|c| c.is_le()),
        BinaryOp::Gt => compare_values(left, right).is_some_and(|c| c.is_gt()),
        BinaryOp::Gte => compare_values(left, right).is_some_and(|c| c.is_ge()),
        BinaryOp::Like => {
            if let (Some(l), Some(r)) = (left.as_str(), right.as_str()) {
                sql_like(r, l)
            } else {
                false
            }
        }
    }
}

fn sql_like(pattern: &str, text: &str) -> bool {
    let p_chars: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t_chars: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (m, n) = (p_chars.len(), t_chars.len());
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if p_chars[i - 1] == '%' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            if p_chars[i - 1] == '%' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if p_chars[i - 1] == '_' || p_chars[i - 1] == t_chars[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[m][n]
}

fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (serde_json::Value::Number(n1), serde_json::Value::Number(n2)) => {
            if let (Some(i1), Some(i2)) = (n1.as_i64(), n2.as_i64()) {
                i1 == i2
            } else if let (Some(f1), Some(f2)) = (n1.as_f64(), n2.as_f64()) {
                (f1 - f2).abs() < f64::EPSILON
            } else {
                false
            }
        }
        (serde_json::Value::String(s), other) | (other, serde_json::Value::String(s)) => {
            match other {
                serde_json::Value::Number(n) => n.to_string() == *s,
                serde_json::Value::Bool(b) => b.to_string() == *s,
                _ => false,
            }
        }
        _ => false,
    }
}

fn compare_values(a: &serde_json::Value, b: &serde_json::Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (serde_json::Value::Number(n1), serde_json::Value::Number(n2)) => {
            if let (Some(i1), Some(i2)) = (n1.as_i64(), n2.as_i64()) {
                Some(i1.cmp(&i2))
            } else if let (Some(f1), Some(f2)) = (n1.as_f64(), n2.as_f64()) {
                f1.partial_cmp(&f2)
            } else {
                None
            }
        }
        (serde_json::Value::String(s1), serde_json::Value::String(s2)) => Some(s1.cmp(s2)),
        _ => None,
    }
}

pub fn parse_live_query(sql: &str, params: &[serde_json::Value]) -> Result<ParsedLiveQuery> {
    let substituted = substitute_params(sql, params);
    let trimmed = substituted.trim();

    let lower = trimmed.to_ascii_lowercase();
    let select_idx = lower.find("select ").ok_or_else(|| {
        lilia_core::LiliaError::new(
            lilia_core::ErrorCode::InvalidInput,
            "Live query must be a SELECT statement",
            false,
        )
    })?;

    let from_idx = lower.find(" from ").ok_or_else(|| {
        lilia_core::LiliaError::new(
            lilia_core::ErrorCode::InvalidInput,
            "Live query missing FROM clause",
            false,
        )
    })?;

    let proj_str = trimmed[select_idx + 7..from_idx].trim();
    let projections: Vec<String> = if proj_str == "*" {
        vec!["*".into()]
    } else {
        proj_str
            .split(',')
            .map(|col| {
                let c = col.trim();
                let lower_c = c.to_ascii_lowercase();
                if let Some(as_idx) = lower_c.rfind(" as ") {
                    c[as_idx + 4..].trim().trim_matches('"').to_string()
                } else {
                    c.split_whitespace()
                        .last()
                        .unwrap_or(c)
                        .trim_matches('"')
                        .to_string()
                }
            })
            .collect()
    };

    let after_from = &trimmed[from_idx + 6..];
    let where_pos = after_from.to_ascii_lowercase().find(" where ");

    let (table_part, where_clause) = match where_pos {
        Some(pos) => {
            let table = after_from[..pos].trim();
            let rest = &after_from[pos + 7..];
            (table, Some(rest))
        }
        None => {
            let end_table = after_from
                .find(|c: char| c.is_whitespace() || c == ';')
                .unwrap_or(after_from.len());
            (&after_from[..end_table], None)
        }
    };

    let table_name = table_part.trim_matches('"').trim_matches('`').to_string();

    let is_json_view = table_name.eq_ignore_ascii_case("lilia_json")
        || table_name.eq_ignore_ascii_case("_lilia_json");

    let (predicate, json_space_filter) = if let Some(where_str) = where_clause {
        let (pred, space) = parse_where_clause(where_str)?;
        (pred, space)
    } else {
        (Predicate::AlwaysTrue, None)
    };

    Ok(ParsedLiveQuery {
        raw_sql: trimmed.to_string(),
        table: table_name,
        is_json_view,
        json_space_filter,
        projections,
        predicate,
    })
}

fn substitute_params(sql: &str, params: &[serde_json::Value]) -> String {
    let mut result = sql.to_string();
    for (i, p) in params.iter().enumerate() {
        let idx = i + 1;
        let placeholder1 = format!("?{idx}");
        let placeholder2 = "?".to_string();

        let val_repr = match p {
            serde_json::Value::Null => "NULL".to_string(),
            serde_json::Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => format!("'{}'", s.replace('\'', "''")),
            other => format!("'{}'", other.to_string().replace('\'', "''")),
        };

        if result.contains(&placeholder1) {
            result = result.replace(&placeholder1, &val_repr);
        } else if result.contains(&placeholder2) {
            if let Some(pos) = result.find('?') {
                result = format!("{}{val_repr}{}", &result[..pos], &result[pos + 1..]);
            }
        }
    }
    result
}

fn parse_where_clause(where_str: &str) -> Result<(Predicate, Option<String>)> {
    let lower = where_str.to_ascii_lowercase();
    let order_by_pos = lower.find(" order by ");
    let limit_pos = lower.find(" limit ");

    let cut_pos = match (order_by_pos, limit_pos) {
        (Some(o), Some(l)) => Some(o.min(l)),
        (Some(o), None) => Some(o),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    };

    let clean_where = if let Some(pos) = cut_pos {
        &where_str[..pos]
    } else {
        where_str
    };
    let clean_where = clean_where.trim_end_matches(';').trim();

    let mut space_filter = None;
    let and_parts = split_where_logical(clean_where, "AND");
    let mut predicates = Vec::new();

    for part in and_parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }

        if let Some(pred) = parse_comparison(p)? {
            if let Predicate::Binary {
                left: ValueExpr::Column(col),
                op: BinaryOp::Eq,
                right: ValueExpr::Literal(serde_json::Value::String(space_name)),
            } = &pred
            {
                if col.eq_ignore_ascii_case("space") {
                    space_filter = Some(space_name.clone());
                }
            }
            predicates.push(pred);
        }
    }

    let final_predicate = match predicates.len() {
        0 => Predicate::AlwaysTrue,
        1 => predicates.remove(0),
        _ => Predicate::And(predicates),
    };

    Ok((final_predicate, space_filter))
}

fn split_where_logical(where_str: &str, op: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth: usize = 0;
    let mut in_quote = false;
    let mut quote_char = ' ';

    let chars: Vec<char> = where_str.chars().collect();
    let op_upper = op.to_ascii_uppercase();
    let op_len = op.len();

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '\'' || c == '"') && depth == 0 {
            if in_quote && c == quote_char {
                in_quote = false;
            } else if !in_quote {
                in_quote = true;
                quote_char = c;
            }
            current.push(c);
            i += 1;
            continue;
        }

        if in_quote {
            current.push(c);
            i += 1;
            continue;
        }

        if c == '(' {
            depth += 1;
            current.push(c);
            i += 1;
            continue;
        } else if c == ')' {
            depth = depth.saturating_sub(1);
            current.push(c);
            i += 1;
            continue;
        }

        if depth == 0 && i + op_len <= chars.len() {
            let slice: String = chars[i..i + op_len].iter().collect();
            if slice.to_ascii_uppercase() == op_upper {
                let prev_ok = i == 0 || chars[i - 1].is_whitespace();
                let next_ok = i + op_len == chars.len() || chars[i + op_len].is_whitespace();
                if prev_ok && next_ok {
                    parts.push(current.trim().to_string());
                    current.clear();
                    i += op_len;
                    continue;
                }
            }
        }

        current.push(c);
        i += 1;
    }

    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn parse_comparison(part: &str) -> Result<Option<Predicate>> {
    let trimmed = part.trim();

    if let Some((left, right)) = trimmed.split_once(">=") {
        return Ok(Some(Predicate::Binary {
            left: parse_val_expr(left.trim())?,
            op: BinaryOp::Gte,
            right: parse_val_expr(right.trim())?,
        }));
    }
    if let Some((left, right)) = trimmed.split_once("<=") {
        return Ok(Some(Predicate::Binary {
            left: parse_val_expr(left.trim())?,
            op: BinaryOp::Lte,
            right: parse_val_expr(right.trim())?,
        }));
    }
    if let Some((left, right)) = trimmed.split_once("!=") {
        return Ok(Some(Predicate::Binary {
            left: parse_val_expr(left.trim())?,
            op: BinaryOp::NotEq,
            right: parse_val_expr(right.trim())?,
        }));
    }
    if let Some((left, right)) = trimmed.split_once("<>") {
        return Ok(Some(Predicate::Binary {
            left: parse_val_expr(left.trim())?,
            op: BinaryOp::NotEq,
            right: parse_val_expr(right.trim())?,
        }));
    }
    if let Some((left, right)) = trimmed.split_once('=') {
        return Ok(Some(Predicate::Binary {
            left: parse_val_expr(left.trim())?,
            op: BinaryOp::Eq,
            right: parse_val_expr(right.trim())?,
        }));
    }
    if let Some((left, right)) = trimmed.split_once('>') {
        return Ok(Some(Predicate::Binary {
            left: parse_val_expr(left.trim())?,
            op: BinaryOp::Gt,
            right: parse_val_expr(right.trim())?,
        }));
    }
    if let Some((left, right)) = trimmed.split_once('<') {
        return Ok(Some(Predicate::Binary {
            left: parse_val_expr(left.trim())?,
            op: BinaryOp::Lt,
            right: parse_val_expr(right.trim())?,
        }));
    }

    Ok(Some(Predicate::AlwaysTrue))
}

fn parse_val_expr(expr: &str) -> Result<ValueExpr> {
    let trimmed = expr.trim();

    if (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
    {
        let content = &trimmed[1..trimmed.len() - 1];
        return Ok(ValueExpr::Literal(serde_json::Value::String(
            content.replace("''", "'"),
        )));
    }

    if let Ok(i) = trimmed.parse::<i64>() {
        return Ok(ValueExpr::Literal(serde_json::Value::Number(i.into())));
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Ok(ValueExpr::Literal(serde_json::Value::Number(n)));
        }
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return Ok(ValueExpr::Literal(serde_json::Value::Bool(true)));
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Ok(ValueExpr::Literal(serde_json::Value::Bool(false)));
    }
    if trimmed.eq_ignore_ascii_case("null") {
        return Ok(ValueExpr::Literal(serde_json::Value::Null));
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("json_extract(") && lower.ends_with(')') {
        let inside = &trimmed[13..trimmed.len() - 1];
        if let Some((col, path)) = inside.split_once(',') {
            let col = col.trim().trim_matches('"').trim_matches('`');
            let path = path.trim().trim_matches('\'').trim_matches('"');
            return Ok(ValueExpr::JsonExtract {
                column: col.to_string(),
                path: path.to_string(),
            });
        }
    }

    Ok(ValueExpr::Column(
        trimmed.trim_matches('"').trim_matches('`').to_string(),
    ))
}
