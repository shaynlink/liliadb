pub mod backup;
pub mod connection;
pub mod cow_vfs;
pub mod ffi;
pub mod function;
pub mod logging;
pub mod session;
pub mod slots;
pub mod statement;
pub mod transaction;
pub mod vtab;

pub use backup::NativeBackup;
pub use connection::{InterruptHandle, NativeConnection};
pub use cow_vfs::{
    ensure_cow_vfs_registered, get_branch_state, register_branch_state, unregister_branch_state,
    BranchVfsState, COW_VFS_NAME,
};
pub use function::{NativeContext, NativeValue};
pub use logging::ensure_sqlite_initialized;
pub use session::{ChangeOp, ChangesetIterator, ConflictAction, NativeSession, RowChange};
pub use slots::{StatementSlots, StmtSlot};
pub use statement::{NativeStatement, StmtStatus};
pub use transaction::NativeTransaction;
pub use vtab::{
    ConstraintOp, IndexConstraint, IndexPlan, VTabValue, VirtualCursor, VirtualTableFactory,
    VirtualTableProvider,
};

pub fn sqlite_version() -> &'static str {
    unsafe {
        std::ffi::CStr::from_ptr(ffi::sqlite3_libversion())
            .to_str()
            .unwrap_or("unknown")
    }
}

