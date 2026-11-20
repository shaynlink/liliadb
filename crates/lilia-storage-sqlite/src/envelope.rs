//! Storage envelope manager for LiliaDB.
//!
//! Provides file-level integration for Dual-Header Symmetric Envelopes:
//! - Pre-flight verification and crash recovery / auto-healing on database open
//! - Standalone protected database export & import with compression and encryption metadata
//! - Verification of envelope integrity and automatic restoration of corrupted Page 0 sectors.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use lilia_core::envelope::{
    CompressionAlgorithm, CompressionProfile, DualHeaderEnvelope, EncryptionMetadata,
    EnvelopeConfig, EnvelopeHeader, EnvelopeRecoveryOutcome, ENVELOPE_HEADER_SIZE, ENVELOPE_MAGIC,
    MIN_ENVELOPE_FILE_SIZE,
};
use lilia_core::{ErrorCode, LiliaError, Result};

/// Storage-level manager for dual-header enveloped files.
#[derive(Debug)]
pub struct EnvelopeManager;

impl EnvelopeManager {
    /// Checks whether the file at `path` starts with the dual-header envelope magic.
    #[must_use]
    pub fn is_enveloped_file(path: impl AsRef<Path>) -> bool {
        let Ok(mut file) = File::open(path.as_ref()) else {
            return false;
        };
        let mut magic = [0u8; 8];
        if file.read_exact(&mut magic).is_err() {
            return false;
        }
        &magic == ENVELOPE_MAGIC
    }

