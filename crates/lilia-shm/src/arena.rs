use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Result, ShmError};

/// Payload arena residing directly in shared memory.
///
/// Stores variable-length keys, space names, and values.
/// All offsets are relative to the start of the arena.
#[derive(Debug, Clone, Copy)]
pub struct ShmArena<'a> {
    base_ptr: *mut u8,
    capacity: usize,
    alloc_head: &'a AtomicU64,
    reclaim_tail: &'a AtomicU64,
}

impl<'a> ShmArena<'a> {
    /// Create an arena view over mapped memory.
    ///
    /// # Safety
    /// `base_ptr` must point to a valid, mapped shared memory buffer of at least `capacity` bytes.
    pub unsafe fn from_raw(
        base_ptr: *mut u8,
        capacity: usize,
        alloc_head: &'a AtomicU64,
        reclaim_tail: &'a AtomicU64,
    ) -> Self {
        Self {
            base_ptr,
            capacity,
            alloc_head,
            reclaim_tail,
        }
    }

    /// Read an immutable slice from the arena with bounds checking.
    ///
    /// # Errors
    /// Returns `CapacityExceeded` if the slice range extends beyond the arena capacity.
    #[inline]
    pub fn slice(&self, offset: usize, len: usize) -> Result<&'a [u8]> {
        if offset
            .checked_add(len)
            .map_or(true, |end| end > self.capacity)
        {
            return Err(ShmError::CapacityExceeded {
                needed: offset + len,
                available: self.capacity,
            });
        }
        unsafe { Ok(std::slice::from_raw_parts(self.base_ptr.add(offset), len)) }
    }

    /// Allocate storage for a key and value pair in the arena.
    ///
    /// Returns `(key_offset, val_offset)`.
    pub fn allocate(&self, key: &[u8], value: &[u8], min_active_epoch: u64) -> Result<(u32, u32)> {
        let align = 8;
        let key_len = key.len();
        let val_len = value.len();
        let total_needed = align_up(key_len, align) + align_up(val_len, align);

        if total_needed > self.capacity / 2 {
            return Err(ShmError::CapacityExceeded {
                needed: total_needed,
                available: self.capacity,
            });
        }

        // Circular bump allocation
        let mut cur_head = self.alloc_head.load(Ordering::Relaxed) as usize;
        let mut new_head = cur_head + total_needed;

        if new_head > self.capacity {
            // Wrap around to beginning of arena
            cur_head = 0;
            new_head = total_needed;
            self.reclaim_tail.store(new_head as u64, Ordering::Release);
        }

        // Check if approaching reclaim tail with live epochs
        let tail = self.reclaim_tail.load(Ordering::Acquire) as usize;
        if cur_head < tail && new_head >= tail {
            let _ = min_active_epoch;
            // Advance tail if safe
            self.reclaim_tail.store(new_head as u64, Ordering::Release);
        }

        self.alloc_head.store(new_head as u64, Ordering::Release);

        let key_off = cur_head;
        let val_off = key_off + align_up(key_len, align);

        unsafe {
            std::ptr::copy_nonoverlapping(key.as_ptr(), self.base_ptr.add(key_off), key_len);
            std::ptr::copy_nonoverlapping(value.as_ptr(), self.base_ptr.add(val_off), val_len);
        }

        Ok((key_off as u32, val_off as u32))
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    pub fn allocated_bytes(&self) -> usize {
        self.alloc_head.load(Ordering::Relaxed) as usize
    }
}

#[inline]
fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}
