//! The codecs of a daemon connection. A client opens a connection with the four-byte preface of
//! its codec; every frame that follows is a big-endian `u32` length and a payload in that codec.
//!
//! - [`Codec::Rkyv`]: `rkyv` archives both ways, for Rust clients ([`read_frame`],
//!   [`write_frame`]).
//! - [`Codec::Js`]: for JavaScript clients, `MessagePack` requests and responses in a JS frame
//!   ([`to_js_frame`]): one JSON text, which the client parses with its native `JSON.parse`,
//!   followed by the raw bytes the JSON refers to.

use std::io::{self, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{read_frame, write_frame, Request, Response, MAX_FRAME_BYTES};

/// The payload format of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Rkyv,
    Js,
}

impl Codec {
    /// The bytes a connection in this codec opens with.
    #[must_use]
    pub const fn preface(self) -> [u8; 4] {
        match self {
            Self::Rkyv => *b"LRK1",
            Self::Js => *b"LJS1",
        }
    }

    /// Opens a connection in this codec (client side).
    ///
    /// # Errors
    ///
    /// The write's failure.
    pub async fn announce(self, stream: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        stream.write_all(&self.preface()).await?;
        stream.flush().await
    }

    /// The codec a connection opens with (server side).
    ///
    /// # Errors
    ///
    /// The read's failure; [`io::ErrorKind::InvalidData`] for an unknown preface.
    pub async fn accept(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Self> {
        let mut preface = [0u8; 4];
        stream.read_exact(&mut preface).await?;
        [Self::Rkyv, Self::Js]
            .into_iter()
            .find(|codec| codec.preface() == preface)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unknown connection preface"))
    }

    /// Reads one request (server side).
    ///
    /// # Errors
    ///
    /// As [`read_frame`]; a payload that does not decode.
    pub async fn read_request(self, stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Request> {
        match self {
            Self::Rkyv => read_frame(stream).await,
            Self::Js => from_msgpack(&read_payload(stream).await?),
        }
    }

    /// Writes one response (server side).
    ///
    /// # Errors
    ///
    /// As [`write_frame`].
    pub async fn write_response(
        self,
        stream: &mut (impl AsyncWrite + Unpin),
        response: &Response,
    ) -> io::Result<()> {
        match self {
            Self::Rkyv => write_frame(stream, response).await,
            Self::Js => write_payload(stream, &to_js_frame(response)?).await,
        }
    }
}

/// `value` in `MessagePack`, maps with field names.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when `value` does not serialize.
pub fn to_msgpack<T: Serialize + ?Sized>(value: &T) -> io::Result<Vec<u8>> {
    rmp_serde::to_vec_named(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The value `bytes` hold in `MessagePack`.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when `bytes` do not decode as a `T`.
pub fn from_msgpack<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    rmp_serde::from_slice(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The raw bytes of a JS frame, set aside while its JSON is written.
#[derive(Default)]
struct Segments {
    /// Offset and length of each segment in `bytes`.
    table: Vec<(u32, u32)>,
    bytes: Vec<u8>,
}

/// The compact JSON formatter, except that a byte string becomes the number of its segment.
struct SegmentFormatter<'a>(&'a mut Segments);

impl serde_json::ser::Formatter for SegmentFormatter<'_> {
    fn write_byte_array<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        value: &[u8],
    ) -> io::Result<()> {
        let segments = &mut *self.0;
        let index = segments.table.len();
        let offset = frame_u32(segments.bytes.len())?;
        segments.table.push((offset, frame_u32(value.len())?));
        segments.bytes.extend_from_slice(value);
        self.write_u64(writer, index as u64)
    }
}

fn frame_u32(n: usize) -> io::Result<u32> {
    u32::try_from(n).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "JS frame over 4 GiB"))
}

/// `value` as a JS frame, all integers little-endian:
///
/// ```text
/// [u32 JSON length][JSON text]
/// [u32 segment count][segment count x (u32 offset, u32 length)][segment bytes]
/// ```
///
/// The JSON is the `serde` form of `value`, except that each byte string (a `serde_bytes`
/// field) is the number of its segment, whose bytes follow the JSON. Versions are decimal
/// strings, as in every human-readable form.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when `value` does not serialize or the frame passes 4 GiB.
pub fn to_js_frame<T: Serialize + ?Sized>(value: &T) -> io::Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(512);
    frame.extend_from_slice(&[0; 4]);
    let mut segments = Segments::default();
    let mut serializer =
        serde_json::Serializer::with_formatter(&mut frame, SegmentFormatter(&mut segments));
    value
        .serialize(&mut serializer)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let json = frame_u32(frame.len() - 4)?;
    frame[..4].copy_from_slice(&json.to_le_bytes());
    frame.reserve(4 + segments.table.len() * 8 + segments.bytes.len());
    frame.extend_from_slice(&frame_u32(segments.table.len())?.to_le_bytes());
    for (offset, length) in segments.table {
        frame.extend_from_slice(&offset.to_le_bytes());
        frame.extend_from_slice(&length.to_le_bytes());
    }
    frame.extend_from_slice(&segments.bytes);
    Ok(frame)
}

async fn read_payload(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn write_payload(stream: &mut (impl AsyncWrite + Unpin), payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|&length| length > 0 && length as usize <= MAX_FRAME_BYTES)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid frame length"))?;
    stream.write_u32(length).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    #[derive(Serialize)]
    struct Entry {
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
        name: &'static str,
    }

    /// The JSON of a frame and its segments.
    fn read(frame: &[u8]) -> (Value, Vec<&[u8]>) {
        let u32_at = |at: usize| u32::from_le_bytes(frame[at..at + 4].try_into().unwrap()) as usize;
        let json = u32_at(0);
        let value = serde_json::from_slice(&frame[4..4 + json]).unwrap();
        let count = u32_at(4 + json);
        let table = 8 + json;
        let bytes = table + count * 8;
        let segments = (0..count)
            .map(|i| {
                let (offset, length) = (u32_at(table + i * 8), u32_at(table + i * 8 + 4));
                &frame[bytes + offset..bytes + offset + length]
            })
            .collect();
        (value, segments)
    }

    #[test]
    fn byte_strings_leave_the_json_for_segments() {
        let entries = [
            Entry {
                key: vec![0, 255],
                value: b"v1".to_vec(),
                name: "a\"b",
            },
            Entry {
                key: vec![],
                value: vec![7; 3],
                name: "c",
            },
        ];
        let frame = to_js_frame(&entries).unwrap();
        let (value, segments) = read(&frame);
        assert_eq!(
            value,
            json!([{"key": 0, "value": 1, "name": "a\"b"}, {"key": 2, "value": 3, "name": "c"}])
        );
        assert_eq!(segments, [&[0, 255][..], b"v1", b"", &[7, 7, 7]]);

        let frame = to_js_frame(&json!({"plain": [1, 2]})).unwrap();
        let (value, segments) = read(&frame);
        assert_eq!(value, json!({"plain": [1, 2]}));
        assert_eq!(segments.len(), 0, "no byte strings, no segments");
    }
}
