//! Stores: GPUI Entity<T> state containers.
//!
//! Each store is an Entity<T> held by AppState. Stores receive
//! TransportEvents and update their state, which triggers GPUI
//! view updates through the notification system.

pub mod connection;
pub mod bot;
pub mod task;
pub mod workbench;
pub mod attention;
pub mod settings;

pub use connection::ConnectionStore;
pub use bot::BotStore;
pub use task::TaskStore;
pub use workbench::WorkbenchStore;
pub use attention::AttentionStore;
pub use settings::SettingsStore;
