use std::sync::atomic::Ordering;

use crate::arena::ShmArena;
use crate::engine::ShmZeroCopyJson;
use crate::error::Result;
use crate::header::{now_millis, ShmHeader};
use crate::page_pool::{ShmPageDesc, ShmPagePool, ShmPageRef};
use crate::participant::{ParticipantHandle, ParticipantSlot, ParticipantTable};
use crate::segment::{ShmBackend, ShmSegment};
use crate::slot::{ShmSlot, ShmZeroCopyEntry};
use crate::slot_map::LockFreeSlotMap;

/// Lightweight, standalone reader for secondary processes (iOS Widgets,
/// macOS Menu Bar apps, Android background workers, Daemons, or CLI tools).
///
/// Has ZERO dependencies on SQLite or IPC sockets.
/// Provides sub-50-nanosecond zero-copy read latency.
pub struct ShmReader {
    participant: Option<ParticipantHandle<'static>>,
    header: &'static ShmHeader,
    segment: ShmSegment,
}

// Safety: ShmReader reads memory-mapped shared memory coordinated through atomic seqlocks.
unsafe impl Send for ShmReader {}
unsafe impl Sync for ShmReader {}

impl std::fmt::Debug for ShmReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmReader")
            .field("path", &self.segment.path())
            .field("capacity", &self.segment.len())
            .finish()
    }
}

impl ShmReader {
    /// Open a shared memory segment in shared read mode.
    pub fn open(backend: ShmBackend) -> Result<Self> {
        let segment = ShmSegment::open_read(&backend)?;
        let header = unsafe { ShmHeader::validate(segment.as_ptr(), segment.len())? };

        let participant = unsafe {
            let ptr = segment
                .as_ptr()
                .add(header.participant_table_offset as usize)
                as *const ParticipantSlot;
            let table = ParticipantTable::from_raw(ptr, header.participant_max as usize);
            table.register(std::process::id()).ok()
        };

        Ok(Self {
            segment,
            header,
            participant,
        })
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

    /// Read a key-value record zero-copy directly from shared memory (< 50 nanoseconds).
    #[inline]
    pub fn get<'a>(&'a self, space: &str, key: &[u8]) -> Option<ShmZeroCopyEntry<'a>> {
        if let Some(ref p) = self.participant {
            p.pin_epoch(self.header.global_epoch.load(Ordering::Acquire));
        }

        let now = now_millis() as i64;
        let arena = self.arena();
        let result = self.slot_map().get_zero_copy(space, key, &arena, now);

        if let Some(ref p) = self.participant {
            p.unpin_epoch();
        }

