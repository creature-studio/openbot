//! Browser tools are runtime-scoped. Browser worker, Chrome and its screenshot
//! bytes always stay behind RuntimeTransport (local UDS or SSH bridge).

use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};
use spark_transport::{BrowserAction, BrowserRequest};

pub struct BrowserOpenTool;
pub struct BrowserSnapshotTool;
pub struct BrowserClickTool;
pub struct BrowserFillTool;
pub struct BrowserScreenshotTool;
pub struct BrowserTabsTool;
pub struct BrowserPressTool;

fn arg(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let rest = json.split_once(&marker)?.1.trim_start().strip_prefix('"')?;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped { escaped = false; }
        else if ch == '\\' { escaped = true; }
        else if ch == '"' {
            return Some(rest[..index].replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\"));
        }
    }
    None
}
fn routed(ctx: Option<&ToolExecutionContext>, runtime_id: &str) -> Result<std::sync::Arc<dyn spark_transport::RuntimeTransport>, String> {
    let ctx = ctx.ok_or("browser tools require a RuntimeTransport routing context")?;
    ctx.transport_for_runtime(runtime_id).ok_or_else(|| format!("no transport for runtime {runtime_id}"))
}
fn run(ctx: Option<&ToolExecutionContext>, runtime_id: &str, action: BrowserAction) -> Result<ToolResult, String> {
    let transport = routed(ctx, runtime_id)?;
    let response = super::context::block_on_transport(transport.browser_request(BrowserRequest { runtime_id: runtime_id.into(), action }))
        .map_err(|e| format!("browser request failed: {e}"))?;
    if let Some(error) = response.error { return Ok(ToolResult::error(error, Some("BROWSER_FAILED".into()))); }
    let content = if let Some(snapshot) = response.snapshot {
        snapshot
    } else if let Some(frame) = response.frame {
        format!("frame_id={} {}x{} {} bytes {}", frame.frame_id, frame.width, frame.height, frame.data.len(), frame.format)
    } else if let Some(tabs) = response.tabs {
        tabs.into_iter().map(|tab| format!("{} {} {}", tab.id, tab.title, tab.url)).collect::<Vec<_>>().join("\n")
    } else { "ok".into() };
    Ok(ToolResult::success(content))
}

macro_rules! browser_tool {
    ($type:ty, $name:literal, $description:literal, $schema:literal, $action:expr) => {
        impl Tool for $type {
            fn definition(&self) -> ToolDefinition { ToolDefinition { name: $name.into(), description: $description.into(), parameters_schema: $schema.into() } }
            fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> { Err(format!("{} requires a RuntimeTransport routing context", $name)) }
            fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> { run(ctx, runtime_id, $action(args)?) }
        }
    };
}

browser_tool!(BrowserOpenTool, "browser.open", "Open a URL in the runtime browser.", r#"{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}"#, |args: &str| Ok(BrowserAction::Open { url: arg(args, "url").ok_or("missing url")? }));
browser_tool!(BrowserSnapshotTool, "browser.snapshot", "Get the semantic snapshot of the runtime browser.", r#"{"type":"object"}"#, |_args: &str| Ok(BrowserAction::Snapshot));
browser_tool!(BrowserClickTool, "browser.click", "Click a semantic browser reference.", r#"{"type":"object","properties":{"ref":{"type":"string"}},"required":["ref"]}"#, |args: &str| Ok(BrowserAction::Click { reference: arg(args, "ref").or_else(|| arg(args, "reference")).ok_or("missing ref")? }));
browser_tool!(BrowserFillTool, "browser.fill", "Fill a semantic browser reference.", r#"{"type":"object","properties":{"ref":{"type":"string"},"text":{"type":"string"}},"required":["ref","text"]}"#, |args: &str| Ok(BrowserAction::Fill { reference: arg(args, "ref").ok_or("missing ref")?, text: arg(args, "text").ok_or("missing text")? }));
browser_tool!(BrowserScreenshotTool, "browser.screenshot", "Capture the latest browser frame.", r#"{"type":"object"}"#, |_args: &str| Ok(BrowserAction::Screenshot { format: "png".into(), quality: 85 }));
browser_tool!(BrowserTabsTool, "browser.tabs", "List runtime browser tabs.", r#"{"type":"object"}"#, |_args: &str| Ok(BrowserAction::Tabs));
browser_tool!(BrowserPressTool, "browser.press", "Press a key in the runtime browser.", r#"{"type":"object","properties":{"key":{"type":"string"}},"required":["key"]}"#, |args: &str| Ok(BrowserAction::Press { key: arg(args, "key").ok_or("missing key")? }));
