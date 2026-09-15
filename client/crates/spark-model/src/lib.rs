//! spark-model: Core data structures for the Spark GPUI client.
//!
//! These types are transport-agnostic. The UI (spark-ui) and transport
//! (spark-transport) both depend on this crate, but neither depends on the other.

pub mod bot;
pub mod task;
pub mod workbench;
pub mod timeline;
pub mod attention;
pub mod connection;
pub mod settings;

pub use bot::{BotId, Bot};
pub use task::{TaskId, TaskStatus, TaskSummary, TaskDetail};
pub use workbench::Workbench;
pub use timeline::{
    TimelineItem, UserMessage, AssistantMessage, StatusItem, ToolItem,
    ToolPresentation, PermissionItem, ArtifactItem, ErrorItem,
    ReadyForCheckItem, ToolStatus as ToolItemStatus,
};
pub use attention::Attention;
pub use connection::{ConnectionStatus, ServerInfo};
pub use settings::Settings;
