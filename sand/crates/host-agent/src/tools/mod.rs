use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: String, // JSON schema as string
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Success,
    Error,
    PermissionRequired,
    Cancelled,
}

impl ToolStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::PermissionRequired => "permission_required",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub status: ToolStatus,
    pub content: String,
    pub artifacts: Vec<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: u64,
    pub duration_ms: u64,
    pub observations: Vec<String>,
    // Legacy field for backward compat
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(content: String) -> Self {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        Self {
            call_id: String::new(),
            tool_name: String::new(),
            status: ToolStatus::Success,
            content,
            artifacts: vec![],
            error_code: None,
            error_message: None,
            started_at: now,
            duration_ms: 0,
            observations: vec![],
            is_error: false,
        }
    }

    pub fn error(content: String, error_code: Option<String>) -> Self {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        Self {
            call_id: String::new(),
            tool_name: String::new(),
            status: ToolStatus::Error,
            content: content.clone(),
            artifacts: vec![],
            error_code,
            error_message: Some(content),
            started_at: now,
            duration_ms: 0,
            observations: vec![],
            is_error: true,
        }
    }

    pub fn permission_required(tool_name: String, reason: String) -> Self {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        Self {
            call_id: String::new(),
            tool_name: tool_name.clone(),
            status: ToolStatus::PermissionRequired,
            content: format!("permission_required: {} needs approval: {}", tool_name, reason),
            artifacts: vec![],
            error_code: Some("PERMISSION_REQUIRED".to_string()),
            error_message: Some(reason),
            started_at: now,
            duration_ms: 0,
            observations: vec![],
            is_error: true,
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            r#"{{"call_id":"{}","tool_name":"{}","status":"{}","error_code":{},"content":{},"duration_ms":{}}}"#,
            self.call_id,
            self.tool_name,
            self.status.as_str(),
            self.error_code.as_ref().map(|c| format!("\"{}\"", c)).unwrap_or_else(|| "null".to_string()),
            serde_json_escape(&self.content),
            self.duration_ms
        )
    }
}

fn serde_json_escape(s: &str) -> String {
    // Very simple escape for JSON string
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r");
    format!("\"{}\"", escaped)
}

pub trait Tool {
    fn definition(&self) -> ToolDefinition;
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String>;
    /// Optional: execute with a machine routing context.
    /// Default implementation falls back to the simple execute().
    fn execute_with_context(
        &self,
        args: &str,
        runtime_id: &str,
        _ctx: Option<&ToolExecutionContext>,
    ) -> Result<ToolResult, String> {
        self.execute(args, runtime_id)
    }
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool + Send + Sync>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register<T: Tool + Send + Sync + 'static>(&mut self, tool: T) {
        let def = tool.definition();
        self.tools.insert(def.name.clone(), Box::new(tool));
    }

    pub fn list(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub fn execute(&self, name: &str, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        if let Some(tool) = self.tools.get(name) {
            tool.execute(args, runtime_id)
        } else {
            Err(format!("tool not found: {}", name))
        }
    }

    /// Execute with machine routing context (Phase 3).
    pub fn execute_with_context(
        &self,
        name: &str,
        args: &str,
        runtime_id: &str,
        ctx: Option<&ToolExecutionContext>,
    ) -> Result<ToolResult, String> {
        if let Some(tool) = self.tools.get(name) {
            tool.execute_with_context(args, runtime_id, ctx)
        } else {
            Err(format!("tool not found: {}", name))
        }
    }
}

// Built-in tools
pub mod exec;
pub mod fs;
pub mod terminal;
pub mod terminal_signal;
pub mod browser;
pub mod computer;
pub mod task;
pub mod context;

pub use context::ToolExecutionContext;

pub use exec::ShellExecTool;
pub use fs::{FsReadTool, FsWriteTool, FsListTool, FsSearchTool, FsStatTool, FsPatchTool, FsMkdirTool, FsRemoveTool, FsRenameTool, FsGlobTool};
pub use terminal::{TerminalOpenTool, TerminalWriteTool, TerminalReadTool};
pub use terminal_signal::{TerminalResizeTool, TerminalSignalTool, TerminalCloseTool};
pub use browser::{BrowserOpenTool, BrowserSnapshotTool, BrowserClickTool, BrowserFillTool, BrowserScreenshotTool, BrowserTabsTool, BrowserPressTool};
pub use computer::{ComputerScreenshotTool, ComputerClickTool, ComputerTypeTool, ComputerMoveTool, ComputerKeyTool, ComputerScrollTool};
pub use task::{TaskCompleteTool, TaskCreateTool, TaskListTool};
