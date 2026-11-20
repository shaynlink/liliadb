//! Full-text search result hits, suggestions, and fuzzy match options.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct FtsHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FtsSuggestion {
    pub term: String,
    pub distance: u8,
    pub doc_count: u32,
    pub exact_prefix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsFuzzyHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
    pub edit_distance: u8,
    pub matched_terms: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FtsFuzzyOptions {
    pub max_distance: u8,
    pub prefix: bool,
    pub damerau: bool,
    pub limit: u32,
}

impl Default for FtsFuzzyOptions {
    fn default() -> Self {
        Self {
            max_distance: 1,
            prefix: true,
            damerau: true,
            limit: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsPhoneticHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
    pub matched_terms: Vec<String>,
    pub phonetic_codes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PhoneticAlgorithm {
    Soundex,
    DoubleMetaphone,
    #[default]
    All,
}
