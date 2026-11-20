use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::error::{Result, ShmError};

pub const PAGE_FLAG_ACTIVE: u32 = 1;

/// Cache-aligned page descriptor for zero-copy snapshots.
#[repr(C, align(32))]
pub struct ShmPageDesc {
    pub page_id: u32,
    pub length: AtomicU32,
    pub version: AtomicU64,
    pub flags: AtomicU32,
    pub pin_count: AtomicU32,
    pub reserved: [u8; 8],
}

impl std::fmt::Debug for ShmPageDesc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmPageDesc")
            .field("page_id", &self.page_id)
            .field("length", &self.length.load(Ordering::Relaxed))
            .field("version", &self.version.load(Ordering::Relaxed))
            .field("flags", &self.flags.load(Ordering::Relaxed))
            .finish()
    }
}

/// Zero-copy snapshot page reference.
#[derive(Debug, Clone, Copy)]
pub struct ShmPageRef<'a> {
    pub page_id: u32,
    pub version: u64,
    pub data: &'a [u8],
}

/// Shared page pool residing in SHM.
#[derive(Debug, Clone, Copy)]
pub struct ShmPagePool<'a> {
    descriptors: &'a [ShmPageDesc],
    data_base: *mut u8,
    page_size: usize,
    num_pages: usize,
}

impl<'a> ShmPagePool<'a> {
    /// Create a page pool view over mapped memory.
    ///
    /// # Safety
    /// `desc_ptr` must point to `num_pages` contiguous `ShmPageDesc` instances.
    /// `data_base` must point to a buffer of at least `num_pages * page_size` bytes.
    pub unsafe fn from_raw(
        desc_ptr: *const ShmPageDesc,
        data_base: *mut u8,
        page_size: usize,
        num_pages: usize,
    ) -> Self {
        Self {
            descriptors: std::slice::from_raw_parts(desc_ptr, num_pages),
            data_base,
            page_size,
            num_pages,
        }
    }

    /// Read a page snapshot zero-copy.
    #[inline]
    pub fn read_page(&self, page_id: u32) -> Option<ShmPageRef<'a>> {
        let idx = page_id as usize;
        if idx >= self.num_pages {
            return None;
        }

        let desc = &self.descriptors[idx];
        let flags = desc.flags.load(Ordering::Acquire);
        if (flags & PAGE_FLAG_ACTIVE) == 0 {
            return None;
        }

        let version = desc.version.load(Ordering::Acquire);
        let len = (desc.length.load(Ordering::Acquire) as usize).min(self.page_size);

        let data =
            unsafe { std::slice::from_raw_parts(self.data_base.add(idx * self.page_size), len) };

        Some(ShmPageRef {
            page_id,
            version,
            data,
        })
    }

    /// Publish or update a page snapshot in the pool.
    pub fn publish_page(&self, page_id: u32, data: &[u8], version: u64) -> Result<()> {
        let idx = page_id as usize;
        if idx >= self.num_pages {
            return Err(ShmError::PageOutOfBounds(page_id, self.num_pages as u32));
        }

        let to_copy = data.len().min(self.page_size);
        unsafe {
            let dest = self.data_base.add(idx * self.page_size);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dest, to_copy);
        }

        let desc = &self.descriptors[idx];
        desc.length.store(to_copy as u32, Ordering::Release);
        desc.flags.store(PAGE_FLAG_ACTIVE, Ordering::Release);
        desc.version.store(version, Ordering::Release);

        Ok(())
    }

    #[inline]
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    #[inline]
    pub fn num_pages(&self) -> usize {
        self.num_pages
    }
}
