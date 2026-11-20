//! The catalog: per-collection secondary indexes, row-level security policies and masking rules,
//! stored under the meta tag and kept in memory with their expressions parsed once.
//!
//! A schema is keyed by collection name; policies and masking rules name their collection in
//! their `table` field, and apply to the key-value namespace and the JSON space of that name.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use lilia_core::{
    eval_expr, eval_filter_predicate, parse_expr, BinaryDoc, Expr, JsonIndexInfo, MaskingRule,
    NamedPolicy, PolicyAction,
};
use lilia_engine::Db;
use serde::{Deserialize, Serialize};

use crate::error::{corruption, Error, Result};
use crate::keys;

/// Parses an index, filter or policy expression.
///
/// # Errors
///
/// [`Error::Expression`] with the parser's reason.
pub fn parse(source: &str) -> Result<Expr> {
    parse_expr(source).map_err(|e| Error::Expression {
        expression: source.to_owned(),
        reason: e.to_string(),
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredSchema {
    #[serde(default)]
    indexes: Vec<StoredIndex>,
    #[serde(default)]
    policies: Vec<NamedPolicy>,
    #[serde(default)]
    masking: Vec<MaskingRule>,
    #[serde(default)]
    dropping: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIndex {
    id: u64,
    name: String,
    expression: String,
    #[serde(default)]
    filter: Option<String>,
    /// `false` while the index is being built from the existing documents.
    ready: bool,
}

/// A secondary index over a JSON space: the values of an expression, optionally only for the
/// documents that satisfy a filter (a partial index).
#[derive(Debug, Clone)]
pub struct Index {
    /// Unique over the database's lifetime, so the entries of a dropped index never mix with
    /// those of a new index of the same name.
    pub id: u64,
    pub name: String,
    pub expression: String,
    pub filter_source: Option<String>,
    pub ready: bool,
    pub expr: Expr,
    pub filter: Option<Expr>,
}

impl Index {
    /// # Errors
    ///
    /// [`Error::Expression`] for an expression or filter that does not parse.
    pub fn new(id: u64, name: &str, expression: &str, filter: Option<&str>) -> Result<Self> {
        Ok(Self {
            id,
            name: name.to_owned(),
            expression: expression.to_owned(),
            filter_source: filter.map(str::to_owned),
            ready: false,
            expr: parse(expression)?,
            filter: filter.map(parse).transpose()?,
        })
    }

    /// The index values `doc` contributes: none when it fails the filter or the expression
    /// cannot be evaluated.
    pub fn values(&self, doc: &BinaryDoc) -> BTreeSet<String> {
        if self
            .filter
            .as_ref()
            .is_some_and(|filter| !eval_filter_predicate(filter, doc))
        {
            return BTreeSet::new();
        }
        eval_expr(&self.expr, doc).map_or_else(
            |_| BTreeSet::new(),
            |value| value.to_index_strings().into_iter().collect(),
        )
    }

    pub fn info(&self, space: &str) -> JsonIndexInfo {
        JsonIndexInfo {
            space: space.to_owned(),
            index_name: self.name.clone(),
            json_path: self.expression.clone(),
            filter: self.filter_source.clone(),
        }
    }
}

/// A policy with its condition parsed.
#[derive(Debug, Clone)]
pub struct CompiledPolicy {
    pub named: NamedPolicy,
    pub condition: Expr,
}

impl CompiledPolicy {
    /// # Errors
    ///
    /// [`Error::Expression`] for a condition that does not parse.
    pub fn new(named: NamedPolicy) -> Result<Self> {
        let condition = parse(&named.policy.condition)?;
        Ok(Self { named, condition })
    }

    /// Whether the policy governs `action`: its own action, or every action for `All`.
    pub fn governs(&self, action: PolicyAction) -> bool {
        self.named.policy.action == PolicyAction::All || self.named.policy.action == action
    }
}

/// A masking rule with its exemption condition parsed.
#[derive(Debug, Clone)]
pub struct CompiledMask {
    pub rule: MaskingRule,
    pub exempt: Option<Expr>,
}

impl CompiledMask {
    /// # Errors
    ///
    /// [`Error::Expression`] for an exemption condition that does not parse.
    pub fn new(rule: MaskingRule) -> Result<Self> {
        let exempt = rule.exempt_condition.as_deref().map(parse).transpose()?;
        Ok(Self { rule, exempt })
    }
}

/// Indexes, policies and masking rules of one collection.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub indexes: Vec<Index>,
    pub policies: Vec<CompiledPolicy>,
    pub masking: Vec<CompiledMask>,
    /// Ids of dropped indexes whose entries are still being deleted.
    pub dropping: Vec<u64>,
}

impl Schema {
    fn compile(stored: StoredSchema) -> Result<Self> {
        let indexes = stored
            .indexes
            .into_iter()
            .map(|i| {
                let mut index = Index::new(i.id, &i.name, &i.expression, i.filter.as_deref())?;
                index.ready = i.ready;
                Ok(index)
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            indexes,
            policies: stored
                .policies
                .into_iter()
                .map(CompiledPolicy::new)
                .collect::<Result<_>>()?,
            masking: stored
                .masking
                .into_iter()
                .map(CompiledMask::new)
                .collect::<Result<_>>()?,
            dropping: stored.dropping,
        })
    }

    /// The stored form of the schema.
    ///
    /// # Errors
    ///
    /// Never in practice: every field serializes to JSON.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let stored = StoredSchema {
            indexes: self
                .indexes
                .iter()
                .map(|i| StoredIndex {
                    id: i.id,
                    name: i.name.clone(),
                    expression: i.expression.clone(),
                    filter: i.filter_source.clone(),
                    ready: i.ready,
                })
                .collect(),
            policies: self.policies.iter().map(|p| p.named.clone()).collect(),
            masking: self.masking.iter().map(|m| m.rule.clone()).collect(),
            dropping: self.dropping.clone(),
        };
        serde_json::to_vec(&stored).map_err(|e| corruption(format!("schema encoding: {e}")))
    }

    pub fn is_empty(&self) -> bool {
        self.indexes.is_empty()
            && self.policies.is_empty()
            && self.masking.is_empty()
            && self.dropping.is_empty()
    }

    pub fn index(&self, name: &str) -> Option<&Index> {
        self.indexes.iter().find(|i| i.name == name)
    }

    /// Whether some policy governs a write action, so writes must read the record they replace.
    pub fn governs_writes(&self) -> bool {
        self.policies
            .iter()
            .any(|p| p.named.policy.action != PolicyAction::Select)
    }
}

/// Every schema of the database, in memory.
#[derive(Debug, Default)]
pub struct Catalog {
    schemas: HashMap<String, Arc<Schema>>,
    /// Next index id to hand out.
    pub next_index_id: u64,
}

impl Catalog {
    /// Reads the catalog stored in `engine`.
    ///
    /// # Errors
    ///
    /// Engine read errors; [`Error::Corruption`] for records that do not decode.
    pub fn load(engine: &Db) -> Result<Self> {
        let mut catalog = Self {
            schemas: HashMap::new(),
            next_index_id: 1,
        };
        if let Some(next) = engine.get(keys::NEXT_INDEX)? {
            let bytes: [u8; 8] = next
                .as_ref()
                .try_into()
                .map_err(|_| corruption("next index id record"))?;
            catalog.next_index_id = u64::from_le_bytes(bytes);
        }
        let end = keys::prefix_end(keys::SCHEMA_PREFIX).unwrap_or_default();
        for entry in engine.range(
            std::ops::Bound::Included(keys::SCHEMA_PREFIX),
            std::ops::Bound::Excluded(&end),
        )? {
            let entry = entry?;
            let name = String::from_utf8(keys::suffix(&entry.key, keys::SCHEMA_PREFIX).to_vec())
                .map_err(|_| corruption("schema name is not UTF-8"))?;
            let stored: StoredSchema = serde_json::from_slice(&entry.value)
                .map_err(|e| corruption(format!("schema of {name}: {e}")))?;
            let schema = Schema::compile(stored)
                .map_err(|e| corruption(format!("schema of {name}: {e}")))?;
            catalog.schemas.insert(name, Arc::new(schema));
        }
        Ok(catalog)
    }

    pub fn schema(&self, name: &str) -> Option<Arc<Schema>> {
        self.schemas.get(name).cloned()
    }

    /// The schema of `name`, or an empty one, to be modified and stored with [`Catalog::set`].
    pub fn schema_or_default(&self, name: &str) -> Schema {
        self.schemas.get(name).map_or_default(|s| Schema::clone(s))
    }

    pub fn set(&mut self, name: &str, schema: Schema) {
        if schema.is_empty() {
            self.schemas.remove(name);
        } else {
            self.schemas.insert(name.to_owned(), Arc::new(schema));
        }
    }

    pub fn schemas(&self) -> impl Iterator<Item = (&String, &Arc<Schema>)> {
        self.schemas.iter()
    }
}
