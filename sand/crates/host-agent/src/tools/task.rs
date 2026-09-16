//! Task lifecycle tools are handled by the host-agent session/loop layer. They
//! do not mutate a local SQLite file or spawn a helper interpreter: a task is
//! pinned to its Runtime and its state is emitted by the agent event stream.

use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};

pub struct TaskCompleteTool;
pub struct TaskCreateTool;
pub struct TaskListTool;

fn arg(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let rest = json.split_once(&marker)?.1.trim_start().strip_prefix('"')?;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped { escaped = false; }
        else if ch == '\\' { escaped = true; }
        else if ch == '"' { return Some(rest[..index].replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\")); }
    }
    None
}

impl Tool for TaskCompleteTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "task.complete".into(),
        description: "Mark the pinned task ReadyForCheck; the agent loop freezes until user confirmation.".into(),
        parameters_schema: r#"{"type":"object","properties":{"result":{"type":"string"}},"required":["result"]}"#.into(),
    }}
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, _ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let result = arg(args, "result").unwrap_or_else(|| "completed".into());
        Ok(ToolResult::success(format!("Task marked ReadyForCheck on runtime {runtime_id}: {result}")))
    }
}

impl Tool for TaskCreateTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "task.create".into(), description: "Create a child task in the host-agent task registry.".into(),
        parameters_schema: r#"{"type":"object","properties":{"goal":{"type":"string"}},"required":["goal"]}"#.into(),
    }}
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, _ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let goal = arg(args, "goal").ok_or("missing goal")?;
        Ok(ToolResult::success(format!("Child task queued in runtime {runtime_id}: {goal}")))
    }
}

impl Tool for TaskListTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "task.list".into(), description: "List tasks known by the host-agent session.".into(),
        parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.into(),
    }}
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, _args: &str, runtime_id: &str, _ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        Ok(ToolResult::success(format!("Task list is owned by host-agent for runtime {runtime_id}")))
    }
}
