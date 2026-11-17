use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use lilia_core::cas::{
    compute_hash, decode_kv_pointer, extract_blob_hashes_from_value, is_kv_pointer,
    BlobDescriptor, BlobGcStats, CasOptions,
};
use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::{NativeConnection, NativeTransaction};

/// Content Addressable Storage (CAS) engine for large BLOBs.
#[derive(Debug, Clone)]
pub struct CasEngine {
    root_dir: PathBuf,
    options: CasOptions,
}

impl CasEngine {
    /// Create a new `CasEngine` pointing to a root directory and configured options.
    #[must_use]
    pub fn new(root_dir: impl Into<PathBuf>, options: CasOptions) -> Self {
        Self {
            root_dir: root_dir.into(),
            options,
        }
    }

    /// Access the CAS configuration options.
    #[must_use]
    pub fn options(&self) -> &CasOptions {
        &self.options
    }

    /// Get the root directory path where blobs are stored.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Ensure that the root directory and the temporary upload directory exist.
    pub fn ensure_dirs(&self) -> Result<()> {
        let tmp_dir = self.root_dir.join(".tmp");
        fs::create_dir_all(&tmp_dir)
            .map_err(|e| LiliaError::new(ErrorCode::Io, format!("failed to create CAS dir: {e}"), false))?;
        Ok(())
    }

    /// Calculate the two-level sharded file path for a content hash.
    ///
    /// For example: `<root>/ab/cd/abcdef01...blob`
    #[must_use]
    pub fn path_for_hash(&self, hash: &str) -> PathBuf {
        if hash.len() >= 4 {
            let shard1 = &hash[0..2];
            let shard2 = &hash[2..4];
            self.root_dir
                .join(shard1)
                .join(shard2)
                .join(format!("{hash}.blob"))
        } else {
            self.root_dir.join(format!("{hash}.blob"))
        }
    }

    /// Check if a blob with the given hash physically exists in sealed storage.
    #[must_use]
    pub fn exists(&self, hash: &str) -> bool {
        self.path_for_hash(hash).is_file()
    }

