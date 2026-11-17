//! Hybrid Logical Clock state generator.

use std::sync::Mutex;

use super::timestamp::{now_ms, Hlc, HlcError};

/// Default maximum allowed clock drift in milliseconds (60 seconds).
pub const DEFAULT_MAX_DRIFT_MS: i64 = 60_000;

/// Internal state of a `HybridClock`.
#[derive(Debug)]
struct ClockState {
    last_millis: i64,
    counter: u32,
    node_id: String,
    max_drift_ms: i64,
}

/// Thread-safe generator of monotonically increasing Hybrid Logical Clocks.
#[derive(Debug)]
pub struct HybridClock {
    state: Mutex<ClockState>,
}

impl HybridClock {
    /// Creates a new `HybridClock` bound to `node_id` with default max drift.
    #[must_use]
    pub fn new(node_id: impl Into<String>) -> Self {
        Self::with_max_drift(node_id, DEFAULT_MAX_DRIFT_MS)
    }

    /// Creates a new `HybridClock` bound to `node_id` with custom max drift.
    #[must_use]
    pub fn with_max_drift(node_id: impl Into<String>, max_drift_ms: i64) -> Self {
        Self {
            state: Mutex::new(ClockState {
                last_millis: 0,
                counter: 0,
                node_id: node_id.into(),
                max_drift_ms,
            }),
        }
    }

    /// Returns the node identifier bound to this clock generator.
    #[must_use]
    pub fn node_id(&self) -> String {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.node_id.clone()
    }

    /// Generates a new HLC timestamp for a local event or outbound message.
    ///
    /// Monotonically advances the clock even if physical time went backward.
    pub fn now(&self) -> Hlc {
        self.tick_at(now_ms())
    }

    /// Ticks the clock using explicit physical milliseconds (deterministic testing helper).
    pub fn tick_at(&self, physical_now_ms: i64) -> Hlc {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if physical_now_ms > state.last_millis {
            state.last_millis = physical_now_ms;
            state.counter = 0;
        } else {
            state.counter = state.counter.saturating_add(1);
        }
        Hlc {
            millis: state.last_millis,
            counter: state.counter,
            node_id: state.node_id.clone(),
        }
    }

    /// Updates this clock upon observing a remote HLC timestamp.
    ///
    /// Guarantees that the returned local timestamp will be strictly greater than
    /// `remote` (`new_local > remote`), preserving absolute causality even if the
    /// local physical clock is far behind.
    ///
    /// # Errors
    /// Returns [`HlcError::ClockDriftExceeded`] if `remote` is further ahead of the local
    /// physical clock than `max_drift_ms`.
    pub fn update(&self, remote: &Hlc) -> Result<Hlc, HlcError> {
        self.update_at(remote, now_ms())
    }

    /// Updates this clock with an explicit local physical time (deterministic testing helper).
    ///
    /// # Errors
    /// Returns [`HlcError::ClockDriftExceeded`] if `remote` is further ahead of `physical_now_ms`
    /// than `max_drift_ms`.
    pub fn update_at(&self, remote: &Hlc, physical_now_ms: i64) -> Result<Hlc, HlcError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Guard against excessive forward clock drift
        if remote.millis - physical_now_ms > state.max_drift_ms {
            return Err(HlcError::ClockDriftExceeded {
                local_ms: physical_now_ms,
                remote_ms: remote.millis,
                max_drift_ms: state.max_drift_ms,
            });
        }

        let l_prev = state.last_millis;
        let l_new = l_prev.max(physical_now_ms).max(remote.millis);

        if l_new == l_prev && l_new == remote.millis {
            state.counter = state.counter.max(remote.counter).saturating_add(1);
        } else if l_new == l_prev {
            state.counter = state.counter.saturating_add(1);
        } else if l_new == remote.millis {
            state.counter = remote.counter.saturating_add(1);
        } else {
            state.counter = 0;
        }

        state.last_millis = l_new;

        Ok(Hlc {
            millis: state.last_millis,
            counter: state.counter,
            node_id: state.node_id.clone(),
        })
    }
}
