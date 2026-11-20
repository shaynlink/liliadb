#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_wrap,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]

use lilia_core::{
    BatchOperation, BitemporalEntry, BitemporalInterval, BitemporalPoint, ErrorCode, LiliaError,
    Result, ValueCodec, BITEMPORAL_INFINITY,
};
use lilia_sqlite_native::NativeTransaction;
use serde_json::json;
use std::sync::Arc;

use crate::json::decode_json_value;
use crate::storage::{now_ms, parse_version, store_version, validate_name};
use crate::Database;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct StoredSlice {
    pub vt_from: i64,
    pub vt_to: i64,
    pub tt_from: i64,
    pub tt_to: i64,
    pub value_encoded: String,
    pub value: serde_json::Value,
    pub version: u64,
}

pub(crate) fn apply_bitemporal_put(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    value: &serde_json::Value,
    vt_from: i64,
    vt_to: i64,
    system_time: Option<i64>,
    codec: &dyn ValueCodec,
) -> Result<u64> {
    validate_name("space", space)?;
    validate_name("id", id)?;

    if vt_from >= vt_to {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("vt_from ({vt_from}) must be strictly less than vt_to ({vt_to})"),
            false,
        ));
    }

    let now = system_time.unwrap_or_else(now_ms);

    // Encode value
    let encoded = if codec.name() == "identity" {
        serde_json::to_string(value)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?
    } else {
        use base64::Engine;
        let raw_bytes = serde_json::to_vec(value)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
        let cipher_bytes = codec.encode(&raw_bytes)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&cipher_bytes);
        serde_json::to_string(&json!({
            "$codec": codec.name(),
            "$data": b64,
        }))
        .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?
    };

    // Calculate next logical version for this (space, id)
    let next_version = {
        let mut stmt = tx.prepare(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM _lilia_bitemporal WHERE space = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        if stmt.step()? {
            parse_version(stmt.column_int64(0))?
        } else {
            1
        }
    };

    // 1. Fetch all currently active slices in TT (tt_from <= now < tt_to) that overlap [vt_from, vt_to)
    let mut stmt = tx.prepare(
        "SELECT vt_from, vt_to, tt_from, tt_to, value, version \
         FROM _lilia_bitemporal \
         WHERE space = ?1 AND id = ?2 \
           AND tt_from <= ?3 AND ?3 < tt_to \
           AND vt_from < ?5 AND ?4 < vt_to \
         ORDER BY vt_from ASC;",
    )?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    stmt.bind_int64(3, now)?;
    stmt.bind_int64(4, vt_from)?;
    stmt.bind_int64(5, vt_to)?;

    let mut overlapping = Vec::new();
    while stmt.step()? {
        let v_from = stmt.column_int64(0);
        let v_to = stmt.column_int64(1);
        let t_from = stmt.column_int64(2);
        let t_to = stmt.column_int64(3);
        let enc = stmt.column_text(4).to_string();
        let ver = parse_version(stmt.column_int64(5))?;
        let dec_val = decode_json_value(&enc, codec).unwrap_or(serde_json::Value::Null);
        overlapping.push(StoredSlice {
            vt_from: v_from,
            vt_to: v_to,
            tt_from: t_from,
            tt_to: t_to,
            value_encoded: enc,
            value: dec_val,
            version: ver,
        });
    }
    drop(stmt);

    // 2. Close active overlapping slices in Transaction Time
    let mut close_stmt = tx.prepare(
        "UPDATE _lilia_bitemporal \
         SET tt_to = ?3 \
         WHERE space = ?1 AND id = ?2 AND vt_from = ?4 AND tt_from = ?5;",
    )?;
    for slice in &overlapping {
        close_stmt.bind_text_static(1, space)?;
        close_stmt.bind_text_static(2, id)?;
        close_stmt.bind_int64(3, now)?;
        close_stmt.bind_int64(4, slice.vt_from)?;
        close_stmt.bind_int64(5, slice.tt_from)?;
        close_stmt.step()?;
        close_stmt.reset()?;
    }
    drop(close_stmt);

    // 3. Prepare replacement slices with temporal interval slicing
    // We will collect slices to insert: (vt_start, vt_end, encoded_value, parsed_value)
    let mut slices_to_insert: Vec<(i64, i64, String, serde_json::Value)> = Vec::new();

    // Check if the earliest overlapping slice extends before vt_from (prefix slice)
    if let Some(first) = overlapping.first() {
        if first.vt_from < vt_from {
            slices_to_insert.push((
                first.vt_from,
                vt_from,
                first.value_encoded.clone(),
                first.value.clone(),
            ));
        }
    }

    // New slice
    slices_to_insert.push((vt_from, vt_to, encoded, value.clone()));

    // Check if the latest overlapping slice extends after vt_to (suffix slice)
    if let Some(last) = overlapping.last() {
        if last.vt_to > vt_to {
            slices_to_insert.push((
                vt_to,
                last.vt_to,
                last.value_encoded.clone(),
                last.value.clone(),
            ));
        }
    }

    // 4. Temporal Coalescing:
    // Check left neighbor right before the first slice to insert
    let min_vt = slices_to_insert.first().map(|s| s.0).unwrap_or(vt_from);
    let mut left_neighbor_opt = None;
    {
        let mut left_stmt = tx.prepare(
            "SELECT vt_from, vt_to, tt_from, value, version \
             FROM _lilia_bitemporal \
             WHERE space = ?1 AND id = ?2 \
               AND tt_from <= ?3 AND ?3 < tt_to \
               AND vt_to = ?4 \
             LIMIT 1;",
        )?;
        left_stmt.bind_text_static(1, space)?;
        left_stmt.bind_text_static(2, id)?;
        left_stmt.bind_int64(3, now)?;
        left_stmt.bind_int64(4, min_vt)?;
        if left_stmt.step()? {
            let lf_from = left_stmt.column_int64(0);
            let lf_to = left_stmt.column_int64(1);
            let lf_tt_from = left_stmt.column_int64(2);
            let enc = left_stmt.column_text(3).to_string();
            let dec_val = decode_json_value(&enc, codec).unwrap_or(serde_json::Value::Null);
            left_neighbor_opt = Some((lf_from, lf_to, lf_tt_from, enc, dec_val));
        }
    }

    // Check right neighbor right after the last slice to insert
    let max_vt = slices_to_insert.last().map(|s| s.1).unwrap_or(vt_to);
    let mut right_neighbor_opt = None;
    {
        let mut right_stmt = tx.prepare(
            "SELECT vt_from, vt_to, tt_from, value, version \
             FROM _lilia_bitemporal \
             WHERE space = ?1 AND id = ?2 \
               AND tt_from <= ?3 AND ?3 < tt_to \
               AND vt_from = ?4 \
             LIMIT 1;",
        )?;
        right_stmt.bind_text_static(1, space)?;
        right_stmt.bind_text_static(2, id)?;
        right_stmt.bind_int64(3, now)?;
        right_stmt.bind_int64(4, max_vt)?;
        if right_stmt.step()? {
            let rt_from = right_stmt.column_int64(0);
            let rt_to = right_stmt.column_int64(1);
            let rt_tt_from = right_stmt.column_int64(2);
            let enc = right_stmt.column_text(3).to_string();
            let dec_val = decode_json_value(&enc, codec).unwrap_or(serde_json::Value::Null);
            right_neighbor_opt = Some((rt_from, rt_to, rt_tt_from, enc, dec_val));
        }
    }

    // Coalesce with left neighbor if values match
    if let Some((lf_from, lf_to, lf_tt_from, _, lf_val)) = left_neighbor_opt {
        if let Some(first_insert) = slices_to_insert.first_mut() {
            if first_insert.0 == lf_to && first_insert.3 == lf_val {
                // Close left neighbor
                let mut close_lf = tx.prepare(
                    "UPDATE _lilia_bitemporal SET tt_to = ?3 WHERE space = ?1 AND id = ?2 AND vt_from = ?4 AND tt_from = ?5;",
                )?;
                close_lf.bind_text_static(1, space)?;
                close_lf.bind_text_static(2, id)?;
                close_lf.bind_int64(3, now)?;
                close_lf.bind_int64(4, lf_from)?;
                close_lf.bind_int64(5, lf_tt_from)?;
                close_lf.step()?;
                // Extend first slice
                first_insert.0 = lf_from;
            }
        }
    }

    // Coalesce with right neighbor if values match
    if let Some((rt_from, rt_to, rt_tt_from, _, rt_val)) = right_neighbor_opt {
        if let Some(last_insert) = slices_to_insert.last_mut() {
            if last_insert.1 == rt_from && last_insert.3 == rt_val {
                // Close right neighbor
                let mut close_rt = tx.prepare(
                    "UPDATE _lilia_bitemporal SET tt_to = ?3 WHERE space = ?1 AND id = ?2 AND vt_from = ?4 AND tt_from = ?5;",
                )?;
                close_rt.bind_text_static(1, space)?;
                close_rt.bind_text_static(2, id)?;
                close_rt.bind_int64(3, now)?;
                close_rt.bind_int64(4, rt_from)?;
                close_rt.bind_int64(5, rt_tt_from)?;
                close_rt.step()?;
                // Extend last slice
                last_insert.1 = rt_to;
            }
        }
    }

    // Internal coalescence within slices_to_insert if multiple adjacent slices have same value
    let mut coalesced: Vec<(i64, i64, String, serde_json::Value)> = Vec::new();
    for slice in slices_to_insert {
        if let Some(prev) = coalesced.last_mut() {
            if prev.1 == slice.0 && prev.3 == slice.3 {
                prev.1 = slice.1;
                continue;
            }
        }
        coalesced.push(slice);
    }

    // 5. Insert coalesced slices into _lilia_bitemporal
    let mut ins_stmt = tx.prepare(
        "INSERT INTO _lilia_bitemporal(space, id, vt_from, vt_to, tt_from, tt_to, value, version) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
    )?;
    for (v_s, v_e, enc_val, _) in coalesced {
        ins_stmt.bind_text_static(1, space)?;
        ins_stmt.bind_text_static(2, id)?;
        ins_stmt.bind_int64(3, v_s)?;
        ins_stmt.bind_int64(4, v_e)?;
        ins_stmt.bind_int64(5, now)?;
        ins_stmt.bind_int64(6, BITEMPORAL_INFINITY)?;
        ins_stmt.bind_text_static(7, &enc_val)?;
        ins_stmt.bind_int64(8, store_version(next_version)?)?;
        ins_stmt.step()?;
        ins_stmt.reset()?;
    }

    Ok(next_version)
}

