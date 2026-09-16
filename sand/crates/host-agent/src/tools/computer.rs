//! Computer-use actions are routed to the desktop owned by the runtime. The
//! client never points these operations at its own display.

use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};
use spark_transport::{ComputerAction, ComputerRequest};

pub struct ComputerScreenshotTool;
pub struct ComputerClickTool;
pub struct ComputerTypeTool;
pub struct ComputerMoveTool;
pub struct ComputerKeyTool;
pub struct ComputerScrollTool;

fn arg(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let rest = json.split_once(&marker)?.1.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
fn number(json: &str, key: &str, default: i32) -> i32 {
    let marker = format!("\"{key}\":");
    json.split_once(&marker).and_then(|(_, rest)| rest.trim_start().split(|c: char| !c.is_ascii_digit() && c != '-').next()).and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn routed(ctx: Option<&ToolExecutionContext>, runtime_id: &str) -> Result<std::sync::Arc<dyn spark_transport::RuntimeTransport>, String> {
    let ctx = ctx.ok_or("computer tools require a RuntimeTransport routing context")?;
    let transport = ctx.transport_for_runtime(runtime_id).ok_or_else(|| format!("no transport for runtime {runtime_id}"))?;
    let machine_id = ctx
        .machine_for_runtime(runtime_id)
        .ok_or_else(|| format!("unknown machine ownership for runtime {runtime_id}"))?;
    if let Some(machine) = ctx.machine(&machine_id) {
        if !machine.capabilities.computer_use && !machine.capabilities.desktop {
            return Err(format!("computer use is not available on machine {}", machine.id));
        }
    }
    Ok(transport)
}
fn run(ctx: Option<&ToolExecutionContext>, runtime_id: &str, action: ComputerAction) -> Result<ToolResult, String> {
    let transport = routed(ctx, runtime_id)?;
    let response = super::context::block_on_transport(transport.computer_request(ComputerRequest { runtime_id: runtime_id.into(), action }))
        .map_err(|e| format!("computer request failed: {e}"))?;
    if let Some(error) = response.error { return Ok(ToolResult::error(error, Some("COMPUTER_FAILED".into()))); }
    Ok(ToolResult::success(response.frame.map(|frame| format!("frame_id={} {}x{} {} bytes", frame.frame_id, frame.width, frame.height, frame.data.len())).unwrap_or_else(|| "ok".into())))
}

macro_rules! computer_tool {
    ($type:ty, $name:literal, $description:literal, $schema:literal, $action:expr) => {
        impl Tool for $type {
            fn definition(&self) -> ToolDefinition { ToolDefinition { name: $name.into(), description: $description.into(), parameters_schema: $schema.into() } }
            fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err(format!("{} requires a RuntimeTransport routing context", $name)) }
            fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> { run(ctx, runtime_id, $action(args)?) }
        }
    };
}

computer_tool!(ComputerScreenshotTool, "computer.screenshot", "Capture the remote runtime desktop.", r#"{"type":"object"}"#, |_args: &str| Ok(ComputerAction::Screenshot));
computer_tool!(ComputerClickTool, "computer.click", "Click on the remote runtime desktop.", r#"{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"button":{"type":"string"}},"required":["x","y"]}"#, |args: &str| Ok(ComputerAction::Click { x: number(args, "x", 0), y: number(args, "y", 0), button: arg(args, "button").unwrap_or_else(|| "left".into()) }));
computer_tool!(ComputerTypeTool, "computer.type", "Type into the remote runtime desktop.", r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#, |args: &str| Ok(ComputerAction::Type { text: arg(args, "text").ok_or("missing text")? }));
computer_tool!(ComputerMoveTool, "computer.move", "Move the remote runtime pointer.", r#"{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]}"#, |args: &str| Ok(ComputerAction::Move { x: number(args, "x", 0), y: number(args, "y", 0) }));
computer_tool!(ComputerKeyTool, "computer.key", "Press a key on the remote runtime desktop.", r#"{"type":"object","properties":{"key":{"type":"string"}},"required":["key"]}"#, |args: &str| Ok(ComputerAction::Key { key: arg(args, "key").ok_or("missing key")? }));
computer_tool!(ComputerScrollTool, "computer.scroll", "Scroll the remote runtime desktop.", r#"{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"delta":{"type":"number"}},"required":["delta"]}"#, |args: &str| Ok(ComputerAction::Scroll { x: number(args, "x", 0), y: number(args, "y", 0), delta: number(args, "delta", 0) }));
