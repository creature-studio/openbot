use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};
use spark_transport::{PtyOpenRequest, PtyReadRequest, PtyWriteRequest};

pub struct TerminalOpenTool;
pub struct TerminalWriteTool;
pub struct TerminalReadTool;

impl TerminalOpenTool { pub fn new() -> Self { Self } }
impl TerminalWriteTool { pub fn new() -> Self { Self } }
impl TerminalReadTool { pub fn new() -> Self { Self } }

fn arg(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let rest = json.split_once(&marker)?.1.trim_start().strip_prefix('"')?;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped { escaped = false; }
        else if ch == '\\' { escaped = true; }
        else if ch == '"' {
            return Some(rest[..index].replace("\\n", "\n").replace("\\r", "\r").replace("\\\"", "\"").replace("\\\\", "\\").replace("\\x03", "\x03").replace("\\x04", "\x04"));
        }
    }
    None
}

fn number(json: &str, key: &str, default: u16) -> u16 {
    let marker = format!("\"{key}\":");
    json.split_once(&marker)
        .and_then(|(_, rest)| rest.trim_start().split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn routed(ctx: Option<&ToolExecutionContext>, runtime_id: &str) -> Result<std::sync::Arc<dyn spark_transport::RuntimeTransport>, String> {
    let ctx = ctx.ok_or("terminal tools require a RuntimeTransport routing context")?;
    ctx.transport_for_runtime(runtime_id)
        .ok_or_else(|| format!("no transport for runtime {runtime_id}"))
}

impl Tool for TerminalOpenTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "terminal.open".into(),
        description: "Open a PTY owned by the runtime.".into(),
        parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"cols":{"type":"number"},"rows":{"type":"number"},"shell":{"type":"string"}},"required":["pty_id"]}"#.into(),
    }}
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err("terminal.open requires a RuntimeTransport routing context".into()) }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = arg(args, "pty_id").ok_or("missing pty_id")?;
        let transport = routed(ctx, runtime_id)?;
        super::context::block_on_transport(transport.open_pty(PtyOpenRequest {
            runtime_id: runtime_id.into(), pty_id: pty_id.clone(), cols: number(args, "cols", 80), rows: number(args, "rows", 24), shell: arg(args, "shell").unwrap_or_else(|| "/bin/bash".into()),
        })).map_err(|e| format!("open pty failed: {e}"))?;
        Ok(ToolResult::success(format!("opened pty {pty_id} in runtime {runtime_id}")))
    }
}

impl Tool for TerminalWriteTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "terminal.write".into(), description: "Write bytes to a runtime PTY.".into(),
        parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"data":{"type":"string"}},"required":["pty_id","data"]}"#.into(),
    }}
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err("terminal.write requires a RuntimeTransport routing context".into()) }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = arg(args, "pty_id").ok_or("missing pty_id")?;
        let data = arg(args, "data").ok_or("missing data")?;
        let transport = routed(ctx, runtime_id)?;
        super::context::block_on_transport(transport.write_pty(PtyWriteRequest { runtime_id: runtime_id.into(), pty_id: pty_id.clone(), data: data.into_bytes() }))
            .map_err(|e| format!("write pty failed: {e}"))?;
        Ok(ToolResult::success(format!("wrote to pty {pty_id}")))
    }
}

impl Tool for TerminalReadTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "terminal.read".into(), description: "Read bytes from a runtime PTY.".into(),
        parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"clear":{"type":"boolean"}},"required":["pty_id"]}"#.into(),
    }}
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err("terminal.read requires a RuntimeTransport routing context".into()) }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = arg(args, "pty_id").ok_or("missing pty_id")?;
        let clear = args.contains("\"clear\":true");
        let transport = routed(ctx, runtime_id)?;
        let response = super::context::block_on_transport(transport.read_pty(PtyReadRequest { runtime_id: runtime_id.into(), pty_id: pty_id.clone(), clear }))
            .map_err(|e| format!("read pty failed: {e}"))?;
        Ok(ToolResult::success(format!("pty {pty_id} output ({} bytes):\n{}", response.data.len(), String::from_utf8_lossy(&response.data))))
    }
}
