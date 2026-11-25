//! Row-level security and masking, enforced at one point for reads and one point for writes.
//!
//! Every read path of [`crate::Session`] and [`crate::Transaction`] hands each record it found
//! to a [`Guard`] before returning it: records a read policy rejects are skipped, and masked
//! fields of the documents that pass are rewritten. Every write is checked by the same guard
//! against the write policies, with the record it replaces and the record it writes.

use std::sync::Arc;

use lilia_core::{
    eval_security_condition, eval_security_condition_kv, mask_document, BinaryDoc, JsonEntry,
    KvEntry, MaskingRule, PolicyAction, SecurityContext,
};
use serde_json::Value;

use crate::catalog::Schema;
use crate::error::{Error, Result};

/// Who is reading or writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Trusted code: policies and masking do not apply.
    System,
    /// A caller identified by its claims and roles: every policy and masking rule applies.
    Caller(SecurityContext),
}

/// The policies and masking rules of one collection, bound to one caller.
#[derive(Debug)]
pub struct Guard<'a> {
    context: Option<&'a SecurityContext>,
    schema: Option<Arc<Schema>>,
}

impl<'a> Guard<'a> {
    /// The guard of `schema` for `access`; system access gets a guard that admits everything.
    #[must_use]
    pub fn new(access: &'a Access, schema: Option<Arc<Schema>>) -> Self {
        match access {
            Access::System => Self {
                context: None,
                schema: None,
            },
            Access::Caller(context) => Self {
                context: Some(context),
                schema,
            },
        }
    }

    fn policies(
        &self,
        action: PolicyAction,
    ) -> impl Iterator<Item = &crate::catalog::CompiledPolicy> {
        self.schema
            .iter()
            .flat_map(|s| s.policies.iter())
            .filter(move |p| p.governs(action))
    }

    /// Whether writes must read the record they replace to be checked.
    #[must_use]
    pub fn checks_writes(&self) -> bool {
        self.schema.as_ref().is_some_and(|s| s.governs_writes())
    }

    /// Whether some policy governs reads of the collection.
    #[must_use]
    pub fn checks_reads(&self) -> bool {
        self.policies(PolicyAction::Select).next().is_some()
    }

    /// Whether the caller may see a record related to a JSON document (a full-text hit, a
    /// vector): through the read policies of `document`, the JSON document of the same id, when
    /// there is one, otherwise of the document `synthetic` makes.
    pub fn related_hit(
        &self,
        document: Option<&BinaryDoc>,
        synthetic: impl FnOnce() -> serde_json::Value,
    ) -> bool {
        if !self.checks_reads() {
            return true;
        }
        let value = document.map_or_else(synthetic, BinaryDoc::to_json_value);
        self.doc_allowed(PolicyAction::Select, &value)
    }

    /// The entry if every read policy admits it.
    #[must_use]
    pub fn kv(&self, entry: KvEntry) -> Option<KvEntry> {
        self.kv_allowed(
            PolicyAction::Select,
            &entry.namespace,
            &entry.key,
            &entry.value,
        )
        .then_some(entry)
    }

    fn kv_allowed(&self, action: PolicyAction, namespace: &str, key: &[u8], value: &[u8]) -> bool {
        self.policies(action)
            .all(|p| eval_security_condition_kv(&p.condition, namespace, key, value, self.context))
    }

    fn doc_allowed(&self, action: PolicyAction, doc: &Value) -> bool {
        self.policies(action)
            .all(|p| eval_security_condition(&p.condition, doc, self.context))
    }

    /// The entry if every read policy admits it, with the fields of the caller's masking rules
    /// masked.
    ///
    /// # Errors
    ///
    /// [`Error::Document`] if the masked document cannot be encoded again.
    pub fn doc(&self, mut entry: JsonEntry) -> Result<Option<JsonEntry>> {
        let Some(schema) = &self.schema else {
            return Ok(Some(entry));
        };
        let reads = self.policies(PolicyAction::Select).next().is_some();
        let empty = Value::Object(serde_json::Map::new());
        let masks: Vec<MaskingRule> = schema
            .masking
            .iter()
            .filter(|m| {
                !m.exempt
                    .as_ref()
                    .is_some_and(|e| eval_security_condition(e, &empty, self.context))
            })
            .map(|m| m.rule.clone())
            .collect();
        if !reads && masks.is_empty() {
            return Ok(Some(entry));
        }
        let mut value = entry.doc.to_json_value();
        if !self.doc_allowed(PolicyAction::Select, &value) {
            return Ok(None);
        }
        if !masks.is_empty() {
            mask_document(&mut value, &masks, self.context, |_, _| false);
            entry.doc =
                BinaryDoc::from_json_value(&value).map_err(|e| Error::Document(e.to_string()))?;
        }
        Ok(Some(entry))
    }

    /// Checks a key-value write: an insert (`old` absent) against the insert policies, an update
    /// against the update policies for both records, a delete (`new` absent) against the delete
    /// policies.
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] naming the collection.
    pub fn check_kv(
        &self,
        namespace: &str,
        key: &[u8],
        old: Option<&[u8]>,
        new: Option<&[u8]>,
    ) -> Result<()> {
        let action = action(old.is_some(), new.is_some());
        let allowed = old
            .into_iter()
            .chain(new)
            .all(|value| self.kv_allowed(action, namespace, key, value));
        if allowed {
            Ok(())
        } else {
            Err(denied(action, namespace))
        }
    }

    /// Checks a document write; see [`Guard::check_kv`].
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] naming the collection.
    pub fn check_doc(
        &self,
        space: &str,
        old: Option<&BinaryDoc>,
        new: Option<&BinaryDoc>,
    ) -> Result<()> {
        let action = action(old.is_some(), new.is_some());
        if self.policies(action).next().is_none() {
            return Ok(());
        }
        let allowed = old
            .into_iter()
            .chain(new)
            .all(|doc| self.doc_allowed(action, &doc.to_json_value()));
        if allowed {
            Ok(())
        } else {
            Err(denied(action, space))
        }
    }
}

const fn action(old: bool, new: bool) -> PolicyAction {
    match (old, new) {
        (false, _) => PolicyAction::Insert,
        (true, true) => PolicyAction::Update,
        (true, false) => PolicyAction::Delete,
    }
}

fn denied(action: PolicyAction, collection: &str) -> Error {
    Error::Denied(format!(
        "a policy of {collection} rejects this {}",
        action.as_str()
    ))
}
