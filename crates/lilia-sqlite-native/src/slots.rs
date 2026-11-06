use crate::statement::NativeStatement;

#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StmtSlot {
    KvUpsertReturning = 0,
    KvGet = 1,
    KvDelete = 2,
    KvVersion = 3,
    JsonPutReturning = 4,
    JsonGet = 5,
    JsonDelete = 6,
    Count = 7,
}

impl StmtSlot {
    pub const ALL: [StmtSlot; Self::Count as usize] = [
        StmtSlot::KvUpsertReturning,
        StmtSlot::KvGet,
        StmtSlot::KvDelete,
        StmtSlot::KvVersion,
        StmtSlot::JsonPutReturning,
        StmtSlot::JsonGet,
        StmtSlot::JsonDelete,
    ];

    pub fn sql(&self) -> &'static str {
        match self {
            Self::KvUpsertReturning => {
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, 1, ?4) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_kv.version + 1, \
                   expires_at_ms = excluded.expires_at_ms \
                 RETURNING version;"
            }
            Self::KvGet => {
                "SELECT value, version, expires_at_ms FROM _lilia_kv WHERE namespace = ?1 AND key = ?2;"
            }
            Self::KvDelete => {
                "DELETE FROM _lilia_kv WHERE namespace = ?1 AND key = ?2;"
            }
            Self::KvVersion => {
                "SELECT version FROM _lilia_kv WHERE namespace = ?1 AND key = ?2;"
            }
            Self::JsonPutReturning => {
                "INSERT INTO _lilia_json(space, id, value, version) \
                 VALUES(?1, ?2, ?3, 1) \
                 ON CONFLICT(space, id) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_json.version + 1 \
                 RETURNING version;"
            }
            Self::JsonGet => {
                "SELECT value, version FROM _lilia_json WHERE space = ?1 AND id = ?2;"
            }
            Self::JsonDelete => {
                "DELETE FROM _lilia_json WHERE space = ?1 AND id = ?2;"
            }
            Self::Count => unreachable!(),
        }
    }
}

pub struct StatementSlots {
    slots: [Option<NativeStatement>; StmtSlot::Count as usize],
}

impl StatementSlots {
    pub fn empty() -> Self {
        const INIT: Option<NativeStatement> = None;
        Self {
            slots: [INIT; StmtSlot::Count as usize],
        }
    }

    #[inline]
    pub fn get_mut(&mut self, slot: StmtSlot) -> Option<&mut NativeStatement> {
        self.slots[slot as usize].as_mut()
    }

    #[inline]
    pub fn set(&mut self, slot: StmtSlot, stmt: NativeStatement) {
        self.slots[slot as usize] = Some(stmt);
    }
}
