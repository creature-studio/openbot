use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(format!("task-{}", uuid::Uuid::new_v4()))
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    WaitingApproval,
    ReadyForCheck,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::WaitingApproval => "Waiting Approval",
            Self::ReadyForCheck => "Ready for Check",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled
        )
    }
}

// ---------------------------------------------------------------------------
// Summary (sidebar list)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskId,
    pub goal: String,
    pub status: TaskStatus,
    /// The machine this task runs on. Fixed when the task is created
    /// (see architecture §二十一: no live migration in v1).
    pub machine_id: super::machine::MachineId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Detail (full task state, loaded on demand)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    pub id: TaskId,
    pub goal: String,
    pub status: TaskStatus,
    pub session_id: String,
    pub runtime_id: String,

    /// Machine the runtime lives on. Chosen at creation, never changed while
    /// the task runs.
    pub machine_id: super::machine::MachineId,

    /// Rich timeline of events.
    pub timeline: Vec<super::timeline::TimelineItem>,

    /// Active attention, if any.
    pub attention: Option<super::attention::Attention>,

    /// Artifacts produced by the task. Remote artifacts stay on their machine
    /// until the user asks to download them (`downloaded == false`).
    pub artifacts: Vec<Artifact>,

    /// Browser snapshot URL (screenshot stream endpoint).
    pub browser_url: Option<String>,
    pub browser_snapshot: Option<Vec<u8>>, // JPEG/WebP frame

    /// Terminal sessions attached to this task.
    pub terminal_ids: Vec<String>,

    /// File changes produced by this task.
    pub file_changes: Vec<FileChange>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Artifact (remote-aware)
// ---------------------------------------------------------------------------

/// A file produced by a task. Artifacts live on the machine that produced
/// them: nothing is copied to the client until the user asks for it
/// (architecture §二十六).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub machine_id: super::machine::MachineId,
    pub runtime_id: String,

    /// Absolute path on `machine_id`.
    pub remote_path: String,

    pub size: u64,

    /// MIME type when known (`image/png`, `application/zip`, ...).
    pub mime: Option<String>,

    /// True when the bytes are available locally (already fetched).
    pub downloaded: bool,

    pub created_at: DateTime<Utc>,
}

impl Artifact {
    pub fn name(&self) -> &str {
        self.remote_path
            .rsplit('/')
            .next()
            .unwrap_or(self.remote_path.as_str())
    }

    /// Small artifacts are worth fetching eagerly; large ones wait for a
    /// `Download` click.
    pub fn is_small(&self) -> bool {
        self.size <= 256 * 1024
    }
}

// ---------------------------------------------------------------------------
// File change (for diff view)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub additions: u32,
    pub deletions: u32,
    /// Unified diff text.
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed { from: String },
}
