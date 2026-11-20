#![allow(unsafe_code)]

//! Core Zero-Copy abstractions and helpers powered by `rkyv`.

use rkyv::rancor::Error as RancorError;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Serialize};

use crate::{ErrorCode, LiliaError, Result};

pub mod entry;

pub use entry::{RelJsonEntry, RelKvEntry};

/// Serialize any typed value into an aligned zero-copy byte buffer.
///
/// # Errors
/// Returns `LiliaError` if serialization fails.
pub fn archive_to_bytes<T>(value: &T) -> Result<AlignedVec>
where
    T: for<'a> Serialize<
        rkyv::api::high::HighSerializer<
            AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            RancorError,
        >,
    >,
{
    rkyv::to_bytes::<RancorError>(value).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("rkyv serialization failed: {e}"),
            false,
        )
    })
}

/// Safely access an archived root from an aligned byte slice with bytecheck validation.
///
/// # Errors
/// Returns `LiliaError` if validation fails.
pub fn access_archived<T>(bytes: &[u8]) -> Result<&T::Archived>
where
    T: Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, RancorError>>,
{
    rkyv::access::<T::Archived, RancorError>(bytes).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("rkyv validation failed: {e}"),
            false,
        )
    })
}

/// Instantaneous (0 nanosecond) unchecked access to an archived root in trusted memory.
///
/// # Safety
/// The caller must ensure that `bytes` is correctly aligned for `T::Archived` and
/// represents a valid serialized instance of `T`.
#[inline]
#[must_use]
pub unsafe fn access_archived_unchecked<T>(bytes: &[u8]) -> &T::Archived
where
    T: Archive,
{
    rkyv::access_unchecked::<T::Archived>(bytes)
}
