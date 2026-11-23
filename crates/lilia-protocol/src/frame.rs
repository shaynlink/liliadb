//! Size-prefixed `rkyv` frames.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::MAX_FRAME_BYTES;

/// Decode one size-prefixed zero-copy rkyv frame into an owned value.
///
/// # Errors
/// Returns an I/O error for truncated, oversized, empty, or malformed frames.
pub async fn read_frame<T>(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<T>
where
    T: rkyv::Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>
        + rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>,
{
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut buffer = rkyv::util::AlignedVec::<16>::with_capacity(length);
    buffer.resize(length, 0);
    stream.read_exact(&mut buffer).await?;
    rkyv::from_bytes::<T, rkyv::rancor::Error>(&buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

/// Encode and write one size-prefixed zero-copy rkyv frame.
///
/// # Errors
/// Returns an I/O error when serialization or writing fails, or when the frame is oversized.
pub async fn write_frame<T>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> io::Result<()>
where
    T: for<'a> rkyv::Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            rkyv::rancor::Error,
        >,
    >,
{
    write_archived_frame(stream, value).await
}

/// Encode and write one zero-copy frame using rkyv.
///
/// # Errors
/// Returns an I/O error when serialization fails or the frame exceeds limits.
pub async fn write_archived_frame<T>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()>
where
    T: for<'a> rkyv::Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            rkyv::rancor::Error,
        >,
    >,
{
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds size limit",
        ));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds u32 length"))?;
    stream.write_u32(length).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

/// Read a zero-copy frame from stream into an aligned buffer with bytecheck validation.
///
/// Returns a borrowed reference `&Archived<T>` without allocating or deserializing into heap objects!
///
/// # Errors
/// Returns an I/O error for truncated or invalid frames.
pub async fn read_archived_frame<'a, T>(
    stream: &mut (impl AsyncRead + Unpin),
    buffer: &'a mut rkyv::util::AlignedVec,
) -> io::Result<&'a T::Archived>
where
    T: rkyv::Archive,
    T::Archived: 'a
        + for<'b> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'b, rkyv::rancor::Error>>,
{
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    buffer.clear();
    buffer.resize(length, 0);
    stream.read_exact(buffer).await?;
    rkyv::access::<T::Archived, rkyv::rancor::Error>(buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}
