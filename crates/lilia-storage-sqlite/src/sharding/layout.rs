use std::path::{Path, PathBuf};

use lilia_core::sharding::ShardingConfig;

/// Layout manager determining disk paths for coordinator, shards, and archives.
#[derive(Debug, Clone)]
pub struct ShardLayout {
    pub coordinator_path: PathBuf,
    pub shards_dir: PathBuf,
    pub is_bundle_dir: bool,
}

impl ShardLayout {
    /// Resolves the filesystem layout for a given base path and sharding configuration.
    #[must_use]
    pub fn resolve(base_path: &Path, config: &ShardingConfig) -> Self {
        let is_dir = base_path.is_dir()
            || base_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("liliadb") || ext.eq_ignore_ascii_case("bundle")
                });

        if let Some(ref explicit_dir) = config.shards_dir {
            Self {
                coordinator_path: if is_dir {
                    base_path.join("coordinator.lilia")
                } else {
                    base_path.to_path_buf()
                },
                shards_dir: explicit_dir.clone(),
                is_bundle_dir: is_dir,
            }
        } else if is_dir {
            Self {
                coordinator_path: base_path.join("coordinator.lilia"),
                shards_dir: base_path.join("shards"),
                is_bundle_dir: true,
            }
        } else {
            // Sibling directory: e.g. "path/to/app.lilia" -> "path/to/app.lilia.shards"
            let mut shards_dir = base_path.to_path_buf();
            let file_name = base_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            shards_dir.set_file_name(format!("{file_name}.shards"));
            Self {
                coordinator_path: base_path.to_path_buf(),
                shards_dir,
                is_bundle_dir: false,
            }
        }
    }

    /// Generates a valid, safe SQLite database alias from a shard ID.
    /// E.g. `2026-10` -> `shard_2026_10`.
    #[must_use]
    pub fn shard_alias(shard_id: &str) -> String {
        let sanitized = shard_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        format!("shard_{sanitized}")
    }

    /// Standard filename for an uncompressed shard database.
    #[must_use]
    pub fn shard_file_name(shard_id: &str) -> String {
        format!("{shard_id}.lilia")
    }

    /// Standard filename for a compressed cold tier Zstandard archive.
    #[must_use]
    pub fn archive_file_name(shard_id: &str) -> String {
        format!("{shard_id}.lilia.zst")
    }

    /// Full path to a shard database file on disk.
    #[must_use]
    pub fn shard_db_path(&self, shard_id: &str) -> PathBuf {
        self.shards_dir.join(Self::shard_file_name(shard_id))
    }

    /// Full path to an archive file on disk.
    #[must_use]
    pub fn archive_path(&self, shard_id: &str) -> PathBuf {
        self.shards_dir.join(Self::archive_file_name(shard_id))
    }

    /// Ensures the shards directory exists on disk.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        if self.is_bundle_dir {
            if let Some(parent) = self.coordinator_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::create_dir_all(&self.shards_dir)?;
        Ok(())
    }
}
