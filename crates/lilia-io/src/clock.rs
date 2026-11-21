//! Wall-clock time. The browser has no system clock for `std`: it is read through JavaScript.

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    platform::now_ms()
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod platform {
    // reason: `Date.now()` is a whole number of milliseconds, far inside the `i64` range.
    #[allow(clippy::cast_possible_truncation)]
    pub fn now_ms() -> i64 {
        js_sys::Date::now() as i64
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod platform {
    pub fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_clock_reads_a_recent_time() {
        // 2024-01-01 in milliseconds: any machine running these tests is past it.
        assert!(super::now_ms() > 1_704_067_200_000);
    }
}
