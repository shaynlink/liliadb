use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::error::{Result, ShmError};

pub const SHM_MAGIC: [u8; 8] = *b"LILIASHM";
pub const SHM_VERSION: u32 = 1;
pub const DEFAULT_SHM_SIZE: usize = 16 * 1024 * 1024; // 16 MiB
pub const DEFAULT_SLOT_CAPACITY: u32 = 16_384;
pub const DEFAULT_PAGE_SIZE: u32 = 4096;
pub const DEFAULT_NUM_PAGES: u32 = 128;
pub const MAX_PARTICIPANTS: u32 = 64;

/// Root header of the Shared Memory segment.
///
/// Guaranteed to be cache-line aligned (64 bytes) to avoid false sharing.
/// All pointers stored within the header or subsequent structures are relative offsets
/// from the segment base pointer, ensuring full ASLR compatibility across processes.
#[repr(C, align(64))]
pub struct ShmHeader {
    pub magic: [u8; 8],
    pub version: u32,
    pub flags: u32,
    pub capacity_bytes: u64,
    pub global_epoch: AtomicU64,
    pub global_seq: AtomicU64,
    pub writer_pid: AtomicU32,
    pub writer_heartbeat_ms: AtomicU64,

    // Slot map configuration
    pub slot_map_offset: u64,
    pub slot_capacity: u32,
    pub slot_count: AtomicU32,

    // Page pool configuration
    pub page_pool_offset: u64,
    pub page_size: u32,
    pub num_pages: u32,

    // Payload arena configuration
    pub arena_offset: u64,
    pub arena_capacity: u64,
    pub arena_alloc_head: AtomicU64,
    pub arena_reclaim_tail: AtomicU64,

    // Inter-process participant table
    pub participant_table_offset: u64,
    pub participant_max: u32,

    pub reserved: [u8; 80],
}

impl std::fmt::Debug for ShmHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmHeader")
            .field("magic", &String::from_utf8_lossy(&self.magic))
            .field("version", &self.version)
            .field("capacity_bytes", &self.capacity_bytes)
            .field("global_epoch", &self.global_epoch.load(Ordering::Relaxed))
            .field("global_seq", &self.global_seq.load(Ordering::Relaxed))
            .field("writer_pid", &self.writer_pid.load(Ordering::Relaxed))
            .field("slot_capacity", &self.slot_capacity)
            .field("slot_count", &self.slot_count.load(Ordering::Relaxed))
            .field("page_size", &self.page_size)
            .field("num_pages", &self.num_pages)
            .field("arena_capacity", &self.arena_capacity)
            .finish()
    }
}

impl ShmHeader {
    /// Initialize a freshly mapped memory buffer with default layout offsets.
    ///
    /// # Safety
    /// The caller must guarantee that `ptr` points to a writable, zero-initialized buffer
    /// of at least `capacity_bytes` bytes.
    pub unsafe fn initialize(
        ptr: *mut u8,
        capacity_bytes: usize,
        slot_capacity: u32,
        page_size: u32,
        num_pages: u32,
        writer_pid: u32,
    ) -> Result<&'static mut Self> {
        let header_size = std::mem::size_of::<Self>();
        let participant_size = (MAX_PARTICIPANTS as usize)
            * std::mem::size_of::<crate::participant::ParticipantSlot>();
        let slot_size = (slot_capacity as usize) * std::mem::size_of::<crate::slot::ShmSlot>();
        let page_pool_headers =
            (num_pages as usize) * std::mem::size_of::<crate::page_pool::ShmPageDesc>();
        let page_pool_data = (num_pages as usize) * (page_size as usize);
        let page_pool_total = page_pool_headers + page_pool_data;

        let participant_table_offset = align_up(header_size, 64);
        let slot_map_offset = align_up(participant_table_offset + participant_size, 64);
        let page_pool_offset = align_up(slot_map_offset + slot_size, 64);
        let arena_offset = align_up(page_pool_offset + page_pool_total, 64);

        if arena_offset >= capacity_bytes {
            return Err(ShmError::CapacityExceeded {
                needed: arena_offset,
                available: capacity_bytes,
            });
        }

        // Fault in and zero-initialize the metadata region (header + participant table + slot map + page pool)
        // to ensure physical page allocation on sparse filesystems (APFS on macOS).
        std::ptr::write_bytes(ptr, 0, arena_offset);

        let arena_capacity = (capacity_bytes - arena_offset) as u64;

        let header = &mut *(ptr as *mut Self);
        header.magic = SHM_MAGIC;
        header.version = SHM_VERSION;
        header.flags = 0;
        header.capacity_bytes = capacity_bytes as u64;
        header.global_epoch = AtomicU64::new(1);
        header.global_seq = AtomicU64::new(1);
        header.writer_pid = AtomicU32::new(writer_pid);
        header.writer_heartbeat_ms = AtomicU64::new(now_millis());

        header.participant_table_offset = participant_table_offset as u64;
        header.participant_max = MAX_PARTICIPANTS;

        header.slot_map_offset = slot_map_offset as u64;
        header.slot_capacity = slot_capacity;
        header.slot_count = AtomicU32::new(0);

        header.page_pool_offset = page_pool_offset as u64;
        header.page_size = page_size;
        header.num_pages = num_pages;

        header.arena_offset = arena_offset as u64;
        header.arena_capacity = arena_capacity;
        header.arena_alloc_head = AtomicU64::new(0);
        header.arena_reclaim_tail = AtomicU64::new(0);
        header.reserved = [0u8; 80];

        Ok(header)
    }

    /// Validate that an existing mapped memory segment contains a valid `ShmHeader`.
    ///
    /// # Safety
    /// The caller must ensure `ptr` points to a mapped region of at least `capacity_bytes`.
    pub unsafe fn validate(ptr: *const u8, capacity_bytes: usize) -> Result<&'static Self> {
        if capacity_bytes < std::mem::size_of::<Self>() {
            return Err(ShmError::CapacityExceeded {
                needed: std::mem::size_of::<Self>(),
                available: capacity_bytes,
            });
        }
        let header = &*(ptr as *const Self);
        if header.magic != SHM_MAGIC {
            return Err(ShmError::CorruptHeader(format!(
                "invalid magic header {:?}",
                &header.magic
            )));
        }
        if header.version != SHM_VERSION {
            return Err(ShmError::VersionMismatch {
                expected: SHM_VERSION,
                found: header.version,
            });
        }
        if header.capacity_bytes as usize > capacity_bytes {
            return Err(ShmError::CapacityExceeded {
                needed: header.capacity_bytes as usize,
                available: capacity_bytes,
            });
        }
        Ok(header)
    }
}

#[inline]
pub(crate) fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
}

#[inline]
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
