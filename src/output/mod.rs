pub mod json;
pub mod plain;

#[cfg(feature = "prometheus")]
pub mod prometheus;

pub use json::output_json;
pub use plain::output_plain;