    /// Inspects and heals an enveloped file at `path` before database access.
    ///
    /// If the file is not an enveloped container (or does not exist), returns `Ok(None)`.
    /// If the file is an enveloped container, it executes the dual-header self-healing
    /// state machine. If sector 0 (or trailer) was damaged during an interrupted write,
    /// it repairs the damaged sector from the surviving valid header.
    ///
    /// # Errors
    /// Returns `ErrorCode::Corrupt` if both headers are destroyed.
    pub fn verify_and_heal_file(path: impl AsRef<Path>) -> Result<Option<EnvelopeRecoveryOutcome>> {
        let p = path.as_ref();
        if !p.exists() {
            return Ok(None);
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(p)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to open file for envelope inspection: {e}"),
                    false,
                )
            })?;

        let len = file.seek(SeekFrom::End(0)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to seek file length: {e}"),
                false,
            )
        })?;

        if len < MIN_ENVELOPE_FILE_SIZE {
            return Ok(None);
        }

        // Check front magic or trailer magic (in case front was zeroed/torn)
        let mut front_magic = [0u8; 8];
        file.seek(SeekFrom::Start(0)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to seek front magic: {e}"),
                false,
            )
        })?;
        let _ = file.read_exact(&mut front_magic);

        let mut rear_magic = [0u8; 8];
        file.seek(SeekFrom::Start(len - ENVELOPE_HEADER_SIZE as u64))
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to seek rear magic: {e}"),
                    false,
                )
            })?;
        let _ = file.read_exact(&mut rear_magic);

        // If neither front nor rear has envelope magic, this is a standard raw database file
        if &front_magic != ENVELOPE_MAGIC && &rear_magic != ENVELOPE_MAGIC {
            return Ok(None);
        }

        // Execute recovery
        let (_env, outcome) = DualHeaderEnvelope::open_and_recover(&mut file)?;
        file.sync_all().map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to sync healed envelope: {e}"),
                false,
            )
        })?;

        Ok(Some(outcome))
    }

    /// Exports a database file into a verified, crash-resistant dual-header envelope container.
    ///
    /// # Errors
    /// Returns an error if source database cannot be read or envelope creation fails.
    pub fn export_database_to_envelope(
        source_db_path: impl AsRef<Path>,
        target_envelope_path: impl AsRef<Path>,
        compression: CompressionProfile,
        encryption: EncryptionMetadata,
        schema_version: u32,
    ) -> Result<()> {
        let src = source_db_path.as_ref();
        let dst = target_envelope_path.as_ref();

        if !src.exists() {
            return Err(LiliaError::new(
                ErrorCode::NotFound,
                format!("source database not found at {}", src.display()),
                false,
            ));
        }

        let raw_payload = std::fs::read(src).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to read source database: {e}"),
                false,
            )
        })?;

        // Optional compression of the payload
        let compressed_payload = match compression.algorithm {
            CompressionAlgorithm::None => raw_payload,
            CompressionAlgorithm::Zstd => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    zstd::encode_all(&raw_payload[..], compression.level as i32).map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Storage,
                            format!("zstd compression failed during envelope export: {e}"),
                            false,
                        )
                    })?
                }
                #[cfg(target_arch = "wasm32")]
                {
                    return Err(LiliaError::new(
                        ErrorCode::Unsupported,
                        "zstd compression not supported on wasm32",
                        false,
                    ));
                }
            }
            CompressionAlgorithm::Deflate => lilia_core::compress_bytes(&raw_payload),
            _ => raw_payload,
        };

        let payload_len = compressed_payload.len() as u64;
        let payload_hash = blake3::hash(&compressed_payload);

        let target_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dst)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to create target envelope at {}: {e}", dst.display()),
                    false,
                )
            })?;

        let config = EnvelopeConfig {
            schema_version,
            resource_profile: 1,
            page_size: 4096,
            compression,
            encryption,
            database_id: src
                .file_name()
                .and_then(|n| n.to_str())
                .map(ToString::to_string),
        };

        let mut envelope = DualHeaderEnvelope::create(target_file, config)?;
        envelope.write_payload(0, &compressed_payload)?;
        envelope.commit_sync(payload_len, Some(*payload_hash.as_bytes()))?;

        envelope.file_mut().sync_all().map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to sync target envelope: {e}"),
                false,
            )
        })?;

        Ok(())
    }

    /// Imports a database payload from a verified dual-header envelope container into a target file.
    ///
    /// # Errors
    /// Returns an error if the envelope is corrupt, decompression fails, or file writing fails.
    pub fn import_database_from_envelope(
        source_envelope_path: impl AsRef<Path>,
        target_db_path: impl AsRef<Path>,
    ) -> Result<EnvelopeHeader> {
        let src = source_envelope_path.as_ref();
        let dst = target_db_path.as_ref();

        let source_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(src)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to open source envelope at {}: {e}", src.display()),
                    false,
                )
            })?;

        let (mut envelope, _outcome) = DualHeaderEnvelope::open_and_recover(source_file)?;
        let header = envelope.header().clone();

        let payload_len = header.payload_len as usize;
        let mut compressed_buf = vec![0u8; payload_len];
        envelope.read_payload(0, &mut compressed_buf)?;

        // Verify payload BLAKE3 checksum if recorded
        if header.payload_checksum != [0u8; 32] {
            let actual_hash = blake3::hash(&compressed_buf);
            if actual_hash.as_bytes() != &header.payload_checksum {
                return Err(LiliaError::new(
                    ErrorCode::Corrupt,
                    "envelope payload BLAKE3 checksum verification failed",
                    false,
                ));
            }
        }

        // Decompress payload
        let uncompressed_payload = match header.compression.algorithm {
            CompressionAlgorithm::None => compressed_buf,
            CompressionAlgorithm::Zstd => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    zstd::decode_all(&compressed_buf[..]).map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Storage,
                            format!("zstd decompression failed: {e}"),
                            false,
                        )
                    })?
                }
                #[cfg(target_arch = "wasm32")]
                {
                    return Err(LiliaError::new(
                        ErrorCode::Unsupported,
                        "zstd decompression not supported on wasm32",
                        false,
                    ));
                }
            }
            CompressionAlgorithm::Deflate => lilia_core::decompress_bytes(&compressed_buf)?,
            _ => compressed_buf,
        };

        // Write uncompressed bytes to target
        let mut target_file = File::create(dst).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to create target database file: {e}"),
                false,
            )
        })?;

        target_file.write_all(&uncompressed_payload).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to write target database file: {e}"),
                false,
            )
        })?;

        target_file.sync_all().map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to sync target database file: {e}"),
                false,
            )
        })?;

        Ok(header)
    }
}