    /// Store a raw byte slice into sealed CAS storage.
    ///
    /// If the blob already exists (identical content hash), it is deduplicated:
    /// no extra physical file is created and the descriptor of the existing file is returned.
    pub fn store(&self, data: &[u8], mime_type: Option<String>) -> Result<BlobDescriptor> {
        let (hash_hex, _) = compute_hash(self.options.hash_algorithm, data);
        let dest_path = self.path_for_hash(&hash_hex);

        if dest_path.is_file() {
            let metadata = fs::metadata(&dest_path).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to read blob metadata: {e}"),
                    false,
                )
            })?;
            let mut desc = BlobDescriptor::new(hash_hex, metadata.len());
            desc.mime_type = mime_type;
            return Ok(desc);
        }

        self.ensure_dirs()?;
        let tmp_filename = format!("{}.tmp", uuid::Uuid::new_v4());
        let tmp_path = self.root_dir.join(".tmp").join(tmp_filename);

        {
            let mut file = File::create(&tmp_path).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to create temporary blob file: {e}"),
                    false,
                )
            })?;
            file.write_all(data).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to write blob data: {e}"),
                    false,
                )
            })?;
            file.sync_all().map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to sync blob file to disk: {e}"),
                    false,
                )
            })?;
        }

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to create shard directory: {e}"),
                    false,
                )
            })?;
        }

        if dest_path.is_file() {
            let _ = fs::remove_file(&tmp_path);
        } else {
            fs::rename(&tmp_path, &dest_path).map_err(|e| {
                let _ = fs::remove_file(&tmp_path);
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to commit blob file: {e}"),
                    false,
                )
            })?;
            let _ = set_sealed_permissions(&dest_path);
        }

        let mut desc = BlobDescriptor::new(hash_hex, data.len() as u64);
        desc.mime_type = mime_type;
        Ok(desc)
    }

    /// Store a stream from a reader into sealed CAS storage with chunked hashing.
    pub fn store_reader<R: Read>(
        &self,
        reader: &mut R,
        mime_type: Option<String>,
    ) -> Result<BlobDescriptor> {
        self.ensure_dirs()?;
        let tmp_filename = format!("{}.tmp", uuid::Uuid::new_v4());
        let tmp_path = self.root_dir.join(".tmp").join(tmp_filename);

        let mut size = 0u64;
        let hash_hex = {
            let mut file = File::create(&tmp_path).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to create temporary blob file: {e}"),
                    false,
                )
            })?;

            let mut buf = [0u8; 64 * 1024];
            let mut hasher = lilia_core::cas::CasHasher::new(self.options.hash_algorithm);
            loop {
                let n = reader.read(&mut buf).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to read input stream: {e}"),
                        false,
                    )
                })?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n]).map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Io,
                        format!("failed to write blob file: {e}"),
                        false,
                    )
                })?;
                size += n as u64;
            }
            file.sync_all().map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to sync blob file: {e}"),
                    false,
                )
            })?;
            let (hash_hex, _) = hasher.finalize();
            hash_hex
        };

        let dest_path = self.path_for_hash(&hash_hex);

        if dest_path.is_file() {
            let _ = fs::remove_file(&tmp_path);
            let metadata = fs::metadata(&dest_path).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to read blob metadata: {e}"),
                    false,
                )
            })?;
            let mut desc = BlobDescriptor::new(hash_hex, metadata.len());
            desc.mime_type = mime_type;
            return Ok(desc);
        }

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to create shard directory: {e}"),
                    false,
                )
            })?;
        }

        fs::rename(&tmp_path, &dest_path).map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to commit blob file: {e}"),
                false,
            )
        })?;
        let _ = set_sealed_permissions(&dest_path);

        let mut desc = BlobDescriptor::new(hash_hex, size);
        desc.mime_type = mime_type;
        Ok(desc)
    }

    /// Read the full contents of a blob by its content hash.
    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for_hash(hash);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&path).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to read blob '{hash}': {e}"),
                false,
            )
        })?;
        Ok(Some(bytes))
    }

    /// Open a read-only `File` handle to stream a blob directly without allocating in memory.
    pub fn open(&self, hash: &str) -> Result<Option<File>> {
        let path = self.path_for_hash(hash);
        if !path.is_file() {
            return Ok(None);
        }
        let file = File::open(&path).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to open blob '{hash}': {e}"),
                false,
            )
        })?;
        Ok(Some(file))
    }

    /// Remove a physical blob file from sealed storage (used by Garbage Collector).
    ///
    /// Returns the number of bytes deleted if the file was found.
    pub fn remove_blob_file(&self, hash: &str) -> Result<u64> {
        let path = self.path_for_hash(hash);
        if !path.is_file() {
            return Ok(0);
        }
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let _ = prepare_for_delete(&path);
        fs::remove_file(&path).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to remove blob '{hash}': {e}"),
                false,
            )
        })?;
        Ok(size)
    }
}

#[cfg(unix)]
fn set_sealed_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
}

#[cfg(windows)]
fn set_sealed_permissions(path: &Path) -> std::io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms)
}

