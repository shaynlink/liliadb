pub mod csv;
pub mod cursor;
pub mod options;
pub mod parquet;
pub mod schema;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use lilia_core::Result;
use lilia_sqlite_native::{NativeConnection, VirtualTableFactory};

pub use self::csv::{CsvFactory, CsvTableProvider};
pub use self::cursor::ArrowBatchCursor;
pub use self::options::{CsvOptions, ParquetOptions};
pub use self::parquet::{ParquetFactory, ParquetTableProvider};

/// Register polyglot VFS module factories (`read_parquet`, `read_csv`) on a native connection.
pub fn register_polyglot_factories(conn: &NativeConnection) -> Result<()> {
    conn.register_vtab_factory("read_parquet", Arc::new(ParquetFactory))?;
    conn.register_vtab_factory("read_csv", Arc::new(CsvFactory))?;
    Ok(())
}

/// Preprocesses SQL queries containing ad-hoc `read_parquet(...)` or `read_csv(...)` table-valued functions.
///
/// Discovers invocations, opens the files with zero-copy mmap, registers ephemeral virtual table modules
/// on the active connection, and rewrites the SQL to reference the registered virtual tables.
pub fn preprocess_polyglot_sql(conn: &NativeConnection, sql: &str) -> Result<String> {
    let mut rewritten = sql.to_string();

    // Repeatedly replace invocations until none remain
    while let Some(call) = find_polyglot_call(&rewritten) {
        let args = split_function_args(&call.args_str);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let mut hasher = DefaultHasher::new();
        call.func_name.hash(&mut hasher);
        call.args_str.hash(&mut hasher);
        let hash = hasher.finish();

        let (table_name, provider) = if call.func_name.eq_ignore_ascii_case("read_parquet") {
            let table_name = format!("_vtab_pq_{hash:016x}");
            let provider = ParquetFactory.create(&table_name, &arg_refs)?;
            (table_name, provider)
        } else {
            let table_name = format!("_vtab_csv_{hash:016x}");
            let provider = CsvFactory.create(&table_name, &arg_refs)?;
            (table_name, provider)
        };

        // Register ephemeral virtual table module on this connection
        conn.register_vtab_module(&table_name, provider)?;

        // Replace the call in rewritten SQL
        rewritten = format!(
            "{}{}{}",
            &rewritten[..call.start],
            table_name,
            &rewritten[call.end..]
        );
    }

    Ok(rewritten)
}

struct PolyglotCall {
    func_name: String,
    args_str: String,
    start: usize,
    end: usize,
}

fn find_polyglot_call(sql: &str) -> Option<PolyglotCall> {
    let lower = sql.to_ascii_lowercase();
    let mut search_idx = 0;

    while search_idx < lower.len() {
        let pq_idx = lower[search_idx..].find("read_parquet");
        let csv_idx = lower[search_idx..].find("read_csv");

        let (found_name, found_rel_idx) = match (pq_idx, csv_idx) {
            (Some(p), Some(c)) => {
                if p <= c {
                    ("read_parquet", p)
                } else {
                    ("read_csv", c)
                }
            }
            (Some(p), None) => ("read_parquet", p),
            (None, Some(c)) => ("read_csv", c),
            (None, None) => return None,
        };

        let abs_start = search_idx + found_rel_idx;
        let name_len = found_name.len();

        // Ensure boundary check before func name
        if abs_start > 0 {
            let prev_ch = sql[..abs_start].chars().next_back().unwrap();
            if prev_ch.is_alphanumeric() || prev_ch == '_' {
                search_idx = abs_start + name_len;
                continue;
            }
        }

        // If preceded by "USING", this is DDL `CREATE VIRTUAL TABLE ... USING read_parquet(...)`
        // which natively dispatches to the registered factory module. Do not rewrite!
        let before = sql[..abs_start].trim_end();
        if before.to_ascii_lowercase().ends_with("using") {
            let before_using = &before[..before.len() - 5];
            if before_using.is_empty()
                || before_using.ends_with(|c: char| c.is_whitespace() || c == ';')
            {
                search_idx = abs_start + name_len;
                continue;
            }
        }

        // Find '(' after func name
        let after_name = &sql[abs_start + name_len..];
        let trimmed_after = after_name.trim_start();
        if !trimmed_after.starts_with('(') {
            search_idx = abs_start + name_len;
            continue;
        }

        let open_paren_offset = abs_start + name_len + (after_name.len() - trimmed_after.len());
        // Find matching ')'
        if let Some(close_paren_offset) = find_matching_paren(sql, open_paren_offset) {
            let args_str = sql[open_paren_offset + 1..close_paren_offset].to_string();
            return Some(PolyglotCall {
                func_name: found_name.to_string(),
                args_str,
                start: abs_start,
                end: close_paren_offset + 1,
            });
        }

        search_idx = abs_start + name_len;
    }

    None
}

fn find_matching_paren(s: &str, open_pos: usize) -> Option<usize> {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut depth = 0;

    for (idx, ch) in s[open_pos..].char_indices() {
        let abs_idx = open_pos + idx;
        match ch {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '(' if !in_single_quote && !in_double_quote => depth += 1,
            ')' if !in_single_quote && !in_double_quote => {
                depth -= 1;
                if depth == 0 {
                    return Some(abs_idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_function_args(inner: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut paren_depth = 0;

    for ch in inner.chars() {
        match ch {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                current.push(ch);
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                current.push(ch);
            }
            '(' if !in_single_quote && !in_double_quote => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' if !in_single_quote && !in_double_quote => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                }
                current.push(ch);
            }
            ',' if !in_single_quote && !in_double_quote && paren_depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}
