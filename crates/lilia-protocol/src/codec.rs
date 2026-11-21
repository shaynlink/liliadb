//! The codecs of a daemon connection. A client opens a connection with the four-byte preface of
//! its codec; every frame that follows is a big-endian `u32` length and a payload in that codec:
//! `rkyv` archives for Rust clients, `MessagePack` for clients that cannot read archives (the
//! Node binding). The `MessagePack` form of a type is its `serde` form, maps with field names.

use std::io;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{read_frame, write_frame, Request, Response, MAX_FRAME_BYTES};

/// The payload format of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Rkyv,
    MessagePack,
}

impl Codec {
    /// The bytes a connection in this codec opens with.
    #[must_use]
    pub const fn preface(self) -> [u8; 4] {
        match self {
            Self::Rkyv => *b"LRK1",
            Self::MessagePack => *b"LMP1",
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
        [Self::Rkyv, Self::MessagePack]
            .into_iter()
            .find(|codec| codec.preface() == preface)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unknown connection preface"))
    }

    /// Reads one request.
    ///
    /// # Errors
    ///
    /// As [`read_frame`]; a payload that does not decode.
    pub async fn read_request(self, stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Request> {
        match self {
            Self::Rkyv => read_frame(stream).await,
            Self::MessagePack => from_msgpack(&read_payload(stream).await?),
        }
    }

    /// Writes one request.
    ///
    /// # Errors
    ///
    /// As [`write_frame`].
    pub async fn write_request(
        self,
        stream: &mut (impl AsyncWrite + Unpin),
        request: &Request,
    ) -> io::Result<()> {
        match self {
            Self::Rkyv => write_frame(stream, request).await,
            Self::MessagePack => write_payload(stream, &to_msgpack(request)?).await,
        }
    }

    /// Reads one response.
    ///
    /// # Errors
    ///
    /// As [`read_frame`]; a payload that does not decode.
    pub async fn read_response(
        self,
        stream: &mut (impl AsyncRead + Unpin),
    ) -> io::Result<Response> {
        match self {
            Self::Rkyv => read_frame(stream).await,
            Self::MessagePack => from_msgpack(&read_payload(stream).await?),
        }
    }

    /// Writes one response.
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
            Self::MessagePack => write_payload(stream, &to_msgpack(response)?).await,
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
