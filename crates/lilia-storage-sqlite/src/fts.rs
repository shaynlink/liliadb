#![allow(clippy::missing_errors_doc)]

use lilia_sqlite_native::StmtSlot;

use crate::storage::{normalize_limit, validate_name};
use crate::Database;
use lilia_core::{FtsHit, Result};

impl Database {
    pub fn fts_index(&self, collection: &str, doc_id: &str, content: &str) -> Result<()> {
        validate_name("collection", collection)?;
        validate_name("doc_id", doc_id)?;
        let mut writer = self.lock_writer()?;
        let mut tx = writer.transaction()?;
        let stmt_del = tx.slot(StmtSlot::FtsIndexDelete)?;
        stmt_del.reset()?;
        stmt_del.bind_text_static(1, collection)?;
        stmt_del.bind_text_static(2, doc_id)?;
        stmt_del.step()?;
        stmt_del.reset()?;

        let stmt_ins = tx.slot(StmtSlot::FtsIndexInsert)?;
        stmt_ins.reset()?;
        stmt_ins.bind_text_static(1, collection)?;
        stmt_ins.bind_text_static(2, doc_id)?;
        stmt_ins.bind_text_static(3, content)?;
        stmt_ins.step()?;
        stmt_ins.reset()?;
        tx.commit()?;
        Ok(())
    }

    pub fn fts_delete(&self, collection: &str, doc_id: &str) -> Result<bool> {
        validate_name("collection", collection)?;
        validate_name("doc_id", doc_id)?;
        let mut writer = self.lock_writer()?;
        let stmt = writer.slot(StmtSlot::FtsIndexDelete)?;
        stmt.reset()?;
        stmt.bind_text_static(1, collection)?;
        stmt.bind_text_static(2, doc_id)?;
        stmt.step()?;
        stmt.reset()?;
        let deleted = writer.changes() > 0;
        Ok(deleted)
    }

    pub fn fts_search(&self, collection: &str, query: &str, limit: u32) -> Result<Vec<FtsHit>> {
        validate_name("collection", collection)?;
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT doc_id, snippet(_lilia_fts, 2, '<b>', '</b>', '...', 32), rank \
             FROM _lilia_fts \
             WHERE collection = ?1 AND _lilia_fts MATCH ?2 \
             ORDER BY rank \
             LIMIT ?3;",
        )?;
        let match_query = if query.contains(':')
            || query.contains('"')
            || query.contains("AND")
            || query.contains("OR")
            || query.contains('*')
        {
            query.to_string()
        } else {
            let tokens: Vec<String> = query
                .split_whitespace()
                .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
                .collect();
            if tokens.is_empty() {
                return Ok(Vec::new());
            }
            tokens.join(" ")
        };

        stmt.bind_text_static(1, collection)?;
        stmt.bind_text_static(2, &match_query)?;
        stmt.bind_int64(3, normalize_limit(limit))?;

        let mut hits = Vec::new();
        while stmt.step()? {
            hits.push(FtsHit {
                collection: collection.to_owned(),
                doc_id: stmt.column_text(0).to_string(),
                snippet: stmt.column_text(1).to_string(),
                rank: stmt.column_double(2),
            });
        }
        Ok(hits)
    }
}
