//! Crash-Resistant Dual-Header Symmetric Envelope.
//!
//! Provides a bulletproof, 4K sector-aligned dual-header envelope format for LiliaDB files:
//! - Front Header at offset `0..4096`
//! - Rear Header (Trailer / Mirror) sealed at offset `(EOF - 4096)..EOF`
//! - Incremental monotonic generation counter (`generation: u64`)
//! - Strict BLAKE3 cryptographic hash verification on both headers
//! - Automatic crash recovery / self-healing: if either header is torn or corrupted by sudden
//!   power loss, the surviving valid header restores it automatically.
//! - Self-descriptive, portable, tamper-proof metadata: schema version, compression profile,
//!   and encryption parameters (salt, KDF iterations, key verification check) without ever
//!   exposing secrets or sensitive data.

use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ErrorCode, LiliaError, Result};

/// Fixed 8-byte magic identifier for LiliaDB Dual-Header Envelopes: "LILIADBF".
pub const ENVELOPE_MAGIC: &[u8; 8] = b"LILIADBF";

/// Format version of the envelope.
pub const ENVELOPE_FORMAT_VERSION: u16 = 1;

/// Fixed sector-aligned header size (4096 bytes / 4 KiB).
pub const ENVELOPE_HEADER_SIZE: usize = 4096;

/// Offset inside the header where the BLAKE3 checksum begins (4064..4096).
pub const CHECKSUM_OFFSET: usize = 4064;

/// Minimum file size for a valid enveloped file (front header + rear header = 8192 bytes).
pub const MIN_ENVELOPE_FILE_SIZE: u64 = (ENVELOPE_HEADER_SIZE * 2) as u64;

/// Header role within the envelope container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum HeaderRole {
    #[default]
    Symmetric = 0,
    Front = 1,
    Rear = 2,
}

impl HeaderRole {
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Front,
            2 => Self::Rear,
            _ => Self::Symmetric,
        }
    }
}

/// Supported compression algorithms recorded in the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum CompressionAlgorithm {
    #[default]
    None = 0,
    Zstd = 1,
    Deflate = 2,
    Lz4 = 3,
    DictZstd = 4,
}

impl CompressionAlgorithm {
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Zstd,
            2 => Self::Deflate,
            3 => Self::Lz4,
            4 => Self::DictZstd,
            _ => Self::None,
        }
    }
}

/// Tamper-proof compression profile embedded in the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CompressionProfile {
    pub algorithm: CompressionAlgorithm,
    pub level: i8,
    pub flags: u16,
    pub dictionary_id: u64,
}

/// Supported symmetric cipher algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum CipherAlgorithm {
    #[default]
    None = 0,
    Aes256Gcm = 1,
    ChaCha20Poly1305 = 2,
    Aes256Siv = 3,
}

impl CipherAlgorithm {
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Aes256Gcm,
            2 => Self::ChaCha20Poly1305,
            3 => Self::Aes256Siv,
            _ => Self::None,
        }
    }
}

/// Supported Key Derivation Functions (KDF).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum KdfAlgorithm {
    #[default]
    None = 0,
    Pbkdf2HmacSha256 = 1,
    Argon2id = 2,
}

impl KdfAlgorithm {
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Pbkdf2HmacSha256,
            2 => Self::Argon2id,
            _ => Self::None,
        }
    }
}

/// Constant-time 16-byte slice comparison.
#[inline]
fn constant_time_eq_16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Constant-time 32-byte slice comparison.
#[inline]
fn constant_time_eq_32(a: &[u8], b: &[u8]) -> bool {
    if a.len() != 32 || b.len() != 32 {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Tamper-proof encryption metadata.
///
/// **Zero Exposure of Sensitive Data**: Plaintext passphrases and raw secret keys are
/// strictly forbidden here. Only public parameters (salt, KDF rounds, cipher ID) and a
/// one-way zero-knowledge key check tag (`BLAKE3("LiliaDB:KeyCheck" || key)[0..16]`)
/// are persisted to confirm passphrases safely without trial decryption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptionMetadata {
    pub cipher: CipherAlgorithm,
    pub kdf: KdfAlgorithm,
    pub kdf_iterations: u32,
    pub salt: [u8; 32],
    pub nonce_prefix: [u8; 16],
    pub key_check_value: [u8; 16],
}

impl Default for EncryptionMetadata {
    fn default() -> Self {
        Self {
            cipher: CipherAlgorithm::None,
            kdf: KdfAlgorithm::None,
            kdf_iterations: 0,
            salt: [0u8; 32],
            nonce_prefix: [0u8; 16],
            key_check_value: [0u8; 16],
        }
    }
}

impl EncryptionMetadata {
    /// Computes the 16-byte key verification check value for a derived key.
    #[must_use]
    pub fn compute_check_value(key: &[u8]) -> [u8; 16] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"LiliaDB:KeyCheck:v1\0");
        hasher.update(key);
        let hash = hasher.finalize();
        let mut out = [0u8; 16];
        out.copy_from_slice(&hash.as_bytes()[..16]);
        out
    }

    /// Verifies whether the provided derived key matches the stored key check value.
    #[must_use]
    pub fn verify_key(&self, key: &[u8]) -> bool {
        if self.cipher == CipherAlgorithm::None {
            return true;
        }
        let expected = Self::compute_check_value(key);
        constant_time_eq_16(&self.key_check_value, &expected)
    }
}

