//! A database whose files were damaged (bytes overwritten, a file truncated or replaced) is opened
//! and read in full: each step must succeed or fail with an error, never panic, read out of bounds,
//! hang or allocate without bound. The checksums are there to turn such damage into errors.
//!
//! The input picks a file of a small database (its `CURRENT`, manifest, journal and sorted runs),
//! how to damage it, where, and the bytes written.

#![no_main]

use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use lilia_engine::{Db, Durability, Options};
use lilia_io::{HardwareProfile, MemVfs, OpenMode, SyncLevel, Vfs};

const DIR: &str = "/db";
const KEYS: u32 = 600;

/// Tiny buffers, so a few hundred writes leave several sorted runs over two levels.
fn options() -> Options {
    Options {
        durability: Durability::Durable,
        memtable_bytes: 8 << 10,
        max_immutable_memtables: 2,
        block_bytes: 512,
        bloom_bits_per_key: 10,
        block_cache_bytes: 64 << 10,
        l0_compaction_trigger: 2,
        l0_stop_writes: 8,
        level1_bytes: 24 << 10,
        level_multiplier: 3,
        target_file_bytes: 8 << 10,
        max_group_bytes: 16 << 10,
        compaction_threads: 1,
        ..Options::for_hardware(&HardwareProfile::minimal())
    }
}

fn key(i: u32) -> Vec<u8> {
    format!("key{i:05}").into_bytes()
}

/// The files of a database written by this build: runs, deletions, and writes still in the
/// journal.
fn image() -> &'static [(PathBuf, Vec<u8>)] {
    static IMAGE: OnceLock<Vec<(PathBuf, Vec<u8>)>> = OnceLock::new();
    IMAGE.get_or_init(|| {
        let vfs = Arc::new(MemVfs::new(0));
        let db = Db::open_with(vfs.clone(), DIR, options()).expect("a new database opens");
        for i in 0..KEYS {
            db.put(
                key(i),
                format!("value {i} {}", "x".repeat((i % 97) as usize)),
            )
            .expect("a write commits");
            if i % 7 == 0 && i > 0 {
                db.delete(key(i - 1)).expect("a deletion commits");
            }
        }
        db.flush().expect("the memtable flushes");
        for i in 0..40 {
            db.put(key(KEYS + i), b"in the journal only".to_vec())
                .expect("a write commits");
        }
        db.close().expect("the database closes");

        let mut names = vfs.list_dir(Path::new(DIR)).expect("the directory lists");
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let path = Path::new(DIR).join(name);
                let file = vfs.open(&path, OpenMode::Read).expect("a file opens");
                let mut bytes = vec![0; file.len().expect("a length") as usize];
                file.read_exact_at(&mut bytes, 0).expect("a file reads");
                (path, bytes)
            })
            .collect()
    })
}

/// `image()` on a new file system, with its file `data[0]` damaged as `data[1]` says.
fn damaged(data: &[u8]) -> Option<Arc<MemVfs>> {
    let (&[pick, mode], rest) = data.split_first_chunk::<2>()?;
    let (offset, bytes) = match rest.split_first_chunk::<4>() {
        Some((offset, bytes)) => (u32::from_le_bytes(*offset) as usize, bytes),
        None => (0, rest),
    };
    let files = image();
    let target = usize::from(pick) % files.len();

    let vfs = Arc::new(MemVfs::new(0));
    vfs.create_dir_all(Path::new(DIR)).ok()?;
    for (index, (path, original)) in files.iter().enumerate() {
        let mut contents = original.clone();
        if index == target {
            let at = offset % (contents.len() + 1);
            match mode % 3 {
                // Bytes overwritten in place (a flipped sector).
                0 => {
                    let end = (at + bytes.len()).min(contents.len());
                    contents[at..end].copy_from_slice(&bytes[..end - at]);
                }
                // A torn tail.
                1 => contents.truncate(at),
                // Another file in its place.
                _ => contents = bytes.to_vec(),
            }
        }
        let file = vfs.open(path, OpenMode::CreateNew).ok()?;
        file.write_all_at(&contents, 0).ok()?;
        file.sync(SyncLevel::Full).ok()?;
    }
    vfs.sync_dir(Path::new(DIR)).ok()?;
    Some(vfs)
}

fuzz_target!(|data: &[u8]| {
    let Some(vfs) = damaged(data) else {
        return;
    };
    let Ok(db) = Db::open_with(vfs, DIR, options()) else {
        return;
    };
    if let Ok(entries) = db.range(Bound::Unbounded, Bound::Unbounded) {
        for entry in entries {
            if entry.is_err() {
                break;
            }
        }
    }
    for i in (0..KEYS + 40).step_by(13) {
        let _ = db.get(&key(i));
    }
    let _ = db.close();
});
