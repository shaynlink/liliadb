//! A request as the daemon reads it from a JavaScript (`MessagePack`) connection: any bytes must
//! decode or be refused, never panic or allocate without bound.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lilia_protocol::{from_msgpack, Request};

fuzz_target!(|data: &[u8]| {
    let _ = from_msgpack::<Request>(data);
});
