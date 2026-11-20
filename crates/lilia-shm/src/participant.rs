use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::error::{Result, ShmError};
use crate::header::now_millis;

pub const PARTICIPANT_FLAG_ACTIVE: u32 = 1;
pub const HEARTBEAT_TIMEOUT_MS: u64 = 15_000;

/// Atomic participant descriptor for inter-process epoch registration.
///
/// Size: exactly 32 bytes, cache-aligned.
#[repr(C, align(32))]
pub struct ParticipantSlot {
    pub pid: AtomicU32,
    pub pinned_epoch: AtomicU64,
    pub last_heartbeat_ms: AtomicU64,
    pub flags: AtomicU32,
    pub reserved: [u8; 8],
}

impl std::fmt::Debug for ParticipantSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParticipantSlot")
            .field("pid", &self.pid.load(Ordering::Relaxed))
            .field("pinned_epoch", &self.pinned_epoch.load(Ordering::Relaxed))
            .field(
                "last_heartbeat_ms",
                &self.last_heartbeat_ms.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl ParticipantSlot {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pid.load(Ordering::Acquire) == 0
    }

    #[inline]
    pub fn reset(&self) {
        self.pinned_epoch.store(0, Ordering::Release);
        self.flags.store(0, Ordering::Release);
        self.last_heartbeat_ms.store(0, Ordering::Release);
        self.pid.store(0, Ordering::Release);
    }
}

/// View over the shared participant table in SHM.
#[derive(Debug, Clone, Copy)]
pub struct ParticipantTable<'a> {
    slots: &'a [ParticipantSlot],
}

impl<'a> ParticipantTable<'a> {
    /// Create a view over the raw participant slot memory.
    ///
    /// # Safety
    /// `ptr` must point to `count` contiguous `ParticipantSlot` instances in SHM.
    pub unsafe fn from_raw(ptr: *const ParticipantSlot, count: usize) -> Self {
        Self {
            slots: std::slice::from_raw_parts(ptr, count),
        }
    }

    /// Register the calling process in an available slot.
    pub fn register(&self, pid: u32) -> Result<ParticipantHandle<'a>> {
        let now = now_millis();
        for (idx, slot) in self.slots.iter().enumerate() {
            // Try claiming empty slot or clean up dead process
            let current_pid = slot.pid.load(Ordering::Acquire);
            let can_claim = if current_pid == 0 {
                true
            } else if !is_process_alive(current_pid)
                || (now.saturating_sub(slot.last_heartbeat_ms.load(Ordering::Relaxed))
                    > HEARTBEAT_TIMEOUT_MS)
            {
                // Dead process cleanup
                slot.reset();
                true
            } else {
                false
            };

            if can_claim
                && slot
                    .pid
                    .compare_exchange(0, pid, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                slot.pinned_epoch.store(0, Ordering::Release);
                slot.last_heartbeat_ms.store(now, Ordering::Release);
                slot.flags.store(PARTICIPANT_FLAG_ACTIVE, Ordering::Release);
                return Ok(ParticipantHandle {
                    slot,
                    slot_index: idx,
                });
            }
        }
        Err(ShmError::CapacityExceeded {
            needed: 1,
            available: 0,
        })
    }

    /// Calculate the minimum pinned epoch among all currently live participants.
    /// Returns `fallback_epoch` if no participant is pinning an epoch.
    pub fn min_active_epoch(&self, fallback_epoch: u64) -> u64 {
        let now = now_millis();
        let mut min_epoch = fallback_epoch;

        for slot in self.slots {
            let pid = slot.pid.load(Ordering::Acquire);
            if pid == 0 {
                continue;
            }

            // Check if participant is alive
            if !is_process_alive(pid)
                || (now.saturating_sub(slot.last_heartbeat_ms.load(Ordering::Relaxed))
                    > HEARTBEAT_TIMEOUT_MS)
            {
                slot.reset();
                continue;
            }

            let pinned = slot.pinned_epoch.load(Ordering::Acquire);
            if pinned > 0 && pinned < min_epoch {
                min_epoch = pinned;
            }
        }

        min_epoch
    }
}

/// RAII handle managing process epoch pinning in shared memory.
#[derive(Debug)]
pub struct ParticipantHandle<'a> {
    slot: &'a ParticipantSlot,
    pub slot_index: usize,
}

impl<'a> ParticipantHandle<'a> {
    #[inline]
    pub fn pin_epoch(&self, epoch: u64) {
        self.slot.pinned_epoch.store(epoch, Ordering::Release);
        self.slot
            .last_heartbeat_ms
            .store(now_millis(), Ordering::Relaxed);
    }

    #[inline]
    pub fn unpin_epoch(&self) {
        self.slot.pinned_epoch.store(0, Ordering::Release);
    }

    #[inline]
    pub fn heartbeat(&self) {
        self.slot
            .last_heartbeat_ms
            .store(now_millis(), Ordering::Relaxed);
    }
}

impl Drop for ParticipantHandle<'_> {
    fn drop(&mut self) {
        self.slot.reset();
    }
}

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // kill(pid, 0) checks existence without sending a signal
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            false
        } else {
            windows_sys::Win32::Foundation::CloseHandle(handle);
            true
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn is_process_alive(_pid: u32) -> bool {
    true
}