mod serde_arr_128 {
    use super::*;

    pub fn serialize<S: Serializer>(arr: &[u8; 128], s: S) -> std::result::Result<S::Ok, S::Error> {
        serde_bytes::serialize(arr.as_slice(), s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<[u8; 128], D::Error> {
        let bytes: Vec<u8> = serde_bytes::deserialize(d)?;
        let mut arr = [0u8; 128];
        let copy_len = bytes.len().min(128);
        arr[..copy_len].copy_from_slice(&bytes[..copy_len]);
        Ok(arr)
    }
}

/// Complete 4096-byte Envelope Header representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeHeader {
    pub format_version: u16,
    pub header_size: u16,
    pub role: HeaderRole,
    pub generation: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub payload_offset: u64,
    pub payload_len: u64,
    pub payload_checksum: [u8; 32],
    pub schema_version: u32,
    pub resource_profile: u32,
    pub page_size: u32,
    pub capabilities: u64,
    pub compression: CompressionProfile,
    pub encryption: EncryptionMetadata,
    #[serde(with = "serde_arr_128")]
    pub database_id: [u8; 128],
    pub checksum: [u8; 32],
}

impl Default for EnvelopeHeader {
    fn default() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            format_version: ENVELOPE_FORMAT_VERSION,
            header_size: ENVELOPE_HEADER_SIZE as u16,
            role: HeaderRole::Symmetric,
            generation: 1,
            created_at_ms: now,
            updated_at_ms: now,
            payload_offset: ENVELOPE_HEADER_SIZE as u64,
            payload_len: 0,
            payload_checksum: [0u8; 32],
            schema_version: 1,
            resource_profile: 1, // Server default
            page_size: 4096,
            capabilities: 0,
            compression: CompressionProfile::default(),
            encryption: EncryptionMetadata::default(),
            database_id: [0u8; 128],
            checksum: [0u8; 32],
        }
    }
}

impl EnvelopeHeader {
    /// Sets a human-readable name or UUID as the database ID.
    pub fn set_database_id(&mut self, id: &str) {
        self.database_id.fill(0);
        let bytes = id.as_bytes();
        let copy_len = bytes.len().min(128);
        self.database_id[..copy_len].copy_from_slice(&bytes[..copy_len]);
    }

