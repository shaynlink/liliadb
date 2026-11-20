use std::path::PathBuf;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;

use crate::arena::ShmArena;
use crate::error::Result;
use crate::header::{
    now_millis, ShmHeader, DEFAULT_NUM_PAGES, DEFAULT_PAGE_SIZE, DEFAULT_SHM_SIZE,
    DEFAULT_SLOT_CAPACITY,
};
use crate::page_pool::{ShmPageDesc, ShmPagePool, ShmPageRef};
use crate::participant::{ParticipantSlot, ParticipantTable};
use crate::reader::ShmReader;
use crate::segment::{ShmBackend, ShmSegment};
use crate::slot::{ShmDataKind, ShmSlot, ShmZeroCopyEntry};
use crate::slot_map::LockFreeSlotMap;

/// Options for configuring the shared memory sub-engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmOptions {
    pub backend: ShmBackend,
    pub size_bytes: usize,
    pub slot_capacity: u32,
    pub page_size: u32,
    pub num_pages: u32,
    pub auto_sync_memory_tier: bool,
}

impl Default for ShmOptions {
    fn default() -> Self {
        Self {
            backend: ShmBackend::Auto(PathBuf::from("lilia.db")),
            size_bytes: DEFAULT_SHM_SIZE,
            slot_capacity: DEFAULT_SLOT_CAPACITY,
            page_size: DEFAULT_PAGE_SIZE,
            num_pages: DEFAULT_NUM_PAGES,
            auto_sync_memory_tier: true,
        }
    }
}

impl ShmOptions {
    pub fn file_backed(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: ShmBackend::FileBacked(path.into()),
            ..Default::default()
        }
    }

    pub fn posix_named(name: impl Into<String>) -> Self {
        Self {
            backend: ShmBackend::PosixNamed(name.into()),
            ..Default::default()
        }
    }

    pub fn auto(db_path: impl Into<PathBuf>) -> Self {
        Self {
            backend: ShmBackend::Auto(db_path.into()),
            ..Default::default()
        }
    }

    pub fn with_size_bytes(mut self, size_bytes: usize) -> Self {
        self.size_bytes = size_bytes;
        self
    }

    pub fn with_slot_capacity(mut self, slot_capacity: u32) -> Self {
        self.slot_capacity = slot_capacity.next_power_of_two();
        self
    }
}

/// Statistics for the SHM sub-engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShmStats {
    pub capacity_bytes: usize,
    pub slot_count: usize,
    pub slot_capacity: usize,
    pub allocated_arena_bytes: usize,
    pub global_epoch: u64,
    pub global_seq: u64,
    pub page_size: usize,
    pub num_pages: usize,
}

/// Zero-copy binary JSON entry borrowed directly from SHM.
#[derive(Debug, Clone, PartialEq)]
pub struct ShmZeroCopyJson<'a> {
    pub id: &'a str,
    pub value_bytes: &'a [u8],
    pub version: u64,
    pub expires_at_ms: Option<i64>,
}

/// Primary coordinator for the LiliaDB Shared Memory Sub-Engine.
pub struct ShmEngine {
    write_lock: Mutex<()>,
    header: &'static ShmHeader,
    segment: ShmSegment,
}

impl std::fmt::Debug for ShmEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmEngine")
            .field("segment", &self.segment)
            .field("header", &self.header)
            .finish()
    }
}

impl ShmEngine {
    /// Create a new shared memory segment as the primary writer process.
    pub fn create(options: ShmOptions) -> Result<Self> {
        let segment = ShmSegment::create_or_open(&options.backend, options.size_bytes)?;
        let writer_pid = std::process::id();

        let header = unsafe {
            ShmHeader::initialize(
                segment.as_ptr(),
                options.size_bytes,
                options.slot_capacity.next_power_of_two(),
                options.page_size,
                options.num_pages,
                writer_pid,
            )?
        };

        Ok(Self {
            segment,
            header,
            write_lock: Mutex::new(()),
        })
    }

    /// Open an existing shared memory segment or create it if not present.
    pub fn open_or_create(options: ShmOptions) -> Result<Self> {
        let path = options.backend.resolve_path();
        if path.exists() {
            let segment = ShmSegment::create_or_open(&options.backend, options.size_bytes)?;
            let header = unsafe { ShmHeader::validate(segment.as_ptr(), segment.len())? };
            Ok(Self {
                segment,
                header,
                write_lock: Mutex::new(()),
            })
        } else {
            Self::create(options)
        }
    }

