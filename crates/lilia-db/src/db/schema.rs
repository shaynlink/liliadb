//! Catalog administration: secondary indexes, row-level security policies, masking rules.

use std::ops::Bound;

use bytes::Bytes;
use lilia_core::{IndexOptions, JsonIndexInfo, MaskingRule, NamedPolicy, Policy};
use lilia_engine::WriteBatch;

use super::{Database, IndexedDoc, MAINTENANCE_CHUNK};
use crate::catalog::{CompiledMask, CompiledPolicy, Index};
use crate::error::{corruption, invalid, Result};
use crate::keys::{self, validate_name};
use crate::record::{decode_doc, now_ms};

impl Database {
    /// Creates a secondary index of `space` over `expression` (a path such as `$.email`, or an
    /// expression such as `lower($.email)`), optionally partial ([`IndexOptions::filter`]).
    /// Returns once the existing documents are indexed; writes keep going meanwhile.
    ///
    /// # Errors
    ///
    /// Invalid names, an index of that name already on the space, an expression that does not
    /// parse, engine errors.
    pub fn create_index(
        &self,
        space: &str,
        name: &str,
        expression: &str,
        options: &IndexOptions,
    ) -> Result<()> {
        validate_name("space", space)?;
        validate_name("index name", name)?;
        let _admin = self.admin.lock();
        let id = {
            let mut catalog = self.catalog.write();
            let mut schema = catalog.schema_or_default(space);
            if schema.index(name).is_some() {
                return Err(invalid(format!("index {name} already exists on {space}")));
            }
            let id = catalog.next_index_id;
            schema
                .indexes
                .push(Index::new(id, name, expression, options.filter.as_deref())?);
            let mut batch = WriteBatch::new();
            batch.put(keys::schema(space), schema.encode()?).put(
                Bytes::from_static(keys::NEXT_INDEX),
                (id + 1).to_le_bytes().to_vec(),
            );
            self.engine.write(batch)?;
            catalog.next_index_id = id + 1;
            catalog.set(space, schema);
            id
        };
        self.build_index(space, id)
    }

    /// Indexes the documents of `space` present when the build starts, then marks index `id`
    /// ready. Writers have maintained the index since its definition was installed; each entry
    /// written here is conditional on its document being unchanged, so a document a writer
    /// changed meanwhile keeps the writer's entries only.
    pub(super) fn build_index(&self, space: &str, id: u64) -> Result<()> {
        let index = self
            .schema(space)
            .and_then(|s| s.indexes.iter().find(|i| i.id == id).cloned())
            .ok_or_else(|| corruption(format!("index {id} of {space} is missing")))?;
        let snapshot = self.engine.snapshot();
        let base = keys::collection(keys::DOC, space);
        let end = keys::prefix_end(&base).unwrap_or_default();
        let mut chunk = Vec::with_capacity(MAINTENANCE_CHUNK);
        for entry in
            self.engine
                .range_at(&snapshot, Bound::Included(&base), Bound::Excluded(&end))?
        {
            let entry = entry?;
            let doc_id = String::from_utf8(keys::suffix(&entry.key, &base).to_vec())
                .map_err(|_| corruption(format!("document id in {space} is not UTF-8")))?;
            let values = index.values(&decode_doc(&entry.value)?);
            chunk.push((entry.key, entry.version, doc_id, values));
            if chunk.len() == MAINTENANCE_CHUNK {
                self.index_chunk(space, id, &chunk)?;
                chunk.clear();
            }
        }
        self.index_chunk(space, id, &chunk)?;
        drop(snapshot);

        let mut catalog = self.catalog.write();
        let mut schema = catalog.schema_or_default(space);
        if let Some(index) = schema.indexes.iter_mut().find(|i| i.id == id) {
            index.ready = true;
        }
        self.store_schema(&mut catalog, space, schema)
    }

