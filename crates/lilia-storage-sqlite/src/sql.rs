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

    conn.register_scalar_function("lilia_h3_lat_lng_to_cell", 3, |ctx, args| {
        if args.len() < 3 || args[0].is_null() || args[1].is_null() || args[2].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let lat = args[0].as_double();
        let lng = args[1].as_double();
        let res = args[2].as_int64();
        let res_u8 = match u8::try_from(res) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::lat_lng_to_cell(lat, lng, res_u8) {
            Ok(cell) => ctx.set_result_int64(cell as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_cell_to_lat", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::cell_to_lat_lng(cell) {
            Ok((lat, _)) => ctx.set_result_double(lat),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_cell_to_lng", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::cell_to_lat_lng(cell) {
            Ok((_, lng)) => ctx.set_result_double(lng),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_to_string", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        ctx.set_result_text(&lilia_core::h3::cell_to_string(cell));
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_from_string", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        match lilia_core::h3::string_to_cell(s) {
            Ok(cell) => ctx.set_result_int64(cell as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_resolution", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::resolution(cell) {
            Ok(res) => ctx.set_result_int64(i64::from(res)),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_parent", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let parent_res = args[1].as_int64();
        let p_res_u8 = match u8::try_from(parent_res) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::cell_to_parent(cell, p_res_u8) {
            Ok(parent) => ctx.set_result_int64(parent as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_distance", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let c1 = args[0].as_int64() as u64;
        let c2 = args[1].as_int64() as u64;
        match lilia_core::h3::grid_distance(c1, c2) {
            Ok(dist) => ctx.set_result_int64(dist),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_is_valid", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        ctx.set_result_int64(if lilia_core::h3::is_valid(cell) { 1 } else { 0 });
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_child_range_min", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let c_res = match u8::try_from(args[1].as_int64()) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::child_range(cell, c_res) {
            Ok((min_val, _)) => ctx.set_result_int64(min_val as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_child_range_max", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let c_res = match u8::try_from(args[1].as_int64()) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::child_range(cell, c_res) {
            Ok((_, max_val)) => ctx.set_result_int64(max_val as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_k_ring", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let k = match u32::try_from(args[1].as_int64()) {
            Ok(v) => v,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::k_ring(cell, k) {
            Ok(neighbors) => {
                let json = serde_json::to_string(&neighbors).unwrap_or_else(|_| "[]".to_string());
                ctx.set_result_text(&json);
            }
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_to_boundary", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::cell_to_boundary(cell) {
            Ok(coords) => {
                let json = lilia_core::coords_to_geopoly_json(&coords);
                ctx.set_result_text(&json);
            }
            Err(_) => ctx.set_result_null(),
        }
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

    conn.register_scalar_function("lilia_iso_date", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let ms = args[0].as_int64();
        let iso = lilia_core::format_ms_to_iso(ms);
        ctx.set_result_text(&iso);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_to_timestamp", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let txt = args[0].as_text();
        match lilia_core::parse_iso_to_ms(txt) {
            Ok(ms) => {
                ctx.set_result_int64(ms);
                Ok(())
            }
            Err(e) => Err(LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false)),
        }
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
    bind_json_params_offset(stmt, params, 1, string_cache)
}

fn bind_json_params_offset(
    stmt: &mut NativeStatement,
    params: &[serde_json::Value],
    offset: i32,
    string_cache: &mut Vec<String>,
) -> Result<()> {
    for (idx, val) in params.iter().enumerate() {
        let col = offset + idx as i32;
        match val {
            serde_json::Value::Null => stmt.bind_null(col)?,
            serde_json::Value::Bool(b) => stmt.bind_int64(col, i64::from(*b))?,
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    stmt.bind_int64(col, i)?;
                } else if let Some(u) = n.as_u64() {
                    stmt.bind_int64(col, u as i64)?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ValidTimeClause {
    Point(i64),
    Between(i64, i64),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TransactionTimeClause {
    Point(i64),
    All,
}

struct BitemporalClauseMatch {
    start: usize,
    end: usize,
    table_name: String,
    tt: TransactionTimeClause,
    vt: ValidTimeClause,
}

fn extract_temporal_value(s: &str) -> Option<(String, usize)> {
    let trimmed = s.trim_start();
    let leading_ws = s.len() - trimmed.len();
    if let Some(stripped) = trimmed.strip_prefix('\'') {
        let end_idx = stripped.find('\'')?;
        let val = stripped[..end_idx].to_string();
        Some((val, leading_ws + end_idx + 2))
    } else if let Some(stripped) = trimmed.strip_prefix('"') {
        let end_idx = stripped.find('"')?;
        let val = stripped[..end_idx].to_string();
        Some((val, leading_ws + end_idx + 2))
    } else {
        let mut len = 0;
        for c in trimmed.chars() {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '+' || c == ':' || c == '.' {
                len += c.len_utf8();
            } else {
                break;
            }
        }
        if len > 0 {
            Some((trimmed[..len].to_string(), leading_ws + len))
        } else {
            None
        }
    }
}

fn find_bitemporal_clause(sql: &str) -> Option<BitemporalClauseMatch> {
    let lower = sql.to_ascii_lowercase();
    let from_keyword = "from ";
    let from_idx = lower.find(from_keyword)?;
    let after_from_idx = from_idx + from_keyword.len();
    let after_from = &sql[after_from_idx..];

    let trimmed = after_from.trim_start();
    let leading_ws = after_from.len() - trimmed.len();
    let table_start_in_sql = after_from_idx + leading_ws;

    let mut table_len = 0;
    for c in trimmed.chars() {
        if c.is_alphanumeric() || c == '_' || c == '$' {
            table_len += c.len_utf8();
        } else {
            break;
        }
    }
    if table_len == 0 {
        return None;
    }
    let table_name = trimmed[..table_len].to_string();
    let after_table_idx = table_start_in_sql + table_len;
    let remainder = &sql[after_table_idx..];

    let mut current_offset = 0;
    let mut tt_clause = None;
    let mut vt_clause = None;

    for _ in 0..2 {
        let rest = &remainder[current_offset..];
        let rest_lower = rest.to_ascii_lowercase();
        let trimmed_rest = rest.trim_start();
        let ws_len = rest.len() - trimmed_rest.len();
        let trimmed_lower = &rest_lower[ws_len..];

        let tt_prefix = if trimmed_lower.starts_with("as of transaction_time ") {
            Some("as of transaction_time ")
        } else if trimmed_lower.starts_with("as of transaction ") {
            Some("as of transaction ")
        } else if trimmed_lower.starts_with("as of system_time ") {
            Some("as of system_time ")
        } else if trimmed_lower.starts_with("as of system time ") {
            Some("as of system time ")
        } else if trimmed_lower.starts_with("as of transaction")
            && (trimmed_lower.len() == 17
                || trimmed_lower[17..].starts_with('\'')
                || trimmed_lower[17..].starts_with('"'))
        {
            Some("as of transaction")
        } else {
            None
        };

        if let Some(prefix) = tt_prefix {
            if tt_clause.is_some() {
                break;
            }
            let after_prefix = &trimmed_rest[prefix.len()..];
            if let Some((val_str, eaten)) = extract_temporal_value(after_prefix) {
                let spec = if val_str.eq_ignore_ascii_case("all") {
                    TransactionTimeClause::All
                } else {
                    let ms = lilia_core::parse_iso_to_ms(&val_str).ok()?;
                    TransactionTimeClause::Point(ms)
                };
                tt_clause = Some(spec);
                current_offset += ws_len + prefix.len() + eaten;
                continue;
            }
        }

        let vt_prefix = if trimmed_lower.starts_with("for valid_time ") {
            Some("for valid_time ")
        } else if trimmed_lower.starts_with("for valid time ") {
            Some("for valid time ")
        } else if trimmed_lower.starts_with("for business_time ") {
            Some("for business_time ")
        } else if trimmed_lower.starts_with("for application_time ") {
            Some("for application_time ")
        } else {
            None
        };

        if let Some(prefix) = vt_prefix {
            if vt_clause.is_some() {
                break;
            }
            let after_prefix = trimmed_rest[prefix.len()..].trim_start();
            let p_ws = (trimmed_rest.len() - prefix.len()) - after_prefix.len();
            let lower_after = after_prefix.to_ascii_lowercase();

            if lower_after.starts_with("between ") {
                let after_between = &after_prefix[8..];
                let (val1_str, eaten1) = extract_temporal_value(after_between)?;
                let rest_between = after_between[eaten1..].trim_start();
                let b_ws = (after_between.len() - eaten1) - rest_between.len();
                let lower_rest_between = rest_between.to_ascii_lowercase();
                if lower_rest_between.starts_with("and ") {
                    let after_and = &rest_between[4..];
                    let (val2_str, eaten2) = extract_temporal_value(after_and)?;
                    let ms1 = lilia_core::parse_iso_to_ms(&val1_str).ok()?;
                    let mut ms2 = lilia_core::parse_iso_to_ms(&val2_str).ok()?;
                    if !val2_str.contains('T') && !val2_str.contains(' ') && ms2 < i64::MAX - 86_400_000 {
                        ms2 += 86_400_000;
                    }
                    vt_clause = Some(ValidTimeClause::Between(ms1, ms2));
                    current_offset += ws_len + prefix.len() + p_ws + 8 + eaten1 + b_ws + 4 + eaten2;
                    continue;
                }
            } else if lower_after.starts_with("as of ") {
                let after_as_of = &after_prefix[6..];
                let (val_str, eaten) = extract_temporal_value(after_as_of)?;
                let spec = if val_str.eq_ignore_ascii_case("all") {
                    ValidTimeClause::All
                } else {
                    let ms = lilia_core::parse_iso_to_ms(&val_str).ok()?;
                    ValidTimeClause::Point(ms)
                };
                vt_clause = Some(spec);
                current_offset += ws_len + prefix.len() + p_ws + 6 + eaten;
                continue;
            } else if let Some((val_str, eaten)) = extract_temporal_value(after_prefix) {
                let spec = if val_str.eq_ignore_ascii_case("all") {
                    ValidTimeClause::All
                } else {
                    let ms = lilia_core::parse_iso_to_ms(&val_str).ok()?;
                    ValidTimeClause::Point(ms)
                };
                vt_clause = Some(spec);
                current_offset += ws_len + prefix.len() + p_ws + eaten;
                continue;
            }
        }

        break;
    }

    if tt_clause.is_none() && vt_clause.is_none() {
        return None;
    }

    let end_in_sql = after_table_idx + current_offset;
    let now = crate::storage::now_ms();
    let final_tt = tt_clause.unwrap_or(TransactionTimeClause::Point(now));
    let final_vt = vt_clause.unwrap_or(match final_tt {
        TransactionTimeClause::Point(t) => ValidTimeClause::Point(t),
        TransactionTimeClause::All => ValidTimeClause::All,
    });

    Some(BitemporalClauseMatch {
        start: table_start_in_sql,
        end: end_in_sql,
        table_name,
        tt: final_tt,
        vt: final_vt,
    })
}

fn build_bitemporal_cte(spec: &BitemporalClauseMatch) -> String {
    let mut where_parts = Vec::new();
    let is_global = spec.table_name == "lilia_bitemporal";
    if !is_global {
        let esc_table = spec.table_name.replace('\'', "''");
        where_parts.push(format!("space = '{esc_table}'"));
    }

    match spec.tt {
        TransactionTimeClause::Point(tt) => {
            where_parts.push(format!("tt_from <= {tt} AND {tt} < tt_to"));
        }
        TransactionTimeClause::All => {}
    }

    match spec.vt {
        ValidTimeClause::Point(vt) => {
            where_parts.push(format!("vt_from <= {vt} AND {vt} < vt_to"));
        }
        ValidTimeClause::Between(v1, v2) => {
            where_parts.push(format!("vt_from < {v2} AND vt_to > {v1}"));
        }
        ValidTimeClause::All => {}
    }

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    let cols = if is_global {
        "(space, id, value, version, vt_from, vt_to, tt_from, tt_to)"
    } else {
        "(id, value, version, vt_from, vt_to, tt_from, tt_to)"
    };

    let select_cols = if is_global {
        "space, id, lilia_decode_json(value) AS value, version, vt_from, vt_to, tt_from, tt_to"
    } else {
        "id, lilia_decode_json(value) AS value, version, vt_from, vt_to, tt_from, tt_to"
    };

    format!(
        "{}{cols} AS (SELECT {select_cols} FROM _lilia_bitemporal {where_clause})",
        spec.table_name
    )
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
        // 1. Check for bitemporal clauses (AS OF TRANSACTION ... FOR VALID_TIME ...)
        if let Some(bitemp_match) = find_bitemporal_clause(sql) {
            let cte_body = build_bitemporal_cte(&bitemp_match);
            let replaced_sql = format!(
                "{}{}{}",
                &sql[..bitemp_match.start],
                &bitemp_match.table_name,
                &sql[bitemp_match.end..]
            );
            let trimmed = replaced_sql.trim();
            let final_sql = if trimmed.to_ascii_lowercase().starts_with("with ") {
                format!("WITH {}, {}", cte_body, &trimmed[5..])
            } else {
                format!("WITH {} {}", cte_body, trimmed)
            };
            return Ok(final_sql);
        }

        // 2. Fall back to standard JSON revisions time travel (AS OF VERSION / AS OF TIMESTAMP)
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

    /// Fluent SQL interface for queries, parameter binding, and execution.
    #[must_use]
    pub fn sql(&self) -> SqlStore<'_> {
        SqlStore { db: self }
    }

    /// Execute a SQL query or mutation with a carray bound to parameter ?1.
    pub fn sql_query_with_carray<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
    ) -> Result<SqlQueryResult> {
        self.sql_query_with_carray_and_params(sql, carray, &[])
    }

    /// Execute a SQL query or mutation with a carray bound to parameter ?1 and additional params to ?2, ?3, ...
    #[allow(clippy::too_many_lines)]
    pub fn sql_query_with_carray_and_params<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
        params: &[serde_json::Value],
    ) -> Result<SqlQueryResult> {
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

            carray.bind_to(&mut stmt, 1)?;

            let mut string_cache = Vec::new();
            bind_json_params_offset(&mut stmt, params, 2, &mut string_cache)?;

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

            carray.bind_to(&mut stmt, 1)?;

            let mut string_cache = Vec::new();
            bind_json_params_offset(&mut stmt, params, 2, &mut string_cache)?;

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

/// Trait for types that can be bound to a SQLite `carray(?1)` parameter.
pub trait CarrayBindable {
    /// Bind the array slice into the statement at the given 1-based parameter column index.
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()>;
}

impl CarrayBindable for [u64] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_uint64_static(col, self)
    }
}

impl CarrayBindable for [i64] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_int64_static(col, self)
    }
}

impl CarrayBindable for [i32] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_int32_static(col, self)
    }
}

impl CarrayBindable for [f64] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_double_static(col, self)
    }
}

impl CarrayBindable for [&str] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_text_transient(col, self)
    }
}

impl<T: CarrayBindable + ?Sized> CarrayBindable for &T {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        (*self).bind_to(stmt, col)
    }
}

impl<T> CarrayBindable for Vec<T>
where
    [T]: CarrayBindable,
{
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        self.as_slice().bind_to(stmt, col)
    }
}

