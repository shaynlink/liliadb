//! Auto-embed: keeps the vector collection named after a JSON collection in sync with one of its
//! text fields.
//!
//! The write funnel only records *which* documents need attention in `_lilia_embed_queue`, inside
//! the transaction that changed them. The [`EmbedWorker`](crate::embed_worker::EmbedWorker) then
//! runs inference outside any lock and reconciles the vector with the document as it is when the
//! result is written, so a document is never rewritten and a stale result is never stored.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lilia_core::{
    AutoEmbedConfig, BuiltinModel, CollectionKind, EmbedStatus, Embedder, ErrorCode, FlePolicy,
    HnswConfig, LiliaError, Result, VectorElementType, VectorHit, VectorMetric,
};
use lilia_sqlite_native::{ChangeOp, ChangesetIterator, NativeConnection, NativeTransaction};

use crate::catalog::CollectionCatalog;
use crate::integrity::{require_disk, IntegrityRegistry};
use crate::storage::validate_name;
use crate::transaction::enqueue_embedding;
use crate::Database;

/// Interval between two looks at the queue while [`Database::wait_for_embeddings`] blocks.
const WAIT_POLL: Duration = Duration::from_millis(5);

/// Embedders supplied when the database was opened, by the model they run.
#[derive(Debug, Default)]
pub(crate) struct Embedders {
    by_model: HashMap<BuiltinModel, Arc<dyn Embedder>>,
}

impl Embedders {
    pub(crate) fn new(embedders: &[Arc<dyn Embedder>]) -> Self {
        Self {
            by_model: embedders
                .iter()
                .map(|embedder| (embedder.model(), Arc::clone(embedder)))
                .collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_model.is_empty()
    }

    pub(crate) fn get(&self, model: BuiltinModel) -> Result<&Arc<dyn Embedder>> {
        self.by_model.get(&model).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::Unsupported,
                format!(
                    "no embedder is registered for model '{}'; pass one to DatabaseOptions::with_embedder",
                    model.id()
                ),
                false,
            )
        })
    }

    /// Embeds `texts` and checks the backend returned one vector of the model dimension per text.
    pub(crate) fn embed(&self, model: BuiltinModel, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let vectors = self.get(model)?.embed(texts)?;
        let dimension = model.dimension() as usize;
        if vectors.len() != texts.len() || vectors.iter().any(|vector| vector.len() != dimension) {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!(
                    "embedder for '{}' returned {} vector(s) for {} text(s) or a dimension other than {dimension}",
                    model.id(),
                    vectors.len(),
                    texts.len()
                ),
                false,
            ));
        }
        Ok(vectors)
    }
}

/// Little-endian `Float32` encoding expected by the vector tables.
pub(crate) fn vector_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for component in vector {
        bytes.extend_from_slice(&component.to_le_bytes());
    }
    bytes
}

/// Refuses a source field covered by field-level encryption: documents reach the write funnel
/// already encrypted, so there would be no text to embed.
pub(crate) fn ensure_source_readable(
    config: &AutoEmbedConfig,
    fle: Option<&FlePolicy>,
) -> Result<()> {
    let Some(rule) = fle.and_then(|policy| {
        policy
            .rules
            .iter()
            .find(|rule| lilia_core::paths_overlap(&config.source_field, &rule.path))
    }) else {
        return Ok(());
    };
    Err(LiliaError::new(
        ErrorCode::InvalidInput,
        format!(
            "auto-embed cannot read '{}': it is covered by field encryption rule '{}'",
            config.source_field, rule.path
        ),
        false,
    ))
}

/// `true` when committing `operations` may have queued documents for embedding.
pub(crate) fn may_queue(
    registry: &IntegrityRegistry,
    operations: &[lilia_core::BatchOperation],
    rules_active: bool,
) -> bool {
    use lilia_core::BatchOperation as Op;
    registry.has_auto_embed()
        && (rules_active
            || operations.iter().any(|operation| {
                matches!(
                    operation,
                    Op::JsonPut { .. }
                        | Op::JsonPutRaw { .. }
                        | Op::JsonPutHlc { .. }
                        | Op::JsonDelete { .. }
                )
            }))
}

