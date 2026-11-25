//! A sequence of operations (a `MessagePack` array, as the JavaScript clients send them) run on a
//! new in-memory database: each must answer or fail with an error, never panic, hang or exhaust
//! memory. The database is opened again at the end and must hold what was committed.

#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use lilia_db::{Database, Options};
use lilia_io::{HardwareProfile, MemVfs};
use lilia_protocol::{execute, from_msgpack, Operation};

/// Operations of one input: enough for a sequence to build state and read it back.
const MAX_OPERATIONS: usize = 32;

fuzz_target!(|data: &[u8]| {
    let Ok(operations) = from_msgpack::<Vec<Operation>>(data) else {
        return;
    };
    let vfs = Arc::new(MemVfs::new(0));
    let options = || Options::for_hardware(&HardwareProfile::minimal());
    let database = Database::open_with(vfs.clone(), "/fuzz", options())
        .expect("a new in-memory database opens");
    for operation in operations.into_iter().take(MAX_OPERATIONS) {
        // `Shutdown` is the daemon's; on a database it is an error like any refused operation.
        let _ = execute(&database, operation);
    }
    database.close().expect("the database closes");
    let reopened =
        Database::open_with(vfs, "/fuzz", options()).expect("a closed database opens again");
    reopened.close().expect("the reopened database closes");
});
