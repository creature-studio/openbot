use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};
use spark_transport::ExecRequest;

/// Shell execution is deliberately transport-only. There is no local fallback:
/// a missing routing context must fail instead of accidentally executing on the
/// host-agent machine (which would violate the task's machine pin).
pub struct ShellExecTool;

impl ShellExecTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ShellExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell.exec".to_string(),
            description: "Execute a shell command inside the selected runtime.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}"#.to_string(),
        }
    }

    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        Err("shell.exec requires a RuntimeTransport routing context".to_string())
    }

    fn execute_with_context(
        &self,
        args: &str,
        runtime_id: &str,
        ctx: Option<&ToolExecutionContext>,
    ) -> Result<ToolResult, String> {
        let command = extract_arg(args, "command").ok_or("missing command")?;
        let destructive_patterns = [
            "rm -rf /",
            "rm -r /",
            "mkfs",
            "dd if=",
            ":(){:|:&};:",
            "chmod -R 777 /",
            "> /dev/sda",
        ];
        if let Some(pattern) = destructive_patterns
            .iter()
            .find(|pattern| command.contains(**pattern))
        {
            return Ok(ToolResult::permission_required(
                "shell.exec".to_string(),
                format!("destructive command pattern: {pattern}"),
            ));
        }

        let ctx = ctx.ok_or("shell.exec requires a RuntimeTransport routing context")?;
        let transport = ctx
            .transport_for_runtime(runtime_id)
            .ok_or_else(|| format!("no transport for runtime {runtime_id}"))?;
        let result = super::context::block_on_transport(transport.exec(ExecRequest {
            runtime_id: runtime_id.to_string(),
            command: vec!["bash".to_string(), "-lc".to_string(), command],
            cwd: None,
            env: std::collections::HashMap::new(),
            timeout_ms: Some(30_000),
            stdin_data: None,
        }))
        .map_err(|error| format!("exec failed for runtime {runtime_id}: {error}"))?;

        let content = format!(
            "exit_code: {}\nstdout ({} bytes):\n{}\nstderr:\n{}",
            result.exit_code.unwrap_or(-1),
            result.stdout.len(),
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
        );
        let mut output = if result.exit_code == Some(0) {
            ToolResult::success(content)
        } else {
            ToolResult::error(content, Some("PROCESS_EXIT_NONZERO".to_string()))
        };
        output.tool_name = "shell.exec".to_string();
        output.duration_ms = result.duration_ms;
        Ok(output)
    }
}

fn extract_arg(json: &str, key: &str) -> Option<String> {
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
            let value = &rest[..index];
            return Some(
                value
                    .replace("\\n", "\n")
                    .replace("\\r", "\r")
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\"),
            );
        }
    }
    None
}