pub(crate) fn apply_bitemporal_delete(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    vt_from: Option<i64>,
    vt_to: Option<i64>,
    system_time: Option<i64>,
    _codec: &dyn ValueCodec,
) -> Result<bool> {
    validate_name("space", space)?;
    validate_name("id", id)?;

    let now = system_time.unwrap_or_else(now_ms);

    match (vt_from, vt_to) {
        (None, None) => {
            // Full deletion of all active slices for (space, id)
            let mut close_all = tx.prepare(
                "UPDATE _lilia_bitemporal \
                 SET tt_to = ?3 \
                 WHERE space = ?1 AND id = ?2 \
                   AND tt_from <= ?3 AND ?3 < tt_to;",
            )?;
            close_all.bind_text_static(1, space)?;
            close_all.bind_text_static(2, id)?;
            close_all.bind_int64(3, now)?;
            close_all.step()?;
            Ok(tx.conn().changes() > 0)
        }
        (Some(del_from), Some(del_to)) => {
            if del_from >= del_to {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("vt_from ({del_from}) must be strictly less than vt_to ({del_to})"),
                    false,
                ));
            }

            // Find overlapping active slices
            let mut stmt = tx.prepare(
                "SELECT vt_from, vt_to, tt_from, tt_to, value, version \
                 FROM _lilia_bitemporal \
                 WHERE space = ?1 AND id = ?2 \
                   AND tt_from <= ?3 AND ?3 < tt_to \
                   AND vt_from < ?5 AND ?4 < vt_to \
                 ORDER BY vt_from ASC;",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_int64(3, now)?;
            stmt.bind_int64(4, del_from)?;
            stmt.bind_int64(5, del_to)?;

            let mut overlapping = Vec::new();
            while stmt.step()? {
                let v_from = stmt.column_int64(0);
                let v_to = stmt.column_int64(1);
                let t_from = stmt.column_int64(2);
                let t_to = stmt.column_int64(3);
                let enc = stmt.column_text(4).to_string();
                let ver = parse_version(stmt.column_int64(5))?;
                overlapping.push(StoredSlice {
                    vt_from: v_from,
                    vt_to: v_to,
                    tt_from: t_from,
                    tt_to: t_to,
                    value_encoded: enc,
                    value: serde_json::Value::Null,
                    version: ver,
                });
            }
            drop(stmt);

            if overlapping.is_empty() {
                return Ok(false);
            }

            // Close overlapping active slices
            let mut close_stmt = tx.prepare(
                "UPDATE _lilia_bitemporal \
                 SET tt_to = ?3 \
                 WHERE space = ?1 AND id = ?2 AND vt_from = ?4 AND tt_from = ?5;",
            )?;
            for slice in &overlapping {
                close_stmt.bind_text_static(1, space)?;
                close_stmt.bind_text_static(2, id)?;
                close_stmt.bind_int64(3, now)?;
                close_stmt.bind_int64(4, slice.vt_from)?;
                close_stmt.bind_int64(5, slice.tt_from)?;
                close_stmt.step()?;
                close_stmt.reset()?;
            }
            drop(close_stmt);

            // Re-insert prefix / suffix slices outside the deletion window
            let next_version = {
                let mut stmt = tx.prepare(
                    "SELECT COALESCE(MAX(version), 0) + 1 FROM _lilia_bitemporal WHERE space = ?1 AND id = ?2;",
                )?;
                stmt.bind_text_static(1, space)?;
                stmt.bind_text_static(2, id)?;
                if stmt.step()? {
                    parse_version(stmt.column_int64(0))?
                } else {
                    1
                }
            };

            let mut ins_stmt = tx.prepare(
                "INSERT INTO _lilia_bitemporal(space, id, vt_from, vt_to, tt_from, tt_to, value, version) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
            )?;

            // Prefix
            if let Some(first) = overlapping.first() {
                if first.vt_from < del_from {
                    ins_stmt.bind_text_static(1, space)?;
                    ins_stmt.bind_text_static(2, id)?;
                    ins_stmt.bind_int64(3, first.vt_from)?;
                    ins_stmt.bind_int64(4, del_from)?;
                    ins_stmt.bind_int64(5, now)?;
                    ins_stmt.bind_int64(6, BITEMPORAL_INFINITY)?;
                    ins_stmt.bind_text_static(7, &first.value_encoded)?;
                    ins_stmt.bind_int64(8, store_version(next_version)?)?;
                    ins_stmt.step()?;
                    ins_stmt.reset()?;
                }
            }

            // Suffix
            if let Some(last) = overlapping.last() {
                if last.vt_to > del_to {
                    ins_stmt.bind_text_static(1, space)?;
                    ins_stmt.bind_text_static(2, id)?;
                    ins_stmt.bind_int64(3, del_to)?;
                    ins_stmt.bind_int64(4, last.vt_to)?;
                    ins_stmt.bind_int64(5, now)?;
                    ins_stmt.bind_int64(6, BITEMPORAL_INFINITY)?;
                    ins_stmt.bind_text_static(7, &last.value_encoded)?;
                    ins_stmt.bind_int64(8, store_version(next_version)?)?;
                    ins_stmt.step()?;
                    ins_stmt.reset()?;
                }
            }

            Ok(true)
        }
        _ => Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "both vt_from and vt_to must be provided, or both omitted for full deletion",
            false,
        )),
    }
}

