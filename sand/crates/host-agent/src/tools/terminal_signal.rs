use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};
use spark_transport::{PtyResizeRequest, PtySignalRequest, PtyWriteRequest};

pub struct TerminalResizeTool;
pub struct TerminalSignalTool;
pub struct TerminalCloseTool;
impl TerminalResizeTool { pub fn new() -> Self { Self } }
impl TerminalSignalTool { pub fn new() -> Self { Self } }
impl TerminalCloseTool { pub fn new() -> Self { Self } }

fn arg(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let rest = json.split_once(&marker)?.1.trim_start().strip_prefix('"')?;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped { escaped = false; }
        else if ch == '\\' { escaped = true; }
        else if ch == '"' { return Some(rest[..index].to_string()); }
    }
    None
}
fn number(json: &str, key: &str, default: u16) -> u16 {
    let marker = format!("\"{key}\":");
    json.split_once(&marker).and_then(|(_, v)| v.trim_start().split(|c: char| !c.is_ascii_digit()).next()).and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn routed(ctx: Option<&ToolExecutionContext>, runtime_id: &str) -> Result<std::sync::Arc<dyn spark_transport::RuntimeTransport>, String> {
    let ctx = ctx.ok_or("terminal tools require a RuntimeTransport routing context")?;
    ctx.transport_for_runtime(runtime_id).ok_or_else(|| format!("no transport for runtime {runtime_id}"))
}
fn signal_number(name: &str) -> i32 {
    match name.to_ascii_uppercase().as_str() {
        "SIGINT" | "CTRLC" | "CTRL-C" => 2,
        "SIGTERM" => 15,
        "SIGKILL" => 9,
        "SIGTSTP" | "CTRLZ" | "CTRL-Z" => 20,
        "SIGHUP" => 1,
        "SIGQUIT" => 3,
        "SIGSTOP" => 19,
        _ => name.parse().unwrap_or(2),
    }
}

impl Tool for TerminalResizeTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "terminal.resize".into(), description: "Resize a runtime PTY.".into(),
        parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"cols":{"type":"number"},"rows":{"type":"number"}},"required":["pty_id"]}"#.into(),
    }}
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err("terminal.resize requires a RuntimeTransport routing context".into()) }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = arg(args, "pty_id").ok_or("missing pty_id")?;
        let transport = routed(ctx, runtime_id)?;
        super::context::block_on_transport(transport.resize_pty(PtyResizeRequest { runtime_id: runtime_id.into(), pty_id: pty_id.clone(), cols: number(args, "cols", 80), rows: number(args, "rows", 24) }))
            .map_err(|e| format!("resize failed: {e}"))?;
        Ok(ToolResult::success(format!("resized pty {pty_id}")))
    }
}

impl Tool for TerminalSignalTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "terminal.signal".into(), description: "Send a signal to a runtime PTY.".into(),
        parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"signal":{"type":"string"}},"required":["pty_id"]}"#.into(),
    }}
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err("terminal.signal requires a RuntimeTransport routing context".into()) }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = arg(args, "pty_id").ok_or("missing pty_id")?;
        let signal = arg(args, "signal").unwrap_or_else(|| "SIGINT".into());
        let number = signal_number(&signal);
        let transport = routed(ctx, runtime_id)?;
        if signal.eq_ignore_ascii_case("CTRLD") || signal.eq_ignore_ascii_case("CTRL-D") {
            super::context::block_on_transport(transport.write_pty(PtyWriteRequest { runtime_id: runtime_id.into(), pty_id: pty_id.clone(), data: vec![0x04] }))
                .map_err(|e| format!("send EOF failed: {e}"))?;
        } else {
            super::context::block_on_transport(transport.signal_pty(PtySignalRequest { runtime_id: runtime_id.into(), pty_id: pty_id.clone(), signal: number }))
                .map_err(|e| format!("signal failed: {e}"))?;
        }
        Ok(ToolResult::success(format!("sent {signal} to pty {pty_id}")))
    }
}

impl Tool for TerminalCloseTool {
    fn definition(&self) -> ToolDefinition { ToolDefinition {
        name: "terminal.close".into(), description: "Close a runtime PTY.".into(),
        parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"}},"required":["pty_id"]}"#.into(),
    }}
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err("terminal.close requires a RuntimeTransport routing context".into()) }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = arg(args, "pty_id").ok_or("missing pty_id")?;
        let transport = routed(ctx, runtime_id)?;
        super::context::block_on_transport(transport.close_pty(runtime_id, &pty_id)).map_err(|e| format!("close failed: {e}"))?;
        Ok(ToolResult::success(format!("closed pty {pty_id}")))
    }
}
