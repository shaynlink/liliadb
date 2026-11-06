use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub protocol: String,
    pub room: String,
    pub power_watts: f64,
    pub state: Value,
}

impl Device {
    pub fn new(
        id: &str,
        name: &str,
        device_type: &str,
        protocol: &str,
        room: &str,
        power_watts: f64,
        state: Value,
    ) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            device_type: device_type.to_string(),
            protocol: protocol.to_string(),
            room: room.to_string(),
            power_watts,
            state,
        }
    }
}

/// Simulated Arduino UART Serial Frame (e.g., transmitted over /dev/ttyACM0)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ArduinoUartPacket {
    pub command: String,
    pub channel: u8,
    pub state: bool,
}

#[allow(dead_code)]
impl ArduinoUartPacket {
    pub fn parse(raw_frame: &str) -> Option<Self> {
        // Expected format: "CH<num>:<ON|OFF>" e.g. "CH1:ON"
        let parts: Vec<&str> = raw_frame.trim().split(':').collect();
        if parts.len() != 2 {
            return None;
        }
        let ch_str = parts[0].strip_prefix("CH")?;
        let channel = ch_str.parse::<u8>().ok()?;
        let state = match parts[1] {
            "ON" => true,
            "OFF" => false,
            _ => return None,
        };
        Some(Self {
            command: raw_frame.to_string(),
            channel,
            state,
        })
    }

    pub fn to_serial_bytes(&self) -> Vec<u8> {
        let state_str = if self.state { "ON" } else { "OFF" };
        format!("CH{}:{}\r\n", self.channel, state_str).into_bytes()
    }
}

/// Simulated ESP32 WiFi / MQTT Telemetry Packet
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esp32Telemetry {
    pub node_id: String,
    pub temperature_c: f64,
    pub humidity_pct: f64,
    pub wifi_rssi_dbm: i32,
    pub uptime_s: u64,
}

/// Returns a pre-configured inventory of local edge devices.
pub fn default_edge_inventory() -> Vec<Device> {
    vec![
        Device::new(
            "mcu_arduino_relay",
            "Arduino Uno 240V Power Relay Board",
            "actuator",
            "serial_uart",
            "utility_room",
            2200.0,
            json!({"relay_1": "OFF", "relay_2": "OFF"}),
        ),
        Device::new(
            "mcu_esp32_climate",
            "ESP32 Living Room Multi-Sensor Node",
            "sensor",
            "wifi_mqtt",
            "living_room",
            2.0,
            json!({"temperature_c": 19.4, "humidity_pct": 48}),
        ),
        Device::new(
            "sensor_pir_hallway",
            "Raspberry Pi GPIO PIR Motion Sensor",
            "sensor",
            "rpi_gpio",
            "hallway",
            0.5,
            json!({"motion_detected": false}),
        ),
        Device::new(
            "actuator_light_hallway",
            "Zigbee Dimmable LED Strip",
            "actuator",
            "zigbee",
            "hallway",
            35.0,
            json!({"power": "OFF", "brightness": 80}),
        ),
        Device::new(
            "actuator_radiator_living",
            "Zigbee Thermostatic Radiator Valve",
            "actuator",
            "zigbee",
            "living_room",
            15.0,
            json!({"valve_open_pct": 0, "target_temp_c": 21.0}),
        ),
    ]
}
