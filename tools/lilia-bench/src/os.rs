//! Process counters read from the operating system: device I/O bytes and peak RSS.

use crate::record::Io;

/// Cumulative device I/O of this process since it started.
#[must_use]
pub fn io_totals() -> Io {
    imp::io_totals()
}

/// Peak resident set size of this process, in bytes.
#[must_use]
pub fn peak_rss_bytes() -> Option<u64> {
    imp::peak_rss_bytes()
}

/// Difference between two cumulative readings.
#[must_use]
pub fn io_delta(before: Io, after: Io) -> Io {
    Io {
        device_bytes_written: after.device_bytes_written.saturating_sub(before.device_bytes_written),
        device_bytes_read: after.device_bytes_read.saturating_sub(before.device_bytes_read),
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(
    unsafe_code,
    reason = "`proc_pid_rusage` and `getrusage` are plain libc FFI with owned out-buffers"
)]
mod imp {
    use crate::record::Io;

    pub fn io_totals() -> Io {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: `info` is a writable, correctly sized `rusage_info_v4`; the flavor matches it.
        let rc = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return Io::default();
        }
        // SAFETY: the call succeeded and initialised the whole struct.
        let info = unsafe { info.assume_init() };
        Io {
            device_bytes_written: info.ri_diskio_byteswritten,
            device_bytes_read: info.ri_diskio_bytesread,
        }
    }

    pub fn peak_rss_bytes() -> Option<u64> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` is a writable `rusage`.
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        // SAFETY: initialised on success. macOS reports `ru_maxrss` in bytes.
        (rc == 0).then(|| u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).unwrap_or(0))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(unsafe_code, reason = "`getrusage` is plain libc FFI with an owned out-buffer")]
mod imp {
    use crate::record::Io;

    pub fn io_totals() -> Io {
        let Ok(text) = std::fs::read_to_string("/proc/self/io") else {
            return Io::default();
        };
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name))
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        Io {
            device_bytes_written: field("write_bytes:"),
            device_bytes_read: field("read_bytes:"),
        }
    }

    pub fn peak_rss_bytes() -> Option<u64> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` is a writable `rusage`.
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        // SAFETY: initialised on success. Linux reports `ru_maxrss` in KiB.
        (rc == 0).then(|| u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).unwrap_or(0) * 1024)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android")))]
mod imp {
    use crate::record::Io;

    pub fn io_totals() -> Io {
        Io::default()
    }

    pub fn peak_rss_bytes() -> Option<u64> {
        None
    }
}

/// Total size of the regular files under `path`.
#[must_use]
pub fn dir_size(path: &std::path::Path) -> u64 {
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .flatten()
            .map(|e| match e.metadata() {
                Ok(m) if m.is_dir() => dir_size(&e.path()),
                Ok(m) => m.len(),
                Err(_) => 0,
            })
            .sum()
    })
}
