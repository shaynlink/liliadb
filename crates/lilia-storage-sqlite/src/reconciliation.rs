//! P2P Decentralized Set Reconciliation Engine via Invertible Bloom Filters (IBF).
//!
//! Enables two independent offline database replicas (e.g. over Bluetooth Low Energy,
//! Wi-Fi Direct, or ad-hoc mesh networks) to reconcile missing and modified records
//! with minimal bandwidth without transmitting their full history or complete ID sets.

#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::type_complexity
)]

use std::collections::HashMap;

pub type P2pItemMap = HashMap<u64, (String, String)>;

pub use lilia_core::ibf::{
    InvertibleBloomFilter, P2pApplyStats, P2pDocumentPayload, ReconciliationItem,
    ReconciliationSummary, StrataEstimator, DEFAULT_IBF_SEED, DEFAULT_NUM_HASHES,
};
use lilia_core::{ConflictResolutionStrategy, ErrorCode, Hlc, LiliaError, Result};

use crate::Database;

impl Database {
    /// Scans documents in a space (or all spaces) and collects composite 128-bit items
    /// alongside a fast in-memory reverse index `id_hash -> (space, id)`.
    pub fn p2p_collect_items(
        &self,
        space: Option<&str>,
    ) -> Result<(Vec<ReconciliationItem>, HashMap<u64, (String, String)>)> {
        let conn = self.read_lock()?;
        let sql = "SELECT COALESCE(j.space, h.space), COALESCE(j.id, h.id), \
                   COALESCE(h.hlc, printf('%d', j.version)), \
                   CASE WHEN j.id IS NULL THEN 1 ELSE 0 END AS is_deleted \
                   FROM _lilia_record_hlc h \
                   LEFT JOIN _lilia_json j ON h.model = 'json' AND h.space = j.space AND h.id = j.id \
                   WHERE (?1 IS NULL OR h.space = ?1) \
                   UNION \
                   SELECT j.space, j.id, printf('%d', j.version), 0 AS is_deleted \
                   FROM _lilia_json j \
                   LEFT JOIN _lilia_record_hlc h ON h.model = 'json' AND h.space = j.space AND h.id = j.id \
                   WHERE h.id IS NULL AND (?1 IS NULL OR j.space = ?1);";

        let mut stmt = conn.prepare(sql)?;
        if let Some(sp) = space {
            stmt.bind_text_static(1, sp)?;
        } else {
            stmt.bind_null(1)?;
        }

        let mut items = Vec::new();
        let mut id_map = HashMap::new();

        while stmt.step()? {
            let sp = stmt.column_text(0);
            let id = stmt.column_text(1);
            let state_repr = stmt.column_text(2);
            let is_deleted = stmt.column_int64(3) != 0;

            let state_bytes = if is_deleted {
                format!("{state_repr}:tombstone").into_bytes()
            } else {
                state_repr.as_bytes().to_vec()
            };

            let item = ReconciliationItem::from_parts("json", sp, id, &state_bytes);
            id_map.insert(item.id_hash, (sp.to_string(), id.to_string()));
            items.push(item);
        }

        Ok((items, id_map))
    }

    /// Builds a Strata Estimator for difference estimation over a space (or global database).
    pub fn p2p_strata(&self, space: Option<&str>) -> Result<StrataEstimator> {
        let (items, _) = self.p2p_collect_items(space)?;
        let mut strata = StrataEstimator::with_default_params(DEFAULT_IBF_SEED);
        for item in items {
            strata.insert(item.to_u128());
        }
        Ok(strata)
    }

    /// Builds an Invertible Bloom Filter with a specified bucket capacity.
    pub fn p2p_ibf(&self, space: Option<&str>, capacity: usize) -> Result<InvertibleBloomFilter> {
        let (items, _) = self.p2p_collect_items(space)?;
        let mut ibf = InvertibleBloomFilter::new(capacity, DEFAULT_NUM_HASHES, DEFAULT_IBF_SEED)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        for item in items {
            ibf.insert(item.to_u128());
        }
        Ok(ibf)
    }

    /// Exports full document payloads for a set of decoded element 128-bit keys.
    pub fn p2p_export_payloads(
        &self,
        elements: &[u128],
        id_map: &HashMap<u64, (String, String)>,
    ) -> Result<Vec<P2pDocumentPayload>> {
        let mut payloads = Vec::with_capacity(elements.len());

        for &elem in elements {
            let item = ReconciliationItem::from_u128(elem);
            if let Some((space, id)) = id_map.get(&item.id_hash) {
                let entry = self.json_get(space, id)?;
                let hlc_record = self.json_get_hlc(space, id)?;

                let (hlc, parent_hlc) = if let Some(rec) = hlc_record {
                    (rec.hlc, rec.parent_hlc)
                } else {
                    let version = entry.as_ref().map_or(1, |e| e.version);
                    (
                        Hlc::new(0, u32::try_from(version).unwrap_or(1), self.hlc.node_id()),
                        None,
                    )
                };

                let value = entry.map(|e| e.to_json_value());
                payloads.push(P2pDocumentPayload {
                    model: "json".to_string(),
                    space: space.clone(),
                    id: id.clone(),
                    value,
                    hlc,
                    parent_hlc,
                });
            }
        }

        Ok(payloads)
    }