pub fn sqlite_source_id() -> &'static str {
    unsafe {
        std::ffi::CStr::from_ptr(ffi::sqlite3_sourceid())
            .to_str()
            .unwrap_or("unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_version() {
        let ver = sqlite_version();
        assert_eq!(ver, "3.54.1", "must be SQLite 3.54.1 amalgamation");
    }

    #[test]
    fn test_native_connection_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let mut conn = NativeConnection::open(&db_path, "NORMAL", 1024 * 1024, 5000).unwrap();
        conn.exec(
            "CREATE TABLE _lilia_kv(
               namespace TEXT NOT NULL,
               key BLOB NOT NULL,
               value BLOB NOT NULL,
               version INTEGER NOT NULL CHECK(version > 0),
               expires_at_ms INTEGER,
               PRIMARY KEY(namespace, key)
             ) STRICT, WITHOUT ROWID;",
        )
        .unwrap();

        conn.begin_immediate().unwrap();
        {
            let stmt = conn.slot(StmtSlot::KvUpsertReturning).unwrap();
            stmt.reset().unwrap();
            stmt.bind_text_static(1, "ns_test").unwrap();
            stmt.bind_blob_static(2, b"my_key").unwrap();
            stmt.bind_blob_static(3, b"my_value").unwrap();
            stmt.bind_null(4).unwrap();

            assert!(stmt.step().unwrap(), "should return a row");
            let version = stmt.column_int64(0);
            assert_eq!(version, 1);
            stmt.reset().unwrap();
        }
        conn.commit().unwrap();

        // Query it back
        {
            let stmt = conn.slot(StmtSlot::KvGet).unwrap();
            stmt.reset().unwrap();
            stmt.bind_text_static(1, "ns_test").unwrap();
            stmt.bind_blob_static(2, b"my_key").unwrap();

            assert!(stmt.step().unwrap(), "must find row");
            assert_eq!(stmt.column_blob(0), b"my_value");
            assert_eq!(stmt.column_int64(1), 1);
            assert!(stmt.column_is_null(2));
            stmt.reset().unwrap();
        }
    }

    #[test]
    fn test_transaction_auto_rollback() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test_rollback.db");

        let mut conn = NativeConnection::open(&db_path, "NORMAL", 1024 * 1024, 5000).unwrap();
        conn.exec("CREATE TABLE t(x INTEGER PRIMARY KEY);").unwrap();

        // Transaction dropped without commit -> rollback
        {
            let tx = conn.transaction().unwrap();
            let mut stmt = tx.prepare("INSERT INTO t(x) VALUES(42);").unwrap();
            assert!(!stmt.step().unwrap());
        }

        let mut check = conn.prepare("SELECT count(*) FROM t;").unwrap();
        assert!(check.step().unwrap());
        assert_eq!(check.column_int64(0), 0);
    }

    #[test]
    fn test_native_savepoint_rollback_and_release() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test_savepoints.db");

        let mut conn = NativeConnection::open(&db_path, "NORMAL", 1024 * 1024, 5000).unwrap();
        conn.exec("CREATE TABLE t(x INTEGER PRIMARY KEY);").unwrap();

        let mut tx = conn.transaction().unwrap();
        tx.exec("INSERT INTO t(x) VALUES(1);").unwrap();

        // Level 1 savepoint
        tx.savepoint("sp1").unwrap();
        tx.exec("INSERT INTO t(x) VALUES(2);").unwrap();

        // Level 2 nested savepoint
        tx.savepoint("sp2").unwrap();
        tx.exec("INSERT INTO t(x) VALUES(3);").unwrap();

        // Rollback sp2 -> row 3 is removed, row 2 is preserved
        tx.rollback_to_savepoint("sp2").unwrap();
        tx.release_savepoint("sp2").unwrap();

        let mut c = tx.prepare("SELECT count(*) FROM t WHERE x = 3;").unwrap();
        assert!(c.step().unwrap());
        assert_eq!(c.column_int64(0), 0);

        let mut c = tx.prepare("SELECT count(*) FROM t WHERE x = 2;").unwrap();
        assert!(c.step().unwrap());
        assert_eq!(c.column_int64(0), 1);

        // Open sp3 and commit it into sp1
        tx.savepoint("sp3").unwrap();
        tx.exec("INSERT INTO t(x) VALUES(4);").unwrap();
        tx.release_savepoint("sp3").unwrap();

        // Rollback sp1 -> rows 2 and 4 removed, row 1 preserved
        tx.rollback_to_savepoint("sp1").unwrap();
        tx.release_savepoint("sp1").unwrap();

        tx.commit().unwrap();

        let mut check = conn.prepare("SELECT x FROM t ORDER BY x;").unwrap();
        assert!(check.step().unwrap());
        assert_eq!(check.column_int64(0), 1);
        assert!(!check.step().unwrap());

        // Validate bad savepoint names
        assert!(conn.savepoint("").is_err());
        assert!(conn.savepoint("bad name with space").is_err());
        assert!(conn.savepoint("inject\"; DROP TABLE t; --").is_err());
    }

    #[test]
    fn test_native_backup() {
        let temp_dir = tempfile::tempdir().unwrap();
        let src_path = temp_dir.path().join("src.db");
        let dst_path = temp_dir.path().join("dst.db");

        let conn = NativeConnection::open(&src_path, "NORMAL", 1024 * 1024, 5000).unwrap();
        conn.exec("CREATE TABLE t(x TEXT); INSERT INTO t VALUES('hello');")
            .unwrap();

        let dest = NativeConnection::open(&dst_path, "NORMAL", 1024 * 1024, 5000).unwrap();
        let mut backup = NativeBackup::new(&dest, &conn).unwrap();
        let done = backup.step(-1).unwrap();
        assert!(done);
        drop(backup);

        let mut check = dest.prepare("SELECT x FROM t;").unwrap();
        assert!(check.step().unwrap());
        assert_eq!(check.column_text(0), "hello");
    }

    #[test]
    fn test_carray_extension() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("carray.db");
        let conn = NativeConnection::open_file(&db_path).unwrap();

        // Integer array: carray(?1) with sqlite3_carray_bind_v2 carries length and type
        let mut stmt = conn.prepare("SELECT value FROM carray(?1);").unwrap();
        let numbers: Vec<i64> = vec![10, 20, 30, 42];
        stmt.bind_carray_int64_static(1, &numbers).unwrap();

        let mut collected = Vec::new();
        while stmt.step().unwrap() {
            collected.push(stmt.column_int64(0));
        }
        assert_eq!(collected, vec![10, 20, 30, 42]);
        stmt.reset().unwrap();

        // Unsigned 64-bit integer array
        let mut stmt_u64 = conn.prepare("SELECT value FROM carray(?1);").unwrap();
        let u64_numbers: Vec<u64> = vec![0x8928308280fffff, 0x8928308281fffff];
        stmt_u64.bind_carray_uint64_static(1, &u64_numbers).unwrap();
        let mut collected_u64 = Vec::new();
        while stmt_u64.step().unwrap() {
            collected_u64.push(stmt_u64.column_int64(0) as u64);
        }
        assert_eq!(collected_u64, u64_numbers);
        stmt_u64.reset().unwrap();

        // Empty array
        stmt_u64.bind_carray_uint64_static(1, &[]).unwrap();
        assert!(!stmt_u64.step().unwrap());

        // Text array
        let mut stmt_text = conn.prepare("SELECT value FROM carray(?1);").unwrap();
        let texts = vec!["alpha", "beta", "gamma"];
        stmt_text.bind_carray_text_transient(1, &texts).unwrap();

        let mut collected_text = Vec::new();
        while stmt_text.step().unwrap() {
            collected_text.push(stmt_text.column_text(0).to_string());
        }
        assert_eq!(
            collected_text,
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn test_generate_series_extension() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("series.db");
        let conn = NativeConnection::open_file(&db_path).unwrap();

        let mut stmt = conn
            .prepare("SELECT value FROM generate_series(1, 5, 1);")
            .unwrap();
        let mut vals = Vec::new();
        while stmt.step().unwrap() {
            vals.push(stmt.column_int64(0));
        }
        assert_eq!(vals, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_decimal_extension() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("decimal.db");
        let conn = NativeConnection::open_file(&db_path).unwrap();

        let mut stmt = conn
            .prepare("SELECT decimal_add('12.34', '56.78'), decimal_cmp('10.5', '10.50');")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_text(0), "69.12");
        assert_eq!(stmt.column_int64(1), 0);
    }

    #[test]
    fn test_geopoly_extension() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("geopoly.db");
        let conn = NativeConnection::open_file(&db_path).unwrap();

        // GeoJSON closed polygon ring: first and last vertex must be identical
        let mut stmt = conn
            .prepare("SELECT geopoly_area('[[0,0],[10,0],[10,10],[0,10],[0,0]]');")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert!((stmt.column_double(0) - 100.0).abs() < 1e-4);
    }

    #[test]
    fn test_dbstat_vtab() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("dbstat.db");
        let conn = NativeConnection::open_file(&db_path).unwrap();
        conn.exec(
            "CREATE TABLE demo(id INTEGER PRIMARY KEY, v TEXT); INSERT INTO demo VALUES(1, 'val');",
        )
        .unwrap();

        let mut stmt = conn
            .prepare("SELECT count(*) FROM dbstat WHERE name = 'demo';")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert!(stmt.column_int64(0) >= 1);
    }

    #[test]
    fn test_stmt_status_reports_full_scan_steps() {
        let temp_dir = tempfile::tempdir().unwrap();
        let conn = NativeConnection::open_file(temp_dir.path().join("scan.db")).unwrap();
        conn.exec(
            "CREATE TABLE demo(id INTEGER PRIMARY KEY, grp INTEGER NOT NULL, v TEXT);
             CREATE INDEX demo_grp ON demo(grp);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200)
             INSERT INTO demo SELECT i, i % 10, 'x' FROM n;",
        )
        .unwrap();

        let mut scan = conn
            .prepare("SELECT count(*) FROM demo WHERE v = 'x';")
            .unwrap();
        assert!(scan.step().unwrap());
        // 200 rows are visited by 199 forward steps after the first row.
        assert!(scan.status(StmtStatus::FullscanStep, true) >= 199);
        assert_eq!(
            scan.status(StmtStatus::FullscanStep, false),
            0,
            "reset must clear the counter"
        );

        let mut indexed = conn
            .prepare("SELECT count(*) FROM demo WHERE grp = 3;")
            .unwrap();
        assert!(indexed.step().unwrap());
        assert_eq!(indexed.status(StmtStatus::FullscanStep, true), 0);
    }

    #[test]
    fn test_native_session_changeset_and_invert() {
        let temp_dir = tempfile::tempdir().unwrap();
        let src_path = temp_dir.path().join("session_src.db");
        let dst_path = temp_dir.path().join("session_dst.db");

        let conn_src = NativeConnection::open_file(&src_path).unwrap();
        let conn_dst = NativeConnection::open_file(&dst_path).unwrap();

        let schema = "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);";
        conn_src.exec(schema).unwrap();
        conn_dst.exec(schema).unwrap();

        // Create and attach session on source
        let session = NativeSession::create(&conn_src, "main").unwrap();
        session.attach_all().unwrap();
        assert!(session.is_empty());

        // Make modifications
        conn_src
            .exec("INSERT INTO users(id, name) VALUES(1, 'Ada');")
            .unwrap();
        conn_src
            .exec("INSERT INTO users(id, name) VALUES(2, 'Charles');")
            .unwrap();
        assert!(!session.is_empty());

        // Generate changeset
        let changeset = session.changeset().unwrap();
        assert!(!changeset.is_empty());

        // Apply changeset to destination
        NativeSession::apply(&conn_dst, &changeset, ConflictAction::Replace).unwrap();

        let mut check_dst = conn_dst
            .prepare("SELECT count(*), name FROM users WHERE id = 1;")
            .unwrap();
        assert!(check_dst.step().unwrap());
        assert_eq!(check_dst.column_int64(0), 1);
        assert_eq!(check_dst.column_text(1), "Ada");
        drop(check_dst);

        // Invert changeset (undo) and apply back
        let inverse = NativeSession::invert_changeset(&changeset).unwrap();
        NativeSession::apply(&conn_dst, &inverse, ConflictAction::Replace).unwrap();

        let mut check_after_undo = conn_dst.prepare("SELECT count(*) FROM users;").unwrap();
        assert!(check_after_undo.step().unwrap());
        assert_eq!(check_after_undo.column_int64(0), 0);
    }

    #[test]
    fn test_native_session_table_filter() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("filter.db");
        let conn = NativeConnection::open_file(&db_path).unwrap();

        conn.exec("CREATE TABLE tracked(id INTEGER PRIMARY KEY, v TEXT); CREATE TABLE ignored(id INTEGER PRIMARY KEY, v TEXT);")
            .unwrap();

        let mut session = NativeSession::create(&conn, "main").unwrap();
        session.set_table_filter(|table| table == "tracked");
        session.attach_all().unwrap();

        // Mutate both tables
        conn.exec("INSERT INTO ignored(id, v) VALUES(1, 'secret');")
            .unwrap();
        assert!(session.is_empty(), "session should ignore 'ignored' table");

        conn.exec("INSERT INTO tracked(id, v) VALUES(1, 'public');")
            .unwrap();
        assert!(!session.is_empty(), "session should record 'tracked' table");

        let cs = session.changeset().unwrap();
        assert!(!cs.is_empty());
    }

    #[test]
    fn test_eponymous_vtab_and_temp_view_union() {
        use std::sync::Arc;

        struct SampleProvider;
        struct SampleCursor {
            rows: Vec<(i64, &'static str, &'static str)>,
            idx: usize,
        }
        impl VirtualCursor for SampleCursor {
            fn filter(
                &mut self,
                idx_num: i32,
                _idx_str: Option<&str>,
                args: &[VTabValue],
            ) -> lilia_core::Result<()> {
                let mut rows = vec![(1, "mem_ns", "alpha"), (2, "other_ns", "beta")];
                if idx_num == 1 {
                    if let Some(VTabValue::Text(ns)) = args.first() {
                        rows.retain(|r| r.1 == ns);
                    }
                }
                self.rows = rows;
                self.idx = 0;
                Ok(())
            }
            fn next(&mut self) -> lilia_core::Result<()> {
                self.idx += 1;
                Ok(())
            }
            fn eof(&self) -> bool {
                self.idx >= self.rows.len()
            }
            fn column(&self, col: i32) -> lilia_core::Result<VTabValue> {
                let r = self.rows[self.idx];
                Ok(match col {
                    0 => VTabValue::Text(r.1.to_string()),
                    1 => VTabValue::Text(r.2.to_string()),
                    _ => VTabValue::Null,
                })
            }
            fn rowid(&self) -> i64 {
                self.rows.get(self.idx).map_or(0, |r| r.0)
            }
        }
        impl VirtualTableProvider for SampleProvider {
            fn schema_ddl(&self) -> &str {
                "CREATE TABLE x(ns TEXT, val TEXT)"
            }
            fn best_index(&self, constraints: &[IndexConstraint]) -> IndexPlan {
                for (i, c) in constraints.iter().enumerate() {
                    if c.usable && c.column == 0 && c.op == ConstraintOp::Eq {
                        return IndexPlan {
                            idx_num: 1,
                            idx_str: None,
                            constraint_usage: vec![(i, 1, true)],
                            estimated_cost: 1.0,
                            estimated_rows: 1,
                            unique_scan: false,
                        };
                    }
                }
                IndexPlan::default()
            }
            fn open_cursor(&self) -> lilia_core::Result<Box<dyn VirtualCursor>> {
                Ok(Box::new(SampleCursor {
                    rows: Vec::new(),
                    idx: 0,
                }))
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("vtab.db");
        let conn = NativeConnection::open_file(&db_path).unwrap();
        conn.exec("CREATE TABLE disk_items(ns TEXT, val TEXT);")
            .unwrap();
        conn.exec("INSERT INTO disk_items VALUES('disk_ns', 'gamma');")
            .unwrap();
        conn.register_vtab_module("vtab_mem_items", Arc::new(SampleProvider))
            .unwrap();
        conn.exec(
            "CREATE TEMP VIEW unified_items AS \
             SELECT ns, val, 'disk' AS tier FROM disk_items \
             UNION ALL \
             SELECT ns, val, 'memory' AS tier FROM vtab_mem_items;",
        )
        .unwrap();

        let mut stmt = conn
            .prepare("SELECT val, tier FROM unified_items WHERE ns = 'mem_ns' ORDER BY val;")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_text(0), "alpha");
        assert_eq!(stmt.column_text(1), "memory");
        assert!(!stmt.step().unwrap());

        let mut all_stmt = conn.prepare("SELECT count(*) FROM unified_items;").unwrap();
        assert!(all_stmt.step().unwrap());
        assert_eq!(all_stmt.column_int64(0), 3);
    }

    #[test]
    fn test_shared_cache_unlock_notify_concurrency() {
        let uri = "file:lilia_unlock_notify_test?mode=memory&cache=shared";
        let writer = NativeConnection::open(std::path::Path::new(uri), "OFF", 0, 5000).unwrap();
        writer
            .exec("CREATE TABLE items(id INTEGER PRIMARY KEY, val INTEGER);")
            .unwrap();

        let reader = NativeConnection::open(std::path::Path::new(uri), "OFF", 0, 5000).unwrap();
        reader.pragma_update("query_only", 1).unwrap();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_r = std::sync::Arc::clone(&stop);
        let reader_handle = std::thread::spawn(move || {
            let mut reads = 0usize;
            while !stop_r.load(std::sync::atomic::Ordering::Relaxed) || reads < 50 {
                let mut stmt = reader.prepare("SELECT count(*) FROM items;").unwrap();
                assert!(stmt.step().unwrap());
                let _ = stmt.column_int64(0);
                reads += 1;
            }
            reads
        });

        for i in 0..400 {
            writer.begin_immediate().unwrap();
            let mut s = writer
                .prepare("INSERT INTO items(id, val) VALUES(?1, ?2);")
                .unwrap();
            s.bind_int64(1, i).unwrap();
            s.bind_int64(2, i * 10).unwrap();
            s.step().unwrap();
            writer.commit().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let reads = reader_handle.join().unwrap();
        assert!(reads >= 50);
    }
}
