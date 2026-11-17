//! HLC timestamp type and parsing.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ErrorCode, LiliaError};

/// Returns current physical timestamp in milliseconds since UNIX epoch.
#[must_use]
pub fn now_ms() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
            })
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Errors related to Hybrid Logical Clocks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HlcError {
    #[error("remote clock drift exceeded: local={local_ms}ms, remote={remote_ms}ms, max_drift={max_drift_ms}ms")]
    ClockDriftExceeded {
        local_ms: i64,
        remote_ms: i64,
        max_drift_ms: i64,
    },
    #[error("invalid HLC string format: {0}")]
    InvalidFormat(String),
}

impl From<HlcError> for LiliaError {
    fn from(err: HlcError) -> Self {
        match err {
            HlcError::ClockDriftExceeded { .. } => {
                LiliaError::new(ErrorCode::Conflict, err.to_string(), false)
            }
            HlcError::InvalidFormat(..) => {
                LiliaError::new(ErrorCode::InvalidInput, err.to_string(), false)
            }
        }
    }
}

/// An immutable Hybrid Logical Clock timestamp.
///
/// Combines an approximate physical time in milliseconds, a monotonic logical counter,
/// and a unique node/actor identifier for deterministic total ordering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    /// Physical or highest observed logical timestamp in milliseconds.
    pub millis: i64,
    /// Monotonic logical counter disambiguating events within the same physical tick.
    pub counter: u32,
    /// Replica or device identifier used as a deterministic tie-breaker.
    pub node_id: String,
}

impl Hlc {
    /// Creates a new `Hlc` timestamp.
    #[must_use]
    pub fn new(millis: i64, counter: u32, node_id: impl Into<String>) -> Self {
        Self {
            millis,
            counter,
            node_id: node_id.into(),
        }
    }

    /// Formats the HLC in a compact form `"<millis>.<counter>"`.
    #[must_use]
    pub fn display_compact(&self) -> String {
        format!("{}.{}", self.millis, self.counter)
    }

    /// Returns true if this HLC causally strictly precedes `other` under the total order.
    #[must_use]
    pub fn precedes(&self, other: &Self) -> bool {
        self < other
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.millis
            .cmp(&other.millis)
            .then_with(|| self.counter.cmp(&other.counter))
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.node_id.is_empty() {
            write!(f, "{}.{}", self.millis, self.counter)
        } else {
            write!(f, "{}.{}@{}", self.millis, self.counter, self.node_id)
        }
    }
}

impl FromStr for Hlc {
    type Err = HlcError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (time_part, node_id) = if let Some((left, right)) = s.split_once('@') {
            (left, right.to_string())
        } else if let Some((left, right)) = s.rsplit_once(':') {
            if right.chars().all(|c| c.is_ascii_digit()) && !left.contains('.') {
                (s, String::new())
            } else {
                (left, right.to_string())
            }
        } else {
            (s, String::new())
        };

        let (millis_str, counter_str) = if let Some((m, c)) = time_part.split_once('.') {
            (m, c)
        } else if let Some((m, c)) = time_part.split_once(':') {
            (m, c)
        } else {
            return Err(HlcError::InvalidFormat(format!(
                "expected <millis>.<counter>, got '{s}'"
            )));
        };

        let millis = millis_str
            .parse::<i64>()
            .map_err(|_| HlcError::InvalidFormat(format!("invalid millis in '{s}'")))?;
        let counter = counter_str
            .parse::<u32>()
            .map_err(|_| HlcError::InvalidFormat(format!("invalid counter in '{s}'")))?;

        Ok(Self {
            millis,
            counter,
            node_id,
        })
    }
}

impl Serialize for Hlc {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_string())
        } else {
            #[derive(Serialize)]
            struct RawHlc<'a> {
                millis: i64,
                counter: u32,
                node_id: &'a str,
            }
            RawHlc {
                millis: self.millis,
                counter: self.counter,
                node_id: &self.node_id,
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Hlc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Obj {
                millis: i64,
                counter: u32,
                #[serde(default)]
                node_id: String,
            },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Str(s) => Self::from_str(&s).map_err(serde::de::Error::custom),
            Repr::Obj {
                millis,
                counter,
                node_id,
            } => Ok(Self::new(millis, counter, node_id)),
        }
    }
}