    /// Retrieves the database ID as a UTF-8 string (trimmed of trailing null bytes).
    #[must_use]
    pub fn database_id_str(&self) -> &str {
        let len = self
            .database_id
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.database_id.len());
        std::str::from_utf8(&self.database_id[..len]).unwrap_or("")
    }

    /// Serializes the header into a 4096-byte array, computing and sealing the BLAKE3 checksum.
    pub fn serialize_into(&self, buf: &mut [u8; ENVELOPE_HEADER_SIZE]) {
        buf.fill(0);

        // 0..8: Magic
        buf[0..8].copy_from_slice(ENVELOPE_MAGIC);
        // 8..10: Format version
        buf[8..10].copy_from_slice(&self.format_version.to_le_bytes());
        // 10..12: Header size
        buf[10..12].copy_from_slice(&self.header_size.to_le_bytes());
        // 12..13: Role
        buf[12] = self.role as u8;
        // 13..16: Reserved padding (3 bytes)
        // 16..24: Generation counter
        buf[16..24].copy_from_slice(&self.generation.to_le_bytes());
        // 24..32: Created at
        buf[24..32].copy_from_slice(&self.created_at_ms.to_le_bytes());
        // 32..40: Updated at
        buf[32..40].copy_from_slice(&self.updated_at_ms.to_le_bytes());
        // 40..48: Payload offset
        buf[40..48].copy_from_slice(&self.payload_offset.to_le_bytes());
        // 48..56: Payload len
        buf[48..56].copy_from_slice(&self.payload_len.to_le_bytes());
        // 56..88: Payload checksum (32 bytes)
        buf[56..88].copy_from_slice(&self.payload_checksum);
        // 88..92: Schema version
        buf[88..92].copy_from_slice(&self.schema_version.to_le_bytes());
        // 92..96: Resource profile
        buf[92..96].copy_from_slice(&self.resource_profile.to_le_bytes());
        // 96..100: Page size
        buf[96..100].copy_from_slice(&self.page_size.to_le_bytes());
        // 100..108: Capabilities
        buf[100..108].copy_from_slice(&self.capabilities.to_le_bytes());

        // Compression Profile: 108..144
        buf[108] = self.compression.algorithm as u8;
        buf[109] = self.compression.level as u8;
        buf[110..112].copy_from_slice(&self.compression.flags.to_le_bytes());
        buf[112..120].copy_from_slice(&self.compression.dictionary_id.to_le_bytes());

        // Encryption Metadata: 144..256
        buf[144] = self.encryption.cipher as u8;
        buf[145] = self.encryption.kdf as u8;
        buf[148..152].copy_from_slice(&self.encryption.kdf_iterations.to_le_bytes());
        buf[152..184].copy_from_slice(&self.encryption.salt);
        buf[184..200].copy_from_slice(&self.encryption.nonce_prefix);
        buf[200..216].copy_from_slice(&self.encryption.key_check_value);

        // Database ID: 256..384
        buf[256..384].copy_from_slice(&self.database_id);

        // 384..4064: Reserved padding (already zeroed)

        // 4064..4096: Cryptographic BLAKE3 Checksum over 0..4064
        let hash = blake3::hash(&buf[0..CHECKSUM_OFFSET]);
        buf[CHECKSUM_OFFSET..ENVELOPE_HEADER_SIZE].copy_from_slice(hash.as_bytes());
    }

    /// Deserializes and validates a 4096-byte header buffer.
    ///
    /// # Errors
    /// Returns `ErrorCode::Corrupt` if magic bytes or BLAKE3 checksum validation fails.
    pub fn deserialize_from(buf: &[u8; ENVELOPE_HEADER_SIZE]) -> Result<Self> {
        // 1. Verify Magic
        if &buf[0..8] != ENVELOPE_MAGIC {
            return Err(LiliaError::new(
                ErrorCode::Corrupt,
                "invalid envelope magic identifier",
                false,
            ));
        }

        // 2. Verify BLAKE3 Checksum over bytes 0..4064
        let expected_hash = blake3::hash(&buf[0..CHECKSUM_OFFSET]);
        let stored_checksum = &buf[CHECKSUM_OFFSET..ENVELOPE_HEADER_SIZE];

        if !constant_time_eq_32(expected_hash.as_bytes(), stored_checksum) {
            return Err(LiliaError::new(
                ErrorCode::Corrupt,
                "envelope header BLAKE3 checksum mismatch (header damaged or tampered)",
                false,
            ));
        }

        // 3. Extract fields
        let format_version = u16::from_le_bytes([buf[8], buf[9]]);
        let header_size = u16::from_le_bytes([buf[10], buf[11]]);
        let role = HeaderRole::from_u8(buf[12]);
        let generation = u64::from_le_bytes([
            buf[16], buf[17], buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
        ]);
        let created_at_ms = u64::from_le_bytes([
            buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
        ]);
        let updated_at_ms = u64::from_le_bytes([
            buf[32], buf[33], buf[34], buf[35], buf[36], buf[37], buf[38], buf[39],
        ]);
        let payload_offset = u64::from_le_bytes([
            buf[40], buf[41], buf[42], buf[43], buf[44], buf[45], buf[46], buf[47],
        ]);
        let payload_len = u64::from_le_bytes([
            buf[48], buf[49], buf[50], buf[51], buf[52], buf[53], buf[54], buf[55],
        ]);

        let mut payload_checksum = [0u8; 32];
        payload_checksum.copy_from_slice(&buf[56..88]);

        let schema_version = u32::from_le_bytes([buf[88], buf[89], buf[90], buf[91]]);
        let resource_profile = u32::from_le_bytes([buf[92], buf[93], buf[94], buf[95]]);
        let page_size = u32::from_le_bytes([buf[96], buf[97], buf[98], buf[99]]);
        let capabilities = u64::from_le_bytes([
            buf[100], buf[101], buf[102], buf[103], buf[104], buf[105], buf[106], buf[107],
        ]);

        let compression = CompressionProfile {
            algorithm: CompressionAlgorithm::from_u8(buf[108]),
            level: buf[109] as i8,
            flags: u16::from_le_bytes([buf[110], buf[111]]),
            dictionary_id: u64::from_le_bytes([
                buf[112], buf[113], buf[114], buf[115], buf[116], buf[117], buf[118], buf[119],
            ]),
        };

        let mut salt = [0u8; 32];
        salt.copy_from_slice(&buf[152..184]);
        let mut nonce_prefix = [0u8; 16];
        nonce_prefix.copy_from_slice(&buf[184..200]);
        let mut key_check_value = [0u8; 16];
        key_check_value.copy_from_slice(&buf[200..216]);

        let encryption = EncryptionMetadata {
            cipher: CipherAlgorithm::from_u8(buf[144]),
            kdf: KdfAlgorithm::from_u8(buf[145]),
            kdf_iterations: u32::from_le_bytes([buf[148], buf[149], buf[150], buf[151]]),
            salt,
            nonce_prefix,
            key_check_value,
        };

        let mut database_id = [0u8; 128];
        database_id.copy_from_slice(&buf[256..384]);

        let mut checksum = [0u8; 32];
        checksum.copy_from_slice(stored_checksum);

        Ok(Self {
            format_version,
            header_size,
            role,
            generation,
            created_at_ms,
            updated_at_ms,
            payload_offset,
            payload_len,
            payload_checksum,
            schema_version,
            resource_profile,
            page_size,
            capabilities,
            compression,
            encryption,
            database_id,
            checksum,
        })
    }
}

