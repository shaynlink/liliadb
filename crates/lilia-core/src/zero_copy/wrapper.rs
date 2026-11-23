//! `rkyv` serialization wrappers for external or non-native types.

use rkyv::rancor::{Fallible, Source};
use rkyv::string::{ArchivedString, StringResolver};
use rkyv::with::{ArchiveWith, DeserializeWith, SerializeWith};
use rkyv::{Place, SerializeUnsized};

/// Wrapper to archive `serde_json::Value` as an `ArchivedString`.
///
/// Avoids recursive AST solver overflow while allowing instant zero-copy
/// string access to JSON fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct AsJsonString;

impl ArchiveWith<serde_json::Value> for AsJsonString {
    type Archived = ArchivedString;
    type Resolver = StringResolver;

    #[inline]
    fn resolve_with(
        field: &serde_json::Value,
        resolver: Self::Resolver,
        out: Place<Self::Archived>,
    ) {
        let s = field.to_string();
        ArchivedString::resolve_from_str(&s, resolver, out);
    }
}

impl<S> SerializeWith<serde_json::Value, S> for AsJsonString
where
    S: Fallible + ?Sized,
    S::Error: Source,
    str: SerializeUnsized<S>,
{
    fn serialize_with(
        field: &serde_json::Value,
        serializer: &mut S,
    ) -> Result<Self::Resolver, S::Error> {
        let s = field.to_string();
        ArchivedString::serialize_from_str(&s, serializer)
    }
}

impl<D> DeserializeWith<ArchivedString, serde_json::Value, D> for AsJsonString
where
    D: Fallible + ?Sized,
    D::Error: Source,
{
    fn deserialize_with(field: &ArchivedString, _: &mut D) -> Result<serde_json::Value, D::Error> {
        serde_json::from_str(field.as_str()).map_err(Source::new)
    }
}
