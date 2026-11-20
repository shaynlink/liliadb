//! Battery-Aware Query Profiling and Pedagogical Predictive Energy Diagnostics.
//!
//! Evaluates the energy consumption of database queries in micro-joules (µJ) by
//! modeling CPU cycles, physical NAND flash I/O (reads, writes, fsyncs), and
//! memory bus wakeups (cache misses, lookaside buffer misses).
//!
//! Provides pedagogical diagnostics and predictive recommendations for mobile and
//! wearable platforms (watchOS, Wear OS, iOS, Android).

use serde::{Deserialize, Serialize};

/// Target hardware profile configuring energy estimation coefficients.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeviceEnergyProfile {
    /// Wearable device (watchOS / Wear OS): ultra-constrained battery (~250-400 mAh),
    /// high penalties for CPU bursts, memory bus wakeups, and NAND flash barriers.
    Smartwatch,
    /// Mobile phone (iOS / Android): standard battery (~3000-5000 mAh),
    /// big.LITTLE architecture with moderate memory bus and flash costs.
    #[default]
    Smartphone,
    /// Ultra-low power embedded or `IoT` node (coin cell or energy harvesting).
    EmbeddedIoT,
    /// Custom energy coefficients.
    Custom(EnergyCoefficients),
}

/// Physical energy coefficients used to compute micro-joules (µJ).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EnergyCoefficients {
    /// Active CPU computational power in µJ per microsecond (active core draw).
    pub cpu_active_uj_per_us: f64,
    /// Energy consumed per SQLite virtual machine bytecode step in µJ.
    pub cpu_vm_step_uj: f64,
    /// Energy to read a 4 KB page from NAND flash on page cache miss in µJ.
    pub flash_page_read_uj: f64,
    /// Energy to program a 4 KB dirty page to NAND flash in µJ.
    pub flash_page_write_uj: f64,
    /// Energy consumed by a physical flash controller commit / sync barrier (fsync) in µJ.
    pub flash_fsync_uj: f64,
    /// Energy penalty for waking up the DRAM controller / memory bus in µJ.
    pub memory_bus_wakeup_uj: f64,
    /// Energy penalty for lookaside buffer fallback / heap allocation in µJ.
    pub lookaside_miss_uj: f64,
}

impl EnergyCoefficients {
    /// Calibrated coefficients for smartwatches (watchOS / Wear OS).
    #[must_use]
    pub const fn smartwatch() -> Self {
        Self {
            cpu_active_uj_per_us: 0.0035,
            cpu_vm_step_uj: 0.0008,
            flash_page_read_uj: 1.8,
            flash_page_write_uj: 9.0,
            flash_fsync_uj: 85.0,
            memory_bus_wakeup_uj: 0.55,
            lookaside_miss_uj: 0.15,
        }
    }

    /// Calibrated coefficients for smartphones (iOS / Android).
    #[must_use]
    pub const fn smartphone() -> Self {
        Self {
            cpu_active_uj_per_us: 0.0025,
            cpu_vm_step_uj: 0.0006,
            flash_page_read_uj: 1.2,
            flash_page_write_uj: 6.5,
            flash_fsync_uj: 65.0,
            memory_bus_wakeup_uj: 0.35,
            lookaside_miss_uj: 0.10,
        }
    }

    /// Calibrated coefficients for ultra-constrained embedded `IoT` nodes.
    #[must_use]
    pub const fn embedded_iot() -> Self {
        Self {
            cpu_active_uj_per_us: 0.0050,
            cpu_vm_step_uj: 0.0015,
            flash_page_read_uj: 3.5,
            flash_page_write_uj: 15.0,
            flash_fsync_uj: 120.0,
            memory_bus_wakeup_uj: 1.20,
            lookaside_miss_uj: 0.40,
        }
    }
}

impl Default for EnergyCoefficients {
    fn default() -> Self {
        Self::smartphone()
    }
}

impl DeviceEnergyProfile {
    /// Returns the active energy coefficients for this profile.
    #[must_use]
    pub fn coefficients(&self) -> EnergyCoefficients {
        match self {
            Self::Smartwatch => EnergyCoefficients::smartwatch(),
            Self::Smartphone => EnergyCoefficients::smartphone(),
            Self::EmbeddedIoT => EnergyCoefficients::embedded_iot(),
            Self::Custom(c) => *c,
        }
    }

