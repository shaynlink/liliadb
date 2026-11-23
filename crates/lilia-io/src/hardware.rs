//! What the engine sizes itself from: probed at open, never assumed at compile time.

/// The machine the database runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareProfile {
    /// Virtual-memory page size in bytes (16 KiB on Apple Silicon, usually 4 KiB elsewhere).
    pub page_size: usize,
    /// Cache-line size in bytes (128 on Apple Silicon, 64 on most x86-64 and ARM cores).
    pub cache_line: usize,
    /// Logical CPUs available to this process.
    pub logical_cpus: usize,
    /// Physical cores of the fastest performance level (all cores on symmetric machines).
    pub performance_cores: usize,
    /// Physical cores of slower, efficiency-oriented levels (0 on symmetric machines).
    pub efficiency_cores: usize,
    /// Physical memory in bytes.
    pub memory_bytes: u64,
}

impl HardwareProfile {
    /// Probes the current machine. Values the platform does not report fall back to
    /// conservative defaults (4 KiB pages, 64-byte lines, all CPUs symmetric, 1 GiB).
    #[must_use]
    pub fn probe() -> Self {
        let logical_cpus = std::thread::available_parallelism().map_or(1, usize::from);
        let mut profile = Self {
            page_size: 4_096,
            cache_line: 64,
            logical_cpus,
            performance_cores: logical_cpus,
            efficiency_cores: 0,
            memory_bytes: 1 << 30,
        };
        platform::refine(&mut profile);
        profile
    }

    /// A small, symmetric machine: 1 core, 256 MiB, 4 KiB pages. Deterministic, for tests.
    #[must_use]
    pub const fn minimal() -> Self {
        Self {
            page_size: 4_096,
            cache_line: 64,
            logical_cpus: 1,
            performance_cores: 1,
            efficiency_cores: 0,
            memory_bytes: 256 << 20,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod platform {
    use super::HardwareProfile;

    #[allow(
        unsafe_code,
        reason = "`sysctlbyname` writes at most `size_of::<u64>()` bytes into a local"
    )]
    fn sysctl_u64(name: &str) -> Option<u64> {
        let name = std::ffi::CString::new(name).ok()?;
        let mut value: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: `value` is a writable u64 and `len` holds its size; the kernel writes no more
        // than `len` bytes and updates `len` to the size written (4 for 32-bit values).
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&raw mut value).cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && (len == 4 || len == 8)).then_some(if len == 4 {
            value & 0xFFFF_FFFF
        } else {
            value
        })
    }

    fn sysctl_usize(name: &str) -> Option<usize> {
        sysctl_u64(name)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|&v| v > 0)
    }

    pub fn refine(profile: &mut HardwareProfile) {
        if let Some(v) = sysctl_usize("hw.pagesize") {
            profile.page_size = v;
        }
        if let Some(v) = sysctl_usize("hw.cachelinesize") {
            profile.cache_line = v;
        }
        if let Some(v) = sysctl_u64("hw.memsize").filter(|&v| v > 0) {
            profile.memory_bytes = v;
        }
        let levels = sysctl_usize("hw.nperflevels").unwrap_or(1);
        if let Some(fast) = sysctl_usize("hw.perflevel0.physicalcpu") {
            profile.performance_cores = fast;
            profile.efficiency_cores = (1..levels)
                .filter_map(|l| sysctl_usize(&format!("hw.perflevel{l}.physicalcpu")))
                .sum();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use super::HardwareProfile;

    #[allow(unsafe_code, reason = "`sysconf` takes and returns plain integers")]
    fn sysconf(name: libc::c_int) -> Option<u64> {
        // SAFETY: no pointers are involved.
        let value = unsafe { libc::sysconf(name) };
        u64::try_from(value).ok().filter(|&v| v > 0)
    }

    fn read_u64(path: &str) -> Option<u64> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    pub fn refine(profile: &mut HardwareProfile) {
        if let Some(v) = sysconf(libc::_SC_PAGESIZE).and_then(|v| usize::try_from(v).ok()) {
            profile.page_size = v;
        }
        if let Some(v) = read_u64("/sys/devices/system/cpu/cpu0/cache/index0/coherency_line_size")
            .and_then(|v| usize::try_from(v).ok())
        {
            profile.cache_line = v;
        }
        if let (Some(pages), Some(size)) =
            (sysconf(libc::_SC_PHYS_PAGES), sysconf(libc::_SC_PAGESIZE))
        {
            profile.memory_bytes = pages.saturating_mul(size);
        }
        // Heterogeneous ARM SoCs expose per-core capacity (1024 for the biggest cores).
        let capacities: Vec<u64> = (0..profile.logical_cpus)
            .filter_map(|cpu| read_u64(&format!("/sys/devices/system/cpu/cpu{cpu}/cpu_capacity")))
            .collect();
        if let Some(&max) = capacities.iter().max() {
            if capacities.len() == profile.logical_cpus {
                let fast = capacities.iter().filter(|&&c| c == max).count();
                profile.performance_cores = fast;
                profile.efficiency_cores = capacities.len() - fast;
            }
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
)))]
mod platform {
    pub fn refine(_profile: &mut super::HardwareProfile) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_plausible_values() {
        let p = HardwareProfile::probe();
        assert!(p.page_size.is_power_of_two() && p.page_size >= 4_096);
        assert!(p.cache_line.is_power_of_two() && p.cache_line >= 32);
        assert!(
            p.performance_cores >= 1
                && p.performance_cores + p.efficiency_cores <= p.logical_cpus.max(1) * 2
        );
        assert!(p.memory_bytes >= 64 << 20);
    }
}
