pub mod normalize;
pub mod tokenize;
pub mod markdown;
pub mod doc;
pub mod query;
pub mod index;
pub mod matcher;
pub mod rank;
pub mod frecency;
pub mod similar;
pub mod vectors;
pub mod engine;
#[cfg(feature = "golden")]
pub mod golden;

pub use engine::{Engine, Hit, SearchOptions};

/// Timing assertions in tests are budgets for a developer machine; shared CI runners are
/// slower and noisier. `FF10K_PERF_SLACK` (a multiplier, e.g. 5) widens them; when it is unset
/// and `CI` is set, 5 is assumed.
#[cfg(test)]
pub(crate) fn perf_slack() -> u128 {
    match std::env::var("FF10K_PERF_SLACK") {
        Ok(v) => v.trim().parse().unwrap_or(1),
        Err(_) => {
            if std::env::var_os("CI").is_some() {
                5
            } else {
                1
            }
        }
    }
}
