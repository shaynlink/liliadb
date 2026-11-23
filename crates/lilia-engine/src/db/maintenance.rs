//! Background work: one thread flushes memtables, `compaction_threads` threads compact runs.
//! All of them run at background priority ([`lilia_io::qos`]).

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use lilia_io::qos;

use super::{remove_unreferenced, Inner, SuperVersion};
use crate::compaction::{self, Compaction, Retention, RunSink};
use crate::error::Result;
use crate::files;
use crate::iter::{InternalIterator, MergingIterator};
use crate::memtable::Memtable;
use crate::options::{Options, LEVELS};
use crate::version::edit::{NewFile, VersionEdit};
use crate::version::Version;

pub fn start(inner: &Arc<Inner>) -> Result<Vec<JoinHandle<()>>> {
    if !inner.options.background_threads {
        return Ok(Vec::new());
    }
    let mut threads = Vec::with_capacity(1 + inner.options.compaction_threads);
    let flusher = Arc::clone(inner);
    threads.push(
        thread::Builder::new()
            .name("lilia-flush".into())
            .spawn(move || flush_loop(&flusher))?,
    );
    for i in 0..inner.options.compaction_threads {
        let compactor = Arc::clone(inner);
        threads.push(
            thread::Builder::new()
                .name(format!("lilia-compact-{i}"))
                .spawn(move || compaction_loop(&compactor))?,
        );
    }
    Ok(threads)
}

/// Without background threads: flushes every sealed memtable, oldest first, then compacts while a
/// level is due. A failure disables writes, as it would on a background thread.
pub fn run_inline(inner: &Inner) -> Result<()> {
    let result = flush_and_compact(inner);
    if let Err(error) = &result {
        inner.fail(error);
    }
    result
}

fn flush_and_compact(inner: &Inner) -> Result<()> {
    while let Some(oldest) = inner.super_version().imm.last().cloned() {
        flush(inner, &oldest)?;
    }
    loop {
        let (compaction, version) = {
            let versions = inner.versions.lock();
            let version = Arc::clone(&versions.current);
            match compaction::pick(&version, &inner.options) {
                Some(compaction) => (compaction, version),
                None => return Ok(()),
            }
        };
        let result = run(inner, &compaction, &version);
        compaction.release();
        result?;
    }
}

/// Whether some level is over its compaction trigger or size budget.
pub fn compaction_due(version: &Version, options: &Options) -> bool {
    version.levels[0].len() >= options.l0_compaction_trigger
        || (1..LEVELS - 1).any(|level| version.level_bytes(level) > options.level_bytes(level))
}

fn flush_loop(inner: &Inner) {
    qos::set_current_thread_background();
    loop {
        let oldest = {
            let mut work = inner.work.lock();
            loop {
                if work.shutdown || work.error.is_some() {
                    return;
                }
                if let Some(oldest) = inner.super_version().imm.last().cloned() {
                    break oldest;
                }
                inner.work_ready.wait(&mut work);
            }
        };
        if let Err(error) = flush(inner, &oldest) {
            inner.fail(&error);
            return;
        }
        inner.signal_progress();
        inner.signal_work();
    }
}

/// Writes the oldest immutable memtable to level 0, then forgets it and its journal.
fn flush(inner: &Inner, memtable: &Arc<Memtable>) -> Result<()> {
    let mut iter = memtable.iter();
    iter.seek_to_first()?;
    let retention = Retention {
        smallest_snapshot: inner.smallest_snapshot(),
        tombstones: None,
    };
    let added = write_runs(inner, 0, &mut iter, &retention)?;
    // Journals up to this memtable's are obsolete once the run is recorded.
    let log_number = memtable.journal + 1;
    let edit = VersionEdit {
        log_number: Some(log_number),
        last_sequence: Some(inner.visible.load(std::sync::atomic::Ordering::Acquire)),
        added,
        ..VersionEdit::default()
    };
    install(inner, edit, Some(memtable))?;
    remove_unreferenced(
        inner.vfs.as_ref(),
        &files::journal(&inner.dir, memtable.journal),
    );
    Ok(())
}

fn write_runs(
    inner: &Inner,
    level: usize,
    iter: &mut dyn InternalIterator,
    retention: &Retention<'_>,
) -> Result<Vec<NewFile>> {
    let mut allocate = || inner.versions.lock().new_file_number();
    let mut sink = RunSink {
        vfs: inner.vfs.as_ref(),
        dir: &inner.dir,
        options: &inner.options,
        sync: inner.options.durability.metadata_sync(),
        level,
        allocate: &mut allocate,
    };
    let added = sink.write(iter, retention)?;
    if !added.is_empty() {
        // The new runs' directory entries must be durable before the manifest names them.
        inner.vfs.sync_dir(&inner.dir)?;
    }
    Ok(added)
}

/// Records `edit` in the manifest and publishes the resulting version (without `flushed`, the
/// memtable whose contents the edit added).
fn install(inner: &Inner, edit: VersionEdit, flushed: Option<&Arc<Memtable>>) -> Result<()> {
    let mut versions = inner.versions.lock();
    let version = versions.log_and_apply(edit)?;
    let mut current = inner.current.write();
    let imm = current
        .imm
        .iter()
        .filter(|m| !flushed.is_some_and(|f| Arc::ptr_eq(m, f)))
        .cloned()
        .collect();
    *current = Arc::new(SuperVersion {
        mem: Arc::clone(&current.mem),
        imm,
        version,
    });
    Ok(())
}

fn compaction_loop(inner: &Inner) {
    qos::set_current_thread_background();
    loop {
        let (compaction, version) = {
            let mut work = inner.work.lock();
            loop {
                if work.shutdown || work.error.is_some() {
                    return;
                }
                // Picked under the version-set lock, from its current version: a compaction
                // installs under this lock and releases its runs afterwards, so a pick sees either
                // its runs still taken or already gone, never released but still listed.
                let versions = inner.versions.lock();
                let version = Arc::clone(&versions.current);
                let picked = compaction::pick(&version, &inner.options);
                drop(versions);
                if let Some(compaction) = picked {
                    break (compaction, version);
                }
                inner.work_ready.wait(&mut work);
            }
        };
        let result = run(inner, &compaction, &version);
        compaction.release();
        if let Err(error) = result {
            inner.fail(&error);
            return;
        }
        inner.signal_progress();
        inner.signal_work();
    }
}

fn run(inner: &Inner, compaction: &Compaction, version: &Version) -> Result<()> {
    let target = compaction.level + 1;
    let mut edit = VersionEdit::default();
    for (offset, inputs) in compaction.inputs.iter().enumerate() {
        edit.deleted
            .extend(inputs.iter().map(|f| (compaction.level + offset, f.number)));
    }
    if compaction.is_trivial_move() {
        let file = &compaction.inputs[0][0];
        edit.added.push(NewFile {
            level: target,
            number: file.number,
            size: file.size,
            smallest: file.smallest.clone(),
            largest: file.largest.clone(),
        });
        return install(inner, edit, None);
    }
    let mut children: Vec<Box<dyn InternalIterator>> = Vec::new();
    for file in compaction.all_inputs() {
        children.push(Box::new(file.table()?.iter()));
    }
    let mut merged = MergingIterator::new(children);
    merged.seek_to_first()?;
    let retention = Retention {
        smallest_snapshot: inner.smallest_snapshot(),
        tombstones: Some((version, target)),
    };
    edit.added = write_runs(inner, target, &mut merged, &retention)?;
    install(inner, edit, None)
}