/// Queues the auto-embedded documents a changeset touched.
///
/// Undo and redo replay raw changesets, which bypass the write funnel; this restores the
/// invariant that every changed document of an auto-embedded collection is queued.
pub(crate) fn enqueue_changeset(
    tx: &mut NativeTransaction<'_>,
    registry: &IntegrityRegistry,
    changeset: &[u8],
) -> Result<bool> {
    if !registry.has_auto_embed() {
        return Ok(false);
    }
    let mut touched = Vec::new();
    let mut changes = ChangesetIterator::new(changeset)?;
    while let Some(change) = changes.next_change()? {
        if change.table != "_lilia_json" {
            continue;
        }
        let key = if change.op == ChangeOp::Insert {
            &change.new_values
        } else {
            &change.old_values
        };
        if let (
            Some(Some(serde_json::Value::String(space))),
            Some(Some(serde_json::Value::String(id))),
        ) = (key.first(), key.get(1))
        {
            if registry.auto_embed_for(space).is_some() {
                touched.push((space.clone(), id.clone()));
            }
        }
    }
    drop(changes);
    for (space, id) in &touched {
        enqueue_embedding(tx, space, id)?;
    }
    Ok(!touched.is_empty())
}

fn queue_count(
    conn: &NativeConnection,
    sql: &str,
    space: Option<&str>,
    attempts: u32,
) -> Result<u64> {
    let mut stmt = conn.prepare(sql)?;
    stmt.bind_int64(1, i64::from(attempts))?;
    if let Some(space) = space {
        stmt.bind_text_static(2, space)?;
    }
    let count = if stmt.step()? {
        stmt.column_int64(0)
    } else {
        0
    };
    Ok(u64::try_from(count).unwrap_or(0))
}

