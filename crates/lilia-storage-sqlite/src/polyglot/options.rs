#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParquetOptions {
    pub batch_size: usize,
}

impl Default for ParquetOptions {
    fn default() -> Self {
        Self { batch_size: 2048 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvOptions {
    pub delimiter: u8,
    pub has_header: bool,
    pub quote: u8,
    pub skip_rows: usize,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            has_header: true,
            quote: b'"',
            skip_rows: 0,
        }
    }
}

/// Strip surrounding single or double quotes from a string literal.
#[must_use]
pub fn unquote(s: &str) -> &str {
    let trimmed = s.trim();
    if (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
        || (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

/// Parse options for `read_parquet` from an argument list.
#[must_use]
pub fn parse_parquet_args(args: &[&str]) -> (String, ParquetOptions) {
    let mut opts = ParquetOptions::default();
    if args.is_empty() {
        return (String::new(), opts);
    }
    let path = unquote(args[0]).to_string();
    for &arg in &args[1..] {
        let (k, v) = parse_key_value(arg);
        if k.eq_ignore_ascii_case("batch_size") {
            if let Ok(bs) = v.parse::<usize>() {
                if bs > 0 {
                    opts.batch_size = bs;
                }
            }
        }
    }
    (path, opts)
}

/// Parse options for `read_csv` from an argument list.
#[must_use]
pub fn parse_csv_args(args: &[&str]) -> (String, CsvOptions) {
    let mut opts = CsvOptions::default();
    if args.is_empty() {
        return (String::new(), opts);
    }
    let path = unquote(args[0]).to_string();
    for &arg in &args[1..] {
        let (k, v) = parse_key_value(arg);
        match k.to_ascii_lowercase().as_str() {
            "delimiter" | "sep" => {
                let unq = unquote(&v);
                if let Some(ch) = unq.chars().next() {
                    opts.delimiter = ch as u8;
                }
            }
            "header" | "has_header" => {
                let unq = unquote(&v).to_ascii_lowercase();
                opts.has_header = unq == "true" || unq == "1";
            }
            "quote" => {
                let unq = unquote(&v);
                if let Some(ch) = unq.chars().next() {
                    opts.quote = ch as u8;
                }
            }
            "skip_rows" | "offset" => {
                if let Ok(sr) = v.parse::<usize>() {
                    opts.skip_rows = sr;
                }
            }
            _ => {}
        }
    }
    (path, opts)
}

fn parse_key_value(s: &str) -> (String, String) {
    if let Some((k, v)) = s.split_once("=>") {
        (k.trim().to_string(), v.trim().to_string())
    } else if let Some((k, v)) = s.split_once('=') {
        (k.trim().to_string(), v.trim().to_string())
    } else if let Some((k, v)) = s.split_once(':') {
        (k.trim().to_string(), v.trim().to_string())
    } else {
        (s.trim().to_string(), String::new())
    }
}