/// Fluent interface for bitemporal operations and 2D temporal queries.
#[derive(Debug, Clone, Copy)]
pub struct BitemporalStore<'a> {
    db: &'a Database,
}

impl<'a> BitemporalStore<'a> {
    pub(crate) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert or update a fact with a specific Valid Time interval `[vt_from, vt_to)`.
    /// Automatically manages Transaction Time (`[now, INFINITY)`) and slices/coalesces existing periods.
    pub fn put(
        &self,
        space: &str,
        id: &str,
        value: serde_json::Value,
        interval: BitemporalInterval,
    ) -> Result<u64> {
        self.put_with_system_time(space, id, value, interval, None)
    }

    /// Insert or update with an explicit system time override (for audit backfill or tests).
    pub fn put_with_system_time(
        &self,
        space: &str,
        id: &str,
        value: serde_json::Value,
        interval: BitemporalInterval,
        system_time: Option<i64>,
    ) -> Result<u64> {
        let results = self.db.batch(&[BatchOperation::BitemporalPut {
            space: space.to_owned(),
            id: id.to_owned(),
            value,
            vt_from: interval.from,
            vt_to: interval.to,
            system_time,
        }])?;
        Ok(results.first().and_then(|r| r.version).unwrap_or(1))
    }

    /// Query the exact state of an entity at a 2D bitemporal point `(as_of_tt, for_vt)`.
    pub fn get(
        &self,
        space: &str,
        id: &str,
        as_of_tt: impl Into<BitemporalPoint>,
        for_vt: impl Into<BitemporalPoint>,
    ) -> Result<Option<BitemporalEntry>> {
        validate_name("space", space)?;
        validate_name("id", id)?;

        let tt = as_of_tt.into().as_ms();
        let vt = for_vt.into().as_ms();

        // Fast path: L1 Cache Hit (0.00 ms)
        if let Some(cached) = self.db.cache.get_bitemporal(space, id, tt, vt) {
            if let Some(ref prefetcher) = self.db.prefetcher {
                prefetcher.on_access(lilia_core::AccessTarget::bitemporal(space, id));
            }
            return Ok(Some((*cached).clone()));
        }

        let conn = self.db.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT vt_from, vt_to, tt_from, tt_to, value, version \
             FROM _lilia_bitemporal \
             WHERE space = ?1 AND id = ?2 \
               AND tt_from <= ?3 AND ?3 < tt_to \
               AND vt_from <= ?4 AND ?4 < vt_to \
             LIMIT 1;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        stmt.bind_int64(3, tt)?;
        stmt.bind_int64(4, vt)?;

        let entry_opt = if stmt.step()? {
            let v_from = stmt.column_int64(0);
            let v_to = stmt.column_int64(1);
            let t_from = stmt.column_int64(2);
            let t_to = stmt.column_int64(3);
            let enc = stmt.column_text(4);
            let ver = parse_version(stmt.column_int64(5))?;
            let val = decode_json_value(enc, self.db.codec.as_ref())?;

            let entry = Arc::new(BitemporalEntry {
                space: space.to_string(),
                id: id.to_string(),
                vt_from: v_from,
                vt_to: v_to,
                tt_from: t_from,
                tt_to: t_to,
                value: val,
                version: ver,
            });
            self.db
                .cache
                .put_bitemporal(space, id, tt, vt, Arc::clone(&entry));
            Some((*entry).clone())
        } else {
            None
        };

        if let Some(ref prefetcher) = self.db.prefetcher {
            prefetcher.on_access(lilia_core::AccessTarget::bitemporal(space, id));
        }

        Ok(entry_opt)
    }

