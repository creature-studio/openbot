use super::{Tool, ToolDefinition, ToolResult, ToolExecutionContext};
use spark_transport::ExecRequest;

pub struct ShellExecTool {
    sand_client: sand_client::SandClient,
}

impl ShellExecTool {
    pub fn new() -> Self {
        Self { sand_client: sand_client::SandClient::new(None) }
    }
}

impl Tool for ShellExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell.exec".to_string(),
            description: "Execute a shell command in the runtime. Returns stdout, stderr, exit code. Destructive commands like rm -rf, git push require approval.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"command":{"type":"string","description":"Shell command to execute, e.g. 'cargo test' or 'ls -la'"}},"required":["command"]}"#.to_string(),
        }
    }

    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }

    fn execute_with_context(
        &self,
        args: &str,
        runtime_id: &str,
        ctx: Option<&ToolExecutionContext>,
    ) -> Result<ToolResult, String> {
        let command = extract_arg(args, "command").ok_or("missing command")?;

        // Permission / destructive check
        let destructive_patterns = ["rm -rf /", "rm -r /", "mkfs", "dd if=", ":(){:|:&};:", "chmod -R 777 /", "> /dev/sda"];
        for pat in destructive_patterns {
            if command.contains(pat) {
                return Ok(super::ToolResult::permission_required("shell.exec".to_string(), format!("destructive command '{}' pattern '{}'", command, pat)));
            }
        }
        let ask_patterns = ["git push", "secret", "external"];
        for pat in ask_patterns {
            if command.contains(pat) {
                eprintln!("[permission] ask required for: {} pattern {}", command, pat);
            }
        }

        let start = std::time::SystemTime::now();

        // Try routing through transport if context available
        if let Some(ctx) = ctx {
            // The runtime decides the machine; the tool does not care which.
            if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                let exec_req = ExecRequest {
                    runtime_id: runtime_id.to_string(),
                    command: vec!["bash".to_string(), "-lc".to_string(), command.clone()],
                    cwd: None,
                    env: std::collections::HashMap::new(),
                    timeout_ms: Some(30000),
                    stdin_data: None,
                };
                // Use tokio runtime to block on async call
                let result = super::context::block_on_transport(transport.exec(exec_req));
                if let Ok(result) = result {
                    let duration = start.elapsed().unwrap_or_default().as_millis() as u64;
                    let content = format!("exit_code: {}\nstdout (transport, {} bytes):\n{}\nstderr:\n{}",
                        result.exit_code.unwrap_or(-1),
                        result.stdout.len(),
                        String::from_utf8_lossy(&result.stdout),
                        String::from_utf8_lossy(&result.stderr));
                    let mut tool_result = if result.exit_code == Some(0) {
                        super::ToolResult::success(content)
                    } else {
                        super::ToolResult::error(content, Some("PROCESS_EXIT_NONZERO".to_string()))
                    };
                    tool_result.tool_name = "shell.exec".to_string();
                    tool_result.duration_ms = duration;
                    return Ok(tool_result);
                }
                return Ok(super::ToolResult::error(
                    format!(
                        "exec failed on machine {}: {}",
                        ctx.machine_for_runtime(runtime_id),
                        result.err().map(|e| e.to_string()).unwrap_or_default()
                    ),
                    Some("EXEC_FAILED".to_string()),
                ));
            }
        }

        // No machine registry (legacy CLI path): talk to the local sandd. This
        // is the only place a tool touches SandClient directly.
        let cmd_vec = vec!["bash".to_string(), "-lc".to_string(), command.clone()];
        match self.sand_client.exec_binary(runtime_id, cmd_vec) {
            Ok((json_resp, stdout, stderr)) => {
                let exit_code = extract_number_field(&json_resp, "exit_code").unwrap_or(0);
                let duration = start.elapsed().unwrap_or_default().as_millis() as u64;
                let content = format!("exit_code: {}\nstdout (binary RPC, {} bytes, no base64):\n{}\nstderr:\n{}",
                    exit_code, stdout.len(), String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
                let mut result = if exit_code == 0 {
                    super::ToolResult::success(content)
                } else {
                    super::ToolResult::error(content, Some("PROCESS_EXIT_NONZERO".to_string()))
                };
                result.tool_name = "shell.exec".to_string();
                result.duration_ms = duration;
                Ok(result)
            }
            Err(_) => {
                // Fallback to JSON RPC
                match self.sand_client.exec(runtime_id, cmd_vec) {
                    Ok(resp) => {
                        let stdout = extract_b64_field(&resp, "stdout_b64")
                            .map(|b64| base64_decode(&b64)).unwrap_or_default();
                        let stderr = extract_b64_field(&resp, "stderr_b64")
                            .map(|b64| base64_decode(&b64)).unwrap_or_default();
                        let exit_code = extract_number_field(&resp, "exit_code").unwrap_or(0);
                        let duration = start.elapsed().unwrap_or_default().as_millis() as u64;
                        let content = format!("exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                            exit_code, String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
                        let mut result = if exit_code == 0 {
                            super::ToolResult::success(content)
                        } else {
                            super::ToolResult::error(content, Some("PROCESS_EXIT_NONZERO".to_string()))
                        };
                        result.tool_name = "shell.exec".to_string();
                        result.duration_ms = duration;
                        Ok(result)
                    }
                    Err(e) => Ok(super::ToolResult::error(format!("exec failed: {}", e), Some("EXEC_FAILED".to_string()))),
                }
            }
        }
    }
}

fn extract_arg(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)?;
    let rest = &json[start+pat.len()..].trim_start();
    if rest.starts_with('"') {
        let rest = &rest[1..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else {
        None
    }
}

fn extract_b64_field(s: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_number_field(s: &str, field: &str) -> Option<i32> {
    let pat = format!("\"{}\":", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/" .iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i]==b'\n' || bytes[i]==b'\r' || bytes[i]==b' ') { i+=1; }
        if i+3 >= bytes.len() { break; }
        let mut vals = [0u8; 4];
        let mut padding = 0;
        let mut valid = true;
        for j in 0..4 {
            if i+j >= bytes.len() { valid=false; break; }
            let b = bytes[i+j];
            if b == b'=' {
                padding+=1;
                vals[j]=0;
            } else {
                let v = table[b as usize];
                if v==255 { valid=false; break; }
                vals[j]=v;
            }
        }
        if !valid { i+=1; continue; }
        let n = ((vals[0] as u32)<<18) | ((vals[1] as u32)<<12) | ((vals[2] as u32)<<6) | (vals[3] as u32);
        out.push(((n>>16)&0xFF) as u8);
        if padding<2 { out.push(((n>>8)&0xFF) as u8); }
        if padding<1 { out.push((n&0xFF) as u8); }
        i+=4;
        if padding>0 { break; }
    }
    out
}