    /// Applies a batch of incoming P2P document changesets using deterministic HLC conflict resolution.
    pub fn p2p_apply_changesets(
        &self,
        payloads: &[P2pDocumentPayload],
        strategy: ConflictResolutionStrategy,
    ) -> Result<P2pApplyStats> {
        let mut stats = P2pApplyStats::default();

        for payload in payloads {
            if let Some(ref val) = payload.value {
                let outcome = self.json_put_hlc(
                    &payload.space,
                    &payload.id,
                    val.clone(),
                    Some(payload.hlc.clone()),
                    payload.parent_hlc.clone(),
                    strategy,
                )?;
                stats.applied_count += 1;
                if outcome.conflict_detected {
                    stats.conflicts_detected += 1;
                    stats.conflicts_resolved += 1;
                }
            } else {
                // Tombstone / Deletion
                self.batch(&[lilia_core::BatchOperation::JsonDelete {
                    space: payload.space.clone(),
                    id: payload.id.clone(),
                    if_version: None,
                }])?;
                stats.applied_count += 1;
            }
        }

        Ok(stats)
    }

    /// Executes complete peer-to-peer reconciliation between this database (`self`) and another (`peer`).
    pub fn p2p_reconcile_with(
        &self,
        peer: &Database,
        space: Option<&str>,
        strategy: ConflictResolutionStrategy,
    ) -> Result<ReconciliationSummary> {
        // Step 1: Collect local items and build Strata Estimators
        let (items_a, id_map_a) = self.p2p_collect_items(space)?;
        let mut strata_a = StrataEstimator::with_default_params(DEFAULT_IBF_SEED);
        for it in &items_a {
            strata_a.insert(it.to_u128());
        }

        let (items_b, id_map_b) = peer.p2p_collect_items(space)?;
        let mut strata_b = StrataEstimator::with_default_params(DEFAULT_IBF_SEED);
        for it in &items_b {
            strata_b.insert(it.to_u128());
        }

        let estimate = strata_a
            .estimate_difference(&strata_b)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        // Fast-Path (1-RTT): All strata decoded directly (d <= 40)
        if estimate.fully_decoded {
            let diff = estimate.direct_diff.unwrap_or_default();
            let payloads_a = self.p2p_export_payloads(&diff.a_minus_b, &id_map_a)?;
            let payloads_b = peer.p2p_export_payloads(&diff.b_minus_a, &id_map_b)?;

            let stats_b = peer.p2p_apply_changesets(&payloads_a, strategy)?;
            let stats_a = self.p2p_apply_changesets(&payloads_b, strategy)?;

            return Ok(ReconciliationSummary {
                rounds_used: 1,
                fast_path: true,
                estimated_diff: estimate.estimated_diff,
                items_sent_a_to_b: payloads_a.len(),
                items_sent_b_to_a: payloads_b.len(),
                stats_a,
                stats_b,
            });
        }

        // Normal-Path (2-RTT): Strata Estimator provided estimate, now size optimal IBF with adaptive retry
        let mut capacity = InvertibleBloomFilter::with_expected_diff(estimate.estimated_diff)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?
            .len();

        let mut diff_opt = None;
        for _ in 0..3 {
            let mut ibf_a =
                InvertibleBloomFilter::new(capacity, DEFAULT_NUM_HASHES, DEFAULT_IBF_SEED)
                    .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            let mut ibf_b =
                InvertibleBloomFilter::new(capacity, DEFAULT_NUM_HASHES, DEFAULT_IBF_SEED)
                    .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

            for it in &items_a {
                ibf_a.insert(it.to_u128());
            }
            for it in &items_b {
                ibf_b.insert(it.to_u128());
            }

            ibf_a
                .subtract(&ibf_b)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

            match ibf_a.decode() {
                Ok(diff) => {
                    diff_opt = Some(diff);
                    break;
                }
                Err(_) => {
                    // Adaptive backoff: increase capacity by 1.5x and retry peeling
                    capacity = (capacity * 3) / 2;
                }
            }
        }

        let diff = diff_opt.ok_or_else(|| {
            LiliaError::new(
                ErrorCode::Storage,
                "IBF decoding stalled after retries",
                false,
            )
        })?;

        let payloads_a = self.p2p_export_payloads(&diff.a_minus_b, &id_map_a)?;
        let payloads_b = peer.p2p_export_payloads(&diff.b_minus_a, &id_map_b)?;

        let stats_b = peer.p2p_apply_changesets(&payloads_a, strategy)?;
        let stats_a = self.p2p_apply_changesets(&payloads_b, strategy)?;

        Ok(ReconciliationSummary {
            rounds_used: 2,
            fast_path: false,
            estimated_diff: estimate.estimated_diff,
            items_sent_a_to_b: payloads_a.len(),
            items_sent_b_to_a: payloads_b.len(),
            stats_a,
            stats_b,
        })
    }
}
