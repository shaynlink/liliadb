use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CypherResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

impl CypherResult {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut list = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut map = serde_json::Map::new();
            for (col, val) in self.columns.iter().zip(row.iter()) {
                map.insert(col.clone(), val.clone());
            }
            list.push(serde_json::Value::Object(map));
        }
        serde_json::Value::Array(list)
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}
