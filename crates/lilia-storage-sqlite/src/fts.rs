#![allow(clippy::missing_errors_doc)]

use crate::storage::{normalize_limit, validate_name};
use crate::Database;
use lilia_core::{
    BatchOperation, CollectionKind, ErrorCode, FtsFuzzyHit, FtsHit, FtsSuggestion, LiliaError,
    Result, StorageTier,
};

impl Database {
    pub(crate) fn is_fts_doc_authorized(
        &self,
        collection: &str,
        doc_id: &str,
        snippet: &str,
    ) -> bool {
        let policies = self.security_registry.policies_for_table(collection);
        if policies.is_empty() {
            return true;
        }
        // 1. If it corresponds to a JSON document in the same space, check json doc authorization
        if let Ok(Some(entry)) = self.read_json_entry(collection, doc_id) {
            return self.is_json_doc_authorized(
                collection,
                &entry.value(),
                lilia_core::security::PolicyAction::Select,
            );
        }
        // 2. Otherwise check condition against synthetic document
        let sec_ctx = crate::security::get_active_context();
        let synthetic = serde_json::json!({
            "collection": collection,
            "doc_id": doc_id,
            "snippet": snippet,
        });
        for np in policies {
            if !np.policy.action.applies_to_read() {
                continue;
            }
            if let Ok(expr) = lilia_core::parse_expr(&np.policy.condition) {
                if !lilia_core::security::eval_security_condition(
                    &expr,
                    &synthetic,
                    sec_ctx.as_ref(),
                ) {
                    return false;
                }
            }
        }
        true
    }

    pub fn fts_index(&self, collection: &str, doc_id: &str, content: &str) -> Result<()> {
        validate_name("collection", collection)?;
        validate_name("doc_id", doc_id)?;
        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        if tier == StorageTier::Buffered {
            self.batch(&[BatchOperation::FtsIndex {
                collection: collection.to_owned(),
                doc_id: doc_id.to_owned(),
                content: content.to_owned(),
            }])?;
            return Ok(());
        }
        let mut writer = self.lock_writer()?;
        let mut tx = writer.transaction()?;
        crate::transaction::apply_fts_index(
            &mut tx,
            collection,
            doc_id,
            content,
            tier == StorageTier::Memory,
        )?;
        tx.commit()?;

        // Update in-memory trie and phonetic vocabulary if already loaded
        let indexed_content = lilia_core::prepare_cjk_for_indexing(content);
        let unique_tokens = crate::transaction::tokenize_terms(&indexed_content);

        if let Some(trie_arc) = self
            .fts_trie_cache
            .read()
            .ok()
            .and_then(|c| c.get(collection).cloned())
        {
            if let Ok(mut trie) = trie_arc.write() {
                for token in &unique_tokens {
                    trie.insert(token, 1);
                }
            }
        }

        if let Some(vocab_arc) = self
            .fts_phonetic_cache
            .read()
            .ok()
            .and_then(|c| c.get(collection).cloned())
        {
            if let Ok(mut vocab) = vocab_arc.write() {
                for token in &unique_tokens {
                    vocab.insert(token, 1);
                }
            }
        }

        Ok(())
    }

    pub fn fts_delete(&self, collection: &str, doc_id: &str) -> Result<bool> {
        validate_name("collection", collection)?;
        validate_name("doc_id", doc_id)?;
        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let mut writer = self.lock_writer()?;
        let mut tx = writer.transaction()?;
        let res = crate::transaction::apply_fts_delete(
            &mut tx,
            collection,
            doc_id,
            tier == StorageTier::Memory,
        )?;
        tx.commit()?;
        Ok(res.deleted)
    }

    pub fn fts_search(&self, collection: &str, query: &str, limit: u32) -> Result<Vec<FtsHit>> {
        validate_name("collection", collection)?;
        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let sql = if tier == StorageTier::Memory {
            "SELECT doc_id, snippet(_lilia_mem_fts, 2, '<b>', '</b>', '...', 32), rank \
             FROM _lilia_mem._lilia_mem_fts \
             WHERE collection = ?1 AND _lilia_mem_fts MATCH ?2 \
             ORDER BY rank \
             LIMIT ?3;"
        } else {
            "SELECT doc_id, snippet(_lilia_fts, 2, '<b>', '</b>', '...', 32), rank \
             FROM _lilia_fts \
             WHERE collection = ?1 AND _lilia_fts MATCH ?2 \
             ORDER BY rank \
             LIMIT ?3;"
        };
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(sql)?;
        let processed_query = if lilia_core::contains_cjk(query)
            && !query.contains(':')
            && !query.contains('"')
            && !query.contains('*')
        {
            lilia_core::prepare_cjk_for_search(query)
        } else {
            query.to_string()
        };

        let match_query = if processed_query.contains(':')
            || processed_query.contains('"')
            || processed_query.contains("AND")
            || processed_query.contains("OR")
            || processed_query.contains('*')
        {
            processed_query
        } else {
            let tokens: Vec<String> = processed_query
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
            let doc_id = stmt.column_text(0).to_string();
            let snippet = stmt.column_text(1).to_string();
            let rank = stmt.column_double(2);
            if !self.is_fts_doc_authorized(collection, &doc_id, &snippet) {
                continue;
            }
            hits.push(FtsHit {
                collection: collection.to_owned(),
                doc_id,
                snippet,
                rank,
            });
        }
        Ok(hits)
    }

