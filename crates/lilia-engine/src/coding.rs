//! Byte-level encodings shared by the journal, the sorted runs and the manifest.

use crate::error::{corruption, Result};

#[allow(
    clippy::cast_possible_truncation,
    reason = "each byte keeps the low 7 bits by design"
)]
pub fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

pub fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// Reads data written by the `put_*` functions; every read checks bounds.
#[derive(Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, what }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn remaining(&self) -> &'a [u8] {
        self.data
    }

    pub fn varint(&mut self) -> Result<u64> {
        // Lengths below 128 are the common case: one byte, no loop.
        if let Some((&byte, rest)) = self.data.split_first() {
            if byte < 0x80 {
                self.data = rest;
                return Ok(u64::from(byte));
            }
        }
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self
                .data
                .split_first()
                .ok_or_else(|| corruption(format!("{}: truncated varint", self.what)))?;
            self.data = rest;
            value |= u64::from(byte & 0x7F) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err(corruption(format!(
            "{}: varint longer than 64 bits",
            self.what
        )))
    }

    pub fn varint_usize(&mut self) -> Result<usize> {
        usize::try_from(self.varint()?)
            .map_err(|_| corruption(format!("{}: length overflows usize", self.what)))
    }

    pub fn u8(&mut self) -> Result<u8> {
        let (&byte, rest) = self
            .data
            .split_first()
            .ok_or_else(|| corruption(format!("{}: truncated byte", self.what)))?;
        self.data = rest;
        Ok(byte)
    }

    pub fn fixed_u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(
            bytes.try_into().map_err(|_| corruption(self.what))?,
        ))
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.data.len() < n {
            return Err(corruption(format!(
                "{}: needs {n} bytes, {} left",
                self.what,
                self.data.len()
            )));
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.varint_usize()?;
        self.take(n)
    }
}

/// CRC32C (Castagnoli), hardware-accelerated where the CPU has it, masked so that a checksum
/// stored inside checksummed data does not checksum to a fixed value.
pub fn masked_crc(parts: &[&[u8]]) -> u32 {
    let crc = parts
        .iter()
        .fold(0, |crc, part| crc32c::crc32c_append(crc, part));
    crc.rotate_right(15).wrapping_add(0xA282_EAD8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip_and_reject_truncation() {
        let values = [0, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX];
        let mut out = Vec::new();
        for v in values {
            put_varint(&mut out, v);
        }
        put_bytes(&mut out, b"payload");
        let mut r = Reader::new(&out, "test");
        for v in values {
            assert_eq!(r.varint().unwrap(), v);
        }
        assert_eq!(r.bytes().unwrap(), b"payload");
        assert!(r.is_empty());
        assert!(Reader::new(&[0x80], "test").varint().is_err());
        assert!(Reader::new(&[5, 1], "test").bytes().is_err());
    }

    #[test]
    fn masked_crc_depends_on_every_part() {
        assert_eq!(masked_crc(&[b"ab", b"c"]), masked_crc(&[b"abc"]));
        assert_ne!(masked_crc(&[b"abc"]), masked_crc(&[b"abd"]));
    }
}