/// Fluent SQL interface for queries, parameter binding, and execution.
#[derive(Debug, Clone, Copy)]
pub struct SqlStore<'a> {
    db: &'a crate::Database,
}

impl<'a> SqlStore<'a> {
    /// Execute a SQL query without parameters.
    pub fn query(&self, sql: &str) -> Result<SqlQueryResult> {
        self.db.sql_query(sql, &[])
    }

    /// Execute a SQL query with parameters.
    pub fn query_with_params(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<SqlQueryResult> {
        self.db.sql_query(sql, params)
    }

    /// Execute a SQL query with a carray bound to parameter ?1.
    pub fn query_with_carray<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
    ) -> Result<SqlQueryResult> {
        self.db.sql_query_with_carray(sql, carray)
    }

    /// Execute a SQL query with a carray bound to parameter ?1 and additional params to ?2, ?3, ...
    pub fn query_with_carray_and_params<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
        params: &[serde_json::Value],
    ) -> Result<SqlQueryResult> {
        self.db.sql_query_with_carray_and_params(sql, carray, params)
    }

    /// Execute a mutating SQL statement and return the number of rows affected.
    pub fn execute(&self, sql: &str) -> Result<u64> {
        let res = self.db.sql_query(sql, &[])?;
        Ok(res.rows_affected)
    }
}
