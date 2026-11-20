use std::sync::atomic::{AtomicU32, Ordering};

use crate::arena::ShmArena;
use crate::error::{Result, ShmError};
use crate::slot::{
    shm_hash, space_hash_16, ShmDataKind, ShmSlot, ShmZeroCopyEntry, SLOT_FLAG_ACTIVE,
    SLOT_FLAG_DELETED, SLOT_FLAG_EMPTY,
};

pub const MAX_PROBES: usize = 32;
pub const SEQLOCK_SPIN_LIMIT: usize = 8;

/// Lock-Free hash map of slots inside shared memory.
#[derive(Debug, Clone, Copy)]
pub struct LockFreeSlotMap<'a> {
    slots: &'a [ShmSlot],
    mask: usize,
    slot_count: &'a AtomicU32,
}

impl<'a> LockFreeSlotMap<'a> {
    /// Create a slot map view over mapped memory.
    ///
    /// # Safety
    /// `ptr` must point to `capacity` contiguous `ShmSlot` instances in SHM.
    /// `capacity` must be a power of two.
    pub unsafe fn from_raw(
        ptr: *const ShmSlot,
        capacity: usize,
        slot_count: &'a AtomicU32,
    ) -> Self {
        assert!(
            capacity.is_power_of_two(),
            "slot capacity must be a power of two"
        );
        Self {
            slots: std::slice::from_raw_parts(ptr, capacity),
            mask: capacity - 1,
            slot_count,
        }
    }

    /// Retrieve a record zero-copy from shared memory (< 50 nanoseconds read path).
    ///
    /// No locks are held, no syscalls are issued, and no heap allocation occurs.
    #[inline]
    pub fn get_zero_copy(
        &self,
        space: &str,
        key: &[u8],
        arena: &ShmArena<'a>,
        now_ms: i64,
    ) -> Option<ShmZeroCopyEntry<'a>> {
        let hash = shm_hash(space, key);
        let start_idx = (hash as usize) & self.mask;
        let sp_hash = space_hash_16(space);

        for probe in 0..MAX_PROBES {
            let slot = &self.slots[(start_idx + probe) & self.mask];
            let slot_hash = slot.key_hash.load(Ordering::Relaxed);

            // Fast negative rejection
            if slot_hash == 0 {
                return None;
            }
            if slot_hash != hash {
                continue;
            }

            // Quick space hash check
            if slot.space_hash.load(Ordering::Relaxed) != sp_hash {
                continue;
            }

            // Seqlock read loop
            for _ in 0..SEQLOCK_SPIN_LIMIT {
                let (seq1, is_writing) = slot.read_begin();
                if is_writing {
                    std::hint::spin_loop();
                    continue;
                }

                let k_off = slot.key_offset.load(Ordering::Relaxed) as usize;
                let k_len = slot.key_len.load(Ordering::Relaxed) as usize;

                if k_len != key.len() {
                    break; // Hash collision, check next probe
                }

                let Ok(k_slice) = arena.slice(k_off, k_len) else {
                    break;
                };

                if k_slice != key {
                    break; // Hash collision, check next probe
                }

                let v_off = slot.val_offset.load(Ordering::Relaxed) as usize;
                let v_len = slot.val_len.load(Ordering::Relaxed) as usize;

                let Ok(v_slice) = arena.slice(v_off, v_len) else {
                    break;
                };

                let version = slot.version.load(Ordering::Relaxed);
                let expires_at = slot.expires_at_ms.load(Ordering::Relaxed);
                let data_kind = slot.data_kind.load(Ordering::Relaxed);
                let flags = slot.flags.load(Ordering::Relaxed);

                // Validate seqlock
                if slot.read_validate(seq1) {
                    if flags != SLOT_FLAG_ACTIVE {
                        return None;
                    }
                    if expires_at > 0 && expires_at <= now_ms {
                        return None;
                    }
                    return Some(ShmZeroCopyEntry {
                        key: k_slice,
                        value: v_slice,
                        version,
                        expires_at_ms: if expires_at > 0 {
                            Some(expires_at)
                        } else {
                            None
                        },
                        kind: ShmDataKind::from_u8(data_kind),
                    });
                }
            }
        }

