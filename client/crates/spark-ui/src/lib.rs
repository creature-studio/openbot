//! spark-ui: GPUI-based UI for the Spark agent workbench.
//!
//! Architecture follows GPUI's Entity<T> ownership model:
//!
//! ```text
//! AppState (held by App)
//! ├── ConnectionStore
//! ├── BotStore
//! ├── TaskStore
//! │    ├── TaskEntity A
//! │    ├── TaskEntity B
//! │    └── TaskEntity C
//! ├── WorkbenchStore
//! ├── AttentionStore
//! └── SettingsStore
//! ```
//!
//! Windows:
//!
//! ```text
//! MainWindow
//! ├── Sidebar
//! ├── TaskTimeline
//! ├── Inspector (right panel)
//! └── Composer (bottom)
//! ```

pub mod stores;
pub mod sidebar;
pub mod timeline;
pub mod composer;
pub mod inspector;
pub mod terminal;
pub mod browser;
pub mod diff;
pub mod attention;
pub mod app;

pub use app::AppState;
pub use stores::{
    ConnectionStore, BotStore, TaskStore, WorkbenchStore, AttentionStore, SettingsStore,
};