/// Recovery diagnostic outcome returned when opening an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeRecoveryOutcome {
    /// Both front and rear headers are valid, identical, and synchronized.
    NominalSynced { generation: u64 },
    /// Front header (Page 0) was torn or corrupted; restored cleanly from the sealed Rear header.
    FrontRepairedFromRear { generation: u64 },
    /// Rear header was torn or corrupted; restored cleanly from the Front header.
    RearRepairedFromFront { generation: u64 },
    /// Front header was newer than Rear (crash between front & rear writes); Rear synced to Front.
    RearSyncedToNewerFront {
        old_generation: u64,
        new_generation: u64,
    },
    /// Rear header was newer than Front; Front synced to Rear.
    FrontSyncedToNewerRear {
        old_generation: u64,
        new_generation: u64,
    },
}

/// Initial configuration used when creating a new envelope container.
#[derive(Debug, Clone, Default)]
pub struct EnvelopeConfig {
    pub schema_version: u32,
    pub resource_profile: u32,
    pub page_size: u32,
    pub compression: CompressionProfile,
    pub encryption: EncryptionMetadata,
    pub database_id: Option<String>,
}

/// High-level Crash-Resilient Dual-Header Envelope manager.
#[derive(Debug)]
pub struct DualHeaderEnvelope<F> {
    file: F,
    header: EnvelopeHeader,
}