        None
    }

    /// Insert or update a slot record in shared memory.
    pub fn insert(
        &self,
        space: &str,
        key: &[u8],
        key_offset: u32,
        val_offset: u32,
        val_len: u32,
        version: u64,
        expires_at_ms: Option<i64>,
        kind: ShmDataKind,
        epoch: u64,
        now_ms: i64,
    ) -> Result<()> {
        let hash = shm_hash(space, key);
        let start_idx = (hash as usize) & self.mask;
        let sp_hash = space_hash_16(space);

        let mut victim_slot: Option<&ShmSlot> = None;
        let mut is_new = false;

        for probe in 0..MAX_PROBES {
            let slot = &self.slots[(start_idx + probe) & self.mask];
            let slot_hash = slot.key_hash.load(Ordering::Relaxed);

            if slot_hash == hash && slot.space_hash.load(Ordering::Relaxed) == sp_hash {
                // Update existing key
                victim_slot = Some(slot);
                is_new = false;
                break;
            }

            if victim_slot.is_none() {
                let flags = slot.flags.load(Ordering::Relaxed);
                let exp = slot.expires_at_ms.load(Ordering::Relaxed);
                if flags == SLOT_FLAG_EMPTY
                    || flags == SLOT_FLAG_DELETED
                    || (exp > 0 && exp <= now_ms)
                {
                    victim_slot = Some(slot);
                    is_new = true;
                }
            }
        }

        let slot = victim_slot.ok_or(ShmError::SlotTableFull(self.slots.len()))?;

        // Seqlock write sequence
        let cur_seq = slot.sequence.load(Ordering::Relaxed);
        slot.sequence
            .store(cur_seq.wrapping_add(1), Ordering::Release);

        slot.epoch.store(epoch, Ordering::Relaxed);
        slot.key_hash.store(hash, Ordering::Relaxed);
        slot.space_hash.store(sp_hash, Ordering::Relaxed);
        slot.key_offset.store(key_offset, Ordering::Relaxed);
        slot.key_len.store(key.len() as u32, Ordering::Relaxed);
        slot.val_offset.store(val_offset, Ordering::Relaxed);
        slot.val_len.store(val_len, Ordering::Relaxed);
        slot.version.store(version, Ordering::Relaxed);
        slot.expires_at_ms
            .store(expires_at_ms.unwrap_or(0), Ordering::Relaxed);
        slot.data_kind.store(kind as u8, Ordering::Relaxed);
        slot.flags.store(SLOT_FLAG_ACTIVE, Ordering::Relaxed);

        // Commit with even sequence number
        slot.sequence
            .store(cur_seq.wrapping_add(2), Ordering::Release);

        if is_new {
            self.slot_count.fetch_add(1, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Mark a key as deleted in shared memory.
    pub fn delete(&self, space: &str, key: &[u8]) -> bool {
        let hash = shm_hash(space, key);
        let start_idx = (hash as usize) & self.mask;
        let sp_hash = space_hash_16(space);

        for probe in 0..MAX_PROBES {
            let slot = &self.slots[(start_idx + probe) & self.mask];
            if slot.key_hash.load(Ordering::Relaxed) == hash
                && slot.space_hash.load(Ordering::Relaxed) == sp_hash
            {
                let cur_seq = slot.sequence.load(Ordering::Relaxed);
                slot.sequence
                    .store(cur_seq.wrapping_add(1), Ordering::Release);

                slot.flags.store(SLOT_FLAG_DELETED, Ordering::Relaxed);
                slot.key_hash.store(0, Ordering::Relaxed);

                slot.sequence
                    .store(cur_seq.wrapping_add(2), Ordering::Release);
                self.slot_count.fetch_sub(1, Ordering::Relaxed);
                return true;
            }
        }

        false
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.slot_count.load(Ordering::Relaxed) as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
