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
pub mod machine;

pub use bot::{BotId, Bot};
pub use task::{TaskId, TaskStatus, TaskSummary, TaskDetail, Artifact, FileChange, FileChangeKind};
pub use workbench::Workbench;
pub use timeline::{
    TimelineItem, UserMessage, AssistantMessage, StatusItem, ToolItem,
    ToolPresentation, PermissionItem, ArtifactItem, ErrorItem,
    ReadyForCheckItem, ToolItemStatus,
};
pub use attention::Attention;
pub use connection::{ConnectionStatus, ServerInfo};
pub use settings::Settings;
pub use machine::{
    Machine, MachineId, MachineKind, MachineStatus, MachineCapabilities,
    MachineMetadata, MachineConnection, SshHostKey, SparkPaths,
    LOCAL_MACHINE_ID, fingerprint_machine_id, machine_ids_match,
};