#[cfg(not(any(unix, windows)))]
fn set_sealed_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn prepare_for_delete(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn prepare_for_delete(path: &Path) -> std::io::Result<()> {
    if let Ok(metadata) = fs::metadata(path) {
        let mut perms = metadata.permissions();
        perms.set_readonly(false);
        let _ = fs::set_permissions(path, perms);
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn prepare_for_delete(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Transactional SQLite Reference Tracking & Garbage Collector
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub fn register_blob_in_db(
    tx: &mut NativeTransaction<'_>,
    descriptor: &BlobDescriptor,
    created_at_ms: i64,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO _lilia_blobs(hash, size_bytes, ref_count, created_at_ms, sealed) \
         VALUES(?1, ?2, 0, ?3, 1) \
         ON CONFLICT(hash) DO NOTHING;",
    )?;
    stmt.bind_text_static(1, &descriptor.hash)?;
    stmt.bind_int64(2, i64::try_from(descriptor.size_bytes).unwrap_or(i64::MAX))?;
    stmt.bind_int64(3, created_at_ms)?;
    stmt.step()?;
    Ok(())
}

pub(crate) fn increment_blob_ref(
    tx: &mut NativeTransaction<'_>,
    source_kind: &str,
    source_space: &str,
    source_id: &[u8],
    blob_hash: &str,
    size_bytes: Option<u64>,
    now_ms: i64,
) -> Result<()> {
    // 1. Ensure row in _lilia_blobs exists
    let mut ensure_stmt = tx.prepare(
        "INSERT INTO _lilia_blobs(hash, size_bytes, ref_count, created_at_ms, sealed) \
         VALUES(?1, ?2, 0, ?3, 1) \
         ON CONFLICT(hash) DO NOTHING;",
    )?;
    ensure_stmt.bind_text_static(1, blob_hash)?;
    ensure_stmt.bind_int64(2, i64::try_from(size_bytes.unwrap_or(0)).unwrap_or(0))?;
    ensure_stmt.bind_int64(3, now_ms)?;
    ensure_stmt.step()?;
    drop(ensure_stmt);

    // 2. Insert into _lilia_blob_refs
    let mut ref_stmt = tx.prepare(
        "INSERT OR IGNORE INTO _lilia_blob_refs(source_kind, source_space, source_id, blob_hash) \
         VALUES(?1, ?2, ?3, ?4);",
    )?;
    ref_stmt.bind_text_static(1, source_kind)?;
    ref_stmt.bind_text_static(2, source_space)?;
    ref_stmt.bind_blob_static(3, source_id)?;
    ref_stmt.bind_text_static(4, blob_hash)?;
    ref_stmt.step()?;
    drop(ref_stmt);

    if tx.conn().changes() > 0 {
        let mut upd_stmt = tx.prepare(
            "UPDATE _lilia_blobs SET ref_count = ref_count + 1 WHERE hash = ?1;",
        )?;
        upd_stmt.bind_text_static(1, blob_hash)?;
        upd_stmt.step()?;
    }
    Ok(())
}

pub(crate) fn decrement_blob_ref(
    tx: &mut NativeTransaction<'_>,
    source_kind: &str,
    source_space: &str,
    source_id: &[u8],
    blob_hash: &str,
) -> Result<()> {
    let mut del_stmt = tx.prepare(
        "DELETE FROM _lilia_blob_refs \
         WHERE source_kind = ?1 AND source_space = ?2 AND source_id = ?3 AND blob_hash = ?4;",
    )?;
    del_stmt.bind_text_static(1, source_kind)?;
    del_stmt.bind_text_static(2, source_space)?;
    del_stmt.bind_blob_static(3, source_id)?;
    del_stmt.bind_text_static(4, blob_hash)?;
    del_stmt.step()?;
    drop(del_stmt);

    if tx.conn().changes() > 0 {
        let mut upd_stmt = tx.prepare(
            "UPDATE _lilia_blobs SET ref_count = MAX(0, ref_count - 1) WHERE hash = ?1;",
        )?;
        upd_stmt.bind_text_static(1, blob_hash)?;
        upd_stmt.step()?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn sync_json_blob_refs(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    prev_val: Option<&serde_json::Value>,
    new_val: &serde_json::Value,
    now_ms: i64,
) -> Result<()> {
    let new_hashes = extract_blob_hashes_from_value(new_val);
    let old_hashes = prev_val
        .map(extract_blob_hashes_from_value)
        .unwrap_or_default();

    for h in &old_hashes {
        if !new_hashes.contains(h) {
            decrement_blob_ref(tx, "json", space, id.as_bytes(), h)?;
        }
    }
    for h in &new_hashes {
        if !old_hashes.contains(h) {
            increment_blob_ref(tx, "json", space, id.as_bytes(), h, None, now_ms)?;
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn delete_json_blob_refs(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT blob_hash FROM _lilia_blob_refs \
         WHERE source_kind = 'json' AND source_space = ?1 AND source_id = ?2;",
    )?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_blob_static(2, id.as_bytes())?;
    let mut hashes = Vec::new();
    while stmt.step()? {
        hashes.push(stmt.column_text(0).to_string());
    }
    drop(stmt);

    for h in hashes {
        decrement_blob_ref(tx, "json", space, id.as_bytes(), &h)?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn sync_kv_blob_ref(
    tx: &mut NativeTransaction<'_>,
    namespace: &str,
    key: &[u8],
    old_raw: Option<&[u8]>,
    new_raw: &[u8],
    now_ms: i64,
) -> Result<()> {
    let old_hash = old_raw.and_then(decode_kv_pointer).map(|(_, h, _)| h);
    let new_hash = if is_kv_pointer(new_raw) {
        decode_kv_pointer(new_raw).map(|(_, h, _)| h)
    } else {
        None
    };

    if old_hash != new_hash {
        if let Some(old) = old_hash {
            decrement_blob_ref(tx, "kv", namespace, key, &old)?;
        }
        if let Some(new) = new_hash {
            increment_blob_ref(tx, "kv", namespace, key, &new, None, now_ms)?;
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn delete_kv_blob_ref(
    tx: &mut NativeTransaction<'_>,
    namespace: &str,
    key: &[u8],
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT blob_hash FROM _lilia_blob_refs \
         WHERE source_kind = 'kv' AND source_space = ?1 AND source_id = ?2;",
    )?;
    stmt.bind_text_static(1, namespace)?;
    stmt.bind_blob_static(2, key)?;
    let mut hashes = Vec::new();
    while stmt.step()? {
        hashes.push(stmt.column_text(0).to_string());
    }
    drop(stmt);

    for h in hashes {
        decrement_blob_ref(tx, "kv", namespace, key, &h)?;
    }
    Ok(())
}

/// Atomic Garbage Collector pass: purges orphaned blobs (`ref_count = 0`) from sealed storage and DB.
#[doc(hidden)]
pub fn purge_orphan_blobs(
    conn: &NativeConnection,
    cas: &CasEngine,
) -> Result<BlobGcStats> {
    let mut stmt = match conn.prepare("SELECT hash FROM _lilia_blobs WHERE ref_count = 0;") {
        Ok(s) => s,
        Err(_) => return Ok(BlobGcStats::default()),
    };

    let mut orphans = Vec::new();
    while stmt.step()? {
        orphans.push(stmt.column_text(0).to_string());
    }
    drop(stmt);

    if orphans.is_empty() {
        return Ok(BlobGcStats::default());
    }

    let mut stats = BlobGcStats::default();

    for hash in orphans {
        // Double check ref_count is still 0
        let mut check_stmt = conn.prepare("SELECT ref_count FROM _lilia_blobs WHERE hash = ?1;")?;
        check_stmt.bind_text_static(1, &hash)?;
        if check_stmt.step()? && check_stmt.column_int64(0) == 0 {
            drop(check_stmt);
            let bytes = cas.remove_blob_file(&hash).unwrap_or(0);
            let mut del_stmt = conn.prepare("DELETE FROM _lilia_blobs WHERE hash = ?1 AND ref_count = 0;")?;
            del_stmt.bind_text_static(1, &hash)?;
            del_stmt.step()?;
            stats.orphans_purged += 1;
            stats.bytes_reclaimed += bytes;
        }
    }

    Ok(stats)
}
