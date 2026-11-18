//! Reed-Solomon parity management, background self-healing, and flash fault tolerance.
//!
//! Provides the companion `.rsp` (Reed-Solomon Parity) sidecar manager for LiliaDB databases.
//! Organizes pages into groups, computes mathematical parity and 16-byte BLAKE3 checksums,
//! detects physical NAND flash bitflips, and performs in-flight page reconstruction.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lilia_core::parity::{
    compute_page_checksum, PageRepairOutcome, ParityConfig, ReedSolomon, RepairSummary,
};
use lilia_core::{ErrorCode, LiliaError, Result};

/// Magic header bytes for the `.rsp` companion file: "LILIARSP".
pub const PARITY_MAGIC: &[u8; 8] = b"LILIARSP";

/// Current `.rsp` format version.
pub const PARITY_FORMAT_VERSION: u32 = 1;

/// Fixed 64-byte file header for `.rsp` sidecars.
pub const HEADER_SIZE: u64 = 64;

/// Manages the Reed-Solomon parity file and performs on-the-fly auto-repair.
#[derive(Debug)]
pub struct ParityManager {
    db_path: PathBuf,
    parity_path: PathBuf,
    config: ParityConfig,
    codec: ReedSolomon,
    lock: Mutex<()>,
}

impl ParityManager {
    /// Creates a new parity manager for the given database path and configuration.
    ///
    /// # Errors
    /// Returns an error if parity configuration is invalid.
    pub fn new(db_path: impl Into<PathBuf>, config: ParityConfig) -> Result<Self> {
        config.validate()?;
        let db_path = db_path.into();
        let parity_path = PathBuf::from(format!("{}.rsp", db_path.to_string_lossy()));
        let codec = ReedSolomon::new(config.data_shards, config.parity_shards)?;

        Ok(Self {
            db_path,
            parity_path,
            config,
            codec,
            lock: Mutex::new(()),
        })
    }

    #[must_use]
    pub fn parity_path(&self) -> &Path {
        &self.parity_path
    }

    #[must_use]
    pub const fn config(&self) -> &ParityConfig {
        &self.config
    }

    /// Fixed size in bytes of a single group record in the `.rsp` file.
    #[must_use]
    pub fn group_record_size(&self, page_size: usize) -> usize {
        // Group header: 8 bytes group_index + 4 bytes page_count + 4 bytes reserved = 16 bytes
        // Checksums: data_shards * 16 bytes
        // Parity blocks: parity_shards * page_size
        16 + (self.config.data_shards * 16) + (self.config.parity_shards * page_size)
    }

    /// Computes the database page size from the SQLite header or defaults to 4096.
    fn detect_page_size(&self) -> usize {
        let Ok(mut file) = File::open(&self.db_path) else {
            return 4096;
        };
        let mut header = [0u8; 100];
        if file.read_exact(&mut header).is_err() {
            return 4096;
        }

        if &header[..16] != b"SQLite format 3\0" {
            return 4096;
        }

        let raw_size = u16::from_be_bytes([header[16], header[17]]);
        if raw_size == 1 {
            65536
        } else if raw_size >= 512 && raw_size.is_power_of_two() {
            raw_size as usize
        } else {
            4096
        }
    }

