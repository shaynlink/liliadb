use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lilia_core::{BatchOperation, Durability};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

mod devices;
mod diagnostics;
mod engine;

use devices::default_edge_inventory;
use diagnostics::DiagnosticsManager;
use engine::AutomationEngine;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    println!("==================================================================");
    println!("⚡ LiliaDB Micro-Project: Smart Home Edge Hub & IoT Gateway (Rust)");
    println!("   (Low-Level Native Systems Architecture for Raspberry Pi & MCUs)");
    println!("==================================================================");

    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("smarthome.lilia");

    println!("\n[1] Initializing low-level embedded LiliaDB engine at:");
    println!("    {}", db_path.display());

    // Open LiliaDB directly in native Rust with balanced durability (WAL for high throughput)
    let mut options = DatabaseOptions::durable(&db_path);
    options.durability = Durability::Balanced;
    let db = Arc::new(Database::open(options)?);

    // -------------------------------------------------------------------------
    // [2] Register Hardware Devices & Microcontrollers (JSON Documents)
    // -------------------------------------------------------------------------
    println!("\n[2] Registering hardware devices and microcontrollers in 'devices' space...");
    let inventory = default_edge_inventory();
    let mut dev_batch = Vec::new();
    for dev in &inventory {
        dev_batch.push(BatchOperation::JsonPut {
            space: "devices".into(),
            id: dev.id.clone(),
            value: serde_json::to_value(dev)?,
            if_version: None,
        });
        println!(
            "  ✓ Registered [{}] ({}) in '{}' [{}]",
            dev.id, dev.name, dev.room, dev.protocol
        );
    }
    db.batch(&dev_batch)?;

    // -------------------------------------------------------------------------
    // [3] Construct Automation Trigger Graph & Mesh Network Topology
    // -------------------------------------------------------------------------
    AutomationEngine::build_graph_pipelines(&db)?;

    // -------------------------------------------------------------------------
    // [4] Start Tokio Reactive CDC Rule Worker on Native Broadcast Channel
    // -------------------------------------------------------------------------
    println!("\n[4] Initializing Native Reactive Automation Engine on db.subscribe()...");
    let rx = db.subscribe();
    let worker_handle = AutomationEngine::start_reactive_rule_worker(Arc::clone(&db), rx);

    // -------------------------------------------------------------------------
    // [5] Microcontroller Telemetry & Heartbeat Emulation (KV with TTL)
    // -------------------------------------------------------------------------
    println!("\n[5] Simulating Microcontroller Telemetry & Heartbeats:");

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;

    // A. Heartbeat registration with TTL (detecting alive vs dead nodes)
    let esp32_hb_key = b"heartbeat:mcu_esp32_climate".to_vec();
    let esp32_hb_val = json!({"status": "ALIVE", "wifi_rssi_dbm": -58}).to_string().into_bytes();
    // 2500 ms TTL
    db.batch(&[
        BatchOperation::KvSet {
            namespace: "heartbeats".into(),
            key: esp32_hb_key,
            value: esp32_hb_val,
            if_version: None,
            expires_at_ms: Some(now_ms + 2500),
        },
        BatchOperation::KvSet {
            namespace: "heartbeats".into(),
            key: b"heartbeat:mcu_arduino_relay".to_vec(),
            value: json!({"status": "ALIVE", "uart_baud": 115200}).to_string().into_bytes(),
            if_version: None,
            expires_at_ms: Some(now_ms + 2500),
        },
    ])?;
    println!("  💓 ESP32 published alive heartbeat (TTL: 2500ms)");
    println!("  💓 Arduino Uno published UART heartbeat (TTL: 2500ms)");

    // B. Emulating incoming PIR sensor trigger on Raspberry Pi GPIO 18
    println!("\n  [Simulating Microcontroller Event 1: Motion detected on GPIO pin 18]");
    let motion_key = b"sensor:sensor_pir_hallway:motion".to_vec();
    db.batch(&[BatchOperation::KvSet {
        namespace: "telemetry".into(),
        key: motion_key,
        value: b"true".to_vec(),
        if_version: None,
        expires_at_ms: None,
    }])?;

    // Allow tokio task to process event
    tokio::time::sleep(Duration::from_millis(50)).await;

    // C. Emulating incoming Temperature Drop from ESP32 DHT22 sensor
    println!("\n  [Simulating Microcontroller Event 2: ESP32 reports 13.8°C via WiFi]");
    let temp_key = b"sensor:mcu_esp32_climate:temp".to_vec();
    db.batch(&[BatchOperation::KvSet {
        namespace: "telemetry".into(),
        key: temp_key,
        value: b"13.8".to_vec(),
        if_version: None,
        expires_at_ms: None,
    }])?;

    tokio::time::sleep(Duration::from_millis(50)).await;

    // -------------------------------------------------------------------------
    // [6] Hardware Incident Logging & FTS5 Lexical Search
    // -------------------------------------------------------------------------
    println!("\n[6] Logging diagnostic events and running FTS5 incident analysis:");
    DiagnosticsManager::seed_diagnostics(&db)?;
    println!("  ✓ Indexed 4 diagnostic messages into SQLite FTS5 engine.");

    DiagnosticsManager::search_diagnostics(&db, "thermal overload OR battery low")?;

    // -------------------------------------------------------------------------
    // [7] Energy Consumption & Protocol Analytics (Declarative SQL)
    // -------------------------------------------------------------------------
    DiagnosticsManager::run_sql_analytics(&db)?;

    // -------------------------------------------------------------------------
    // [8] Cleanup and Integrity Verification
    // -------------------------------------------------------------------------
    println!("\n[8] Verifying storage integrity and shutting down cleanly...");
    worker_handle.abort();

    let is_healthy = db.integrity_check()?;
    println!(
        "  Storage integrity status: {}",
        if is_healthy { "HEALTHY (WAL OK)" } else { "CORRUPT" }
    );

    println!("\n[✓] Smart Home Edge Hub native Rust project completed successfully!");
    Ok(())
}