    /// Retrieves or lazily loads the [`lilia_core::TermTrie`] for a collection.
    pub(crate) fn get_or_load_fts_trie(
        &self,
        collection: &str,
    ) -> Result<std::sync::Arc<std::sync::RwLock<lilia_core::TermTrie>>> {
        validate_name("collection", collection)?;
        {
            let cache = self.fts_trie_cache.read().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "fts trie lock poisoned", false)
            })?;
            if let Some(trie) = cache.get(collection) {
                return Ok(std::sync::Arc::clone(trie));
            }
        }

        let mut cache = self
            .fts_trie_cache
            .write()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "fts trie lock poisoned", false))?;
        if let Some(trie) = cache.get(collection) {
            return Ok(std::sync::Arc::clone(trie));
        }

        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        let sql = if tier == StorageTier::Memory {
            "SELECT term, doc_count FROM _lilia_mem._lilia_mem_fts_terms WHERE collection = ?1;"
        } else {
            "SELECT term, doc_count FROM _lilia_fts_terms WHERE collection = ?1;"
        };

        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(sql)?;
        stmt.bind_text_static(1, collection)?;

        let mut trie = lilia_core::TermTrie::new();
        let mut row_count = 0;
        while stmt.step()? {
            let term = stmt.column_text(0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let count = stmt.column_int64(1).max(1) as u32;
            trie.insert(term, count);
            row_count += 1;
        }

        // If _lilia_fts_terms had no rows, fallback to scanning _lilia_fts
        if row_count == 0 {
            let fallback_sql = if tier == StorageTier::Memory {
                "SELECT content FROM _lilia_mem._lilia_mem_fts WHERE collection = ?1;"
            } else {
                "SELECT content FROM _lilia_fts WHERE collection = ?1;"
            };
            let mut fts_stmt = conn.prepare(fallback_sql)?;
            fts_stmt.bind_text_static(1, collection)?;
            while fts_stmt.step()? {
                let content = fts_stmt.column_text(0);
                for token in crate::transaction::tokenize_terms(content) {
                    trie.insert(&token, 1);
                }
            }
        }

        let trie_arc = std::sync::Arc::new(std::sync::RwLock::new(trie));
        cache.insert(collection.to_owned(), std::sync::Arc::clone(&trie_arc));
        Ok(trie_arc)
    }

    /// Retrieves or lazily loads the [`lilia_core::PhoneticVocabulary`] for a collection.
    pub(crate) fn get_or_load_fts_phonetic(
        &self,
        collection: &str,
    ) -> Result<std::sync::Arc<std::sync::RwLock<lilia_core::PhoneticVocabulary>>> {
        validate_name("collection", collection)?;
        {
            let cache = self.fts_phonetic_cache.read().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "fts phonetic lock poisoned", false)
            })?;
            if let Some(vocab) = cache.get(collection) {
                return Ok(std::sync::Arc::clone(vocab));
            }
        }

        let mut cache = self.fts_phonetic_cache.write().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "fts phonetic lock poisoned", false)
        })?;
        if let Some(vocab) = cache.get(collection) {
            return Ok(std::sync::Arc::clone(vocab));
        }

        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        let sql = if tier == StorageTier::Memory {
            "SELECT term, doc_count FROM _lilia_mem._lilia_mem_fts_terms WHERE collection = ?1;"
        } else {
            "SELECT term, doc_count FROM _lilia_fts_terms WHERE collection = ?1;"
        };

        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(sql)?;
        stmt.bind_text_static(1, collection)?;

        let mut vocab = lilia_core::PhoneticVocabulary::new();
        let mut row_count = 0;
        while stmt.step()? {
            let term = stmt.column_text(0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let count = stmt.column_int64(1).max(1) as u32;
            vocab.insert(term, count);
            row_count += 1;
        }

        // If _lilia_fts_terms had no rows, fallback to scanning _lilia_fts
        if row_count == 0 {
            let fallback_sql = if tier == StorageTier::Memory {
                "SELECT content FROM _lilia_mem._lilia_mem_fts WHERE collection = ?1;"
            } else {
                "SELECT content FROM _lilia_fts WHERE collection = ?1;"
            };
            let mut fts_stmt = conn.prepare(fallback_sql)?;
            fts_stmt.bind_text_static(1, collection)?;
            while fts_stmt.step()? {
                let content = fts_stmt.column_text(0);
                for token in crate::transaction::tokenize_terms(content) {
                    vocab.insert(&token, 1);
                }
            }
        }

        let vocab_arc = std::sync::Arc::new(std::sync::RwLock::new(vocab));
        cache.insert(collection.to_owned(), std::sync::Arc::clone(&vocab_arc));
        Ok(vocab_arc)
    }

    /// Performs fast fuzzy autocompletion / term suggestion against the collection vocabulary
    /// using a compiled Levenshtein automaton.
    pub fn fts_suggest(
        &self,
        collection: &str,
        prefix: &str,
        max_distance: u8,
        limit: u32,
    ) -> Result<Vec<FtsSuggestion>> {
        validate_name("collection", collection)?;
        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let clean_prefix = prefix.trim().to_lowercase();
        if clean_prefix.is_empty() {
            return Ok(Vec::new());
        }

        // Adaptive distance: avoid blowing up candidate space for short inputs
        let effective_dist = if clean_prefix.chars().count() <= 2 {
            max_distance.min(1)
        } else {
            max_distance.min(2)
        };

        let trie_arc = self.get_or_load_fts_trie(collection)?;
        let trie = trie_arc
            .read()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "fts trie lock poisoned", false))?;

        let auto = lilia_core::LevenshteinAutomaton::for_prefix(&clean_prefix, effective_dist);
        let suggestions =
            trie.search_automaton(&auto, crate::storage::sql_count(normalize_limit(limit)));
        Ok(suggestions)
    }

    /// Performs fuzzy full-text search with typo tolerance ($D \le 2$).
    ///
    /// Expands misspelled query words with candidate dictionary terms, queries SQLite FTS5,
    /// and ranks results combining BM25 and Levenshtein edit distance.
    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub fn fts_fuzzy_search(
        &self,
        collection: &str,
        query: &str,
        max_distance: u8,
        limit: u32,
    ) -> Result<Vec<FtsFuzzyHit>> {
        validate_name("collection", collection)?;
        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let trie_arc = self.get_or_load_fts_trie(collection)?;
        let trie = trie_arc
            .read()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "fts trie lock poisoned", false))?;

        // Extract words from query
        let query_words: Vec<String> = query
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect();

        if query_words.is_empty() {
            return Ok(Vec::new());
        }

        let effective_dist = max_distance.min(2);
        let mut expanded_clauses = Vec::new();
        let mut term_distances: std::collections::HashMap<String, u8> =
            std::collections::HashMap::new();

        for word in &query_words {
            if trie.contains(word) {
                // Exact match in dictionary
                term_distances.insert(word.clone(), 0);
                expanded_clauses.push(format!("\"{}\"", word.replace('"', "\"\"")));
            } else if effective_dist > 0 {
                // Look up closest candidates with Levenshtein automaton
                let auto = lilia_core::LevenshteinAutomaton::for_exact(word, effective_dist);
                let mut candidates = trie.search_automaton(&auto, 3);
                if candidates.is_empty() {
                    if let Ok(vocab_arc) = self.get_or_load_fts_phonetic(collection) {
                        if let Ok(vocab) = vocab_arc.read() {
                            for pc in vocab.find_candidates(word, 3) {
                                candidates.push(lilia_core::FtsSuggestion {
                                    term: pc.term,
                                    distance: 1,
                                    doc_count: pc.doc_count,
                                    exact_prefix: false,
                                });
                            }
                        }
                    }
                }
                if candidates.is_empty() {
                    expanded_clauses.push(format!("\"{}\"", word.replace('"', "\"\"")));
                } else {
                    let mut or_parts = Vec::new();
                    or_parts.push(format!("\"{}\"", word.replace('"', "\"\"")));
                    for cand in candidates {
                        term_distances
                            .entry(cand.term.clone())
                            .and_modify(|d| *d = (*d).min(cand.distance))
                            .or_insert(cand.distance);
                        or_parts.push(format!("\"{}\"", cand.term.replace('"', "\"\"")));
                    }
                    expanded_clauses.push(format!("({})", or_parts.join(" OR ")));
                }
            } else {
                expanded_clauses.push(format!("\"{}\"", word.replace('"', "\"\"")));
            }
        }

        let match_query = expanded_clauses.join(" AND ");

        let sql = if tier == StorageTier::Memory {
            "SELECT doc_id, snippet(_lilia_mem_fts, 2, '<b>', '</b>', '...', 32), rank \
             FROM _lilia_mem._lilia_mem_fts \
             WHERE collection = ?1 AND _lilia_mem_fts MATCH ?2 \
             ORDER BY rank \
             LIMIT ?3;"
        } else {
            "SELECT doc_id, snippet(_lilia_fts, 2, '<b>', '</b>', '...', 32), rank \
             FROM _lilia_fts \
             WHERE collection = ?1 AND _lilia_fts MATCH ?2 \
             ORDER BY rank \
             LIMIT ?3;"
        };

        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(sql)?;
        stmt.bind_text_static(1, collection)?;
        stmt.bind_text_static(2, &match_query)?;
        stmt.bind_int64(3, normalize_limit(limit))?;

        let mut hits = Vec::new();
        while stmt.step()? {
            let doc_id = stmt.column_text(0).to_string();
            let snippet = stmt.column_text(1).to_string();
            let base_rank = stmt.column_double(2);

            if !self.is_fts_doc_authorized(collection, &doc_id, &snippet) {
                continue;
            }

            let mut matched_terms = Vec::new();
            let mut min_dist = u8::MAX;

            for (term, &dist) in &term_distances {
                if snippet.to_lowercase().contains(&term.to_lowercase()) {
                    matched_terms.push(term.clone());
                    min_dist = min_dist.min(dist);
                }
            }

            let effective_doc_dist = if min_dist == u8::MAX { 0 } else { min_dist };

            let adjusted_rank = if base_rank < 0.0 {
                base_rank / (1.0 + f64::from(effective_doc_dist) * 0.5)
            } else {
                base_rank * (1.0 + f64::from(effective_doc_dist) * 0.5)
            };

            hits.push(FtsFuzzyHit {
                collection: collection.to_owned(),
                doc_id,
                snippet,
                rank: adjusted_rank,
                edit_distance: effective_doc_dist,
                matched_terms,
            });
        }

        hits.sort_by(|a, b| {
            a.edit_distance.cmp(&b.edit_distance).then_with(|| {
                a.rank
                    .partial_cmp(&b.rank)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        Ok(hits)
    }

    /// Performs phonetic full-text search using Double Metaphone and Soundex candidate expansion.
    ///
    /// Finds documents where words phonetically match the query (e.g., "Gérard" finds "Guerard",
    /// "Smith" finds "Smyth", "Pfizer" finds "Fizer").
    #[allow(clippy::too_many_lines)]
    pub fn fts_phonetic_search(
        &self,
        collection: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<lilia_core::FtsPhoneticHit>> {
        validate_name("collection", collection)?;
        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let vocab_arc = self.get_or_load_fts_phonetic(collection)?;
        let vocab = vocab_arc.read().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "fts phonetic lock poisoned", false)
        })?;

        // Extract words from query
        let query_words: Vec<String> = query
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect();

        if query_words.is_empty() {
            return Ok(Vec::new());
        }

        let mut expanded_clauses = Vec::new();
        let mut term_similarities: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        let mut phonetic_codes = Vec::new();

        for word in &query_words {
            let dm = lilia_core::double_metaphone(word);
            let sx = lilia_core::soundex(word);
            if !dm.primary.is_empty() && !phonetic_codes.contains(&dm.primary) {
                phonetic_codes.push(dm.primary.clone());
            }
            if let Some(ref alt) = dm.alternate {
                if !alt.is_empty() && !phonetic_codes.contains(alt) {
                    phonetic_codes.push(alt.clone());
                }
            }
            if !sx.is_empty() && !phonetic_codes.contains(&sx) {
                phonetic_codes.push(sx.clone());
            }

            let candidates = vocab.find_candidates(word, 5);
            let mut or_parts = Vec::new();
            or_parts.push(format!("\"{}\"", word.replace('"', "\"\"")));
            term_similarities.insert(word.clone(), 1.0);

            for cand in candidates {
                term_similarities
                    .entry(cand.term.clone())
                    .and_modify(|s| *s = (*s).max(cand.similarity))
                    .or_insert(cand.similarity);
                or_parts.push(format!("\"{}\"", cand.term.replace('"', "\"\"")));
            }

            if or_parts.len() == 1 {
                expanded_clauses.push(or_parts.remove(0));
            } else {
                expanded_clauses.push(format!("({})", or_parts.join(" OR ")));
            }
        }

        let match_query = expanded_clauses.join(" AND ");

        let sql = if tier == StorageTier::Memory {
            "SELECT doc_id, snippet(_lilia_mem_fts, 2, '<b>', '</b>', '...', 32), rank \
             FROM _lilia_mem._lilia_mem_fts \
             WHERE collection = ?1 AND _lilia_mem_fts MATCH ?2 \
             ORDER BY rank \
             LIMIT ?3;"
        } else {
            "SELECT doc_id, snippet(_lilia_fts, 2, '<b>', '</b>', '...', 32), rank \
             FROM _lilia_fts \
             WHERE collection = ?1 AND _lilia_fts MATCH ?2 \
             ORDER BY rank \
             LIMIT ?3;"
        };

        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(sql)?;
        stmt.bind_text_static(1, collection)?;
        stmt.bind_text_static(2, &match_query)?;
        stmt.bind_int64(3, normalize_limit(limit))?;

        let mut hits = Vec::new();
        while stmt.step()? {
            let doc_id = stmt.column_text(0).to_string();
            let snippet = stmt.column_text(1).to_string();
            let base_rank = stmt.column_double(2);

            if !self.is_fts_doc_authorized(collection, &doc_id, &snippet) {
                continue;
            }

            let snippet_lower = snippet.to_lowercase();
            let mut matched_terms = Vec::new();
            let mut max_sim: f64 = 0.5;

            for (term, &sim) in &term_similarities {
                if snippet_lower.contains(&term.to_lowercase()) {
                    matched_terms.push(term.clone());
                    max_sim = max_sim.max(sim);
                }
            }

            let adjusted_rank = if base_rank < 0.0 {
                base_rank * max_sim
            } else {
                base_rank / max_sim.max(0.1)
            };

            hits.push(lilia_core::FtsPhoneticHit {
                collection: collection.to_owned(),
                doc_id,
                snippet,
                rank: adjusted_rank,
                matched_terms,
                phonetic_codes: phonetic_codes.clone(),
            });
        }

        hits.sort_by(|a, b| {
            a.rank
                .partial_cmp(&b.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(hits)
    }

    /// Loads custom dictionary terms into a collection's vocabulary index and in-memory trie.
    pub fn fts_load_dictionary(&self, collection: &str, words: &[&str]) -> Result<()> {
        validate_name("collection", collection)?;
        let tier = self.catalog.tier_for(CollectionKind::Fts, collection);
        let in_memory_db = tier == StorageTier::Memory;

        let sql = if in_memory_db {
            "INSERT INTO _lilia_mem._lilia_mem_fts_terms(collection, term, doc_count) \
             VALUES(?1, ?2, 1) \
             ON CONFLICT(collection, term) DO UPDATE SET doc_count = doc_count + 1;"
        } else {
            "INSERT INTO _lilia_fts_terms(collection, term, doc_count) \
             VALUES(?1, ?2, 1) \
             ON CONFLICT(collection, term) DO UPDATE SET doc_count = doc_count + 1;"
        };

        let mut writer = self.lock_writer()?;
        let tx = writer.transaction()?;
        {
            let mut stmt = tx.conn().prepare(sql)?;
            for &word in words {
                let term = word.trim().to_lowercase();
                if !term.is_empty() {
                    stmt.reset()?;
                    stmt.bind_text_static(1, collection)?;
                    stmt.bind_text_static(2, &term)?;
                    stmt.step()?;
                }
            }
        }
        tx.commit()?;

        if let Some(trie_arc) = self
            .fts_trie_cache
            .read()
            .ok()
            .and_then(|c| c.get(collection).cloned())
        {
            if let Ok(mut trie) = trie_arc.write() {
                for &word in words {
                    let term = word.trim().to_lowercase();
                    if !term.is_empty() {
                        trie.insert(&term, 1);
                    }
                }
            }
        }

        if let Some(vocab_arc) = self
            .fts_phonetic_cache
            .read()
            .ok()
            .and_then(|c| c.get(collection).cloned())
        {
            if let Ok(mut vocab) = vocab_arc.write() {
                for &word in words {
                    let term = word.trim().to_lowercase();
                    if !term.is_empty() {
                        vocab.insert(&term, 1);
                    }
                }
            }
        }

        Ok(())
    }
}
