use serde::de::DeserializeOwned;
use serde_json::Value;
use std::ops::{Deref, Index};

/// A record passing through the reactive dataflow pipeline.
/// Provides ergonomic access to fields, nested/dotted properties, and typed deserialization.
#[derive(Debug, Clone, PartialEq)]
pub struct DataflowDoc {
    pub id: String,
    pub data: Value,
}

impl DataflowDoc {
    #[must_use]
    pub fn new(id: impl Into<String>, data: Value) -> Self {
        Self {
            id: id.into(),
            data,
        }
    }

    /// Resolves a dotted or nested path, e.g. `"region.name"` or `"status"`.
    #[must_use]
    pub fn resolve_path<'a>(&'a self, path: &str) -> Option<&'a Value> {
        if path.is_empty() {
            return Some(&self.data);
        }

        // 1. Direct key match (e.g. data has field "region.name" literally)
        if let Some(val) = self.data.get(path) {
            return Some(val);
        }

        // 2. Traversal through dotted segments (e.g. data["region"]["name"])
        let mut curr = &self.data;
        for part in path.split('.') {
            match curr {
                Value::Object(map) => {
                    curr = map.get(part)?;
                }
                Value::Array(arr) => {
                    let idx = part.parse::<usize>().ok()?;
                    curr = arr.get(idx)?;
                }
                _ => return None,
            }
        }
        Some(curr)
    }

    /// Extracts a string field value by key or dotted path.
    #[must_use]
    pub fn get_str(&self, path: &str) -> Option<&str> {
        self.resolve_path(path).and_then(Value::as_str)
    }

    /// Extracts a floating-point numeric value by key or dotted path.
    #[must_use]
    pub fn get_f64(&self, path: &str) -> Option<f64> {
        self.resolve_path(path).and_then(|v| match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse::<f64>().ok(),
            _ => None,
        })
    }

    /// Extracts an integer value by key or dotted path.
    #[must_use]
    pub fn get_i64(&self, path: &str) -> Option<i64> {
        self.resolve_path(path).and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse::<i64>().ok(),
            _ => None,
        })
    }

    /// Extracts a boolean value by key or dotted path.
    #[must_use]
    pub fn get_bool(&self, path: &str) -> Option<bool> {
        self.resolve_path(path).and_then(Value::as_bool)
    }

    /// Sets or merges a field into the document.
    pub fn set_field(&mut self, key: &str, val: Value) {
        if let Value::Object(ref mut map) = self.data {
            map.insert(key.to_string(), val);
        } else {
            let mut map = serde_json::Map::new();
            map.insert(key.to_string(), val);
            self.data = Value::Object(map);
        }
    }

    /// Attempts to deserialize the document into a strongly-typed struct `T`.
    ///
    /// # Errors
    /// Returns a deserialization error if the JSON value does not match `T`.
    pub fn to_typed<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.data.clone())
    }
}

impl Deref for DataflowDoc {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl Index<&str> for DataflowDoc {
    type Output = Value;

    fn index(&self, index: &str) -> &Self::Output {
        self.resolve_path(index).unwrap_or(&Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_dataflow_doc_path_resolution() {
        let doc = DataflowDoc::new(
            "o1",
            json!({
                "status": "delivered",
                "amount": 99.5,
                "customer_id": "c1",
                "region": {
                    "name": "Europe",
                    "code": "EU"
                }
            }),
        );

        assert_eq!(doc["status"], "delivered");
        assert_eq!(doc.get_str("status"), Some("delivered"));
        assert_eq!(doc.get_f64("amount"), Some(99.5));
        assert_eq!(doc.get_str("region.name"), Some("Europe"));
        assert_eq!(doc["region.name"], "Europe");
        assert_eq!(doc["region"]["code"], "EU");
    }
}
