mod args;
mod daemon;
mod models;
mod zk;

use clap::Parser;
use serde_json::json;

use args::{Arguments, Command, Output};

fn main() {
    if let Err(error) = run() {
        let structured = error
            .downcast_ref::<lilia_core::LiliaError>()
            .cloned()
            .unwrap_or_else(|| {
                let code = if error.downcast_ref::<serde_json::Error>().is_some() {
                    lilia_core::ErrorCode::InvalidInput
                } else if error.downcast_ref::<std::io::Error>().is_some() {
                    lilia_core::ErrorCode::Io
                } else {
                    lilia_core::ErrorCode::Storage
                };
                lilia_core::LiliaError::new(code, error.to_string(), false)
            });
        let payload = serde_json::to_string(&json!({"ok": false, "error": structured}))
            .unwrap_or_else(|_| "{\"ok\":false}".into());
        eprintln!("{payload}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let value = match arguments.command {
        Command::Daemon { command } => {
            daemon::execute(command, &arguments.database, &arguments.open)?
        }
        Command::Zk { command } => zk::execute(command, &arguments.database, &arguments.open)?,
        Command::Database(command) => {
            models::execute(command, &arguments.database, &arguments.open)?
        }
    };
    print_value(arguments.output, &value)?;
    Ok(())
}

fn print_value(output: Output, value: &serde_json::Value) -> anyhow::Result<()> {
    match output {
        Output::Human => {
            if let (Some(cols), Some(rows)) = (
                value.get("columns").and_then(serde_json::Value::as_array),
                value.get("rows").and_then(serde_json::Value::as_array),
            ) {
                if !cols.is_empty() {
                    let col_names: Vec<String> = cols
                        .iter()
                        .map(|c| c.as_str().unwrap_or("").to_owned())
                        .collect();
                    print_table(&col_names, rows);
                    return Ok(());
                }
            } else if let Some(items) = value.as_array() {
                if items.is_empty() {
                    println!("(empty list)");
                    return Ok(());
                }
                if let Some(first_obj) = items.first().and_then(|item| item.as_object()) {
                    let cols: Vec<String> = first_obj.keys().cloned().collect();
                    let rows: Vec<serde_json::Value> = items
                        .iter()
                        .map(|item| {
                            let obj = item.as_object();
                            let row: Vec<serde_json::Value> = cols
                                .iter()
                                .map(|col| {
                                    obj.and_then(|o| o.get(col))
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null)
                                })
                                .collect();
                            serde_json::Value::Array(row)
                        })
                        .collect();
                    print_table(&cols, &rows);
                    return Ok(());
                }
            }
            println!("{}", serde_json::to_string_pretty(value)?);
        }
        Output::Json => println!("{}", serde_json::to_string(value)?),
        Output::Jsonl => {
            if let (Some(cols), Some(rows)) = (
                value.get("columns").and_then(serde_json::Value::as_array),
                value.get("rows").and_then(serde_json::Value::as_array),
            ) {
                for row in rows {
                    if let Some(row_items) = row.as_array() {
                        let mut map = serde_json::Map::new();
                        for (i, col) in cols.iter().enumerate() {
                            if let Some(col_name) = col.as_str() {
                                let item = row_items.get(i).unwrap_or(&serde_json::Value::Null);
                                map.insert(col_name.to_owned(), item.clone());
                            }
                        }
                        println!("{}", serde_json::to_string(&map)?);
                    }
                }
                return Ok(());
            } else if let Some(items) = value.as_array() {
                for item in items {
                    println!("{}", serde_json::to_string(item)?);
                }
                return Ok(());
            }
            println!("{}", serde_json::to_string(value)?);
        }
    }
    Ok(())
}

fn print_table(cols: &[String], rows: &[serde_json::Value]) {
    let cell = |item: &serde_json::Value| match item {
        serde_json::Value::String(s) => s.clone(),
        _ => item.to_string(),
    };
    let mut widths: Vec<usize> = cols.iter().map(String::len).collect();
    for items in rows.iter().filter_map(serde_json::Value::as_array) {
        for (width, item) in widths.iter_mut().zip(items) {
            *width = (*width).max(cell(item).len());
        }
    }
    let line = |cells: Vec<String>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let w = widths.get(i).copied().unwrap_or(0);
                format!("{text:<w$}")
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    println!("{}", line(cols.to_vec()));
    println!(
        "{}",
        widths
            .iter()
            .map(|&w| "-".repeat(w))
            .collect::<Vec<_>>()
            .join("-+-")
    );
    for items in rows.iter().filter_map(serde_json::Value::as_array) {
        println!("{}", line(items.iter().map(cell).collect()));
    }
    println!("({} rows)", rows.len());
}
