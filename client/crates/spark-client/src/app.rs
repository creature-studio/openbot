//! Re-export AppState from spark-ui so main.rs can use it cleanly.

#[allow(unused_imports)] // main.rs event routing will use this next
pub use spark_ui::AppState;
