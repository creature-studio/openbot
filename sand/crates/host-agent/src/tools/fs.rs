//! Agent filesystem tools. Every operation is an FsRequest on the runtime's
//! transport; neither local disk APIs nor SFTP are allowed here. This is what
//! keeps a local task and an SSH task semantically identical.

use std::path::PathBuf;

use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};
use spark_transport::{FsRequest, FsResponse};

pub struct FsReadTool;
pub struct FsWriteTool;
pub struct FsListTool;
pub struct FsSearchTool;
pub struct FsStatTool;
pub struct FsPatchTool;
pub struct FsMkdirTool;
pub struct FsRemoveTool;
pub struct FsRenameTool;
pub struct FsGlobTool;

macro_rules! transport_tool {
    ($name:literal, $description:literal, $schema:literal, $request:expr, $runtime_id:expr, $ctx:expr) => {{
        let ctx = $ctx.ok_or("filesystem tools require a RuntimeTransport routing context")?;
        let transport = ctx
            .transport_for_runtime($runtime_id)
            .ok_or_else(|| format!("no transport for runtime {}", $runtime_id))?;
        let response = super::context::block_on_transport(
            transport.fs_request($runtime_id, $request),
        )
        .map_err(|error| format!("filesystem request failed: {error}"))?;
        Ok(fs_result(response))
    }};
}

fn fs_result(response: FsResponse) -> ToolResult {
    match response {
        FsResponse::Ok { data } => ToolResult::success(String::from_utf8_lossy(&data).to_string()),
        FsResponse::Error { message } => ToolResult::error(message, Some("FS_FAILED".to_string())),
    }
}

fn arg(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let rest = json.split_once(&marker)?.1.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(
                rest[..index]
                    .replace("\\n", "\n")
                    .replace("\\r", "\r")
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\"),
            );
        }
    }
    None
}

fn bool_arg(json: &str, key: &str, default: bool) -> bool {
    let marker = format!("\"{key}\":");
    json.split_once(&marker)
        .map(|(_, rest)| rest.trim_start().starts_with("true"))
        .unwrap_or(default)
}

fn required(args: &str, key: &str) -> Result<String, String> {
    arg(args, key).ok_or_else(|| format!("missing {key}"))
}

macro_rules! impl_simple_tool {
    ($type:ty, $tool_name:literal, $description:literal, $schema:literal, $body:expr) => {
        impl Tool for $type {
            fn definition(&self) -> ToolDefinition {
                ToolDefinition {
                    name: $tool_name.to_string(),
                    description: $description.to_string(),
                    parameters_schema: $schema.to_string(),
                }
            }
            fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
                Err(format!("{} requires a RuntimeTransport routing context", $tool_name))
            }
            fn execute_with_context(
                &self,
                args: &str,
                runtime_id: &str,
                ctx: Option<&ToolExecutionContext>,
            ) -> Result<ToolResult, String> {
                $body(args, runtime_id, ctx)
            }
        }
    };
}

impl_simple_tool!(
    FsReadTool,
    "file.read",
    "Read a file in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        transport_tool!("file.read", "", "", FsRequest::Read { path: PathBuf::from(path) }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsWriteTool,
    "file.write",
    "Write a file in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        let content = required(args, "content")?;
        transport_tool!("file.write", "", "", FsRequest::Write { path: PathBuf::from(path), data: content.into_bytes() }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsListTool,
    "file.list",
    "List files in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        transport_tool!("file.list", "", "", FsRequest::List { path: PathBuf::from(path) }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsSearchTool,
    "file.search",
    "Search file contents in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"}},"required":["path","query"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        let query = required(args, "query").or_else(|_| required(args, "pattern"))?;
        transport_tool!("file.search", "", "", FsRequest::Search { path: PathBuf::from(path), query }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsStatTool,
    "file.stat",
    "Inspect a file in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        transport_tool!("file.stat", "", "", FsRequest::Stat { path: PathBuf::from(path) }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsPatchTool,
    "file.patch",
    "Apply a patch in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"},"patch":{"type":"string"}},"required":["path","patch"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        let patch = required(args, "patch")?;
        transport_tool!("file.patch", "", "", FsRequest::Patch { path: PathBuf::from(path), patch: patch.into_bytes() }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsMkdirTool,
    "file.mkdir",
    "Create a directory in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        transport_tool!("file.mkdir", "", "", FsRequest::Mkdir { path: PathBuf::from(path), recursive: true }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsRemoveTool,
    "file.remove",
    "Remove a file or directory in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"},"recursive":{"type":"boolean"}},"required":["path"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let path = required(args, "path")?;
        if path == "/" || path.is_empty() {
            return Ok(ToolResult::permission_required("file.remove".to_string(), "refusing to remove the workspace root".to_string()));
        }
        transport_tool!("file.remove", "", "", FsRequest::Remove { path: PathBuf::from(path), recursive: bool_arg(args, "recursive", false) }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsRenameTool,
    "file.rename",
    "Rename a file in the runtime workspace.",
    r#"{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"}},"required":["from","to"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let from = required(args, "from")?;
        let to = required(args, "to")?;
        transport_tool!("file.rename", "", "", FsRequest::Rename { from: PathBuf::from(from), to: PathBuf::from(to) }, runtime_id, ctx)
    }
);

impl_simple_tool!(
    FsGlobTool,
    "file.glob",
    "Find paths matching a glob in the runtime workspace.",
    r#"{"type":"object","properties":{"path":{"type":"string"},"pattern":{"type":"string"}},"required":["pattern"]}"#,
    |args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>| {
        let pattern = required(args, "pattern")?;
        let path = arg(args, "path").unwrap_or_else(|| ".".to_string());
        transport_tool!("file.glob", "", "", FsRequest::Glob { path: PathBuf::from(path), pattern }, runtime_id, ctx)
    }
);
