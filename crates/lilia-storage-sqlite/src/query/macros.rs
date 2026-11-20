//! Declarative macros for `LiliaQL` query projection and document filtering.

pub use lilia_core::doc;

/// Convenient macro to construct a [`Projection`](crate::query::Projection) using JSON-like key-value syntax.
///
/// # Examples
/// ```rust,no_run
/// use lilia_storage_sqlite::projection;
///
/// let p = projection! {
///     "user_name": "$.name",
///     "friend_name": "$graph.target.name",
///     "relevance_score": "$rrf_score",
/// };
/// ```
#[macro_export]
macro_rules! projection {
    ($( $key:literal : $val:expr ),* $(,)?) => {{
        let mut proj = $crate::query::Projection::new();
        $(
            proj.add_field($key, $val);
        )*
        proj
    }};
    ($( $key:ident : $val:expr ),* $(,)?) => {{
        let mut proj = $crate::query::Projection::new();
        $(
            proj.add_field(stringify!($key), $val);
        )*
        proj
    }};
}
