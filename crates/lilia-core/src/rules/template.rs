use serde_json::Value;

/// Evaluation context available for variable substitution in rule actions.
#[derive(Debug, Clone, Default)]
pub struct RuleContext {
    /// Current ISO 8601 UTC timestamp (e.g. "2026-11-13T04:15:00Z").
    pub now_iso: String,
    /// Current UNIX epoch timestamp in milliseconds.
    pub now_ms: i64,
    /// Triggering mutation key or document ID.
    pub key: String,
    /// Triggering document ID.
    pub id: String,
    /// Triggering collection or namespace.
    pub collection: String,
    /// Full event payload / document if available.
    pub event_payload: Option<Value>,
    /// Sliding window metrics if triggered by a temporal window.
    pub window_count: Option<u64>,
    pub window_sum: Option<f64>,
    pub window_avg: Option<f64>,
}

impl RuleContext {
    #[must_use]
    pub fn new(now_ms: i64, collection: impl Into<String>, id: impl Into<String>) -> Self {
        let coll = collection.into();
        let i = id.into();
        let now_iso = format_iso_timestamp(now_ms);
        Self {
            now_iso,
            now_ms,
            key: i.clone(),
            id: i,
            collection: coll,
            event_payload: None,
            window_count: None,
            window_sum: None,
            window_avg: None,
        }
    }

    #[must_use]
    pub fn with_payload(mut self, payload: Value) -> Self {
        self.event_payload = Some(payload);
        self
    }

    #[must_use]
    pub fn with_window_stats(mut self, count: u64, sum: Option<f64>, avg: Option<f64>) -> Self {
        self.window_count = Some(count);
        self.window_sum = sum;
        self.window_avg = avg;
        self
    }

    /// Resolve a template variable such as `"$now"`, `"$key"`, `"$event.status"`.
    #[must_use]
    pub fn resolve_variable(&self, var: &str) -> Option<Value> {
        match var {
            "$now" => Some(Value::String(self.now_iso.clone())),
            "$now_ms" => Some(Value::Number(self.now_ms.into())),
            "$key" => Some(Value::String(self.key.clone())),
            "$id" => Some(Value::String(self.id.clone())),
            "$collection" | "$space" => Some(Value::String(self.collection.clone())),
            "$window.count" => self.window_count.map(|c| Value::Number(c.into())),
            "$window.sum" => self.window_sum.and_then(serde_json::Number::from_f64).map(Value::Number),
            "$window.avg" => self.window_avg.and_then(serde_json::Number::from_f64).map(Value::Number),
            _ => {
                if let Some(path) = var.strip_prefix("$event.") {
                    if let Some(ref payload) = self.event_payload {
                        return extract_path(payload, path);
                    }
                }
                None
            }
        }
    }
}

/// Recursively interpolate template variables within a JSON structure.
#[must_use]
pub fn interpolate_value(val: &Value, ctx: &RuleContext) -> Value {
    match val {
        Value::String(s) => {
            if s.starts_with('$') {
                if let Some(resolved) = ctx.resolve_variable(s) {
                    return resolved;
                }
            }
            // Also support inline replacement e.g. "prefix_$key"
            if s.contains('$') {
                let mut out = s.clone();
                if out.contains("$now") {
                    out = out.replace("$now", &ctx.now_iso);
                }
                if out.contains("$now_ms") {
                    out = out.replace("$now_ms", &ctx.now_ms.to_string());
                }
                if out.contains("$key") {
                    out = out.replace("$key", &ctx.key);
                }
                if out.contains("$id") {
                    out = out.replace("$id", &ctx.id);
                }
                if out.contains("$collection") {
                    out = out.replace("$collection", &ctx.collection);
                }
                Value::String(out)
            } else {
                Value::String(s.clone())
            }
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|item| interpolate_value(item, ctx)).collect())
        }
        Value::Object(map) => {
            let mut new_map = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                new_map.insert(k.clone(), interpolate_value(v, ctx));
            }
            Value::Object(new_map)
        }
        _ => val.clone(),
    }
}

/// Interpolate a string template (for keys, namespaces, etc.).
#[must_use]
pub fn interpolate_string(s: &str, ctx: &RuleContext) -> String {
    if !s.contains('$') {
        return s.to_string();
    }
    let mut out = s.to_string();
    if out.contains("$now") {
        out = out.replace("$now", &ctx.now_iso);
    }
    if out.contains("$now_ms") {
        out = out.replace("$now_ms", &ctx.now_ms.to_string());
    }
    if out.contains("$key") {
        out = out.replace("$key", &ctx.key);
    }
    if out.contains("$id") {
        out = out.replace("$id", &ctx.id);
    }
    if out.contains("$collection") {
        out = out.replace("$collection", &ctx.collection);
    }
    out
}

fn extract_path(val: &Value, path: &str) -> Option<Value> {
    let clean = path.trim_start_matches("$.").trim_start_matches('/');
    let mut curr = val;
    for segment in clean.split('.') {
        if segment.is_empty() {
            continue;
        }
        match curr {
            Value::Object(map) => {
                curr = map.get(segment)?;
            }
            Value::Array(arr) => {
                let idx: usize = segment.parse().ok()?;
                curr = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(curr.clone())
}

fn format_iso_timestamp(timestamp_ms: i64) -> String {
    let secs = timestamp_ms.div_euclid(1000);
    let nsecs = u32::try_from(timestamp_ms.rem_euclid(1000) * 1_000_000).unwrap_or(0);
    // Simple fast formatting for ISO 8601 UTC
    let days_since_epoch = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400);

    let (year, month, day) = civil_from_days(days_since_epoch);
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    let _ = nsecs; // Keep standard second precision or millisecond if needed
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

// Convert days since UNIX epoch (1970-01-01) to civil year, month, day
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_template_interpolation() {
        let ctx = RuleContext::new(1_700_000_000_000, "events", "p1")
            .with_payload(json!({ "heart_rate": 150, "status": "critical" }));

        let doc = json!({
            "target": "$key",
            "now": "$now",
            "hr": "$event.heart_rate",
            "msg": "Alert for $key at $now"
        });

        let interpolated = interpolate_value(&doc, &ctx);
        assert_eq!(interpolated["target"], "p1");
        assert_eq!(interpolated["hr"], 150);
        assert!(interpolated["now"].as_str().unwrap().contains("T"));
    }
}
