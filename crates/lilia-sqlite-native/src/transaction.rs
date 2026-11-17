use lilia_core::Result;

use crate::connection::NativeConnection;
use crate::slots::StmtSlot;
use crate::statement::NativeStatement;

pub struct NativeTransaction<'a> {
    conn: &'a mut NativeConnection,
    committed: bool,
}

impl std::fmt::Debug for NativeTransaction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTransaction")
            .field("committed", &self.committed)
            .finish()
    }
}

impl<'a> NativeTransaction<'a> {
    pub fn new(conn: &'a mut NativeConnection) -> Result<Self> {
        conn.begin_immediate()?;
        Ok(Self {
            conn,
            committed: false,
        })
    }

    /// Wraps an already-active connection (e.g. inside an outer transaction or savepoint)
    /// without issuing `BEGIN IMMEDIATE;`. Dropping this wrapper will not issue a rollback.
    #[inline]
    pub fn wrap_active(conn: &'a mut NativeConnection) -> Self {
        Self {
            conn,
            committed: true,
        }
    }

    #[inline]
    pub fn savepoint(&mut self, name: &str) -> Result<()> {
        self.conn.savepoint(name)
    }

    #[inline]
    pub fn release_savepoint(&mut self, name: &str) -> Result<()> {
        self.conn.release_savepoint(name)
    }

    #[inline]
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        self.conn.rollback_to_savepoint(name)
    }

    #[inline]
    pub fn cache_flush(&self) -> Result<()> {
        self.conn.cache_flush()
    }

    #[inline]
    pub fn commit(mut self) -> Result<()> {
        self.conn.commit()?;
        self.committed = true;
        Ok(())
    }

    #[inline]
    pub fn rollback(mut self) -> Result<()> {
        self.conn.rollback()?;
        self.committed = true;
        Ok(())
    }

    #[inline]
    pub fn slot(&mut self, slot: StmtSlot) -> Result<&mut NativeStatement> {
        self.conn.slot(slot)
    }

    #[inline]
    pub fn prepare(&self, sql: &str) -> Result<NativeStatement> {
        self.conn.prepare(sql)
    }

    /// See [`NativeConnection::prepare_denying_writes`].
    pub fn prepare_denying_writes(
        &self,
        sql: &str,
        protected_tables: &[&str],
    ) -> Result<NativeStatement> {
        self.conn.prepare_denying_writes(sql, protected_tables)
    }

    #[inline]
    pub fn exec(&self, sql: &str) -> Result<()> {
        self.conn.exec(sql)
    }

    #[inline]
    pub fn conn(&self) -> &NativeConnection {
        self.conn
    }

    #[inline]
    pub fn conn_mut(&mut self) -> &mut NativeConnection {
        self.conn
    }
}

impl Drop for NativeTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.rollback();
        }
    }
}
