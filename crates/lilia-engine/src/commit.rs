//! Group commit: concurrent writers share one journal write and one sync.
//!
//! A writer queues its batch. The writer at the head of the queue becomes the leader: it takes
//! every queued batch (up to a byte budget), commits them together, hands each writer its own
//! result, and passes leadership to the next queued writer. A writer alone in the queue commits
//! immediately, so grouping adds no latency.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::batch::WriteBatch;
use crate::error::Result;

/// A queued batch and, once committed, its result.
#[derive(Debug)]
pub struct Writer {
    pub batch: WriteBatch,
    result: Mutex<Option<Result<u64>>>,
    wake: Condvar,
}

impl Writer {
    /// Records the outcome for this writer.
    pub fn finish(&self, result: Result<u64>) {
        *self.result.lock() = Some(result);
    }
}

#[derive(Debug, Default)]
struct State {
    queue: VecDeque<Arc<Writer>>,
    leading: bool,
}

#[derive(Debug, Default)]
pub struct CommitQueue {
    state: Mutex<State>,
}

impl CommitQueue {
    /// Commits `batch`, possibly with others: `commit_group` is called by the leader with the
    /// whole group and must call [`Writer::finish`] on every writer of it.
    pub fn commit(&self, batch: WriteBatch, max_group_bytes: usize, commit_group: impl FnOnce(&[Arc<Writer>])) -> Result<u64> {
        let me = Arc::new(Writer { batch, result: Mutex::new(None), wake: Condvar::new() });
        let mut state = self.state.lock();
        state.queue.push_back(Arc::clone(&me));
        loop {
            if let Some(result) = me.result.lock().take() {
                return result;
            }
            let at_head = state.queue.front().is_some_and(|w| Arc::ptr_eq(w, &me));
            if !state.leading && at_head {
                break;
            }
            me.wake.wait(&mut state);
        }

        state.leading = true;
        let mut group = Vec::new();
        let mut bytes = 0;
        while let Some(next) = state.queue.front() {
            let size = next.batch.approximate_bytes();
            if !group.is_empty() && bytes + size > max_group_bytes {
                break;
            }
            bytes += size;
            group.extend(state.queue.pop_front());
        }
        drop(state);

        commit_group(&group);

        let mut state = self.state.lock();
        state.leading = false;
        for writer in &group[1..] {
            writer.wake.notify_one();
        }
        if let Some(next) = state.queue.front() {
            next.wake.notify_one();
        }
        drop(state);
        let result = me.result.lock().take();
        result.unwrap_or_else(|| Err(crate::error::corruption("commit leader left its own batch without a result")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn concurrent_writers_are_grouped_and_each_gets_its_own_result() {
        let queue = Arc::new(CommitQueue::default());
        let sequence = Arc::new(AtomicU64::new(0));
        let groups = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..16)
            .map(|t| {
                let (queue, sequence, groups) = (Arc::clone(&queue), Arc::clone(&sequence), Arc::clone(&groups));
                std::thread::spawn(move || {
                    let mut seen = Vec::new();
                    for i in 0..200 {
                        let mut batch = WriteBatch::new();
                        batch.put(format!("{t}-{i}"), "v");
                        let got = queue
                            .commit(batch, 1 << 20, |group| {
                                groups.fetch_add(1, Ordering::Relaxed);
                                std::thread::sleep(std::time::Duration::from_micros(50));
                                for writer in group {
                                    writer.finish(Ok(sequence.fetch_add(1, Ordering::Relaxed) + 1));
                                }
                            })
                            .unwrap();
                        seen.push(got);
                    }
                    seen
                })
            })
            .collect();
        let mut all: Vec<u64> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 16 * 200, "every writer got a distinct result");
        assert!(groups.load(Ordering::Relaxed) < 16 * 200, "some commits were grouped");
    }
}
