use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Instant;

use lilia_core::sharding::ShardArchiveResult;
use lilia_core::{ErrorCode, LiliaError, Result};

pub const MAGIC_ARCHIVE: &[u8; 4] = b"LILZ";
pub const ARCHIVE_VERSION: u16 = 1;

/// Compresses an uncompressed SQLite shard database file into a verified Zstandard archive.
///
/// # Errors
/// Returns an error if the source file cannot be read, compression fails, integrity check fails,
/// or cleanup of the original uncompressed database file fails.
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
pub fn compress_shard(
    source_path: &Path,
    archive_path: &Path,
    shard_id: &str,
    zstd_level: i32,
) -> Result<ShardArchiveResult> {
    let start_time = Instant::now();

    if !source_path.exists() {
        return Err(LiliaError::new(
            ErrorCode::NotFound,
            format!("shard database file not found at {}", source_path.display()),
            false,
        ));
    }

    let uncompressed_bytes = std::fs::read(source_path).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to read shard file for compression: {e}"),
            false,
        )
    })?;

    let original_size = uncompressed_bytes.len() as u64;
    let checksum = blake3::hash(&uncompressed_bytes);
    let checksum_hex = checksum.to_hex().to_string();

    let compressed_payload =
        zstd::encode_all(&uncompressed_bytes[..], zstd_level).map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("zstd compression failed: {e}"),
                false,
            )
        })?;

    let temp_archive_path = archive_path.with_extension("zst.tmp");
    let mut file = File::create(&temp_archive_path).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to create temp archive file: {e}"),
            false,
        )
    })?;

    // Write header
    file.write_all(MAGIC_ARCHIVE).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to write magic: {e}"),
            false,
        )
    })?;
    file.write_all(&ARCHIVE_VERSION.to_le_bytes())
        .map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("failed to write version: {e}"),
                false,
            )
        })?;

    let id_bytes = shard_id.as_bytes();
    let id_len = u8::try_from(id_bytes.len())
        .map_err(|_| LiliaError::new(ErrorCode::InvalidInput, "shard id too long", false))?;
    file.write_all(&[id_len]).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to write id len: {e}"),
            false,
        )
    })?;
    file.write_all(id_bytes).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to write shard id: {e}"),
            false,
        )
    })?;
    file.write_all(&original_size.to_le_bytes()).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to write size: {e}"),
            false,
        )
    })?;
    file.write_all(checksum.as_bytes()).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to write checksum: {e}"),
            false,
        )
    })?;

    // Write compressed payload
    file.write_all(&compressed_payload).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to write compressed data: {e}"),
            false,
        )
    })?;
    file.flush().map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to flush archive: {e}"),
            false,
        )
    })?;
    drop(file);

    // Pre-verification: Verify decompression test before committing
    verify_archive(&temp_archive_path, &checksum_hex)?;

    // Atomically rename temp archive to final path
    std::fs::rename(&temp_archive_path, archive_path).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to finalize archive file: {e}"),
            false,
        )
    })?;

    let compressed_size = (std::fs::metadata(archive_path).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to stat archive: {e}"),
            false,
        )
    })?)
    .len();

    let compression_ratio = if original_size == 0 {
        1.0
    } else {
        compressed_size as f64 / original_size as f64
    };

    // Remove source database file and stray WAL/SHM files
    let _ = std::fs::remove_file(source_path);
    let wal_path = source_path.with_file_name(format!(
        "{}-wal",
        source_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    let shm_path = source_path.with_file_name(format!(
        "{}-shm",
        source_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    let _ = std::fs::remove_file(wal_path);
    let _ = std::fs::remove_file(shm_path);

    let elapsed = u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
    let archived_at_ms = crate::storage::now_ms();

    Ok(ShardArchiveResult {
        shard_id: shard_id.to_string(),
        archive_path: archive_path.to_path_buf(),
        original_size_bytes: original_size,
        compressed_size_bytes: compressed_size,
        compression_ratio,
        checksum: checksum_hex,
        duration_ms: elapsed,
        archived_at_ms,
    })
}

/// Decompresses an archived shard file (`.zst`) back into a full uncompressed SQLite database.
///
/// # Errors
/// Returns `Corrupt`, `NotFound` or `Storage` errors detected here, and propagates errors from the
/// operations it performs.
#[allow(clippy::cast_precision_loss)]
pub fn decompress_shard(archive_path: &Path, destination_path: &Path) -> Result<()> {
    if !archive_path.exists() {
        return Err(LiliaError::new(
            ErrorCode::NotFound,
            format!("archive file not found at {}", archive_path.display()),
            false,
        ));
    }

    let archive_bytes = std::fs::read(archive_path).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to read archive file: {e}"),
            false,
        )
    })?;

    if archive_bytes.len() < 4 + 2 + 1 + 8 + 32 {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "archive file header is truncated",
            false,
        ));
    }

    if &archive_bytes[0..4] != MAGIC_ARCHIVE {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "invalid archive magic bytes",
            false,
        ));
    }

    let version = u16::from_le_bytes([archive_bytes[4], archive_bytes[5]]);
    if version != ARCHIVE_VERSION {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            format!("unsupported archive version {version}"),
            false,
        ));
    }

    let id_len = archive_bytes[6] as usize;
    let header_offset = 7 + id_len + 8 + 32;
    if archive_bytes.len() < header_offset {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "archive truncated before payload",
            false,
        ));
    }

    let expected_checksum_bytes = &archive_bytes[7 + id_len + 8..header_offset];
    let payload = &archive_bytes[header_offset..];

    let decompressed = zstd::decode_all(payload).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("zstd decompression failed: {e}"),
            false,
        )
    })?;

    let decompressed_hash = blake3::hash(&decompressed);
    if decompressed_hash.as_bytes() != expected_checksum_bytes {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "decompressed checksum mismatch: data is corrupted",
            false,
        ));
    }

    let temp_dest = destination_path.with_extension("lilia.tmp");
    std::fs::write(&temp_dest, &decompressed).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to write decompressed shard file: {e}"),
            false,
        )
    })?;

    std::fs::rename(&temp_dest, destination_path).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to finalize decompressed shard file: {e}"),
            false,
        )
    })?;

    // Remove the archive file now that decompression succeeded
    let _ = std::fs::remove_file(archive_path);

    Ok(())
}

