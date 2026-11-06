pub mod connection;
pub mod ffi;
pub mod slots;
pub mod statement;

pub use connection::{InterruptHandle, NativeConnection};
pub use slots::{StatementSlots, StmtSlot};
pub use statement::NativeStatement;

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
}