    fn index_chunk(&self, space: &str, id: u64, chunk: &[IndexedDoc]) -> Result<()> {
        let batch_of = |docs: &[IndexedDoc]| {
            let mut batch = WriteBatch::new();
            for (key, version, doc_id, values) in docs {
                batch.expect(key.clone(), Some(*version));
                for value in values {
                    batch.put(keys::index_entry(space, id, value, doc_id), Bytes::new());
                }
            }
            batch
        };
        if chunk.is_empty() {
            return Ok(());
        }
        match self.engine.write(batch_of(chunk)) {
            Ok(_) => Ok(()),
            // Some documents changed since the snapshot: index the others one by one.
            Err(lilia_engine::Error::Conflict { .. }) => {
                for doc in chunk {
                    match self.engine.write(batch_of(std::slice::from_ref(doc))) {
                        Ok(_) | Err(lilia_engine::Error::Conflict { .. }) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Drops index `name` of `space`; `false` if there is none.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn drop_index(&self, space: &str, name: &str) -> Result<bool> {
        validate_name("space", space)?;
        let _admin = self.admin.lock();
        let id = {
            let mut catalog = self.catalog.write();
            let mut schema = catalog.schema_or_default(space);
            let Some(position) = schema.indexes.iter().position(|i| i.name == name) else {
                return Ok(false);
            };
            let id = schema.indexes.remove(position).id;
            schema.dropping.push(id);
            self.store_schema(&mut catalog, space, schema)?;
            id
        };
        self.clear_index(space, id)?;
        Ok(true)
    }

    /// Deletes every entry of dropped index `id`, then forgets it. Nothing writes entries of a
    /// dropped index, so the deletes need no conditions.
    pub(super) fn clear_index(&self, space: &str, id: u64) -> Result<()> {
        let prefix = keys::index(space, id);
        let end = keys::prefix_end(&prefix).unwrap_or_default();
        loop {
            let mut batch = WriteBatch::new();
            for entry in self
                .engine
                .range(Bound::Included(&prefix), Bound::Excluded(&end))?
                .take(MAINTENANCE_CHUNK * 4)
            {
                batch.delete(entry?.key);
            }
            if batch.is_empty() {
                break;
            }
            self.engine.write(batch)?;
        }
        let mut catalog = self.catalog.write();
        let mut schema = catalog.schema_or_default(space);
        schema.dropping.retain(|&d| d != id);
        self.store_schema(&mut catalog, space, schema)
    }

    /// The indexes of `space`, including those still being built.
    #[must_use]
    pub fn indexes(&self, space: &str) -> Vec<JsonIndexInfo> {
        self.schema(space)
            .map_or_default(|s| s.indexes.iter().map(|i| i.info(space)).collect())
    }

    // ---- row-level security and masking ----

    /// Creates the row-level security policy `name` on the collection `policy.table`.
    ///
    /// # Errors
    ///
    /// Invalid names, a policy of that name already, a condition that does not parse, engine
    /// errors.
    pub fn create_policy(&self, name: &str, policy: Policy) -> Result<()> {
        validate_name("policy name", name)?;
        validate_name("policy table", &policy.table)?;
        let created_at_ms = u64::try_from(now_ms()).unwrap_or_default();
        let compiled = CompiledPolicy::new(NamedPolicy {
            name: name.to_owned(),
            policy,
            created_at_ms,
        })?;
        let mut catalog = self.catalog.write();
        if catalog
            .schemas()
            .any(|(_, s)| s.policies.iter().any(|p| p.named.name == name))
        {
            return Err(invalid(format!("policy {name} already exists")));
        }
        let table = compiled.named.policy.table.clone();
        let mut schema = catalog.schema_or_default(&table);
        schema.policies.push(compiled);
        self.store_schema(&mut catalog, &table, schema)
    }

    /// Drops policy `name`; `false` if there is none.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn drop_policy(&self, name: &str) -> Result<bool> {
        let mut catalog = self.catalog.write();
        let Some(table) = catalog
            .schemas()
            .find(|(_, s)| s.policies.iter().any(|p| p.named.name == name))
            .map(|(t, _)| t.clone())
        else {
            return Ok(false);
        };
        let mut schema = catalog.schema_or_default(&table);
        schema.policies.retain(|p| p.named.name != name);
        self.store_schema(&mut catalog, &table, schema)?;
        Ok(true)
    }

    /// Every policy, by name.
    #[must_use]
    pub fn policies(&self) -> Vec<NamedPolicy> {
        let catalog = self.catalog.read();
        let mut policies: Vec<NamedPolicy> = catalog
            .schemas()
            .flat_map(|(_, s)| s.policies.iter().map(|p| p.named.clone()))
            .collect();
        policies.sort_by(|a, b| a.name.cmp(&b.name));
        policies
    }

    /// Creates a masking rule, replacing the rule of the same table and path.
    ///
    /// # Errors
    ///
    /// Invalid table name, an exemption condition that does not parse, engine errors.
    pub fn create_masking_rule(&self, rule: MaskingRule) -> Result<()> {
        validate_name("masking table", &rule.table)?;
        let compiled = CompiledMask::new(rule)?;
        let mut catalog = self.catalog.write();
        let table = compiled.rule.table.clone();
        let mut schema = catalog.schema_or_default(&table);
        schema
            .masking
            .retain(|m| !m.rule.matches_path(&compiled.rule.path));
        schema.masking.push(compiled);
        self.store_schema(&mut catalog, &table, schema)
    }

    /// Drops the masking rule of `table` on `path`; `false` if there is none.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn drop_masking_rule(&self, table: &str, path: &str) -> Result<bool> {
        let mut catalog = self.catalog.write();
        let mut schema = catalog.schema_or_default(table);
        let before = schema.masking.len();
        schema.masking.retain(|m| !m.rule.matches_path(path));
        if schema.masking.len() == before {
            return Ok(false);
        }
        self.store_schema(&mut catalog, table, schema)?;
        Ok(true)
    }

    /// Every masking rule, by table and path.
    #[must_use]
    pub fn masking_rules(&self) -> Vec<MaskingRule> {
        let catalog = self.catalog.read();
        let mut rules: Vec<MaskingRule> = catalog
            .schemas()
            .flat_map(|(_, s)| s.masking.iter().map(|m| m.rule.clone()))
            .collect();
        rules.sort_by(|a, b| (&a.table, &a.path).cmp(&(&b.table, &b.path)));
        rules
    }
}
