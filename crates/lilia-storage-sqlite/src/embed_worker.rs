#[cfg(not(target_arch = "wasm32"))]
use std::collections::{BTreeSet, HashMap};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{mpsc, Arc, RwLock, Weak};
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use lilia_core::{
    AutoEmbedConfig, BatchOperation, DatabaseEvent, EmbedOptions, ErrorCode, HnswIndex, LiliaError,
    Result, ValueCodec, VectorElementType,
};
#[cfg(not(target_arch = "wasm32"))]
use lilia_sqlite_native::{NativeConnection, NativeTransaction, StmtSlot};

#[cfg(not(target_arch = "wasm32"))]
use crate::embed::{vector_bytes, Embedders};
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::{JsonOptions, Resources};
#[cfg(not(target_arch = "wasm32"))]
use crate::integrity::IntegrityRegistry;
#[cfg(not(target_arch = "wasm32"))]
use crate::lifecycle::Lifecycle;
#[cfg(not(target_arch = "wasm32"))]
use crate::storage::now_ms;

/// Pause after a pass that failed for a reason other than the database closing (busy writer,
/// transient I/O error).
#[cfg(not(target_arch = "wasm32"))]
const ERROR_PAUSE: Duration = Duration::from_secs(1);

/// Largest exponent applied to the retry backoff.
#[cfg(not(target_arch = "wasm32"))]
const MAX_BACKOFF_SHIFT: i64 = 16;

#[cfg(not(target_arch = "wasm32"))]
type HnswCache = RwLock<HashMap<String, Arc<RwLock<HnswIndex>>>>;

/// Everything the embedding thread needs, held without keeping the database alive.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct EmbedCore {
    pub(crate) resources: Weak<Resources>,
    pub(crate) lifecycle: Weak<Lifecycle>,
    pub(crate) integrity: Arc<IntegrityRegistry>,
    pub(crate) embedders: Arc<Embedders>,
    pub(crate) options: EmbedOptions,
    pub(crate) hnsw_cache: Arc<HnswCache>,
    pub(crate) codec: Arc<dyn ValueCodec>,
    pub(crate) dict_registry: Arc<lilia_core::DictionaryRegistry>,
    pub(crate) json_options: JsonOptions,
    pub(crate) hlc: Arc<lilia_core::HybridClock>,
    pub(crate) cas: Arc<crate::cas::CasEngine>,
    pub(crate) events: Arc<tokio::sync::broadcast::Sender<DatabaseEvent>>,
}

/// What one pass over the queue achieved.
#[cfg(not(target_arch = "wasm32"))]
enum Pass {
    /// Documents were handled; more may be waiting.
    Worked,
    /// Nothing is due. `retry_in` is the delay before the earliest scheduled retry, if any.
    Idle { retry_in: Option<Duration> },
}

/// A queued document together with the text it held when the pass started.
#[cfg(not(target_arch = "wasm32"))]
struct Job {
    space: String,
    id: String,
    /// `None` when the collection is no longer auto-embedded: the queue entry is just dropped.
    config: Option<Arc<AutoEmbedConfig>>,
    text: Option<String>,
}

/// Result of the inference step for one [`Job`].
#[cfg(not(target_arch = "wasm32"))]
enum Outcome {
    Vector(Vec<u8>),
    /// The document has no text to embed (deleted, field missing or blank): its vector goes.
    Remove,
    /// The collection stopped being auto-embedded while the document was queued.
    Forget,
    Failed(String),
}

#[cfg(not(target_arch = "wasm32"))]
fn closed() -> LiliaError {
    LiliaError::new(ErrorCode::Closed, "database resources dropped", false)
}

#[cfg(not(target_arch = "wasm32"))]
fn source_text(
    conn: &mut NativeConnection,
    core: &EmbedCore,
    config: &AutoEmbedConfig,
    space: &str,
    id: &str,
) -> Result<Option<String>> {
    let stmt = conn.slot(StmtSlot::JsonGet)?;
    stmt.reset()?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    let text = if stmt.step()? {
        let format = stmt.column_int64(1);
        let dict_id = stmt.column_int64(2);
        let doc = crate::json::decode_stored_doc(
            stmt,
            0,
            format,
            dict_id,
            space,
            core.codec.as_ref(),
            &core.dict_registry,
            None,
        )?;
        config.source_text(&doc).map(str::to_owned)
    } else {
        None
    };
    stmt.reset()?;
    Ok(text)
}

#[cfg(not(target_arch = "wasm32"))]
fn dequeue(tx: &mut NativeTransaction<'_>, job: &Job) -> Result<()> {
    let stmt = tx.slot(StmtSlot::EmbedDequeue)?;
    stmt.reset()?;
    stmt.bind_text_static(1, &job.space)?;
    stmt.bind_text_static(2, &job.id)?;
    stmt.step()?;
    stmt.reset()
}