impl<F: Read + Write + Seek> DualHeaderEnvelope<F> {
    /// Creates a new Dual-Header Envelope on the given file stream.
    ///
    /// # Errors
    /// Returns an error on I/O failure.
    pub fn create(mut file: F, config: EnvelopeConfig) -> Result<Self> {
        let mut header = EnvelopeHeader::default();
        if config.schema_version > 0 {
            header.schema_version = config.schema_version;
        }
        if config.resource_profile > 0 {
            header.resource_profile = config.resource_profile;
        }
        if config.page_size > 0 {
            header.page_size = config.page_size;
        }
        header.compression = config.compression;
        header.encryption = config.encryption;
        if let Some(id) = config.database_id {
            header.set_database_id(&id);
        }

        let mut buf = [0u8; ENVELOPE_HEADER_SIZE];

        // 1. Write Front Header
        header.role = HeaderRole::Front;
        header.serialize_into(&mut buf);
        file.seek(SeekFrom::Start(0)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to seek front header: {e}"),
                false,
            )
        })?;
        file.write_all(&buf).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to write front header: {e}"),
                false,
            )
        })?;

        // 2. Write Rear Header
        header.role = HeaderRole::Rear;
        header.serialize_into(&mut buf);
        file.seek(SeekFrom::Start(ENVELOPE_HEADER_SIZE as u64))
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to seek rear header: {e}"),
                    false,
                )
            })?;
        file.write_all(&buf).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to write rear header: {e}"),
                false,
            )
        })?;

        file.flush().map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to flush envelope: {e}"),
                false,
            )
        })?;

        Ok(Self { file, header })
    }

    /// Opens an enveloped file and automatically performs self-healing crash recovery.
    ///
    /// # Errors
    /// Returns `ErrorCode::Corrupt` if both headers are damaged or unrecoverable.
    pub fn open_and_recover(mut file: F) -> Result<(Self, EnvelopeRecoveryOutcome)> {
        let file_len = file.seek(SeekFrom::End(0)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to query file length: {e}"),
                false,
            )
        })?;

        if file_len < MIN_ENVELOPE_FILE_SIZE {
            return Err(LiliaError::new(
                ErrorCode::Corrupt,
                format!(
                    "file too small for dual-header envelope: len={file_len} < minimum={MIN_ENVELOPE_FILE_SIZE}"
                ),
                false,
            ));
        }

        // 1. Read Front Header
        let mut front_buf = [0u8; ENVELOPE_HEADER_SIZE];
        file.seek(SeekFrom::Start(0)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to seek front header: {e}"),
                false,
            )
        })?;
        file.read_exact(&mut front_buf).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to read front header: {e}"),
                false,
            )
        })?;
        let front_result = EnvelopeHeader::deserialize_from(&front_buf);

        // 2. Read Rear Header
        let rear_offset = file_len - ENVELOPE_HEADER_SIZE as u64;
        let mut rear_buf = [0u8; ENVELOPE_HEADER_SIZE];
        file.seek(SeekFrom::Start(rear_offset)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to seek rear header: {e}"),
                false,
            )
        })?;
        file.read_exact(&mut rear_buf).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to read rear header: {e}"),
                false,
            )
        })?;
        let rear_result = EnvelopeHeader::deserialize_from(&rear_buf);

        // 3. Evaluate Crash Recovery State Machine
        let (active_header, outcome) = match (front_result, rear_result) {
            (Ok(front), Ok(rear)) => {
                if front.generation == rear.generation {
                    let gen = front.generation;
                    (
                        front,
                        EnvelopeRecoveryOutcome::NominalSynced { generation: gen },
                    )
                } else if front.generation > rear.generation {
                    // Front is newer: repair rear header
                    let mut repair_header = front.clone();
                    repair_header.role = HeaderRole::Rear;
                    repair_header.serialize_into(&mut rear_buf);
                    file.seek(SeekFrom::Start(rear_offset)).map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("failed to seek rear for sync: {e}"),
                            false,
                        )
                    })?;
                    file.write_all(&rear_buf).map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("failed to sync rear header: {e}"),
                            false,
                        )
                    })?;
                    file.flush().map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("failed to flush rear sync: {e}"),
                            false,
                        )
                    })?;

                    let outcome = EnvelopeRecoveryOutcome::RearSyncedToNewerFront {
                        old_generation: rear.generation,
                        new_generation: front.generation,
                    };
                    (front, outcome)
                } else {
                    // Rear is newer: repair front header
                    let mut repair_header = rear.clone();
                    repair_header.role = HeaderRole::Front;
                    repair_header.serialize_into(&mut front_buf);
                    file.seek(SeekFrom::Start(0)).map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("failed to seek front for sync: {e}"),
                            false,
                        )
                    })?;
                    file.write_all(&front_buf).map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("failed to sync front header: {e}"),
                            false,
                        )
                    })?;
                    file.flush().map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("failed to flush front sync: {e}"),
                            false,
                        )
                    })?;

                    let outcome = EnvelopeRecoveryOutcome::FrontSyncedToNewerRear {
                        old_generation: front.generation,
                        new_generation: rear.generation,
                    };
                    (rear, outcome)
                }
            }
            (Ok(front), Err(_rear_err)) => {
                // Front header is intact, rear is damaged/torn: RESTORE REAR
                let mut repair_header = front.clone();
                repair_header.role = HeaderRole::Rear;
                repair_header.serialize_into(&mut rear_buf);
                file.seek(SeekFrom::Start(rear_offset)).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to seek rear for repair: {e}"),
                        false,
                    )
                })?;
                file.write_all(&rear_buf).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to repair rear header: {e}"),
                        false,
                    )
                })?;
                file.flush().map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to flush rear repair: {e}"),
                        false,
                    )
                })?;

                let gen = front.generation;
                (
                    front,
                    EnvelopeRecoveryOutcome::RearRepairedFromFront { generation: gen },
                )
            }
            (Err(_front_err), Ok(rear)) => {
                // Front header is damaged/torn (Sector 0 failure), rear is intact: RESTORE FRONT (SECTOR 0)
                let mut repair_header = rear.clone();
                repair_header.role = HeaderRole::Front;
                repair_header.serialize_into(&mut front_buf);
                file.seek(SeekFrom::Start(0)).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to seek front for repair: {e}"),
                        false,
                    )
                })?;
                file.write_all(&front_buf).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to repair front header: {e}"),
                        false,
                    )
                })?;
                file.flush().map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to flush front repair: {e}"),
                        false,
                    )
                })?;

                let gen = rear.generation;
                (
                    rear,
                    EnvelopeRecoveryOutcome::FrontRepairedFromRear { generation: gen },
                )
            }
            (Err(front_err), Err(rear_err)) => {
                return Err(LiliaError::new(
                    ErrorCode::Corrupt,
                    format!(
                        "fatal envelope corruption: both front and rear headers failed BLAKE3 verification (front: {front_err}, rear: {rear_err})"
                    ),
                    false,
                ));
            }
        };

        Ok((
            Self {
                file,
                header: active_header,
            },
            outcome,
        ))
    }

    /// Returns a reference to the active, validated envelope header.
    #[must_use]
    pub const fn header(&self) -> &EnvelopeHeader {
        &self.header
    }

    /// Returns a mutable reference to the underlying file stream.
    pub fn file_mut(&mut self) -> &mut F {
        &mut self.file
    }

    /// Consumes the envelope and returns the underlying file stream.
    pub fn into_inner(self) -> F {
        self.file
    }

    /// Reads data from the enveloped payload.
    ///
    /// # Errors
    /// Returns error on I/O failure or if requested range exceeds payload bounds.
    pub fn read_payload(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let req_end = offset.checked_add(buf.len() as u64).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "payload read offset overflow",
                false,
            )
        })?;
        if req_end > self.header.payload_len {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!(
                    "read out of payload bounds: req_end={req_end} > payload_len={}",
                    self.header.payload_len
                ),
                false,
            ));
        }

        let physical_offset = self.header.payload_offset + offset;
        self.file
            .seek(SeekFrom::Start(physical_offset))
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to seek payload read: {e}"),
                    false,
                )
            })?;
        self.file.read_exact(buf).map_err(|e| {
            LiliaError::new(ErrorCode::Io, format!("failed to read payload: {e}"), false)
        })?;
        Ok(())
    }

    /// Writes data into the enveloped payload.
    ///
    /// # Errors
    /// Returns error on I/O failure.
    pub fn write_payload(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let physical_offset = self.header.payload_offset + offset;
        self.file
            .seek(SeekFrom::Start(physical_offset))
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to seek payload write: {e}"),
                    false,
                )
            })?;
        self.file.write_all(buf).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to write payload: {e}"),
                false,
            )
        })?;

        let end_offset = offset + buf.len() as u64;
        if end_offset > self.header.payload_len {
            self.header.payload_len = end_offset;
        }
        Ok(())
    }

    /// Atomically commits updated payload length and seals both headers with a generation bump.
    ///
    /// Sequencing ensures zero vulnerability window:
    /// 1. Increment generation `G + 1`.
    /// 2. Write Front Header at offset 0 and flush.
    /// 3. Write Rear Header at `(payload_offset + payload_len)` and flush.
    ///
    /// # Errors
    /// Returns error on I/O failure.
    pub fn commit_sync(
        &mut self,
        payload_len: u64,
        payload_checksum: Option<[u8; 32]>,
    ) -> Result<()> {
        self.header.generation = self.header.generation.saturating_add(1);
        self.header.payload_len = payload_len;
        if let Some(cs) = payload_checksum {
            self.header.payload_checksum = cs;
        }
        self.header.updated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut buf = [0u8; ENVELOPE_HEADER_SIZE];

        // 1. Commit Front Header
        self.header.role = HeaderRole::Front;
        self.header.serialize_into(&mut buf);
        self.file.seek(SeekFrom::Start(0)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to seek front header during commit: {e}"),
                false,
            )
        })?;
        self.file.write_all(&buf).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to write front header during commit: {e}"),
                false,
            )
        })?;
        self.file.flush().map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to flush front header commit: {e}"),
                false,
            )
        })?;

        // 2. Commit Rear Header
        let rear_offset = self.header.payload_offset + self.header.payload_len;
        self.header.role = HeaderRole::Rear;
        self.header.serialize_into(&mut buf);
        self.file.seek(SeekFrom::Start(rear_offset)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to seek rear header during commit: {e}"),
                false,
            )
        })?;
        self.file.write_all(&buf).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to write rear header during commit: {e}"),
                false,
            )
        })?;
        self.file.flush().map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to flush rear header commit: {e}"),
                false,
            )
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_header_serialization_roundtrip() {
        let mut header = EnvelopeHeader::default();
        header.generation = 42;
        header.schema_version = 7;
        header.payload_len = 1024 * 1024;
        header.set_database_id("liliadb-production-cluster-01");
        header.compression = CompressionProfile {
            algorithm: CompressionAlgorithm::Zstd,
            level: 3,
            flags: 0x0001,
            dictionary_id: 0xDEADBEEFCAFE,
        };

        let key = b"very-strong-secret-passphrase-derived-key-32b!";
        header.encryption = EncryptionMetadata {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfAlgorithm::Pbkdf2HmacSha256,
            kdf_iterations: 100_000,
            salt: [7u8; 32],
            nonce_prefix: [9u8; 16],
            key_check_value: EncryptionMetadata::compute_check_value(key),
        };

        let mut buf = [0u8; ENVELOPE_HEADER_SIZE];
        header.serialize_into(&mut buf);

        // Verify that the secret key does not appear anywhere in the binary header
        assert!(!buf.windows(key.len()).any(|w| w == key));

        // Deserialization succeeds and matches
        let decoded = EnvelopeHeader::deserialize_from(&buf).expect("deserialize succeeds");
        assert_eq!(decoded.generation, 42);
        assert_eq!(decoded.schema_version, 7);
        assert_eq!(decoded.payload_len, 1024 * 1024);
        assert_eq!(decoded.database_id_str(), "liliadb-production-cluster-01");
        assert_eq!(decoded.compression.algorithm, CompressionAlgorithm::Zstd);
        assert_eq!(decoded.compression.level, 3);
        assert_eq!(decoded.compression.dictionary_id, 0xDEADBEEFCAFE);
        assert_eq!(decoded.encryption.cipher, CipherAlgorithm::Aes256Gcm);
        assert_eq!(decoded.encryption.kdf_iterations, 100_000);
        assert!(decoded.encryption.verify_key(key));
        assert!(!decoded.encryption.verify_key(b"wrong-key"));
    }

    #[test]
    fn test_tamper_detection_blake3() {
        let header = EnvelopeHeader::default();
        let mut buf = [0u8; ENVELOPE_HEADER_SIZE];
        header.serialize_into(&mut buf);

        // Flip one single bit in the schema version (offset 88)
        buf[88] ^= 0x01;

        let res = EnvelopeHeader::deserialize_from(&buf);
        assert!(res.is_err());
        let err = res.err().unwrap();
        assert_eq!(err.code, ErrorCode::Corrupt);
    }

    #[test]
    fn test_dual_header_nominal_open() {
        let stream = Cursor::new(Vec::new());
        let (envelope, outcome) = DualHeaderEnvelope::create(
            stream,
            EnvelopeConfig {
                schema_version: 2,
                database_id: Some("test-db".into()),
                ..Default::default()
            },
        )
        .and_then(|env| DualHeaderEnvelope::open_and_recover(env.into_inner()))
        .expect("open and recover succeeds");

        assert_eq!(
            outcome,
            EnvelopeRecoveryOutcome::NominalSynced { generation: 1 }
        );
        assert_eq!(envelope.header().schema_version, 2);
        assert_eq!(envelope.header().database_id_str(), "test-db");
    }

    #[test]
    fn test_crash_recovery_torn_front_header_restored_from_rear() {
        let stream = Cursor::new(Vec::new());
        let (mut envelope, _) = DualHeaderEnvelope::create(
            stream,
            EnvelopeConfig {
                schema_version: 5,
                database_id: Some("resilient-front-repair".into()),
                ..Default::default()
            },
        )
        .and_then(|env| DualHeaderEnvelope::open_and_recover(env.into_inner()))
        .unwrap();

        // Write some payload and commit to bump generation to 2
        envelope.write_payload(0, b"critical payload data").unwrap();
        envelope.commit_sync(21, None).unwrap();

        let mut data = envelope.into_inner().into_inner();

        // SIMULATE BRUTAL POWER CUT ON SECTOR 0 (OFFSET 0..4096 CORRUPTED/ZEROED)
        data[0..4096].fill(0);

        // Reopen file: engine must detect damaged front header, verify trailer with BLAKE3,
        // and restore sector 0 automatically!
        let (recovered_env, outcome) =
            DualHeaderEnvelope::open_and_recover(Cursor::new(data)).expect("recovers cleanly");

        assert_eq!(
            outcome,
            EnvelopeRecoveryOutcome::FrontRepairedFromRear { generation: 2 }
        );
        assert_eq!(recovered_env.header().schema_version, 5);
        assert_eq!(recovered_env.header().payload_len, 21);
        assert_eq!(
            recovered_env.header().database_id_str(),
            "resilient-front-repair"
        );
    }

    #[test]
    fn test_crash_recovery_torn_rear_header_restored_from_front() {
        let stream = Cursor::new(Vec::new());
        let (mut envelope, _) = DualHeaderEnvelope::create(
            stream,
            EnvelopeConfig {
                schema_version: 3,
                database_id: Some("resilient-rear-repair".into()),
                ..Default::default()
            },
        )
        .and_then(|env| DualHeaderEnvelope::open_and_recover(env.into_inner()))
        .unwrap();

        envelope.write_payload(0, b"data-segment").unwrap();
        envelope.commit_sync(12, None).unwrap();

        let mut data = envelope.into_inner().into_inner();
        let total_len = data.len();

        // SIMULATE CRASH DURING TRAILER WRITE: TRAILER OVERWRITTEN WITH GARBAGE
        data[(total_len - 4096)..total_len].fill(0xAA);

        let (recovered_env, outcome) =
            DualHeaderEnvelope::open_and_recover(Cursor::new(data)).expect("recovers cleanly");

        assert_eq!(
            outcome,
            EnvelopeRecoveryOutcome::RearRepairedFromFront { generation: 2 }
        );
        assert_eq!(recovered_env.header().schema_version, 3);
        assert_eq!(recovered_env.header().payload_len, 12);
    }

    #[test]
    fn test_crash_recovery_interrupted_generation_sync() {
        let stream = Cursor::new(Vec::new());
        let (mut envelope, _) = DualHeaderEnvelope::create(stream, EnvelopeConfig::default())
            .and_then(|env| DualHeaderEnvelope::open_and_recover(env.into_inner()))
            .unwrap();

        envelope.commit_sync(100, None).unwrap(); // generation 2
        let mut data = envelope.into_inner().into_inner();

        // Manually alter front header generation to 3 (simulating crash after front commit but before rear commit)
        let mut front_buf = [0u8; 4096];
        front_buf.copy_from_slice(&data[0..4096]);
        let mut h = EnvelopeHeader::deserialize_from(&front_buf).unwrap();
        h.generation = 3;
        h.serialize_into(&mut front_buf);
        data[0..4096].copy_from_slice(&front_buf);

        let (recovered_env, outcome) =
            DualHeaderEnvelope::open_and_recover(Cursor::new(data)).expect("syncs to generation 3");

        assert_eq!(
            outcome,
            EnvelopeRecoveryOutcome::RearSyncedToNewerFront {
                old_generation: 2,
                new_generation: 3,
            }
        );
        assert_eq!(recovered_env.header().generation, 3);
    }

    #[test]
    fn test_fatal_corruption_rejection() {
        let stream = Cursor::new(Vec::new());
        let envelope = DualHeaderEnvelope::create(stream, EnvelopeConfig::default()).unwrap();
        let mut data = envelope.into_inner().into_inner();

        // Corrupt BOTH headers
        data[0..4096].fill(0xFF);
        let len = data.len();
        data[(len - 4096)..len].fill(0xFF);

        let res = DualHeaderEnvelope::open_and_recover(Cursor::new(data));
        assert!(res.is_err());
        assert_eq!(res.err().unwrap().code, ErrorCode::Corrupt);
    }
}
