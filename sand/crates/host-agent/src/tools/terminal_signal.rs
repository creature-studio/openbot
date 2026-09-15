use super::{Tool, ToolDefinition, ToolExecutionContext, ToolResult};

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
        self.execute_with_context(args, runtime_id, None)
    }

    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        let cols = extract_number(args, "cols").unwrap_or(80);
        let rows = extract_number(args, "rows").unwrap_or(24);

        // The PTY lives inside a runtime on some machine; resizing it must go to
        // that same sandd, or the window size would apply somewhere else.
        if let Some(ctx) = ctx {
            if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                let request = spark_transport::PtyResizeRequest {
                    runtime_id: runtime_id.to_string(),
                    pty_id: pty_id.clone(),
                    cols: cols as u16,
                    rows: rows as u16,
                };
                return match super::context::block_on_transport(transport.resize_pty(request)) {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "resized {} to {}x{} on machine {} (SIGWINCH sent)",
                        pty_id, cols, rows, ctx.machine_for_runtime(runtime_id)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("resize failed: {}", e), None)),
                };
            }
        }

        match self.client.resize_pty(runtime_id, &pty_id, cols as u16, rows as u16) {
            Ok(resp) => Ok(ToolResult::success(format!("resized {} to {}x{}: {}", pty_id, cols, rows, resp))),
            Err(e) => Ok(ToolResult::error(format!("resize failed: {}", e), None)),
        }
    }
}

impl Tool for TerminalSignalTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "terminal.signal".to_string(),
            description: "Send signal to PTY: Ctrl+C = SIGINT (2), Ctrl+D via EOF, SIGTERM (15), SIGKILL (9), SIGTSTP Ctrl+Z (20). Supports signal names: SIGINT, CtrlC, CtrlD, SIGTERM, etc. Handles raw mode and ANSI. Uses tcgetpgrp+kill(-pgid) for foreground group.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"signal":{"type":"string","description":"signal number or name: 2/SIGINT/CtrlC, 15/SIGTERM, 9/SIGKILL, 20/CtrlZ, CtrlD"}},"required":["pty_id"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }

    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        let signal = extract_arg(args, "signal").unwrap_or_else(|| "SIGINT".to_string());

        // Signals must reach the PTY *inside the runtime*, on whichever machine
        // that runtime lives on. Ctrl+C/D go as raw bytes on the pty stream
        // (that is what a real terminal sends); named signals go as numbers.
        if let Some(ctx) = ctx {
            if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                let lowered = signal.to_lowercase();
                if matches!(lowered.as_str(), "ctrld" | "ctrl_d" | "eof" | "4" | "\u{4}") {
                    let request = spark_transport::PtyWriteRequest {
                        runtime_id: runtime_id.to_string(),
                        pty_id: pty_id.clone(),
                        data: vec![0x04],
                    };
                    return match super::context::block_on_transport(transport.write_pty(request)) {
                        Ok(()) => Ok(ToolResult::success(format!(
                            "sent Ctrl+D (EOF 0x04) to {} on machine {}",
                            pty_id, ctx.machine_for_runtime(runtime_id)
                        ))),
                        Err(e) => Ok(ToolResult::error(format!("CtrlD failed: {}", e), None)),
                    };
                }
                if matches!(lowered.as_str(), "ctrlc" | "ctrl_c") {
                    let request = spark_transport::PtyWriteRequest {
                        runtime_id: runtime_id.to_string(),
                        pty_id: pty_id.clone(),
                        data: vec![0x03],
                    };
                    if super::context::block_on_transport(transport.write_pty(request)).is_ok() {
                        return Ok(ToolResult::success(format!(
                            "sent Ctrl+C (0x03) to {} on machine {}",
                            pty_id, ctx.machine_for_runtime(runtime_id)
                        )));
                    }
                }
                if let Some(number) = signal_number(&signal) {
                    let request = spark_transport::PtySignalRequest {
                        runtime_id: runtime_id.to_string(),
                        pty_id: pty_id.clone(),
                        signal: number,
                    };
                    return match super::context::block_on_transport(transport.signal_pty(request)) {
                        Ok(()) => Ok(ToolResult::success(format!(
                            "signal {} ({}) to {} on machine {}",
                            signal, number, pty_id, ctx.machine_for_runtime(runtime_id)
                        ))),
                        Err(e) => Ok(ToolResult::error(format!("signal failed: {}", e), None)),
                    };
                }
                return Ok(ToolResult::error(
                    format!("unknown signal: {}", signal),
                    Some("BAD_SIGNAL".to_string()),
                ));
            }
        }

        if signal.to_lowercase() == "ctrld" || signal.to_lowercase() == "ctrl_d" || signal == "\x04" || signal == "4" || signal.to_lowercase() == "eof" {
            match self.client.write_pty_binary(runtime_id, &pty_id, &[0x04]) {
                Ok(resp) => return Ok(ToolResult::success(format!("sent Ctrl+D (EOF 0x04) to {}: {}", pty_id, resp))),
                Err(e) => return Ok(ToolResult::error(format!("CtrlD failed: {}", e), None)),
            }
        }
        if signal.to_lowercase() == "ctrlc" || signal.to_lowercase() == "ctrl_c" {
            if let Ok(resp) = self.client.signal_pty_str(runtime_id, &pty_id, "SIGINT") {
                if resp.contains("\"ok\":true") {
                    return Ok(ToolResult::success(format!("sent Ctrl+C SIGINT to {}: {}", pty_id, resp)));
                }
            }
            match self.client.write_pty_binary(runtime_id, &pty_id, &[0x03]) {
                Ok(resp) => return Ok(ToolResult::success(format!("sent Ctrl+C (0x03) to {}: {}", pty_id, resp))),
                Err(e) => return Ok(ToolResult::error(format!("CtrlC failed: {}", e), None)),
            }
        }
        let resp = if signal.parse::<i32>().is_ok() {
            let num: i32 = signal.parse().unwrap();
            self.client.signal_pty(runtime_id, &pty_id, num)
        } else {
            self.client.signal_pty_str(runtime_id, &pty_id, &signal)
        };
        match resp {
            Ok(r) => Ok(ToolResult::success(format!("signal {} to {}: {}", signal, pty_id, r))),
            Err(e) => Ok(ToolResult::error(format!("signal failed: {}", e), None)),
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
        self.execute_with_context(args, runtime_id, None)
    }

    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        if let Some(ctx) = ctx {
            if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                return match super::context::block_on_transport(
                    transport.close_pty(runtime_id, &pty_id),
                ) {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "closed {} on machine {}",
                        pty_id,
                        ctx.machine_for_runtime(runtime_id)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("close failed: {}", e), None)),
                };
            }
        }
        match self.client.close_pty(runtime_id, &pty_id) {
            Ok(resp) => Ok(ToolResult::success(format!("closed {}: {}", pty_id, resp))),
            Err(e) => Ok(ToolResult::error(format!("close failed: {}", e), None)),
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

/// Numeric value of a signal the model may name, matching `kill(2)`.
fn signal_number(signal: &str) -> Option<i32> {
    let upper = signal.trim().to_uppercase();
    if let Ok(number) = upper.parse::<i32>() {
        return Some(number);
    }
    Some(match upper.trim_start_matches("SIG") {
        "INT" | "CTRLC" => 2,
        "QUIT" => 3,
        "KILL" => 9,
        "TERM" => 15,
        "TSTP" | "CTRLZ" => 20,
        "CONT" => 18,
        "HUP" => 1,
        "USR1" => 10,
        "USR2" => 12,
        _ => return None,
    })
}
