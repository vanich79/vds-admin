//! Metric storage concerns: extraction, aggregation and retention.

pub mod aggregation;
pub mod retention;
pub mod samples;

pub use aggregation::MetricsAggregationService;
pub use retention::RetentionService;
pub use samples::{samples_from_check, samples_from_snapshot};