/// Verifies the structural integrity and checksum of an archive without keeping the uncompressed data.
///
/// # Errors
/// Returns `Corrupt` or `Storage` errors detected here, and propagates errors from the
/// operations it performs.
pub fn verify_archive(archive_path: &Path, expected_checksum_hex: &str) -> Result<()> {
    let mut file = File::open(archive_path).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("failed to open archive for verification: {e}"),
            false,
        )
    })?;

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to read magic: {e}"),
            false,
        )
    })?;
    if &magic != MAGIC_ARCHIVE {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "invalid archive magic",
            false,
        ));
    }

    let mut version_bytes = [0u8; 2];
    file.read_exact(&mut version_bytes).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to read version: {e}"),
            false,
        )
    })?;

    let mut id_len_buf = [0u8; 1];
    file.read_exact(&mut id_len_buf).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to read id len: {e}"),
            false,
        )
    })?;
    let id_len = id_len_buf[0] as usize;

    let mut id_buf = vec![0u8; id_len];
    file.read_exact(&mut id_buf).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to read shard id: {e}"),
            false,
        )
    })?;

    let mut size_buf = [0u8; 8];
    file.read_exact(&mut size_buf).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to read size: {e}"),
            false,
        )
    })?;

    let mut checksum_buf = [0u8; 32];
    file.read_exact(&mut checksum_buf).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to read checksum: {e}"),
            false,
        )
    })?;

    let mut payload = Vec::new();
    file.read_to_end(&mut payload).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to read payload: {e}"),
            false,
        )
    })?;

    let decompressed = zstd::decode_all(&payload[..]).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("archive verification decompression failed: {e}"),
            false,
        )
    })?;

    let actual_hash = blake3::hash(&decompressed).to_hex().to_string();
    if actual_hash != expected_checksum_hex {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            format!("checksum mismatch: expected {expected_checksum_hex}, got {actual_hash}"),
            false,
        ));
    }

    Ok(())
}
