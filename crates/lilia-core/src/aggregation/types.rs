use serde::{Deserialize, Serialize};

/// Direction of sorting for aggregation results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Sort criteria specifying the field to sort by and direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub field: String,
    pub direction: SortDirection,
}

impl Sort {
    #[must_use]
    pub fn asc(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            direction: SortDirection::Asc,
        }
    }

    #[must_use]
    pub fn desc(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            direction: SortDirection::Desc,
        }
    }
}

/// Statistical and streaming aggregation functions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Aggregate {
    Count {
        output: String,
    },
    Sum {
        field: String,
        output: String,
    },
    Avg {
        field: String,
        output: String,
    },
    Min {
        field: String,
        output: String,
    },
    Max {
        field: String,
        output: String,
    },
    Variance {
        field: String,
        output: String,
    },
    StdDev {
        field: String,
        output: String,
    },
    Quantile {
        field: String,
        quantile: f64,
        output: String,
    },
    ApproxCountDistinct {
        field: String,
        output: String,
    },
    AnomalyCount {
        field: String,
        threshold: f64,
        output: String,
    },
    HomomorphicSum {
        field: String,
        output: String,
        key_id: Option<String>,
    },
}

impl Aggregate {
    #[must_use]
    pub fn count(output: impl Into<String>) -> Self {
        Self::Count {
            output: output.into(),
        }
    }

    #[must_use]
    pub fn sum(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::Sum {
            field: field.into(),
            output: output.into(),
        }
    }

    #[must_use]
    pub fn avg(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::Avg {
            field: field.into(),
            output: output.into(),
        }
    }

    #[must_use]
    pub fn min(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::Min {
            field: field.into(),
            output: output.into(),
        }
    }

    #[must_use]
    pub fn max(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::Max {
            field: field.into(),
            output: output.into(),
        }
    }

    #[must_use]
    pub fn variance(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::Variance {
            field: field.into(),
            output: output.into(),
        }
    }

    #[must_use]
    pub fn stddev(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::StdDev {
            field: field.into(),
            output: output.into(),
        }
    }

    #[must_use]
    pub fn quantile(field: impl Into<String>, quantile: f64, output: impl Into<String>) -> Self {
        Self::Quantile {
            field: field.into(),
            quantile,
            output: output.into(),
        }
    }

    #[must_use]
    pub fn approx_count_distinct(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::ApproxCountDistinct {
            field: field.into(),
            output: output.into(),
        }
    }

    #[must_use]
    pub fn anomaly_count(
        field: impl Into<String>,
        threshold: f64,
        output: impl Into<String>,
    ) -> Self {
        Self::AnomalyCount {
            field: field.into(),
            threshold,
            output: output.into(),
        }
    }

    #[must_use]
    pub fn homomorphic_sum(field: impl Into<String>, output: impl Into<String>) -> Self {
        Self::HomomorphicSum {
            field: field.into(),
            output: output.into(),
            key_id: None,
        }
    }

    #[must_use]
    pub fn homomorphic_sum_with_key(
        field: impl Into<String>,
        output: impl Into<String>,
        key_id: impl Into<String>,
    ) -> Self {
        Self::HomomorphicSum {
            field: field.into(),
            output: output.into(),
            key_id: Some(key_id.into()),
        }
    }

    #[must_use]
    pub fn output_name(&self) -> &str {
        match self {
            Self::Count { output }
            | Self::Sum { output, .. }
            | Self::Avg { output, .. }
            | Self::Min { output, .. }
            | Self::Max { output, .. }
            | Self::Variance { output, .. }
            | Self::StdDev { output, .. }
            | Self::Quantile { output, .. }
            | Self::ApproxCountDistinct { output, .. }
            | Self::AnomalyCount { output, .. }
            | Self::HomomorphicSum { output, .. } => output,
        }
    }

    #[must_use]
    pub fn target_field(&self) -> Option<&str> {
        match self {
            Self::Count { .. } => None,
            Self::Sum { field, .. }
            | Self::Avg { field, .. }
            | Self::Min { field, .. }
            | Self::Max { field, .. }
            | Self::Variance { field, .. }
            | Self::StdDev { field, .. }
            | Self::Quantile { field, .. }
            | Self::ApproxCountDistinct { field, .. }
            | Self::AnomalyCount { field, .. }
            | Self::HomomorphicSum { field, .. } => Some(field),
        }
    }
}