impl Database {
    /// Checks that `config` can be enabled on the JSON collection `space`.
    pub(crate) fn validate_auto_embed(&self, space: &str, config: &AutoEmbedConfig) -> Result<()> {
        validate_name("collection", space)?;
        if cfg!(target_arch = "wasm32") {
            return Err(LiliaError::new(
                ErrorCode::Unsupported,
                "auto-embed needs a background thread, which this target does not provide",
                false,
            ));
        }
        if config.source_field.trim().is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "auto-embed source_field must name a document field",
                false,
            ));
        }
        require_disk(self, CollectionKind::Json, space)?;
        require_disk(self, CollectionKind::Vector, space)?;
        self.embedders.get(config.model)?;
        let spec = self.catalog.spec_for(CollectionKind::Json, space);
        ensure_source_readable(config, spec.fle_policy.as_ref())?;

        let dimension = config.model.dimension();
        let incompatible = |found: String| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!(
                    "vector collection '{space}' holds {found}, but model '{}' produces Float32 vectors of dimension {dimension}",
                    config.model.id()
                ),
                false,
            )
        };
        if let Some(index) = self.vector_get_index_info(space)? {
            if index.dim != dimension || index.element_type != VectorElementType::Float32 {
                return Err(incompatible(format!(
                    "an index of dimension {} ({:?})",
                    index.dim, index.element_type
                )));
            }
        }
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT 1 FROM _lilia_vectors \
             WHERE collection = ?1 AND (dim <> ?2 OR element_type <> ?3) LIMIT 1;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_int64(2, i64::from(dimension))?;
        stmt.bind_int64(3, i64::from(VectorElementType::Float32 as u8))?;
        if stmt.step()? {
            return Err(incompatible("vectors of another shape".to_owned()));
        }
        Ok(())
    }

    /// Enables (`Some`) or disables (`None`) auto-embed on the JSON collection `space`.
    ///
    /// Enabling creates the HNSW index when it is missing and queues every existing document.
    /// Disabling stops the embedding and empties the queue; the vectors already stored are kept.
    pub fn apply_json_auto_embed(
        &self,
        space: &str,
        config: Option<&AutoEmbedConfig>,
    ) -> Result<()> {
        validate_name("collection", space)?;
        if let Some(config) = config {
            self.validate_auto_embed(space, config)?;
            if self.vector_get_index_info(space)?.is_none() {
                self.vector_create_index(
                    space,
                    HnswConfig {
                        dim: config.model.dimension(),
                        metric: VectorMetric::Cosine,
                        element_type: VectorElementType::Float32,
                        ..HnswConfig::default()
                    },
                )?;
            }
        }

        let mut spec = self.catalog.spec_for(CollectionKind::Json, space);
        spec.auto_embed = config.cloned();
        let mut writer = self.lock_writer()?;
        {
            let tx = writer.transaction()?;
            CollectionCatalog::persist_spec(tx.conn(), &spec)?;
            if config.is_some() {
                self.queue_collection(&tx, space)?;
            } else {
                let mut stmt = tx.prepare("DELETE FROM _lilia_embed_queue WHERE space = ?1;")?;
                stmt.bind_text_static(1, space)?;
                stmt.step()?;
                drop(stmt);
            }
            tx.commit()?;
        }
        self.catalog.set_spec(spec);
        self.integrity.set_auto_embed(space, config.cloned());
        drop(writer);
        self.resources.notify_embed_worker();
        Ok(())
    }

    fn queue_collection(&self, tx: &NativeTransaction<'_>, space: &str) -> Result<()> {
        for schema in self.data_schemas() {
            let table = crate::sharding::qualified(schema.as_deref(), "_lilia_json");
            let mut stmt = tx.prepare(&format!(
                "INSERT INTO _lilia_embed_queue(space, id) \
                 SELECT space, id FROM {table} WHERE space = ?1 \
                 ON CONFLICT(space, id) DO UPDATE SET \
                   attempts = 0, next_attempt_ms = 0, last_error = NULL;"
            ))?;
            stmt.bind_text_static(1, space)?;
            stmt.step()?;
        }
        Ok(())
    }

    /// Queues every document of the auto-embedded collection `space` again.
    pub fn requeue_auto_embed(&self, space: &str) -> Result<()> {
        validate_name("collection", space)?;
        let Some(config) = self.integrity.auto_embed_for(space) else {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("JSON collection '{space}' is not auto-embedded"),
                false,
            ));
        };
        self.embedders.get(config.model)?;
        let mut writer = self.lock_writer()?;
        {
            let tx = writer.transaction()?;
            self.queue_collection(&tx, space)?;
            tx.commit()?;
        }
        drop(writer);
        self.resources.notify_embed_worker();
        Ok(())
    }

    pub fn auto_embed_status(&self, space: &str) -> Result<EmbedStatus> {
        validate_name("collection", space)?;
        let max_attempts = self.embed_options.max_attempts;
        let conn = self.read_lock()?;
        let pending = queue_count(
            &conn,
            "SELECT COUNT(*) FROM _lilia_embed_queue WHERE attempts < ?1 AND space = ?2;",
            Some(space),
            max_attempts,
        )?;
        let failed = queue_count(
            &conn,
            "SELECT COUNT(*) FROM _lilia_embed_queue WHERE attempts >= ?1 AND space = ?2;",
            Some(space),
            max_attempts,
        )?;
        let mut stmt = conn.prepare(
            "SELECT last_error FROM _lilia_embed_queue \
             WHERE space = ?1 AND last_error IS NOT NULL \
             ORDER BY next_attempt_ms DESC LIMIT 1;",
        )?;
        stmt.bind_text_static(1, space)?;
        let last_error = if stmt.step()? {
            Some(stmt.column_text(0).to_owned())
        } else {
            None
        };
        Ok(EmbedStatus {
            pending,
            failed,
            last_error,
        })
    }

    /// Blocks until every queued document has its vector, or `timeout` elapses.
    ///
    /// Documents that exhausted their attempts are not waited for; they are reported by
    /// [`JsonStore::embed_status`](crate::JsonStore::embed_status).
    ///
    /// # Errors
    ///
    /// Returns `Timeout` when documents are still pending after `timeout`, and `Unsupported`
    /// when documents are pending but the database was opened without an embedder.
    pub fn wait_for_embeddings(&self, timeout: Duration) -> Result<()> {
        if !self.integrity.has_auto_embed() {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        loop {
            let pending = {
                let conn = self.read_lock()?;
                queue_count(
                    &conn,
                    "SELECT COUNT(*) FROM _lilia_embed_queue WHERE attempts < ?1;",
                    None,
                    self.embed_options.max_attempts,
                )?
            };
            if pending == 0 {
                return Ok(());
            }
            if self.embedders.is_empty() {
                return Err(LiliaError::new(
                    ErrorCode::Unsupported,
                    format!("{pending} document(s) await embedding but no embedder is registered"),
                    false,
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LiliaError::new(
                    ErrorCode::Timeout,
                    format!("{pending} document(s) still await embedding"),
                    true,
                ));
            }
            std::thread::sleep(remaining.min(WAIT_POLL));
        }
    }

    /// Searches the auto-embedded collection `collection` for the documents closest to `text`.
    ///
    /// The query is embedded with the model of the collection; hits carry the document ids.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the collection is not auto-embedded and `Unsupported` when no
    /// embedder is registered for its model.
    pub fn vector_search_text(
        &self,
        collection: &str,
        text: &str,
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        validate_name("collection", collection)?;
        let Some(config) = self.integrity.auto_embed_for(collection) else {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("JSON collection '{collection}' is not auto-embedded"),
                false,
            ));
        };
        let mut vectors = self.embedders.embed(config.model, &[text])?;
        let query = vector_bytes(&vectors.swap_remove(0));
        self.vector_search(
            collection,
            &query,
            config.model.dimension(),
            VectorElementType::Float32,
            VectorMetric::Cosine,
            limit,
        )
    }
}
