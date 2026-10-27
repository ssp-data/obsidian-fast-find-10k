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
