use super::{Tool, ToolDefinition, ToolResult};

pub struct TerminalResizeTool {
    client: sand_client::SandClient,
}
pub struct TerminalSignalTool {
    client: sand_client::SandClient,
}
pub struct TerminalCloseTool {
    client: sand_client::SandClient,
}

impl TerminalResizeTool {
    pub fn new() -> Self { Self { client: sand_client::SandClient::new(None) } }
}
impl TerminalSignalTool {
    pub fn new() -> Self { Self { client: sand_client::SandClient::new(None) } }
}
impl TerminalCloseTool {
    pub fn new() -> Self { Self { client: sand_client::SandClient::new(None) } }
}

impl Tool for TerminalResizeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "terminal.resize".to_string(),
            description: "Resize PTY terminal (cols, rows). Sends SIGWINCH.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"cols":{"type":"number"},"rows":{"type":"number"}},"required":["pty_id"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        let cols = extract_number(args, "cols").unwrap_or(80);
        let rows = extract_number(args, "rows").unwrap_or(24);
        match self.client.resize_pty(runtime_id, &pty_id, cols as u16, rows as u16) {
            Ok(resp) => Ok(ToolResult { content: format!("resized {} to {}x{}: {}", pty_id, cols, rows, resp), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("resize failed: {}", e), is_error: true }),
        }
    }
}

impl Tool for TerminalSignalTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "terminal.signal".to_string(),
            description: "Send signal to PTY: Ctrl+C = SIGINT (2), Ctrl+D via EOF, SIGTERM (15), SIGKILL (9), SIGTSTP Ctrl+Z (20). Supports signal names: SIGINT, CtrlC, CtrlD, SIGTERM, etc. Handles raw mode and ANSI.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"signal":{"type":"string","description":"signal number or name: 2/SIGINT/CtrlC, 15/SIGTERM, 9/SIGKILL, 20/CtrlZ, CtrlD"}},"required":["pty_id"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        let signal = extract_arg(args, "signal").unwrap_or_else(|| "SIGINT".to_string());
        // Handle CtrlD specially: send EOF byte 0x04 via WritePty binary
        if signal.to_lowercase() == "ctrld" || signal.to_lowercase() == "ctrl_d" || signal == "\x04" || signal == "4" || signal.to_lowercase() == "eof" {
            match self.client.write_pty_binary(runtime_id, &pty_id, &[0x04]) {
                Ok(resp) => return Ok(ToolResult { content: format!("sent Ctrl+D (EOF 0x04) to {}: {}", pty_id, resp), is_error: false }),
                Err(e) => return Ok(ToolResult { content: format!("CtrlD failed: {}", e), is_error: true }),
            }
        }
        // Handle CtrlC as signal or byte
        if signal.to_lowercase() == "ctrlc" || signal.to_lowercase() == "ctrl_c" {
            // Try signal first, fallback to byte 0x03
            if let Ok(resp) = self.client.signal_pty_str(runtime_id, &pty_id, "SIGINT") {
                if resp.contains("\"ok\":true") {
                    return Ok(ToolResult { content: format!("sent Ctrl+C SIGINT to {}: {}", pty_id, resp), is_error: false });
                }
            }
            // Fallback to byte
            match self.client.write_pty_binary(runtime_id, &pty_id, &[0x03]) {
                Ok(resp) => return Ok(ToolResult { content: format!("sent Ctrl+C (0x03) to {}: {}", pty_id, resp), is_error: false }),
                Err(e) => return Ok(ToolResult { content: format!("CtrlC failed: {}", e), is_error: true }),
            }
        }
        // Generic signal
        let resp = if signal.parse::<i32>().is_ok() {
            let num: i32 = signal.parse().unwrap();
            self.client.signal_pty(runtime_id, &pty_id, num)
        } else {
            self.client.signal_pty_str(runtime_id, &pty_id, &signal)
        };
        match resp {
            Ok(r) => Ok(ToolResult { content: format!("signal {} to {}: {}", signal, pty_id, r), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("signal failed: {}", e), is_error: true }),
        }
    }
}

impl Tool for TerminalCloseTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "terminal.close".to_string(),
            description: "Close PTY terminal.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"}},"required":["pty_id"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        match self.client.close_pty(runtime_id, &pty_id) {
            Ok(resp) => Ok(ToolResult { content: format!("closed {}: {}", pty_id, resp), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("close failed: {}", e), is_error: true }),
        }
    }
}

fn extract_arg(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)?;
    let rest = json[start+pat.len()..].trim_start();
    if rest.starts_with('"') {
        let mut end = None;
        let mut escaped = false;
        for (i, c) in rest[1..].char_indices() {
            if escaped { escaped=false; continue; }
            if c=='\\' { escaped=true; continue; }
            if c=='"' { end=Some(i); break; }
        }
        let end = end?;
        let raw = &rest[1..1+end];
        Some(raw.replace("\\n","\n").replace("\\\"","\"").replace("\\\\","\\"))
    } else { None }
}

fn extract_number(json: &str, key: &str) -> Option<i32> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)?;
    let rest = json[start+pat.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c!='-').unwrap_or(rest.len());
    rest[..end].parse().ok()
}