    /// Calculates energy consumption in micro-joules (µJ) from measured raw metrics.
    #[allow(clippy::cast_precision_loss)]
    pub fn calculate_energy(&self, metrics: &mut QueryEnergyMetrics) -> f64 {
        let coef = self.coefficients();

        // 1. CPU Energy
        let cpu_uj = (metrics.execution_time_us as f64 * coef.cpu_active_uj_per_us)
            + (metrics.cpu_vm_steps as f64 * coef.cpu_vm_step_uj)
            + (metrics.sort_operations as f64 * (coef.cpu_vm_step_uj * 25.0))
            + (metrics.auto_index_inserts as f64 * (coef.cpu_vm_step_uj * 10.0));

        // 2. Physical NAND Flash I/O Energy
        let flash_uj = (metrics.page_cache_misses as f64 * coef.flash_page_read_uj)
            + (metrics.page_cache_writes as f64 * coef.flash_page_write_uj)
            + (metrics.fsync_count as f64 * coef.flash_fsync_uj);

        // 3. Memory Bus Wakeups & DRAM Energy
        // A page cache miss forces memory bus activation and DRAM row fetch.
        // Lookaside misses force heap allocation and memory traffic.
        let bus_wakeups = metrics.page_cache_misses + metrics.lookaside_misses;
        let mem_uj = (bus_wakeups as f64 * coef.memory_bus_wakeup_uj)
            + (metrics.lookaside_misses as f64 * coef.lookaside_miss_uj);

        let total = cpu_uj + flash_uj + mem_uj;
        metrics.cpu_energy_uj = (cpu_uj * 100.0).round() / 100.0;
        metrics.flash_energy_uj = (flash_uj * 100.0).round() / 100.0;
        metrics.memory_energy_uj = (mem_uj * 100.0).round() / 100.0;
        metrics.total_energy_uj = (total * 10.0).round() / 10.0;
        metrics.total_energy_uj
    }
}

/// Comprehensive hardware and execution metrics collected for a query.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct QueryEnergyMetrics {
    /// Total wall-clock execution time in microseconds.
    pub execution_time_us: u64,
    /// Number of virtual machine operations executed (`SQLITE_STMTSTATUS_VM_STEP`).
    pub cpu_vm_steps: u64,
    /// Number of full-table scan loop steps (`SQLITE_STMTSTATUS_FULLSCAN_STEP`).
    pub full_scan_steps: u64,
    /// Number of sort operations performed (`SQLITE_STMTSTATUS_SORT`).
    pub sort_operations: u64,
    /// Rows inserted into transient automatic indexes (`SQLITE_STMTSTATUS_AUTOINDEX`).
    pub auto_index_inserts: u64,
    /// Number of page cache hits (`SQLITE_DBSTATUS_CACHE_HIT`).
    pub page_cache_hits: u64,
    /// Number of page cache misses requiring physical flash read (`SQLITE_DBSTATUS_CACHE_MISS`).
    pub page_cache_misses: u64,
    /// Number of dirty cache pages written to disk/WAL (`SQLITE_DBSTATUS_CACHE_WRITE`).
    pub page_cache_writes: u64,
    /// Number of physical disk syncs executed during query (`fsync`).
    pub fsync_count: u64,
    /// Lookaside buffer misses that caused heap allocation (`SQLITE_DBSTATUS_LOOKASIDE_MISS_*`).
    pub lookaside_misses: u64,
    /// Estimated CPU energy consumption in micro-joules (µJ).
    pub cpu_energy_uj: f64,
    /// Estimated flash NAND I/O energy consumption in micro-joules (µJ).
    pub flash_energy_uj: f64,
    /// Estimated memory bus wakeup energy consumption in micro-joules (µJ).
    pub memory_energy_uj: f64,
    /// Total estimated energy in micro-joules (µJ).
    pub total_energy_uj: f64,
}

/// Diagnostic categorization of query efficiency and anti-patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnergyDiagnosis {
    /// Full sequential scan over a table without an index.
    #[serde(rename = "FULL_TABLE_SCAN_DETECTED")]
    FullTableScanDetected,
    /// Sorting requires an in-memory or on-disk temporary B-Tree (`USE TEMP B-TREE`).
    #[serde(rename = "TEMPORARY_B_TREE_SORT")]
    TemporaryBTreeSort,
    /// Multiple physical fsync barriers executed synchronously.
    #[serde(rename = "EXCESSIVE_FSYNC_DETECTED")]
    ExcessiveFsyncDetected,
    /// Heavy cache misses or lookaside misses causing constant memory bus wakeups.
    #[serde(rename = "HIGH_MEMORY_BUS_PRESSURE")]
    HighMemoryBusPressure,
    /// Unindexed nested loop join across multiple tables.
    #[serde(rename = "UNINDEXED_JOIN")]
    UnindexedJoin,
    /// Query already uses an optimal index seek or covering index.
    #[serde(rename = "OPTIMAL_INDEX_USED")]
    OptimalIndexUsed,
    /// Standard execution with normal profile.
    #[serde(rename = "NORMAL")]
    Normal,
}

