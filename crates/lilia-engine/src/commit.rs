//! Group commit: concurrent writers share one journal write and one sync.
//!
//! A writer queues its batch. The writer at the head of the queue becomes the leader: it takes
//! every queued batch (up to a byte budget), commits them together, hands each writer its own
//! result, and passes leadership to the next queued writer. A writer alone in the queue commits
//! immediately, so grouping adds no latency.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

/// A queued item and, once its group is committed, its result.
#[derive(Debug)]
pub struct Member<T, R> {
    pub item: T,
    weight: usize,
    result: Mutex<Option<R>>,
    wake: Condvar,
}

impl<T, R> Member<T, R> {
    /// Records the outcome for this member.
    pub fn finish(&self, result: R) {
        *self.result.lock() = Some(result);
    }
}

#[derive(Debug)]
struct State<T, R> {
    queue: VecDeque<Arc<Member<T, R>>>,
    leading: bool,
}

/// Leader/follower group commit of items of type `T` with results of type `R`. The engine
/// commits write batches through one; a layer above that must serialize its own commits (the
/// ledger of `lilia-db`) groups them through another, so its writers still share syncs.
///
/// Followers wait on their own condition variable and leadership passes to the head of the queue
/// in arrival order, so a writer that just committed cannot take leadership back from writers
/// already waiting, which would shrink every group to one.
#[derive(Debug)]
pub struct GroupCommit<T, R> {
    state: Mutex<State<T, R>>,
}

impl<T, R> Default for GroupCommit<T, R> {
    fn default() -> Self {
        Self { state: Mutex::new(State { queue: VecDeque::new(), leading: false }) }
    }
}

impl<T, R> GroupCommit<T, R> {
    /// Commits `item`, possibly with others: the leader calls `commit_group` with a group of at
    /// most `max_group_weight` total weight (or one item), and it must call [`Member::finish`]
    /// on every member. Returns this item's result; `None` if the leader left it without one.
    pub fn commit(&self, item: T, weight: usize, max_group_weight: usize, commit_group: impl FnOnce(&[Arc<Member<T, R>>])) -> Option<R> {
        let me = Arc::new(Member { item, weight, result: Mutex::new(None), wake: Condvar::new() });
        let mut state = self.state.lock();
        state.queue.push_back(Arc::clone(&me));
        loop {
            if let Some(result) = me.result.lock().take() {
                return Some(result);
            }
            let at_head = state.queue.front().is_some_and(|w| Arc::ptr_eq(w, &me));
            if !state.leading && at_head {
                break;
            }
            me.wake.wait(&mut state);
        }

        state.leading = true;
        let mut group = Vec::new();
        let mut total = 0;
        while let Some(next) = state.queue.front() {
            if !group.is_empty() && total + next.weight > max_group_weight {
                break;
            }
            total += next.weight;
            group.extend(state.queue.pop_front());
        }
        drop(state);

        commit_group(&group);

        let mut state = self.state.lock();
        state.leading = false;
        for member in &group[1..] {
            member.wake.notify_one();
        }
        if let Some(next) = state.queue.front() {
            next.wake.notify_one();
        }
        drop(state);
        let result = me.result.lock().take();
        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use crate::batch::WriteBatch;
    use crate::error::Result;

    #[test]
    fn concurrent_writers_are_grouped_and_each_gets_its_own_result() {
        let queue: Arc<GroupCommit<WriteBatch, Result<u64>>> = Arc::new(GroupCommit::default());
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
                            .commit(batch, 1, 1 << 20, |group| {
                                groups.fetch_add(1, Ordering::Relaxed);
                                std::thread::sleep(std::time::Duration::from_micros(50));
                                for writer in group {
                                    writer.finish(Ok(sequence.fetch_add(1, Ordering::Relaxed) + 1));
                                }
                            })
                            .unwrap()
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