    #[inline]
    fn participant_table(&self) -> ParticipantTable<'_> {
        unsafe {
            let ptr = self
                .segment
                .as_ptr()
                .add(self.header.participant_table_offset as usize)
                as *const ParticipantSlot;
            ParticipantTable::from_raw(ptr, self.header.participant_max as usize)
        }
    }

    #[inline]
    fn slot_map(&self) -> LockFreeSlotMap<'_> {
        unsafe {
            let ptr = self
                .segment
                .as_ptr()
                .add(self.header.slot_map_offset as usize) as *const ShmSlot;
            LockFreeSlotMap::from_raw(
                ptr,
                self.header.slot_capacity as usize,
                &self.header.slot_count,
            )
        }
    }

    #[inline]
    fn arena(&self) -> ShmArena<'_> {
        unsafe {
            let ptr = self.segment.as_ptr().add(self.header.arena_offset as usize);
            ShmArena::from_raw(
                ptr,
                self.header.arena_capacity as usize,
                &self.header.arena_alloc_head,
                &self.header.arena_reclaim_tail,
            )
        }
    }

    #[inline]
    fn page_pool(&self) -> ShmPagePool<'_> {
        unsafe {
            let desc_ptr = self
                .segment
                .as_ptr()
                .add(self.header.page_pool_offset as usize)
                as *const ShmPageDesc;
            let data_offset = (self.header.page_pool_offset as usize)
                + (self.header.num_pages as usize) * std::mem::size_of::<ShmPageDesc>();
            let data_ptr = self.segment.as_ptr().add(data_offset);
            ShmPagePool::from_raw(
                desc_ptr,
                data_ptr,
                self.header.page_size as usize,
                self.header.num_pages as usize,
            )
        }
    }

    /// Put a key-value record into shared memory.
    pub fn put_kv(
        &self,
        space: &str,
        key: &[u8],
        value: &[u8],
        version: u64,
        expires_at_ms: Option<i64>,
    ) -> Result<()> {
        let _guard = self.write_lock.lock();
        let now = now_millis() as i64;
        let global_epoch = self.header.global_epoch.load(Ordering::Acquire);
        let min_epoch = self.participant_table().min_active_epoch(global_epoch);

        let arena = self.arena();
        let (k_off, v_off) = arena.allocate(key, value, min_epoch)?;

        let slot_map = self.slot_map();
        slot_map.insert(
            space,
            key,
            k_off,
            v_off,
            value.len() as u32,
            version,
            expires_at_ms,
            ShmDataKind::Raw,
            global_epoch,
            now,
        )?;

        self.header.global_seq.fetch_add(1, Ordering::Release);
        self.header
            .writer_heartbeat_ms
            .store(now as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Put a binary JSON record into shared memory.
    pub fn put_json(
        &self,
        space: &str,
        id: &str,
        binary_bytes: &[u8],
        version: u64,
        expires_at_ms: Option<i64>,
    ) -> Result<()> {
        let _guard = self.write_lock.lock();
        let now = now_millis() as i64;
        let global_epoch = self.header.global_epoch.load(Ordering::Acquire);
        let min_epoch = self.participant_table().min_active_epoch(global_epoch);

        let key = id.as_bytes();
        let arena = self.arena();
        let (k_off, v_off) = arena.allocate(key, binary_bytes, min_epoch)?;

        let slot_map = self.slot_map();
        slot_map.insert(
            space,
            key,
            k_off,
            v_off,
            binary_bytes.len() as u32,
            version,
            expires_at_ms,
            ShmDataKind::JsonBinary,
            global_epoch,
            now,
        )?;

        self.header.global_seq.fetch_add(1, Ordering::Release);
        self.header
            .writer_heartbeat_ms
            .store(now as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Archive a typed value and write it into shared memory.
    pub fn put_archived<T>(
        &self,
        space: &str,
        key: &[u8],
        value: &T,
        version: u64,
        expires_at_ms: Option<i64>,
    ) -> Result<()>
    where
        T: for<'a> rkyv::Serialize<
            rkyv::api::high::HighSerializer<
                rkyv::util::AlignedVec,
                rkyv::ser::allocator::ArenaHandle<'a>,
                rkyv::rancor::Error,
            >,
        >,
    {
        let bytes = lilia_core::archive_to_bytes(value)?;
        self.put_kv(space, key, &bytes, version, expires_at_ms)
    }

    /// Archive a JSON document and write it into shared memory.
    pub fn put_archived_doc(
        &self,
        space: &str,
        id: &str,
        doc: &lilia_core::json::JsonDocument,
        version: u64,
        expires_at_ms: Option<i64>,
    ) -> Result<()> {
        let bytes = doc.to_archived_bytes()?;
        self.put_json(space, id, &bytes, version, expires_at_ms)
    }

    /// Read an archived typed struct directly from shared memory (< 50 nanoseconds read path).
    #[inline]
    pub fn get_archived<'a, T: rkyv::Archive>(
        &'a self,
        space: &str,
        key: &[u8],
    ) -> Option<&'a T::Archived> {
        let entry = self.get_zero_copy(space, key)?;
        unsafe { Some(rkyv::access_unchecked::<T::Archived>(entry.value)) }
    }

    /// Read an archived JSON document directly from shared memory (< 50 nanoseconds read path).
    #[inline]
    pub fn get_archived_doc<'a>(
        &'a self,
        space: &str,
        id: &str,
    ) -> Option<&'a lilia_core::json::ArchivedJsonDocument> {
        let entry = self.get_zero_copy(space, id.as_bytes())?;
        unsafe { Some(lilia_core::json::access_archived_doc_unchecked(entry.value)) }
    }

    /// Read a key-value record zero-copy from shared memory (< 50 nanoseconds read path).
    #[inline]
    pub fn get_zero_copy<'a>(&'a self, space: &str, key: &[u8]) -> Option<ShmZeroCopyEntry<'a>> {
        let now = now_millis() as i64;
        let arena = self.arena();
        self.slot_map().get_zero_copy(space, key, &arena, now)
    }

    /// Read a JSON record zero-copy from shared memory (< 50 nanoseconds read path).
    #[inline]
    pub fn json_get_zero_copy<'a>(&'a self, space: &str, id: &str) -> Option<ShmZeroCopyJson<'a>> {
        let entry = self.get_zero_copy(space, id.as_bytes())?;
        let id_str = std::str::from_utf8(entry.key).ok()?;
        Some(ShmZeroCopyJson {
            id: id_str,
            value_bytes: entry.value,
            version: entry.version,
            expires_at_ms: entry.expires_at_ms,
        })
    }

    /// Delete a key from shared memory.
    pub fn delete(&self, space: &str, key: &[u8]) -> bool {
        let _guard = self.write_lock.lock();
        let deleted = self.slot_map().delete(space, key);
        if deleted {
            self.header.global_seq.fetch_add(1, Ordering::Release);
        }
        deleted
    }

    /// Publish a snapshot page frame into the shared page pool.
    pub fn publish_page(&self, page_id: u32, data: &[u8], version: u64) -> Result<()> {
        let _guard = self.write_lock.lock();
        self.page_pool().publish_page(page_id, data, version)
    }

    /// Read a snapshot page zero-copy.
    #[inline]
    pub fn read_page<'a>(&'a self, page_id: u32) -> Option<ShmPageRef<'a>> {
        self.page_pool().read_page(page_id)
    }

    /// Advance the global epoch and coordinate garbage collection.
    pub fn advance_epoch(&self) -> u64 {
        let _guard = self.write_lock.lock();
        self.header.global_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Update writer heartbeat timestamp.
    pub fn heartbeat(&self) {
        self.header
            .writer_heartbeat_ms
            .store(now_millis(), Ordering::Relaxed);
    }

    /// Create a standalone, lightweight reader for secondary processes.
    pub fn create_reader(&self) -> Result<ShmReader> {
        ShmReader::open(ShmBackend::FileBacked(self.segment.path().to_path_buf()))
    }

    /// Get current statistics.
    pub fn stats(&self) -> ShmStats {
        ShmStats {
            capacity_bytes: self.segment.len(),
            slot_count: self.slot_map().len(),
            slot_capacity: self.header.slot_capacity as usize,
            allocated_arena_bytes: self.arena().allocated_bytes(),
            global_epoch: self.header.global_epoch.load(Ordering::Relaxed),
            global_seq: self.header.global_seq.load(Ordering::Relaxed),
            page_size: self.header.page_size as usize,
            num_pages: self.header.num_pages as usize,
        }
    }
}
