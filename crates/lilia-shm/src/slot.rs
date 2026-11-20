use std::sync::atomic::{AtomicI64, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

pub const SLOT_FLAG_EMPTY: u8 = 0;
pub const SLOT_FLAG_ACTIVE: u8 = 1;
pub const SLOT_FLAG_DELETED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShmDataKind {
    Raw = 0,
    JsonBinary = 1,
    Encrypted = 2,
    Compressed = 3,
}

impl ShmDataKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::JsonBinary,
            2 => Self::Encrypted,
            3 => Self::Compressed,
            _ => Self::Raw,
        }
    }
}

/// Cache-line aligned (64 bytes) lock-free slot header for shared memory records.
#[repr(C, align(64))]
pub struct ShmSlot {
    pub sequence: AtomicU64,
    pub epoch: AtomicU64,
    pub key_hash: AtomicU64,

    pub key_offset: AtomicU32,
    pub key_len: AtomicU32,
    pub val_offset: AtomicU32,
    pub val_len: AtomicU32,

    pub version: AtomicU64,
    pub expires_at_ms: AtomicI64,

    pub data_kind: AtomicU8,
    pub flags: AtomicU8,
    pub space_hash: AtomicU16,
    pub reserved: [u8; 12],
}

impl std::fmt::Debug for ShmSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmSlot")
            .field("sequence", &self.sequence.load(Ordering::Relaxed))
            .field("epoch", &self.epoch.load(Ordering::Relaxed))
            .field("key_hash", &self.key_hash.load(Ordering::Relaxed))
            .field("key_offset", &self.key_offset.load(Ordering::Relaxed))
            .field("key_len", &self.key_len.load(Ordering::Relaxed))
            .field("val_offset", &self.val_offset.load(Ordering::Relaxed))
            .field("val_len", &self.val_len.load(Ordering::Relaxed))
            .field("version", &self.version.load(Ordering::Relaxed))
            .field("expires_at_ms", &self.expires_at_ms.load(Ordering::Relaxed))
            .field("flags", &self.flags.load(Ordering::Relaxed))
            .finish()
    }
}

impl ShmSlot {
    #[inline]
    pub fn is_active(&self) -> bool {
        self.flags.load(Ordering::Acquire) == SLOT_FLAG_ACTIVE
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.flags.load(Ordering::Acquire) == SLOT_FLAG_EMPTY
    }

    /// Read seqlock state: returns `(sequence, is_writing)`.
    #[inline]
    pub fn read_begin(&self) -> (u64, bool) {
        let seq = self.sequence.load(Ordering::Acquire);
        (seq, (seq & 1) != 0)
    }

    /// Validate seqlock state: returns true if no write occurred during the read.
    #[inline]
    pub fn read_validate(&self, original_seq: u64) -> bool {
        std::sync::atomic::fence(Ordering::Acquire);
        self.sequence.load(Ordering::Acquire) == original_seq
    }
}

/// Zero-copy borrowed view of a record residing directly in shared memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShmZeroCopyEntry<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
    pub version: u64,
    pub expires_at_ms: Option<i64>,
    pub kind: ShmDataKind,
}

/// Fast, deterministic 64-bit mixer for (space, key) tuple without heap allocations.
#[inline]
pub fn shm_hash(space: &str, key: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in space.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= 0xff;
    h = h.wrapping_mul(0x0100_0000_01b3);
    for &b in key {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    if h == 0 {
        1
    } else {
        h
    }
}

#[inline]
pub fn space_hash_16(space: &str) -> u16 {
    let mut h: u16 = 0x811c;
    for &b in space.as_bytes() {
        h ^= b as u16;
        h = h.wrapping_mul(0x0103);
    }
    h
}
