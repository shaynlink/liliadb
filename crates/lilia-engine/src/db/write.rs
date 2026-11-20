//! The write path: group commit into the journal, then into the memtable.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use bytes::Bytes;
use lilia_io::OpenMode;

use super::{Inner, SuperVersion};
use crate::batch::{self, Op, WriteBatch};
use crate::commit::Member;

/// A batch waiting in the commit queue.
type Writer = Member<WriteBatch, Result<u64>>;
use crate::error::{Error, Result};
use crate::files;
use crate::journal::JournalWriter;
use crate::key::{Kind, MAX_SEQUENCE};
use crate::memtable::Memtable;

impl Inner {
    pub fn write(&self, batch: WriteBatch) -> Result<u64> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let weight = batch.approximate_bytes();
        self.queue
            .commit(batch, weight, self.options.max_group_bytes, |group| self.commit_group(group))
            .unwrap_or_else(|| Err(crate::error::corruption("commit leader left a batch without a result")))
    }

    /// Newest version of `key` among committed writes; `None` if absent or deleted.
    fn latest_version(current: &SuperVersion, key: &[u8]) -> Result<Option<u64>> {
        Ok(current.lookup(key, MAX_SEQUENCE)?.filter(|f| f.kind == Kind::Put).map(|f| f.sequence))
    }

    /// Run by the commit leader: validates, journals and applies a group of batches.
    fn commit_group(&self, group: &[Arc<Writer>]) {
        if let Err(error) = self.make_room() {
            for writer in group {
                writer.finish(Err(error.clone()));
            }
            return;
        }
        let current = self.super_version();
        let mut next = self.visible.load(Ordering::Acquire) + 1;
        // Versions written by earlier batches of this group, seen by later expectations.
        let mut written: HashMap<Bytes, Option<u64>> = HashMap::new();
        let mut accepted: Vec<(&Arc<Writer>, u64)> = Vec::new();
        let mut records: Vec<Vec<u8>> = Vec::new();
        for writer in group {
            match Self::admit(&current, &writer.item, &written, next) {
                Err(error) => writer.finish(Err(error)),
                Ok(()) if writer.item.is_empty() => writer.finish(Ok(next - 1)),
                Ok(()) => {
                    let mut record = Vec::with_capacity(writer.item.approximate_bytes() + 16);
                    batch::encode(&mut record, next, writer.item.ops());
                    records.push(record);
                    for (i, op) in writer.item.ops().iter().enumerate() {
                        let version = matches!(op, Op::Put { .. }).then_some(next + i as u64);
                        written.insert(op.key().clone(), version);
                    }
                    accepted.push((writer, next));
                    next += writer.item.len() as u64;
                }
            }
        }
        if accepted.is_empty() {
            return;
        }

        let journaled = {
            let mut journal = self.journal.lock();
            journal.append(records.iter().map(Vec::as_slice)).and_then(|()| match self.options.durability.commit_sync() {
                Some(level) => journal.sync(level),
                None => Ok(()),
            })
        };
        if let Err(error) = journaled {
            // The journal may hold any prefix of this group: nothing can be acknowledged, and no
            // later write may be journaled after an unknown state.
            let error = Error::from(error);
            self.fail(&error);
            for (writer, _) in accepted {
                writer.finish(Err(error.clone()));
            }
            return;
        }
        for &(writer, first) in &accepted {
            for (i, op) in writer.item.ops().iter().enumerate() {
                current.mem.insert(first + i as u64, op);
            }
        }
        self.visible.store(next - 1, Ordering::Release);
        for (writer, first) in accepted {
            writer.finish(Ok(first));
        }
    }

    /// Checks one batch of a group: sequence space and expectations.
    fn admit(current: &SuperVersion, batch: &WriteBatch, written: &HashMap<Bytes, Option<u64>>, next: u64) -> Result<()> {
        if next + batch.len() as u64 > MAX_SEQUENCE {
            return Err(Error::InvalidArgument("sequence numbers exhausted".into()));
        }
        if let Some(expected) = batch.position.filter(|&p| p != next) {
            return Err(Error::SequenceMoved { expected, actual: next });
        }
        for expectation in &batch.expectations {
            let actual = match written.get(&expectation.key) {
                Some(&version) => version,
                None => Self::latest_version(current, &expectation.key)?,
            };
            if actual != expectation.version {
                return Err(Error::Conflict { key: expectation.key.to_vec(), expected: expectation.version, actual });
            }
        }
        Ok(())
    }

    /// Makes sure the active memtable has room, switching to a new one (and a new journal) when
    /// it is full or a flush was requested, and stalling while flushes or level-0 compactions
    /// are too far behind. Only the commit leader calls this.
    fn make_room(&self) -> Result<()> {
        loop {
            if let Some(error) = self.failure() {
                return Err(error);
            }
            let current = self.super_version();
            let requested = self.switch_requested.load(Ordering::Acquire);
            if current.mem.approximate_bytes() < self.options.memtable_bytes && !requested {
                return Ok(());
            }
            if requested && current.mem.is_empty() {
                self.switch_requested.store(false, Ordering::Release);
                self.signal_progress();
                return Ok(());
            }
            let behind = current.imm.len() >= self.options.max_immutable_memtables
                || current.version.levels[0].len() >= self.options.l0_stop_writes;
            if behind {
                let mut work = self.work.lock();
                let current = self.super_version();
                if work.error.is_none()
                    && (current.imm.len() >= self.options.max_immutable_memtables
                        || current.version.levels[0].len() >= self.options.l0_stop_writes)
                {
                    self.work_ready.notify_all();
                    self.progress.wait(&mut work);
                }
                continue;
            }
            if let Err(error) = self.switch_memtable() {
                // The old journal's final sync may have failed: its state is unknown.
                self.fail(&error);
                return Err(error);
            }
            self.switch_requested.store(false, Ordering::Release);
        }
    }

    /// Seals the active memtable for flushing and starts a new one with its own journal.
    fn switch_memtable(&self) -> Result<()> {
        let number = self.versions.lock().new_file_number();
        let file = self.vfs.open(&files::journal(&self.dir, number), OpenMode::CreateNew)?;
        file.preallocate(self.options.memtable_bytes as u64 + (1 << 20))?;
        // The new journal's directory entry, and the old journal's last writes, must be durable
        // before any write lands in the new journal: replay reads journals in order.
        self.vfs.sync_dir(&self.dir)?;
        {
            let mut journal = self.journal.lock();
            journal.sync(self.options.durability.metadata_sync())?;
            *journal = JournalWriter::new(file);
        }
        let mut current = self.current.write();
        let mut imm = Vec::with_capacity(current.imm.len() + 1);
        imm.push(Arc::clone(&current.mem));
        imm.extend(current.imm.iter().cloned());
        *current = Arc::new(SuperVersion { mem: Arc::new(Memtable::new(number, self.options.memtable_bytes)), imm, version: Arc::clone(&current.version) });
        drop(current);
        self.signal_work();
        Ok(())
    }
}