        result
    }

    /// Read a binary JSON record zero-copy directly from shared memory (< 50 nanoseconds).
    #[inline]
    pub fn json_get<'a>(&'a self, space: &str, id: &str) -> Option<ShmZeroCopyJson<'a>> {
        let entry = self.get(space, id.as_bytes())?;
        let id_str = std::str::from_utf8(entry.key).ok()?;
        Some(ShmZeroCopyJson {
            id: id_str,
            value_bytes: entry.value,
            version: entry.version,
            expires_at_ms: entry.expires_at_ms,
        })
    }

    /// Read a binary JSON document and extract a string path zero-copy.
    pub fn json_extract_str(&self, space: &str, id: &str, path: &str) -> Option<String> {
        if let Some(s) = self.json_extract_str_zero_copy(space, id, path) {
            return Some(s.to_string());
        }
        let json_entry = self.json_get(space, id)?;
        let binary_doc =
            lilia_core::json::BinaryDoc::from_bytes(json_entry.value_bytes.to_vec()).ok()?;
        binary_doc.extract_str(path).map(ToOwned::to_owned)
    }

    /// Read an archived typed struct directly from shared memory (0 nanosecond cast).
    #[inline]
    pub fn get_archived<'a, T: rkyv::Archive>(
        &'a self,
        space: &str,
        key: &[u8],
    ) -> Option<&'a T::Archived> {
        let entry = self.get(space, key)?;
        unsafe { Some(rkyv::access_unchecked::<T::Archived>(entry.value)) }
    }

    /// Safely read an archived typed struct with bytecheck validation.
    pub fn get_archived_checked<'a, T>(
        &'a self,
        space: &str,
        key: &[u8],
    ) -> Result<Option<&'a T::Archived>>
    where
        T: rkyv::Archive,
        T::Archived: for<'b> rkyv::bytecheck::CheckBytes<
            rkyv::api::high::HighValidator<'b, rkyv::rancor::Error>,
        >,
    {
        let Some(entry) = self.get(space, key) else {
            return Ok(None);
        };
        let archived = lilia_core::access_archived::<T>(entry.value)?;
        Ok(Some(archived))
    }

    /// Read an archived JSON document directly from shared memory (0 nanosecond cast).
    #[inline]
    pub fn get_archived_doc<'a>(
        &'a self,
        space: &str,
        id: &str,
    ) -> Option<&'a lilia_core::json::ArchivedJsonDocument> {
        let entry = self.get(space, id.as_bytes())?;
        unsafe { Some(lilia_core::json::access_archived_doc_unchecked(entry.value)) }
    }

    /// Read an archived JSON document view directly from shared memory (0 nanosecond cast).
    #[inline]
    pub fn get_archived_json<'a>(
        &'a self,
        space: &str,
        id: &str,
    ) -> Option<lilia_core::json::ArchivedJsonView<'a>> {
        let doc = self.get_archived_doc(space, id)?;
        Some(doc.root_view())
    }

    /// Read an archived JSON document and extract a string slice zero-copy directly from shared memory.
    #[inline]
    pub fn json_extract_str_zero_copy<'a>(
        &'a self,
        space: &str,
        id: &str,
        path: &str,
    ) -> Option<&'a str> {
        let doc = self.get_archived_doc(space, id)?;
        doc.extract_str(path)
    }

    /// Read an archived JSON document and extract an integer zero-copy directly from shared memory.
    #[inline]
    pub fn json_extract_i64_zero_copy(
        &self,
        space: &str,
        id: &str,
        path: &str,
    ) -> Option<i64> {
        let doc = self.get_archived_doc(space, id)?;
        doc.extract_i64(path)
    }

    /// Read an archived JSON document and extract a float zero-copy directly from shared memory.
    #[inline]
    pub fn json_extract_f64_zero_copy(
        &self,
        space: &str,
        id: &str,
        path: &str,
    ) -> Option<f64> {
        let doc = self.get_archived_doc(space, id)?;
        doc.extract_f64(path)
    }

    /// Read an archived JSON document and extract a boolean zero-copy directly from shared memory.
    #[inline]
    pub fn json_extract_bool_zero_copy(
        &self,
        space: &str,
        id: &str,
        path: &str,
    ) -> Option<bool> {
        let doc = self.get_archived_doc(space, id)?;
        doc.extract_bool(path)
    }

    /// Read a snapshot page zero-copy.
    #[inline]
    pub fn read_page<'a>(&'a self, page_id: u32) -> Option<ShmPageRef<'a>> {
        self.page_pool().read_page(page_id)
    }

    /// Send a reader heartbeat.
    pub fn heartbeat(&self) {
        if let Some(ref p) = self.participant {
            p.heartbeat();
        }
    }

    /// Check if the primary writer process is still alive.
    pub fn is_writer_alive(&self) -> bool {
        let writer_pid = self.header.writer_pid.load(Ordering::Acquire);
        if writer_pid == 0 {
            return false;
        }
        let now = now_millis();
        let last_hb = self.header.writer_heartbeat_ms.load(Ordering::Acquire);
        now.saturating_sub(last_hb) < crate::participant::HEARTBEAT_TIMEOUT_MS
    }
}