    /// Retrieve the complete non-overlapping timeline of an entity across Valid Time,
    /// exactly as known by the system at `as_of_tt`.
    pub fn timeline(
        &self,
        space: &str,
        id: &str,
        as_of_tt: impl Into<BitemporalPoint>,
    ) -> Result<Vec<BitemporalEntry>> {
        validate_name("space", space)?;
        validate_name("id", id)?;

        let tt = as_of_tt.into().as_ms();

        let conn = self.db.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT vt_from, vt_to, tt_from, tt_to, value, version \
             FROM _lilia_bitemporal \
             WHERE space = ?1 AND id = ?2 \
               AND tt_from <= ?3 AND ?3 < tt_to \
             ORDER BY vt_from ASC;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        stmt.bind_int64(3, tt)?;

        let mut timeline = Vec::new();
        while stmt.step()? {
            let v_from = stmt.column_int64(0);
            let v_to = stmt.column_int64(1);
            let t_from = stmt.column_int64(2);
            let t_to = stmt.column_int64(3);
            let enc = stmt.column_text(4);
            let ver = parse_version(stmt.column_int64(5))?;
            let val = decode_json_value(enc, self.db.codec.as_ref())?;

            timeline.push(BitemporalEntry {
                space: space.to_string(),
                id: id.to_string(),
                vt_from: v_from,
                vt_to: v_to,
                tt_from: t_from,
                tt_to: t_to,
                value: val,
                version: ver,
            });
        }

        Ok(timeline)
    }

