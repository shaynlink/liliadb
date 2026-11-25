//! A request as the daemon reads it from an `rkyv` connection: any bytes must validate or be
//! refused, never be read out of bounds or panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lilia_protocol::Request;
use rkyv::util::AlignedVec;

fuzz_target!(|data: &[u8]| {
    // The daemon reads each frame into a 16-byte aligned buffer.
    let mut frame = AlignedVec::<16>::with_capacity(data.len());
    frame.extend_from_slice(data);
    let _ = rkyv::from_bytes::<Request, rkyv::rancor::Error>(&frame);
});