impl EnergyDiagnosis {
    /// Returns the diagnostic identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FullTableScanDetected => "FULL_TABLE_SCAN_DETECTED",
            Self::TemporaryBTreeSort => "TEMPORARY_B_TREE_SORT",
            Self::ExcessiveFsyncDetected => "EXCESSIVE_FSYNC_DETECTED",
            Self::HighMemoryBusPressure => "HIGH_MEMORY_BUS_PRESSURE",
            Self::UnindexedJoin => "UNINDEXED_JOIN",
            Self::OptimalIndexUsed => "OPTIMAL_INDEX_USED",
            Self::Normal => "NORMAL",
        }
    }
}

/// High-level pedagogical and predictive diagnostic report for a query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryEnergyDiagnostic {
    /// The analyzed SQL query or `LiliaQL` pipeline.
    pub query: String,
    /// Execution duration in microseconds (µs).
    pub execution_time_us: u64,
    /// Estimated total energy consumed by the query in micro-joules (µJ).
    pub estimated_energy_uj: f64,
    /// Primary diagnostic categorization.
    pub diagnosis: EnergyDiagnosis,
    /// Pedagogical recommendation explaining how to optimize battery consumption.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
    /// Synthesized DDL recommendation (e.g. `CREATE INDEX ...`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_index_sql: Option<String>,
    /// Predicted energy consumption in µJ if the recommendation is applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicted_energy_uj: Option<f64>,
    /// Predicted percentage reduction in energy consumption (e.g. 96.0%).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_savings_percent: Option<f64>,
    /// Detailed hardware metrics breakdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<QueryEnergyMetrics>,
    /// Pedagogical breakdown of execution steps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan_details: Vec<String>,
}

impl QueryEnergyDiagnostic {
    /// Constructs a clear, pedagogical diagnostic report.
    #[must_use]
    pub fn new(
        query: impl Into<String>,
        execution_time_us: u64,
        estimated_energy_uj: f64,
        diagnosis: EnergyDiagnosis,
        recommendation: Option<String>,
    ) -> Self {
        Self {
            query: query.into(),
            execution_time_us,
            estimated_energy_uj,
            diagnosis,
            recommendation,
            recommended_index_sql: None,
            predicted_energy_uj: None,
            energy_savings_percent: None,
            metrics: None,
            plan_details: Vec::new(),
        }
    }

    /// Formats the diagnostic into the exact JSON format.
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_energy_profiles_and_calculation() {
        let profile = DeviceEnergyProfile::Smartphone;
        let mut metrics = QueryEnergyMetrics {
            execution_time_us: 4200,
            cpu_vm_steps: 10_000,
            full_scan_steps: 500,
            sort_operations: 0,
            auto_index_inserts: 0,
            page_cache_hits: 15,
            page_cache_misses: 2,
            page_cache_writes: 0,
            fsync_count: 0,
            lookaside_misses: 4,
            ..Default::default()
        };

        let energy_uj = profile.calculate_energy(&mut metrics);
        assert!(energy_uj > 0.0);
        assert!(metrics.cpu_energy_uj > 0.0);
        assert!(metrics.flash_energy_uj > 0.0);
        assert!(metrics.memory_energy_uj > 0.0);
        assert_eq!(metrics.total_energy_uj, energy_uj);

        // Smartwatch profile should assign higher energy penalty to bus wakeups and flash
        let watch_profile = DeviceEnergyProfile::Smartwatch;
        let mut watch_metrics = metrics;
        let watch_energy_uj = watch_profile.calculate_energy(&mut watch_metrics);
        assert!(
            watch_energy_uj > energy_uj,
            "Smartwatch energy should be higher than smartphone due to higher relative overhead: watch={watch_energy_uj}, phone={energy_uj}"
        );
    }

    #[test]
    fn test_diagnostic_serialization_exact_schema() {
        let diag = QueryEnergyDiagnostic::new(
            "SELECT * FROM logs WHERE level = 'ERROR'",
            4200,
            18.4,
            EnergyDiagnosis::FullTableScanDetected,
            Some("Créer l'index partiel idx_logs_error réduirait la consommation énergétique de 96 %.".to_string()),
        );

        let json_str = serde_json::to_string(&diag).unwrap();
        assert!(json_str.contains("\"query\":\"SELECT * FROM logs WHERE level = 'ERROR'\""));
        assert!(json_str.contains("\"execution_time_us\":4200"));
        assert!(json_str.contains("\"estimated_energy_uj\":18.4"));
        assert!(json_str.contains("\"diagnosis\":\"FULL_TABLE_SCAN_DETECTED\""));
        assert!(json_str.contains("\"recommendation\":\"Créer l'index partiel idx_logs_error réduirait la consommation énergétique de 96 %.\""));
    }
}
