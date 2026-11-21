//! The names the bindings take for enumerations (`"inbound"`, `"cosine"`, `"durable"`...), each
//! parsed in one place. An absent name means the default; an unknown one is refused.

use lilia_core::{
    CentralityType, ComponentType, Direction, DownsampleAggregation, ErrorCode, LiliaError,
    ResourceProfile, VectorElementType, VectorMetric,
};
use lilia_db::Durability;

fn unknown(what: &str, name: &str) -> LiliaError {
    LiliaError::new(
        ErrorCode::InvalidInput,
        format!("unknown {what} '{name}'"),
        false,
    )
}

/// `"outbound"` (default, also `"outgoing"`), `"inbound"` (also `"incoming"`) or `"both"`.
///
/// # Errors
///
/// An unknown name.
pub fn direction(name: Option<&str>) -> Result<Direction, LiliaError> {
    match name {
        None | Some("outbound" | "outgoing") => Ok(Direction::Outbound),
        Some("inbound" | "incoming") => Ok(Direction::Inbound),
        Some("both") => Ok(Direction::Both),
        Some(other) => Err(unknown("direction", other)),
    }
}

/// `"float32"` (default), `"int8"` or `"bit"`.
///
/// # Errors
///
/// An unknown name.
pub fn element_type(name: Option<&str>) -> Result<VectorElementType, LiliaError> {
    match name {
        None | Some("float32") => Ok(VectorElementType::Float32),
        Some("int8") => Ok(VectorElementType::Int8),
        Some("bit") => Ok(VectorElementType::Bit),
        Some(other) => Err(unknown("vector element type", other)),
    }
}

/// `"cosine"` (default), `"l2"`, `"dot"` or `"hamming"`.
///
/// # Errors
///
/// An unknown name.
pub fn metric(name: Option<&str>) -> Result<VectorMetric, LiliaError> {
    match name {
        None | Some("cosine") => Ok(VectorMetric::Cosine),
        Some("l2") => Ok(VectorMetric::L2),
        Some("dot") => Ok(VectorMetric::Dot),
        Some("hamming") => Ok(VectorMetric::Hamming),
        Some(other) => Err(unknown("vector metric", other)),
    }
}

/// `"betweenness"`, `"closeness"` or `"degree"`.
///
/// # Errors
///
/// An unknown name.
pub fn centrality(name: &str) -> Result<CentralityType, LiliaError> {
    match name {
        "betweenness" => Ok(CentralityType::Betweenness),
        "closeness" => Ok(CentralityType::Closeness),
        "degree" => Ok(CentralityType::Degree),
        other => Err(unknown("centrality", other)),
    }
}

/// `"strongly_connected"` or `"weakly_connected"` (also in camel case).
///
/// # Errors
///
/// An unknown name.
pub fn component_type(name: &str) -> Result<ComponentType, LiliaError> {
    match name {
        "strongly_connected" | "stronglyConnected" => Ok(ComponentType::StronglyConnected),
        "weakly_connected" | "weaklyConnected" => Ok(ComponentType::WeaklyConnected),
        other => Err(unknown("component type", other)),
    }
}

/// `"avg"` (default), `"min"`, `"max"`, `"sum"`, `"first"`, `"last"` or `"count"`, in any case.
///
/// # Errors
///
/// An unknown name.
pub fn aggregation(name: Option<&str>) -> Result<DownsampleAggregation, LiliaError> {
    let Some(name) = name else {
        return Ok(DownsampleAggregation::Avg);
    };
    match name.to_ascii_lowercase().as_str() {
        "avg" => Ok(DownsampleAggregation::Avg),
        "min" => Ok(DownsampleAggregation::Min),
        "max" => Ok(DownsampleAggregation::Max),
        "sum" => Ok(DownsampleAggregation::Sum),
        "first" => Ok(DownsampleAggregation::First),
        "last" => Ok(DownsampleAggregation::Last),
        "count" => Ok(DownsampleAggregation::Count),
        _ => Err(unknown("downsample aggregation", name)),
    }
}

/// `"durable"` (default), `"balanced"` or `"performance"`.
///
/// # Errors
///
/// An unknown name.
pub fn durability(name: Option<&str>) -> Result<Durability, LiliaError> {
    match name {
        None | Some("durable") => Ok(Durability::Durable),
        Some("balanced") => Ok(Durability::Balanced),
        Some("performance") => Ok(Durability::Performance),
        Some(other) => Err(unknown("durability", other)),
    }
}

/// `"server"` (default), `"mobile"` or `"constrained"`.
///
/// # Errors
///
/// An unknown name.
pub fn profile(name: Option<&str>) -> Result<ResourceProfile, LiliaError> {
    match name {
        None | Some("server") => Ok(ResourceProfile::Server),
        Some("mobile") => Ok(ResourceProfile::Mobile),
        Some("constrained") => Ok(ResourceProfile::Constrained),
        Some(other) => Err(unknown("resource profile", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_with_their_aliases_and_defaults() {
        assert_eq!(direction(None).unwrap(), Direction::Outbound);
        assert_eq!(direction(Some("incoming")).unwrap(), Direction::Inbound);
        assert_eq!(metric(Some("l2")).unwrap(), VectorMetric::L2);
        assert_eq!(
            component_type("weaklyConnected").unwrap(),
            ComponentType::WeaklyConnected
        );
        assert_eq!(
            aggregation(Some("MAX")).unwrap(),
            DownsampleAggregation::Max
        );
        assert_eq!(durability(None).unwrap(), Durability::Durable);
        assert_eq!(profile(Some("mobile")).unwrap(), ResourceProfile::Mobile);
        let error = direction(Some("sideways")).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }
}