#[cfg(not(target_arch = "wasm32"))]
impl EmbedCore {
    fn upgrade(&self) -> Result<(Arc<Resources>, Arc<Lifecycle>)> {
        match (self.resources.upgrade(), self.lifecycle.upgrade()) {
            (Some(resources), Some(lifecycle)) => Ok((resources, lifecycle)),
            _ => Err(closed()),
        }
    }

    /// Runs one pass: claims due documents, embeds them without holding any lock, then stores
    /// the vectors in a single write transaction.
    fn run_pass(&self) -> Result<Pass> {
        let jobs = match self.claim()? {
            Ok(jobs) => jobs,
            Err(idle) => return Ok(idle),
        };
        let outcomes = self.infer(&jobs);
        self.store(&jobs, outcomes)?;
        Ok(Pass::Worked)
    }

    /// Reads the next due queue entries and the text of their documents.
    fn claim(&self) -> Result<std::result::Result<Vec<Job>, Pass>> {
        let (resources, lifecycle) = self.upgrade()?;
        let mut conn = resources.read_lock(&lifecycle)?;
        let now = now_ms();
        let max_attempts = i64::from(self.options.max_attempts);

        let mut due = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT space, id FROM _lilia_embed_queue \
                 WHERE attempts < ?1 AND next_attempt_ms <= ?2 \
                 ORDER BY next_attempt_ms, space, id LIMIT ?3;",
            )?;
            stmt.bind_int64(1, max_attempts)?;
            stmt.bind_int64(2, now)?;
            stmt.bind_int64(
                3,
                i64::try_from(self.options.batch_size.max(1)).unwrap_or(i64::MAX),
            )?;
            while stmt.step()? {
                due.push((
                    stmt.column_text(0).to_owned(),
                    stmt.column_text(1).to_owned(),
                ));
            }
        }

        if due.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT MIN(next_attempt_ms) FROM _lilia_embed_queue WHERE attempts < ?1;",
            )?;
            stmt.bind_int64(1, max_attempts)?;
            let retry_in = if stmt.step()? && !stmt.column_is_null(0) {
                let wait = u64::try_from(stmt.column_int64(0) - now).unwrap_or(0);
                Some(Duration::from_millis(wait.max(1)))
            } else {
                None
            };
            return Ok(Err(Pass::Idle { retry_in }));
        }

        let mut jobs = Vec::with_capacity(due.len());
        for (space, id) in due {
            let config = self.integrity.auto_embed_for(&space);
            let text = match &config {
                Some(config) => source_text(&mut conn, self, config, &space, &id)?,
                None => None,
            };
            jobs.push(Job {
                space,
                id,
                config,
                text,
            });
        }
        Ok(Ok(jobs))
    }

    /// Embeds the claimed texts. A failing batch is retried text by text so one bad document
    /// cannot fail its neighbours.
    fn infer(&self, jobs: &[Job]) -> Vec<Outcome> {
        let mut outcomes: Vec<Outcome> = jobs
            .iter()
            .map(|job| match (&job.config, &job.text) {
                (None, _) => Outcome::Forget,
                (Some(_), None) => Outcome::Remove,
                (Some(_), Some(_)) => Outcome::Failed(String::new()),
            })
            .collect();

        let mut by_model: HashMap<lilia_core::BuiltinModel, Vec<usize>> = HashMap::new();
        for (index, job) in jobs.iter().enumerate() {
            if let (Some(config), Some(_)) = (&job.config, &job.text) {
                by_model.entry(config.model).or_default().push(index);
            }
        }

        for (model, indices) in by_model {
            let texts: Vec<&str> = indices
                .iter()
                .filter_map(|&index| jobs[index].text.as_deref())
                .collect();
            match self.embedders.embed(model, &texts) {
                Ok(vectors) => {
                    for (&index, vector) in indices.iter().zip(&vectors) {
                        outcomes[index] = Outcome::Vector(vector_bytes(vector));
                    }
                }
                Err(_) => {
                    for (&index, text) in indices.iter().zip(&texts) {
                        outcomes[index] = match self.embedders.embed(model, &[text]) {
                            Ok(vectors) => Outcome::Vector(vector_bytes(&vectors[0])),
                            Err(error) => Outcome::Failed(error.message),
                        };
                    }
                }
            }
        }
        outcomes
    }

    /// Writes the outcomes. Each one is applied only if the document still holds the text that
    /// was embedded; otherwise its queue entry stays and the next pass handles the new text.
    fn store(&self, jobs: &[Job], outcomes: Vec<Outcome>) -> Result<()> {
        let (resources, lifecycle) = self.upgrade()?;
        let mut writer = resources.lock_writer(&lifecycle, None)?;
        let mut events = Vec::new();
        let mut touched = BTreeSet::new();

        let committed = (|| {
            let mut tx = writer.transaction()?;
            for (job, outcome) in jobs.iter().zip(outcomes) {
                self.store_one(&mut tx, job, outcome, &mut events, &mut touched)?;
            }
            tx.commit()
        })();
        drop(writer);

        if let Err(error) = committed {
            // The in-memory graphs may hold nodes of the rolled-back transaction: drop them so
            // the next access rebuilds them from the vector table.
            let mut cache = self
                .hnsw_cache
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for collection in &touched {
                cache.remove(collection);
            }
            return Err(error);
        }

        if self.events.receiver_count() > 0 {
            for event in events {
                let _ = self.events.send(event);
            }
        }
        resources.notify_checkpointer();
        Ok(())
    }

    fn store_one(
        &self,
        tx: &mut NativeTransaction<'_>,
        job: &Job,
        outcome: Outcome,
        events: &mut Vec<DatabaseEvent>,
        touched: &mut BTreeSet<String>,
    ) -> Result<()> {
        let current = self.integrity.auto_embed_for(&job.space);
        let (Some(config), Some(current)) = (&job.config, &current) else {
            // Dropped from the registry before or during the pass: nothing left to maintain.
            return dequeue(tx, job);
        };
        if config != current
            || source_text(tx.conn_mut(), self, current, &job.space, &job.id)? != job.text
        {
            return Ok(());
        }

        let operation = match outcome {
            Outcome::Forget => return dequeue(tx, job),
            Outcome::Failed(error) => return self.record_failure(tx, job, &error),
            Outcome::Vector(vector) => BatchOperation::VectorSet {
                collection: job.space.clone(),
                id: job.id.clone(),
                dim: config.model.dimension(),
                element_type: VectorElementType::Float32,
                vector,
            },
            Outcome::Remove => BatchOperation::VectorDelete {
                collection: job.space.clone(),
                id: job.id.clone(),
            },
        };
        touched.insert(job.space.clone());
        let result = crate::transaction::apply(
            tx,
            &operation,
            self.codec.as_ref(),
            &self.dict_registry,
            &self.json_options,
            self.hlc.as_ref(),
            &self.cas,
            None,
            None,
        )?;
        crate::vector::sync_hnsw_for_op_with_cache(
            tx,
            &self.hnsw_cache,
            self.codec.as_ref(),
            &operation,
        )?;
        let deleted = matches!(operation, BatchOperation::VectorDelete { .. });
        if !deleted || result.deleted {
            events.push(DatabaseEvent::VectorMutated {
                collection: job.space.clone(),
                id: job.id.clone(),
                deleted,
            });
        }
        dequeue(tx, job)
    }

    fn record_failure(&self, tx: &mut NativeTransaction<'_>, job: &Job, error: &str) -> Result<()> {
        let mut stmt = tx.prepare(
            "UPDATE _lilia_embed_queue SET \
               next_attempt_ms = ?3 + ?4 * (1 << MIN(attempts, ?5)), \
               attempts = attempts + 1, \
               last_error = ?6 \
             WHERE space = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, &job.space)?;
        stmt.bind_text_static(2, &job.id)?;
        stmt.bind_int64(3, now_ms())?;
        stmt.bind_int64(
            4,
            i64::try_from(self.options.retry_backoff_ms).unwrap_or(i64::MAX >> MAX_BACKOFF_SHIFT),
        )?;
        stmt.bind_int64(5, MAX_BACKOFF_SHIFT)?;
        stmt.bind_text_static(6, error)?;
        stmt.step()?;
        Ok(())
    }
}

