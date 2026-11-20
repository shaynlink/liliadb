//! Markdown tables and number formatting.

use crate::record::Record;

/// A Markdown table; columns whose cells are all numbers (or `—`) are right-aligned.
#[derive(Debug, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    #[must_use]
    pub fn new<S: Into<String>>(headers: impl IntoIterator<Item = S>) -> Self {
        Self { headers: headers.into_iter().map(Into::into).collect(), rows: Vec::new() }
    }

    pub fn row<S: Into<String>>(&mut self, cells: impl IntoIterator<Item = S>) {
        self.rows.push(cells.into_iter().map(Into::into).collect());
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn render(&self) -> String {
        let numeric = |i: usize| {
            i > 0
                && self.rows.iter().all(|row| {
                    row.get(i).is_none_or(|cell| cell == "—" || cell.starts_with(|c: char| c.is_ascii_digit()))
                })
        };
        let mut out = format!("| {} |\n|", self.headers.join(" | "));
        for i in 0..self.headers.len() {
            out.push_str(if numeric(i) { " ---: |" } else { " :--- |" });
        }
        out.push('\n');
        for row in &self.rows {
            out.push_str(&format!("| {} |\n", row.join(" | ")));
        }
        out
    }
}

/// `1234567.8` → `1,234,568`; small values keep significant digits.
#[must_use]
pub fn count(x: f64) -> String {
    if !x.is_finite() {
        return "—".into();
    }
    if x < 10.0 {
        return format!("{x:.2}");
    }
    if x < 100.0 {
        return format!("{x:.1}");
    }
    let digits = format!("{:.0}", x.round());
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Microseconds as µs, ms or s.
#[must_use]
pub fn duration_us(us: f64) -> String {
    if us < 10.0 {
        format!("{us:.2} µs")
    } else if us < 1_000.0 {
        format!("{us:.0} µs")
    } else if us < 1_000_000.0 {
        format!("{:.2} ms", us / 1e3)
    } else {
        format!("{:.2} s", us / 1e6)
    }
}

#[must_use]
pub fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
}

#[must_use]
pub fn ratio(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.2}×")
    } else {
        "—".into()
    }
}

/// Integer parameter of a record.
#[must_use]
pub fn param_u64(record: &Record, name: &str) -> Option<u64> {
    record.params.get(name).and_then(serde_json::Value::as_u64)
}

/// String parameter of a record.
#[must_use]
pub fn param_str<'a>(record: &'a Record, name: &str) -> Option<&'a str> {
    record.params.get(name).and_then(serde_json::Value::as_str)
}
