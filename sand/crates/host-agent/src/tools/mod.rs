use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: String, // JSON schema as string
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

pub trait Tool {
    fn definition(&self) -> ToolDefinition;
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String>;
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
}

// Built-in tools
pub mod exec;
pub mod fs;
pub mod terminal;
pub mod terminal_signal;
pub mod browser;
pub mod computer;
pub mod task;

pub use exec::ShellExecTool;
pub use fs::{FsReadTool, FsWriteTool, FsListTool, FsSearchTool, FsStatTool, FsPatchTool, FsMkdirTool, FsRemoveTool, FsRenameTool, FsGlobTool};
pub use terminal::{TerminalOpenTool, TerminalWriteTool, TerminalReadTool};
pub use terminal_signal::{TerminalResizeTool, TerminalSignalTool, TerminalCloseTool};
pub use browser::{BrowserOpenTool, BrowserSnapshotTool, BrowserClickTool, BrowserFillTool, BrowserScreenshotTool, BrowserTabsTool, BrowserPressTool};
pub use computer::{ComputerScreenshotTool, ComputerClickTool, ComputerTypeTool, ComputerMoveTool, ComputerKeyTool, ComputerScrollTool};
pub use task::{TaskCompleteTool, TaskCreateTool, TaskListTool};
