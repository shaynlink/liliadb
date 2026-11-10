#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::sync::Arc;

use lilia_core::{ErrorCode, LiliaError, Result, SqlQueryResult, ValueCodec};
use lilia_sqlite_native::{ffi, NativeConnection, NativeStatement};

pub(crate) fn register_sql_functions(
    conn: &NativeConnection,
    codec: &Arc<dyn ValueCodec>,
) -> Result<()> {
    let codec_kv = Arc::clone(codec);
    conn.register_scalar_function("lilia_decode_kv", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let bytes = args[0].as_blob();
        match codec_kv.decode(bytes) {
            Ok(dec) => {
                ctx.set_result_blob(&dec);
                Ok(())
            }
            Err(e) => Err(LiliaError::new(ErrorCode::Storage, e.to_string(), false)),
        }
    })?;

    let codec_json = Arc::clone(codec);
    conn.register_scalar_function("lilia_decode_json", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let text = args[0].as_text();

        if text.starts_with("{\"$codec\":") {
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(text) {
                if let Some(b64) = json_val.get("$data").and_then(serde_json::Value::as_str) {
                    use base64::Engine;
                    if let Ok(ciphertext) = base64::engine::general_purpose::STANDARD.decode(b64) {
                        if let Ok(plaintext) = codec_json.decode(&ciphertext) {
                            if let Ok(plain_str) = String::from_utf8(plaintext) {
                                ctx.set_result_text(&plain_str);
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }

        ctx.set_result_text(text);
        Ok(())
    })?;

    let codec_vec = Arc::clone(codec);
    conn.register_scalar_function("lilia_decode_vector", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let bytes = args[0].as_blob();
        match codec_vec.decode(bytes) {
            Ok(dec) => {
                ctx.set_result_blob(&dec);
                Ok(())
            }
            Err(e) => Err(LiliaError::new(ErrorCode::Storage, e.to_string(), false)),
        }
    })?;

    conn.register_scalar_function("lilia_vec_distance_cosine", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b1 = args[0].as_blob();
        let b2 = args[1].as_blob();
        let v1 = bytes_to_f32_vec(b1);
        let v2 = bytes_to_f32_vec(b2);
        let dist = lilia_core::quantization::cosine_distance_f32(&v1, &v2);
        ctx.set_result_double(dist);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_distance_l2", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b1 = args[0].as_blob();
        let b2 = args[1].as_blob();
        let v1 = bytes_to_f32_vec(b1);
        let v2 = bytes_to_f32_vec(b2);
        let dist = lilia_core::quantization::l2_distance_f32(&v1, &v2);
        ctx.set_result_double(dist);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_distance_hamming", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b1 = args[0].as_blob();
        let b2 = args[1].as_blob();
        let dist = lilia_core::quantization::hamming_distance_bits(b1, b2);
        ctx.set_result_int64(i64::from(dist));
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_to_json", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let v = bytes_to_f32_vec(b);
        let s = serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string());
        ctx.set_result_text(&s);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_from_json", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let text = args[0].as_text();
        if let Ok(floats) = serde_json::from_str::<Vec<f32>>(text) {
            let mut bytes = Vec::with_capacity(floats.len() * 4);
            for f in floats {
                bytes.extend_from_slice(&f.to_le_bytes());
            }
            ctx.set_result_blob(&bytes);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_quantize_sq8", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let f = bytes_to_f32_vec(b);
        let (q, _) = lilia_core::quantization::quantize_f32_to_i8(&f);
        let bytes: Vec<u8> = q.into_iter().map(|x| x as u8).collect();
        ctx.set_result_blob(&bytes);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_quantize_bit", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let f = bytes_to_f32_vec(b);
        let bits = lilia_core::quantization::quantize_f32_to_bits(&f);
        ctx.set_result_blob(&bits);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_dimension", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let dim = (b.len() / 4) as i64;
        ctx.set_result_int64(dim);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_geo_distance", 4, |ctx, args| {
        if args.len() < 4
            || args[0].is_null()
            || args[1].is_null()
            || args[2].is_null()
            || args[3].is_null()
        {
            ctx.set_result_null();
            return Ok(());
        }
        let lat1 = args[0].as_double();
        let lon1 = args[1].as_double();
        let lat2 = args[2].as_double();
        let lon2 = args[3].as_double();
        let dist = lilia_core::haversine_distance_meters(lat1, lon1, lat2, lon2);
        ctx.set_result_double(dist);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_geo_circle", 4, |ctx, args| {
        if args.len() < 4
            || args[0].is_null()
            || args[1].is_null()
            || args[2].is_null()
            || args[3].is_null()
        {
            ctx.set_result_null();
            return Ok(());
        }
        let lon = args[0].as_double();
        let lat = args[1].as_double();
        let radius = args[2].as_double();
        let vertices = args[3].as_int64();
        let n_vert = if vertices <= 0 {
            16
        } else {
            usize::try_from(vertices).unwrap_or(16)
        };
        let poly_json = lilia_core::circle_to_geopoly_json(lon, lat, radius, n_vert);
        ctx.set_result_text(&poly_json);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_ts_decompress", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let blob = args[0].as_blob();
        if let Ok(points) = lilia_core::decompress_timeseries(blob) {
            let json = serde_json::to_string(&points).unwrap_or_else(|_| "[]".to_string());
            ctx.set_result_text(&json);
            Ok(())
        } else {
            ctx.set_result_null();
            Ok(())
        }
    })?;

    conn.register_scalar_function("lilia_ts_downsample", 3, |ctx, args| {
        if args.len() < 3 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let text = args[0].as_text();
        let bucket_ms = args[1].as_int64();
        let agg_str = args[2].as_text().to_lowercase();
        let agg = match agg_str.as_str() {
            "min" => lilia_core::DownsampleAggregation::Min,
            "max" => lilia_core::DownsampleAggregation::Max,
            "sum" => lilia_core::DownsampleAggregation::Sum,
            "first" => lilia_core::DownsampleAggregation::First,
            "last" => lilia_core::DownsampleAggregation::Last,
            "count" => lilia_core::DownsampleAggregation::Count,
            _ => lilia_core::DownsampleAggregation::Avg,
        };
        if let Ok(points) = serde_json::from_str::<Vec<lilia_core::TimeSeriesPoint>>(text) {
            let downsampled = lilia_core::downsample_points(&points, bucket_ms, agg);
            let json = serde_json::to_string(&downsampled).unwrap_or_else(|_| "[]".to_string());
            ctx.set_result_text(&json);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    Ok(())
}

fn bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn read_column_as_json(stmt: &NativeStatement, col: i32) -> serde_json::Value {
    match stmt.column_type(col) {
        ffi::SQLITE_INTEGER => serde_json::Value::Number(stmt.column_int64(col).into()),
        ffi::SQLITE_FLOAT => serde_json::Number::from_f64(stmt.column_double(col))
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        ffi::SQLITE_TEXT => serde_json::Value::String(stmt.column_text(col).to_string()),
        ffi::SQLITE_BLOB => serde_json::Value::String(hex::encode(stmt.column_blob(col))),
        _ => serde_json::Value::Null,
    }
}

fn bind_json_params(
    stmt: &mut NativeStatement,
    params: &[serde_json::Value],
    string_cache: &mut Vec<String>,
) -> Result<()> {
    for (idx, val) in params.iter().enumerate() {
        let col = (idx + 1) as i32;
        match val {
            serde_json::Value::Null => stmt.bind_null(col)?,
            serde_json::Value::Bool(b) => stmt.bind_int64(col, i64::from(*b))?,
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    stmt.bind_int64(col, i)?;
                } else if let Some(f) = n.as_f64() {
                    stmt.bind_double(col, f)?;
                } else {
                    let s = n.to_string();
                    string_cache.push(s);
                    stmt.bind_text_static(col, string_cache.last().unwrap())?;
                }
            }
            serde_json::Value::String(s) => {
                stmt.bind_text_static(col, s)?;
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                let s = val.to_string();
                string_cache.push(s);
                stmt.bind_text_static(col, string_cache.last().unwrap())?;
            }
        }
    }
    Ok(())
}

fn extract_string_literal(sql: &str, field: &str) -> Option<String> {
    let lower_sql = sql.to_ascii_lowercase();
    let pattern_eq = format!("{field}=");
    let pattern_space_eq = format!("{field} =");

    let pos = lower_sql
        .find(&pattern_eq)
        .or_else(|| lower_sql.find(&pattern_space_eq))?;
    let slice = &sql[pos..];
    let eq_idx = slice.find('=')?;
    let after_eq = slice[eq_idx + 1..].trim_start();
    if let Some(stripped) = after_eq.strip_prefix('\'') {
        let closing = stripped.find('\'')?;
        Some(stripped[..closing].to_string())
    } else if let Some(stripped) = after_eq.strip_prefix('"') {
        let closing = stripped.find('"')?;
        Some(stripped[..closing].to_string())
    } else {
        None
    }
}

fn find_as_of_clause(sql: &str) -> Option<(usize, usize, String, String, u64)> {
    let lower = sql.to_ascii_lowercase();
    let as_of_keyword = " as of ";
    let idx = lower.find(as_of_keyword)?;

    let before_as_of = sql[..idx].trim_end();
    let from_idx = before_as_of.to_ascii_lowercase().rfind("from ")?;
    let table_name_part = before_as_of[from_idx + 5..].trim();
    let table_name = table_name_part.split_whitespace().last()?.to_string();

    let after_as_of = &sql[idx + as_of_keyword.len()..];
    let mut parts = after_as_of.split_whitespace();
    let mode_str = parts.next()?;
    let mode_upper = mode_str.to_ascii_uppercase();
    if mode_upper != "VERSION" && mode_upper != "TIMESTAMP" {
        return None;
    }

    let val_str = parts.next()?;
    let clean_val_str: String = val_str.chars().take_while(char::is_ascii_digit).collect();
    let val: u64 = clean_val_str.parse().ok()?;

    let clause_sub = format!("{table_name}{as_of_keyword}{mode_str} {clean_val_str}");
    let clause_sub_lower = clause_sub.to_ascii_lowercase();
    let start_in_orig = lower.find(&clause_sub_lower)?;
    let end_in_orig = start_in_orig + clause_sub.len();

    Some((start_in_orig, end_in_orig, table_name, mode_upper, val))
}

impl crate::Database {
    fn resolve_time_travel_docs(
        &self,
        table_name: &str,
        mode: &str,
        target_val: u64,
        sql_context: &str,
    ) -> Result<Vec<(String, String, String, u64)>> {
        let is_global_view = table_name == "lilia_json";
        let target_space = if is_global_view {
            extract_string_literal(sql_context, "space")
        } else {
            Some(table_name.to_string())
        };

        let target_id = extract_string_literal(sql_context, "id");
        let mut entries = Vec::new();

        if let Some(space) = target_space {
            if let Some(id) = target_id {
                let entry_opt = if mode == "VERSION" {
                    self.json_get_at_version(&space, &id, target_val)?
                } else {
                    #[allow(clippy::cast_possible_wrap)]
                    self.json_get_at_timestamp(&space, &id, target_val as i64)?
                };
                if let Some(entry) = entry_opt {
                    entries.push((
                        space,
                        id,
                        serde_json::to_string(&entry.value).unwrap_or_else(|_| "{}".to_string()),
                        entry.version,
                    ));
                }
            } else {
                let conn = self.read_lock()?;
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT id FROM _lilia_json_revisions WHERE space = ?1 AND version <= ?2 LIMIT 10000;",
                )?;
                stmt.bind_text_static(1, &space)?;
                stmt.bind_int64(2, i64::try_from(target_val).unwrap_or(i64::MAX))?;
                let mut ids = Vec::new();
                while stmt.step()? {
                    ids.push(stmt.column_text(0).to_string());
                }
                drop(stmt);

                for id in ids {
                    let entry_opt = if mode == "VERSION" {
                        self.json_get_at_version(&space, &id, target_val)?
                    } else {
                        #[allow(clippy::cast_possible_wrap)]
                        self.json_get_at_timestamp(&space, &id, target_val as i64)?
                    };
                    if let Some(entry) = entry_opt {
                        entries.push((
                            space.clone(),
                            id,
                            serde_json::to_string(&entry.value)
                                .unwrap_or_else(|_| "{}".to_string()),
                            entry.version,
                        ));
                    }
                }
            }
        }

        Ok(entries)
    }

    fn preprocess_time_travel_sql(&self, sql: &str) -> Result<String> {
        let Some((start, end, table_name, mode, target_val)) = find_as_of_clause(sql) else {
            return Ok(sql.to_string());
        };

        let docs = self.resolve_time_travel_docs(&table_name, &mode, target_val, sql)?;
        let is_global = table_name == "lilia_json";

        let cte_body = if docs.is_empty() {
            if is_global {
                format!("{table_name}(space, id, value, version) AS (SELECT '' AS space, '' AS id, '{{}}' AS value, 0 AS version WHERE 0)")
            } else {
                format!("{table_name}(id, value, version) AS (SELECT '' AS id, '{{}}' AS value, 0 AS version WHERE 0)")
            }
        } else {
            let mut select_clauses = Vec::new();
            for (space, id, json_str, ver) in docs {
                let esc_space = space.replace('\'', "''");
                let esc_id = id.replace('\'', "''");
                let esc_json = json_str.replace('\'', "''");
                if is_global {
                    select_clauses.push(format!(
                        "SELECT '{esc_space}', '{esc_id}', '{esc_json}', {ver}"
                    ));
                } else {
                    select_clauses.push(format!("SELECT '{esc_id}', '{esc_json}', {ver}"));
                }
            }
            let cols = if is_global {
                "(space, id, value, version)"
            } else {
                "(id, value, version)"
            };
            format!(
                "{table_name}{cols} AS ({})",
                select_clauses.join(" UNION ALL ")
            )
        };

        let replaced_sql = format!("{}{table_name}{}", &sql[..start], &sql[end..]);
        let trimmed_replaced = replaced_sql.trim();

        let final_sql = if trimmed_replaced.to_ascii_lowercase().starts_with("with ") {
            format!("WITH {}, {}", cte_body, &trimmed_replaced[5..])
        } else {
            format!("WITH {} {}", cte_body, trimmed_replaced)
        };

        Ok(final_sql)
    }

    /// Execute a SQL query or mutation.
    /// Read-only queries (SELECT, EXPLAIN) are executed on the concurrent reader pool.
    /// Mutating queries (INSERT, UPDATE, DELETE) are executed via the exclusive writer queue.
    #[allow(clippy::too_many_lines)]
    pub fn sql_query(&self, sql: &str, params: &[serde_json::Value]) -> Result<SqlQueryResult> {
        let effective_sql = self.preprocess_time_travel_sql(sql)?;
        let trimmed = effective_sql.trim();
        if trimmed.is_empty() {
            return Ok(SqlQueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                rows_affected: 0,
            });
        }

        let is_readonly = {
            let conn = self.read_lock()?;
            let stmt = conn.prepare(trimmed)?;
            stmt.readonly()
        };

        if is_readonly {
            let conn = self.read_lock()?;
            let mut stmt = conn.prepare(trimmed)?;
            let col_count = stmt.column_count();
            let mut columns = Vec::with_capacity(col_count);
            for i in 0..col_count {
                columns.push(stmt.column_name(i as i32).to_string());
            }

            let mut string_cache = Vec::new();
            bind_json_params(&mut stmt, params, &mut string_cache)?;

            let mut rows = Vec::new();
            while stmt.step()? {
                let mut row_vals = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    row_vals.push(read_column_as_json(&stmt, i as i32));
                }
                rows.push(row_vals);
            }

            Ok(SqlQueryResult {
                columns,
                rows,
                rows_affected: 0,
            })
        } else {
            let mut writer = self.lock_writer()?;
            let tx = writer.transaction()?;
            let mut stmt = tx.prepare(trimmed)?;
            let col_count = stmt.column_count();
            let mut columns = Vec::with_capacity(col_count);
            for i in 0..col_count {
                columns.push(stmt.column_name(i as i32).to_string());
            }

            let mut string_cache = Vec::new();
            bind_json_params(&mut stmt, params, &mut string_cache)?;

            let mut rows = Vec::new();
            let rows_affected = if col_count > 0 {
                while stmt.step()? {
                    let mut row_vals = Vec::with_capacity(col_count);
                    for i in 0..col_count {
                        row_vals.push(read_column_as_json(&stmt, i as i32));
                    }
                    rows.push(row_vals);
                }
                0
            } else {
                stmt.step()?;
                u64::try_from(tx.conn().changes()).unwrap_or(0)
            };
            drop(stmt);
            tx.commit()?;

            Ok(SqlQueryResult {
                columns,
                rows,
                rows_affected,
            })
        }
    }
}
