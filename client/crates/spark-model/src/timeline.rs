//! Timeline — the core data structure for agent activity display.
//!
//! A timeline is a sequence of items that represent everything an agent
//! did during a task. It replaces "chat" as the primary UI concept.
//!
//! Key design rules:
//! - Tool stdout chunks do NOT each become a separate TimelineItem.
//!   Instead, ToolStarted → updates on the same item → ToolCompleted.
//! - The UI must virtualize the timeline (GPUI uniform_list).
//! - Running items auto-expand; completed ones are collapsed by default.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TimelineItem {
    UserMessage(UserMessage),
    AssistantMessage(AssistantMessage),
    Status(StatusItem),
    Tool(ToolItem),
    Permission(PermissionItem),
    Artifact(ArtifactItem),
    Error(ErrorItem),
    ReadyForCheck(ReadyForCheckItem),
}

impl TimelineItem {
    pub fn id(&self) -> &str {
        match self {
            Self::UserMessage(i) => &i.id,
            Self::AssistantMessage(i) => &i.id,
            Self::Status(i) => &i.id,
            Self::Tool(i) => &i.id,
            Self::Permission(i) => &i.id,
            Self::Artifact(i) => &i.id,
            Self::Error(i) => &i.id,
            Self::ReadyForCheck(i) => &i.id,
        }
    }

    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Self::UserMessage(i) => i.at,
            Self::AssistantMessage(i) => i.at,
            Self::Status(i) => i.at,
            Self::Tool(i) => i.at,
            Self::Permission(i) => i.at,
            Self::Artifact(i) => i.at,
            Self::Error(i) => i.at,
            Self::ReadyForCheck(i) => i.at,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self,
            Self::Tool(ToolItem {
                status: ToolItemStatus::Running,
                ..
            })
        )
    }
}

// ---------------------------------------------------------------------------
// User message
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub id: String,
    pub content: String,
    pub at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Assistant message (model text output, NOT a tool call)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub id: String,
    pub content: String,
    /// Partial streaming content appended before finalization.
    pub streaming: bool,
    pub at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Status (thinking, working, idle indicators)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusItem {
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
    pub at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Tool call — the big one
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolItem {
    pub id: String,
    pub call_id: String,

    /// Tool name as known to the agent, e.g. "shell.exec", "file.read".
    pub tool_name: String,

    /// Rich presentation type derived from tool_name.
    pub presentation: ToolPresentation,

    /// Human-readable label, e.g. "搜索文件", "运行测试".
    pub label: Option<String>,

    /// Machine-readable arguments (collapsed by default).
    pub args_json: Option<String>,

    /// Collected stdout / result text.
    pub output: Option<String>,

    /// Status.
    pub status: ToolItemStatus,

    /// Duration once completed.
    pub duration_ms: Option<u64>,

    /// Error info, if failed.
    pub error: Option<String>,

    /// For file-edit tools: a compact diff summary.
    pub diff_summary: Option<DiffSummary>,

    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolItemStatus {
    Running,
    Success,
    Error,
    PermissionRequired,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub file_path: String,
    pub additions: u32,
    pub deletions: u32,
}

// ---------------------------------------------------------------------------
// Tool presentation — maps tool_name → visual style
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolPresentation {
    /// shell.exec, etc.
    Shell,
    /// file.read, file.list, file.search, file.glob, file.stat
    FileRead,
    /// file.write, file.patch, file.mkdir, file.remove, file.rename
    FileEdit,
    /// browser.*
    Browser,
    /// terminal.*
    Terminal,
    /// computer.*
    Computer,
    /// task.complete, task.create, etc.
    Task,
    /// Catch-all
    Generic,
}

impl ToolPresentation {
    /// Derive presentation from a tool name string.
    pub fn from_tool_name(name: &str) -> Self {
        match name {
            n if n.starts_with("shell.") => Self::Shell,
            n if n.starts_with("file.read")
                || n.starts_with("file.list")
                || n.starts_with("file.search")
                || n.starts_with("file.glob")
                || n.starts_with("file.stat") =>
            {
                Self::FileRead
            }
            n if n.starts_with("file.") => Self::FileEdit,
            n if n.starts_with("browser.") => Self::Browser,
            n if n.starts_with("terminal.") => Self::Terminal,
            n if n.starts_with("computer.") => Self::Computer,
            n if n.starts_with("task.") => Self::Task,
            _ => Self::Generic,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Shell => "⌘",
            Self::FileRead => "📄",
            Self::FileEdit => "✏️",
            Self::Browser => "🌐",
            Self::Terminal => "⌨️",
            Self::Computer => "🖥️",
            Self::Task => "📋",
            Self::Generic => "🔧",
        }
    }
}

// ---------------------------------------------------------------------------
// Permission request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionItem {
    pub id: String,
    pub tool_name: String,
    pub reason: String,
    pub detail: Option<String>,
    /// None = still pending, Some = resolved.
    pub resolved: Option<bool>, // true = allowed, false = denied
    pub at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Artifact
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactItem {
    pub id: String,
    pub name: String,
    pub kind: String, // "file", "image", "url", etc.
    pub url: Option<String>,
    pub content_preview: Option<String>,
    pub at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorItem {
    pub id: String,
    pub message: String,
    pub recoverable: bool,
    pub at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// ReadyForCheck — agent says "I'm done, please review"
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyForCheckItem {
    pub id: String,
    pub summary: String,
    pub files_changed: u32,
    pub tests_passed: bool,
    pub browser_verified: bool,
    pub artifacts: Vec<String>,
    pub resolved: Option<bool>, // true = confirmed, false = needs more work
    pub at: DateTime<Utc>,
}
