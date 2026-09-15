pub mod agent;
pub mod model;
pub mod tools;
pub mod runtime;
pub mod events;
pub mod api;
pub mod persistence;

pub use agent::{AgentSession, AgentSessionId, AgentStatus, Attention, AgentContext, AgentLoop, Task, TaskManager};
pub use model::{Model, ModelResponse, MockModel, OpenAICompatibleModel};
pub use tools::{ToolRegistry, Tool, ToolDefinition, ToolResult};
pub use runtime::RuntimeManager;
pub use events::{EventBus, AgentEvent};
pub use api::HostAgentApi;
pub use persistence::SqlitePersistence;

pub fn default_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(tools::exec::ShellExecTool::new());
    registry.register(tools::fs::FsReadTool);
    registry.register(tools::fs::FsWriteTool);
    registry.register(tools::fs::FsListTool);
    registry.register(tools::fs::FsSearchTool);
    registry.register(tools::fs::FsStatTool);
    registry.register(tools::fs::FsPatchTool);
    registry.register(tools::terminal::TerminalOpenTool::new());
    registry.register(tools::terminal::TerminalWriteTool::new());
    registry.register(tools::terminal::TerminalReadTool::new());
    registry.register(tools::browser::BrowserOpenTool);
    registry.register(tools::browser::BrowserSnapshotTool);
    registry.register(tools::browser::BrowserClickTool);
    registry.register(tools::browser::BrowserFillTool);
    registry.register(tools::browser::BrowserScreenshotTool);
    registry.register(tools::browser::BrowserTabsTool);
    registry.register(tools::browser::BrowserPressTool);
    registry.register(tools::computer::ComputerScreenshotTool);
    registry.register(tools::computer::ComputerClickTool);
    registry.register(tools::computer::ComputerTypeTool);
    registry.register(tools::computer::ComputerMoveTool);
    registry.register(tools::computer::ComputerKeyTool);
    registry.register(tools::computer::ComputerScrollTool);
    registry.register(tools::task::TaskCompleteTool);
    registry.register(tools::task::TaskCreateTool);
    registry.register(tools::task::TaskListTool);
    registry
}