/// Background thread that embeds queued documents and stores their vectors.
///
/// It only holds weak references to the database resources, so it can never keep a closed
/// database alive. Dropping the worker wakes the thread and joins it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct EmbedWorker {
    wake: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(crate) struct EmbedWorker;

#[cfg(target_arch = "wasm32")]
impl EmbedWorker {
    #[allow(clippy::unused_self)]
    pub(crate) fn notify(&self) {}
}

#[cfg(not(target_arch = "wasm32"))]
impl EmbedWorker {
    pub(crate) fn start(core: EmbedCore) -> Result<Self> {
        let (wake, receive) = mpsc::sync_channel(1);

        let worker = thread::Builder::new()
            .name("lilia-embed".into())
            .spawn(move || loop {
                // Drain everything that is due, including what a previous run left behind.
                let pause = loop {
                    if matches!(receive.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
                        return;
                    }
                    match core.run_pass() {
                        Ok(Pass::Worked) => {}
                        Ok(Pass::Idle { retry_in }) => break retry_in,
                        Err(error) if error.code == ErrorCode::Closed => return,
                        Err(_) => break Some(ERROR_PAUSE),
                    }
                };
                let woken = match pause {
                    Some(pause) => !matches!(
                        receive.recv_timeout(pause),
                        Err(mpsc::RecvTimeoutError::Disconnected)
                    ),
                    None => receive.recv().is_ok(),
                };
                if !woken {
                    return;
                }
            })
            .map_err(|error| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to spawn embedding worker: {error}"),
                    false,
                )
            })?;

        Ok(Self {
            wake: Some(wake),
            worker: Some(worker),
        })
    }

    /// Tells the thread that documents may have been queued. Never blocks: a pending wake-up
    /// already covers the new documents.
    pub(crate) fn notify(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for EmbedWorker {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            // The worker thread can itself release the last reference to the database
            // resources; joining its own handle would deadlock.
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}
