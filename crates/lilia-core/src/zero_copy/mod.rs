//! Core Zero-Copy abstractions and helpers powered by `rkyv`.

use rkyv::rancor::Error as RancorError;
use rkyv::Archive;

use crate::{ErrorCode, LiliaError, Result};

pub mod wrapper;

pub use wrapper::AsJsonString;

/// Safely access an archived root from an aligned byte slice with bytecheck validation.
///
/// # Errors
/// Returns `LiliaError` if validation fails.
pub fn access_archived<T>(bytes: &[u8]) -> Result<&T::Archived>
where
    T: Archive,
    T::Archived:
        for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, RancorError>>,
{
    rkyv::access::<T::Archived, RancorError>(bytes).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("rkyv validation failed: {e}"),
            false,
        )
    })
}