    /// Synchronizes Reed-Solomon parity blocks for all pages currently in the database file.
    ///
    /// # Errors
    /// Returns an error if reading database pages or writing the `.rsp` file fails.
    pub fn sync_all(&self) -> Result<()> {
        let _guard = self.lock.lock().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "parity manager lock poisoned", false)
        })?;

        if !self.db_path.exists() {
            return Ok(());
        }

        let mut db_file = File::open(&self.db_path).map_err(|e| {
            LiliaError::new(ErrorCode::Io, format!("cannot open db file: {e}"), false)
        })?;

        let file_len = db_file
            .metadata()
            .map_err(|e| {
                LiliaError::new(ErrorCode::Io, format!("cannot get db metadata: {e}"), false)
            })?
            .len();

        let page_size = self.detect_page_size();
        if file_len < page_size as u64 {
            return Ok(());
        }

        let total_pages = (file_len / page_size as u64) as usize;
        let k = self.config.data_shards;
        let m = self.config.parity_shards;
        let total_groups = total_pages.div_ceil(k);
        let record_size = self.group_record_size(page_size);

        let mut parity_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.parity_path)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("cannot open parity file: {e}"),
                    false,
                )
            })?;

        // Write 64-byte Header
        let mut header = [0u8; 64];
        header[..8].copy_from_slice(PARITY_MAGIC);
        header[8..12].copy_from_slice(&PARITY_FORMAT_VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&(page_size as u32).to_le_bytes());
        header[16..20].copy_from_slice(&(k as u32).to_le_bytes());
        header[20..24].copy_from_slice(&(m as u32).to_le_bytes());
        header[24..32].copy_from_slice(&(total_groups as u64).to_le_bytes());
        header[32..40].copy_from_slice(&(total_pages as u64).to_le_bytes());

        parity_file.seek(SeekFrom::Start(0)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("seek parity header error: {e}"),
                false,
            )
        })?;
        parity_file.write_all(&header).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("write parity header error: {e}"),
                false,
            )
        })?;

        // Iterate groups and calculate parity
        let mut page_buffers = vec![vec![0u8; page_size]; k];
        let mut parity_buffers = vec![vec![0u8; page_size]; m];

        for g in 0..total_groups {
            let start_page = g * k + 1;
            let end_page = ((g + 1) * k).min(total_pages);
            let active_count = end_page - start_page + 1;

            let mut checksums = vec![[0u8; 16]; k];

            for (slot_idx, p_id) in (start_page..=end_page).enumerate() {
                let offset = (p_id as u64 - 1) * page_size as u64;
                db_file.seek(SeekFrom::Start(offset)).map_err(|e| {
                    LiliaError::new(ErrorCode::Io, format!("seek page {p_id} error: {e}"), false)
                })?;
                db_file
                    .read_exact(&mut page_buffers[slot_idx])
                    .map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("read page {p_id} error: {e}"),
                            false,
                        )
                    })?;
                checksums[slot_idx] = compute_page_checksum(&page_buffers[slot_idx]);
            }

            // Zero-fill remaining slots in last group if active_count < k
            for slot_idx in active_count..k {
                page_buffers[slot_idx].fill(0);
                checksums[slot_idx] = compute_page_checksum(&page_buffers[slot_idx]);
            }

            // Encode Reed-Solomon parity
            let data_slices: Vec<&[u8]> = page_buffers.iter().map(Vec::as_slice).collect();
            let mut parity_slices: Vec<&mut [u8]> =
                parity_buffers.iter_mut().map(Vec::as_mut_slice).collect();

            self.codec.encode(&data_slices, &mut parity_slices)?;

            // Assemble and write group record
            let group_offset = HEADER_SIZE + (g as u64 * record_size as u64);
            parity_file
                .seek(SeekFrom::Start(group_offset))
                .map_err(|e| {
                    LiliaError::new(ErrorCode::Io, format!("seek group {g} error: {e}"), false)
                })?;

            // 16-byte record header
            parity_file
                .write_all(&(g as u64).to_le_bytes())
                .map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("write group index error: {e}"),
                        false,
                    )
                })?;
            parity_file
                .write_all(&(active_count as u32).to_le_bytes())
                .map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("write group page count error: {e}"),
                        false,
                    )
                })?;
            parity_file.write_all(&0u32.to_le_bytes()).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("write group reserved error: {e}"),
                    false,
                )
            })?;

            // Write checksums: k * 16 bytes
            for c in &checksums {
                parity_file.write_all(c).map_err(|e| {
                    LiliaError::new(ErrorCode::Io, format!("write checksum error: {e}"), false)
                })?;
            }

            // Write parity shards: m * page_size bytes
            for p in &parity_buffers {
                parity_file.write_all(p).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("write parity shard error: {e}"),
                        false,
                    )
                })?;
            }
        }

        parity_file.sync_all().map_err(|e| {
            LiliaError::new(ErrorCode::Io, format!("sync parity file error: {e}"), false)
        })?;

        Ok(())
    }

    /// Verifies whether page `page_id` (1-indexed) in the database matches its stored parity checksum.
    ///
    /// # Errors
    /// Returns error on I/O failure or if parity file has not been initialized.
    pub fn verify_page(&self, page_id: u32) -> Result<bool> {
        if page_id == 0 || !self.parity_path.exists() || !self.db_path.exists() {
            return Ok(true);
        }

        let _guard = self.lock.lock().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "parity manager lock poisoned", false)
        })?;

        let page_size = self.detect_page_size();
        let k = self.config.data_shards;
        let g = (page_id as usize - 1) / k;
        let slot_in_group = (page_id as usize - 1) % k;
        let record_size = self.group_record_size(page_size);

        let mut parity_file = File::open(&self.parity_path).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("cannot open parity file: {e}"),
                false,
            )
        })?;

        let mut header = [0u8; 64];
        if parity_file.read_exact(&mut header).is_err() || &header[..8] != PARITY_MAGIC {
            return Ok(true);
        }

        let total_pages = u64::from_le_bytes([
            header[32], header[33], header[34], header[35], header[36], header[37], header[38],
            header[39],
        ]) as u32;

        if page_id > total_pages {
            return Ok(true);
        }

        let checksum_offset =
            HEADER_SIZE + (g as u64 * record_size as u64) + 16 + (slot_in_group as u64 * 16);
        parity_file
            .seek(SeekFrom::Start(checksum_offset))
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("seek checksum offset error: {e}"),
                    false,
                )
            })?;

        let mut expected_checksum = [0u8; 16];
        parity_file
            .read_exact(&mut expected_checksum)
            .map_err(|e| {
                LiliaError::new(ErrorCode::Io, format!("read checksum error: {e}"), false)
            })?;

        let mut db_file = File::open(&self.db_path).map_err(|e| {
            LiliaError::new(ErrorCode::Io, format!("cannot open db file: {e}"), false)
        })?;

        let file_len = db_file
            .metadata()
            .map_err(|e| {
                LiliaError::new(ErrorCode::Io, format!("cannot get db metadata: {e}"), false)
            })?
            .len();

        let page_offset = (page_id as u64 - 1) * page_size as u64;
        if page_offset + page_size as u64 > file_len {
            // Page is truncated or missing on disk
            return Ok(false);
        }

        db_file.seek(SeekFrom::Start(page_offset)).map_err(|e| {
            LiliaError::new(ErrorCode::Io, format!("seek db page error: {e}"), false)
        })?;

        let mut page_buf = vec![0u8; page_size];
        db_file.read_exact(&mut page_buf).map_err(|e| {
            LiliaError::new(ErrorCode::Io, format!("read db page error: {e}"), false)
        })?;

        let actual_checksum = compute_page_checksum(&page_buf);
        Ok(actual_checksum == expected_checksum)
    }

    /// Reconstructs a corrupted database page in-place using Reed-Solomon erasure decoding.
    ///
    /// # Errors
    /// Returns error if the parity file is missing, surviving shards are insufficient,
    /// or physical rewrite fails.
    pub fn reconstruct_and_repair_page(&self, page_id: u32) -> Result<PageRepairOutcome> {
        if page_id == 0 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "page_id must be non-zero (1-indexed)",
                false,
            ));
        }

        let _guard = self.lock.lock().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "parity manager lock poisoned", false)
        })?;

        let page_size = self.detect_page_size();
        let k = self.config.data_shards;
        let m = self.config.parity_shards;
        let g = (page_id as usize - 1) / k;
        let target_slot = (page_id as usize - 1) % k;
        let record_size = self.group_record_size(page_size);

        let mut parity_file = File::open(&self.parity_path).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("cannot open parity file for repair: {e}"),
                false,
            )
        })?;

        let group_offset = HEADER_SIZE + (g as u64 * record_size as u64);
        parity_file
            .seek(SeekFrom::Start(group_offset))
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("seek group record error: {e}"),
                    false,
                )
            })?;

        let mut record_header = [0u8; 16];
        parity_file.read_exact(&mut record_header).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("read group header error: {e}"),
                false,
            )
        })?;

        let active_count = u32::from_le_bytes([
            record_header[8],
            record_header[9],
            record_header[10],
            record_header[11],
        ]) as usize;

        // Read all checksums for group
        let mut expected_checksums = vec![[0u8; 16]; k];
        for c in &mut expected_checksums {
            parity_file.read_exact(c).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("read group checksums error: {e}"),
                    false,
                )
            })?;
        }

        // Read parity shards
        let mut parity_shards = vec![vec![0u8; page_size]; m];
        for p in &mut parity_shards {
            parity_file.read_exact(p).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("read parity block error: {e}"),
                    false,
                )
            })?;
        }

        let mut db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.db_path)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("cannot open db for repair write: {e}"),
                    false,
                )
            })?;

        // Read all other pages in the group
        let start_page = g * k + 1;
        let mut data_buffers = vec![vec![0u8; page_size]; k];

        for (slot_idx, p_id) in (start_page..start_page + active_count).enumerate() {
            if slot_idx != target_slot {
                let offset = (p_id as u64 - 1) * page_size as u64;
                db_file.seek(SeekFrom::Start(offset)).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("seek sister page {p_id} error: {e}"),
                        false,
                    )
                })?;
                db_file
                    .read_exact(&mut data_buffers[slot_idx])
                    .map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Io,
                            format!("read sister page {p_id} error: {e}"),
                            false,
                        )
                    })?;

                // Validate sister page checksum
                let c = compute_page_checksum(&data_buffers[slot_idx]);
                if c != expected_checksums[slot_idx] {
                    return Err(LiliaError::new(
                        ErrorCode::Corrupt,
                        format!("multiple page corruptions in group {g}: sister page {p_id} also corrupted"),
                        false,
                    ));
                }
            }
        }

        // Padding slots beyond active_count
        for buf in &mut data_buffers[active_count..k] {
            buf.fill(0);
        }

        // Assemble shards slice for Reed-Solomon reconstruction
        let mut shards: Vec<Option<&[u8]>> = Vec::with_capacity(k + m);
        for (slot_idx, buf) in data_buffers.iter().enumerate() {
            if slot_idx == target_slot {
                shards.push(None); // Erased!
            } else {
                shards.push(Some(buf.as_slice()));
            }
        }
        for p in &parity_shards {
            shards.push(Some(p.as_slice()));
        }

        // Reconstruct erased page
        let recovered = self.codec.reconstruct_erased(&shards, &[target_slot])?;
        if recovered.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to reconstruct page {page_id}"),
                false,
            ));
        }

        let reconstructed_bytes = &recovered[0].1;
        let recovered_checksum = compute_page_checksum(reconstructed_bytes);

        if recovered_checksum != expected_checksums[target_slot] {
            return Err(LiliaError::new(
                ErrorCode::Corrupt,
                format!("checksum of reconstructed page {page_id} does not match expected parity checksum"),
                false,
            ));
        }

        // Write the reconstructed page back to the database file
        let page_offset = (page_id as u64 - 1) * page_size as u64;
        db_file.seek(SeekFrom::Start(page_offset)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("seek for repair write error: {e}"),
                false,
            )
        })?;
        db_file.write_all(reconstructed_bytes).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("write repaired page error: {e}"),
                false,
            )
        })?;
        db_file.sync_all().map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("sync repaired page error: {e}"),
                false,
            )
        })?;

        Ok(PageRepairOutcome {
            page_id,
            group_index: g as u64,
            checksum_matches: true,
            bytes_reconstructed: page_size,
        })
    }

    /// Patrolls all database pages and automatically repairs any detected bitflips or corruptions.
    ///
    /// # Errors
    /// Returns an error if parity file is unreadable.
    pub fn auto_repair_database(&self) -> Result<RepairSummary> {
        if !self.parity_path.exists() || !self.db_path.exists() {
            return Ok(RepairSummary::default());
        }

        let mut parity_file = File::open(&self.parity_path).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("cannot open parity file: {e}"),
                false,
            )
        })?;

        let mut header = [0u8; 64];
        if parity_file.read_exact(&mut header).is_err() || &header[..8] != PARITY_MAGIC {
            return Ok(RepairSummary::default());
        }

        let total_groups = u64::from_le_bytes([
            header[24], header[25], header[26], header[27], header[28], header[29], header[30],
            header[31],
        ]) as usize;

        let total_pages = u64::from_le_bytes([
            header[32], header[33], header[34], header[35], header[36], header[37], header[38],
            header[39],
        ]) as usize;

        let mut summary = RepairSummary {
            total_pages_inspected: total_pages,
            total_groups_inspected: total_groups,
            bitflips_detected: 0,
            pages_repaired: 0,
            unrecoverable_pages: 0,
        };

        for p_id in 1..=total_pages as u32 {
            match self.verify_page(p_id) {
                Ok(true) => {}
                Ok(false) => {
                    summary.bitflips_detected += 1;
                    match self.reconstruct_and_repair_page(p_id) {
                        Ok(outcome) if outcome.checksum_matches => {
                            summary.pages_repaired += 1;
                        }
                        _ => {
                            summary.unrecoverable_pages += 1;
                        }
                    }
                }
                Err(_) => {
                    summary.unrecoverable_pages += 1;
                }
            }
        }

        Ok(summary)
    }
}
