use lilia_core::Result;
use lilia_storage_sqlite::Database;
use serde_json::json;

pub struct DiagnosticsManager;

impl DiagnosticsManager {
    /// Seed diagnostic log entries into SQLite FTS5 index
    pub fn seed_diagnostics(db: &Database) -> Result<()> {
        let logs = [
            (
                "diag_01",
                "WARNING: ESP32 climate node in living_room reported weak WiFi RSSI -85dBm. Packet retransmission rate high.",
            ),
            (
                "diag_02",
                "CRITICAL: Arduino 240V Relay Ch1 thermal safety fuse tripped due to overload current.",
            ),
            (
                "diag_03",
                "NOTICE: Zigbee hallway light firmware OTA upgrade completed successfully to version 2.4.1.",
            ),
            (
                "diag_04",
                "BATTERY: Wireless door contact battery voltage low (2.3V). Replace CR2032 cell.",
            ),
        ];

        for (id, text) in logs {
            db.fts_index("diagnostics", id, text)?;
        }
        Ok(())
    }

    /// Search hardware diagnostics using FTS5 BM25 lexical ranking
    pub fn search_diagnostics(db: &Database, query: &str) -> Result<()> {
        println!("\n  🔎 Searching hardware diagnostics for: \"{query}\"");
        let hits = db.fts_search("diagnostics", query, 5)?;
        if hits.is_empty() {
            println!("    (no diagnostic incidents matched)");
        } else {
            for hit in hits {
                println!(
                    "    -> Incident [{}] (rank: {:.4})\n       \"{}\"",
                    hit.doc_id, hit.rank, hit.snippet
                );
            }
        }
        Ok(())
    }

    /// Execute analytical SQL queries for edge power consumption & protocol distribution
    pub fn run_sql_analytics(db: &Database) -> Result<()> {
        println!("\n[7] Running analytical SQL queries on edge device data:");

        // 1. Power consumption by room
        println!("  📊 Electrical Power Consumption breakdown by room:");
        let power_res = db.sql_query(
            "SELECT
               json_extract(value, '$.room') AS room,
               COUNT(*) AS device_count,
               ROUND(SUM(CAST(json_extract(value, '$.power_watts') AS REAL)), 2) AS total_power_watts
             FROM lilia_json
             WHERE space = ?1
             GROUP BY room
             ORDER BY total_power_watts DESC",
            &[json!("devices")],
        )?;

        for row in &power_res.rows {
            let room = row.get(0).and_then(|v| v.as_str()).unwrap_or("unknown");
            let count = row.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
            let watts = row.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
            println!("     - Room '{room}': {count} device(s), {watts:.1} Watts total");
        }

        // 2. Protocol distribution across local edge hardware
        println!("\n  📊 Protocol distribution across local edge hardware:");
        let proto_res = db.sql_query(
            "SELECT
               json_extract(value, '$.protocol') AS protocol,
               COUNT(*) AS count
             FROM lilia_json
             WHERE space = ?1
             GROUP BY protocol
             ORDER BY count DESC",
            &[json!("devices")],
        )?;

        for row in &proto_res.rows {
            let proto = row.get(0).and_then(|v| v.as_str()).unwrap_or("unknown");
            let count = row.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
            println!("     - Protocol '{proto}': {count} device(s)");
        }

        Ok(())
    }
}
