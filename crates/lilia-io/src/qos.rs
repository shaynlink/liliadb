//! Scheduling class of the engine's own threads.
//!
//! The engine never changes the priority of a thread it does not own: the application decides how
//! urgent its calls are. Its background threads (flushes, compactions) lower themselves so they
//! yield the fastest cores to the foreground.

/// Marks the calling thread as background maintenance work.
///
/// Apple platforms: `QOS_CLASS_UTILITY`, which the scheduler keeps mostly on efficiency cores.
/// Linux and Android: niceness 10 for this thread. Windows: background processing mode.
/// Elsewhere: no effect. Failure is not an error: the work still runs, at normal priority.
pub fn set_current_thread_background() {
    platform::background();
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod platform {
    #[allow(unsafe_code)] // reason: `pthread_set_qos_class_self_np` only affects the calling thread.
    pub fn background() {
        // SAFETY: valid QoS class and relative priority 0; no pointers.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    #[allow(unsafe_code)] // reason: `gettid` and `setpriority` take and return plain integers.
    pub fn background() {
        // SAFETY: no pointers; on Linux a thread id is a valid `PRIO_PROCESS` target and changes
        // only that thread's niceness.
        unsafe {
            let tid = libc::gettid();
            libc::setpriority(libc::PRIO_PROCESS, libc::id_t::try_from(tid).unwrap_or(0), 10);
        }
    }
}

#[cfg(windows)]
mod platform {
    #[allow(unsafe_code)] // reason: `SetThreadPriority` on the calling thread's pseudo-handle.
    pub fn background() {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
        // SAFETY: the pseudo-handle of the current thread is always valid.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android", windows)))]
mod platform {
    pub fn background() {}
}

#[cfg(test)]
mod tests {
    #[test]
    fn background_does_not_panic() {
        std::thread::spawn(super::set_current_thread_background).join().unwrap();
    }
}