    /// Retrieve the full 2D bitemporal audit trail of an entity across all Transaction Time
    /// and Valid Time revisions.
    pub fn history(&self, space: &str, id: &str) -> Result<Vec<BitemporalEntry>> {
        validate_name("space", space)?;
        validate_name("id", id)?;

        let conn = self.db.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT vt_from, vt_to, tt_from, tt_to, value, version \
             FROM _lilia_bitemporal \
             WHERE space = ?1 AND id = ?2 \
             ORDER BY tt_from ASC, vt_from ASC;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;

        let mut hist = Vec::new();
        while stmt.step()? {
            let v_from = stmt.column_int64(0);
            let v_to = stmt.column_int64(1);
            let t_from = stmt.column_int64(2);
            let t_to = stmt.column_int64(3);
            let enc = stmt.column_text(4);
            let ver = parse_version(stmt.column_int64(5))?;
            let val = decode_json_value(enc, self.db.codec.as_ref())?;

            hist.push(BitemporalEntry {
                space: space.to_string(),
                id: id.to_string(),
                vt_from: v_from,
                vt_to: v_to,
                tt_from: t_from,
                tt_to: t_to,
                value: val,
                version: ver,
            });
        }

        Ok(hist)
    }

    /// Scan entities in a space as of a 2D bitemporal point.
    pub fn scan(
        &self,
        space: &str,
        as_of_tt: impl Into<BitemporalPoint>,
        for_vt: impl Into<BitemporalPoint>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<BitemporalEntry>> {
        validate_name("space", space)?;
        let tt = as_of_tt.into().as_ms();
        let vt = for_vt.into().as_ms();
        let safe_limit = limit.min(100_000);

        let conn = self.db.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, vt_from, vt_to, tt_from, tt_to, value, version \
             FROM _lilia_bitemporal \
             WHERE space = ?1 \
               AND tt_from <= ?2 AND ?2 < tt_to \
               AND vt_from <= ?3 AND ?3 < vt_to \
               AND (?4 IS NULL OR id > ?4) \
             ORDER BY id ASC \
             LIMIT ?5;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_int64(2, tt)?;
        stmt.bind_int64(3, vt)?;
        match after_id {
            Some(a) => stmt.bind_text_static(4, a)?,
            None => stmt.bind_null(4)?,
        }
        stmt.bind_int64(5, i64::from(safe_limit))?;

        let mut results = Vec::new();
        while stmt.step()? {
            let id = stmt.column_text(0).to_string();
            let v_from = stmt.column_int64(1);
            let v_to = stmt.column_int64(2);
            let t_from = stmt.column_int64(3);
            let t_to = stmt.column_int64(4);
            let enc = stmt.column_text(5);
            let ver = parse_version(stmt.column_int64(6))?;
            let val = decode_json_value(enc, self.db.codec.as_ref())?;

            results.push(BitemporalEntry {
                space: space.to_string(),
                id,
                vt_from: v_from,
                vt_to: v_to,
                tt_from: t_from,
                tt_to: t_to,
                value: val,
                version: ver,
            });
        }

        Ok(results)
    }

    /// Retract/delete an entity over a specific Valid Time interval, or delete entirely.
    pub fn delete(
        &self,
        space: &str,
        id: &str,
        interval: Option<BitemporalInterval>,
    ) -> Result<bool> {
        let (vt_from, vt_to) = match interval {
            Some(iv) => (Some(iv.from), Some(iv.to)),
            None => (None, None),
        };
        let results = self.db.batch(&[BatchOperation::BitemporalDelete {
            space: space.to_owned(),
            id: id.to_owned(),
            vt_from,
            vt_to,
            system_time: None,
        }])?;
        Ok(results.first().is_some_and(|r| r.deleted))
    }
}

impl Database {
    /// Fluent bitemporal interface for 2D temporal data modeling, queries, and timelines.
    #[must_use]
    pub fn bitemporal(&self) -> BitemporalStore<'_> {
        BitemporalStore::new(self)
    }
}
