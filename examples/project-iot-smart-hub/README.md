# Micro-Project: Smart Home Edge Hub & IoT Gateway (Low-Level Native Rust)

A high-performance, 100% local-first systems application written in **low-level native Rust**, designed for **Raspberry Pi, industrial SBCs, and embedded edge gateways**, interfacing with microcontrollers (**Arduino via serial UART, ESP32 via WiFi/MQTT, GPIO sensors, relays**).

---

## Why Native Rust?

Edge gateways demand minimal memory footprints, zero garbage collection pauses, and sub-millisecond reaction times. By using LiliaDB directly in native Rust:
- **Zero FFI / Zero Node.js runtime overhead**: Embedded SQLite in-process storage.
- **Sub-millisecond reaction (< 200 µs)**: Telemetry mutations trigger immediate rule evaluation over Tokio channels.
- **Strongly Typed Multimodel Storage**: Compile-time type safety across KV, JSON, Graph, FTS5, and SQL operations.

---

## Architecture: Microcontrollers to Native Rust Engine

```mermaid
graph TD
    subgraph "Hardware & Microcontrollers"
        ESP32["ESP32 (WiFi/MQTT): Climate DHT22"]
        Arduino["Arduino Uno (UART/Serial): 240V Relays"]
        RPI_GPIO["Raspberry Pi (GPIO 18): PIR Motion Sensor"]
        ZigbeeValve["Zigbee Mesh: Radiator Valve"]
    end

    subgraph "Native Rust Edge Engine (lilia-storage-sqlite)"
        KV["db.kv_set / get: Telemetry & MCU Heartbeats (TTL)"]
        JSON["db.json_put / get: Hardware Device Registry"]
        Graph["db.graph_reachability: Trigger Pipelines & Mesh Topology"]
        FTS["db.fts_index / search: Hardware Diagnostics & Log Search"]
        SQL["db.sql_query: Power Consumption Analytics"]
        CDC["db.subscribe(): Tokio Broadcast Reactive Engine"]
    end

    ESP32 -->|Telemetry| KV
    Arduino -->|Heartbeat| KV
    RPI_GPIO -->|Motion Event| KV

    KV -->|Mutations| CDC
    CDC -->|Evaluate Graph Rules| Graph
    CDC -->|Atomic Command (db.batch)| Arduino
    CDC -->|Atomic Command (db.batch)| ZigbeeValve
    CDC -->|Log Incident| FTS
```

---

## How to Run

From the root of the repository:

```bash
cargo run -p project-iot-smart-hub
```

Or run in release mode for maximum performance:

```bash
cargo run --release -p project-iot-smart-hub
```

---

## Key Native Capabilities Explored

### 1. Opening Embedded Engine with Balanced WAL Durability
```rust
let options = DatabaseOptions::balanced(&db_path);
let db = Arc::new(Database::open(options)?);
```

### 2. Device Registry & JSON Storage
```rust
db.json_put("devices", &dev.id, serde_json::to_value(dev)?)?;
```

### 3. Mesh Reachability & Automation Graph Pipelines
```rust
// Multi-hop reachability query
let reachable = db.graph_reachability("mesh", &["hub_gateway".to_string()], Direction::Outgoing)?;
```

### 4. Microcontroller Heartbeats with Automatic TTL Expiration (KV)
```rust
// Self-expiring heartbeat: if node dies, entry disappears automatically
db.kv_set("heartbeats", &esp32_key, &esp32_val, None, Some(now_ms + 2500))?;
```

### 5. Tokio Reactive CDC Subscriber (`db.subscribe()`)
```rust
let mut rx = db.subscribe();
tokio::spawn(async move {
    while let Ok(event) = rx.recv().await {
        if let DatabaseEvent::KvMutated { namespace, key, .. } = event {
            // Evaluates rules in < 200 µs without polling
        }
    }
});
```

### 6. Atomic Multi-Model Transactions (`db.batch()`)
Actuate relay, log incident in FTS5, and increment counter in one single ACID commit:
```rust
db.batch(&[
    BatchOperation::JsonPut { space: "devices".into(), id: "actuator_light".into(), value: new_state, if_version: None },
    BatchOperation::FtsIndex { collection: "diagnostics".into(), doc_id: "act_101".into(), content: "Light switched ON".into() },
    BatchOperation::KvSet { namespace: "counters".into(), key: b"actuations".to_vec(), value: b"1".to_vec(), if_version: None, expires_at_ms: None },
])?;
```

### 7. Analytical SQL Projections
```rust
let power_res = db.sql_query(
    "SELECT json_extract(value, '$.room') AS room, SUM(CAST(json_extract(value, '$.power_watts') AS REAL)) AS total_watts
     FROM lilia_json WHERE space = ?1 GROUP BY room",
    &[json!("devices")],
)?;
```
