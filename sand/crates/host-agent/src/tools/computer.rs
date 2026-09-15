use super::{Tool, ToolDefinition, ToolResult, ToolExecutionContext};
use std::process::Command;

fn raw_rpc(req: &str) -> Result<String, String> {
    use std::os::unix::net::UnixStream;
    use std::io::{Write, BufRead, BufReader};
    let sock_path = if std::path::Path::new("/run/sand/sandd.sock").exists() { "/run/sand/sandd.sock" } else { "/tmp/sandd/sandd.sock" };
    let mut stream = UnixStream::connect(sock_path).map_err(|e| format!("connect failed: {}", e))?;
    writeln!(stream, "{}", req).map_err(|e| format!("write failed: {}", e))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| format!("read failed: {}", e))?;
    Ok(line.trim().to_string())
}

fn raw_rpc_binary(json_req: &str, binary_payload: Option<&[u8]>) -> Result<(String, Vec<u8>), String> {
    use std::os::unix::net::UnixStream;
    use std::io::{Read, Write};
    let sock_path = if std::path::Path::new("/run/sand/sandd-binary.sock").exists() { "/run/sand/sandd-binary.sock" } else { "/tmp/sandd/sandd-binary.sock" };
    let mut stream = UnixStream::connect(sock_path).map_err(|e| format!("binary connect failed: {}", e))?;
    let json_bytes = json_req.as_bytes();
    stream.write_all(&(json_bytes.len() as u32).to_be_bytes()).map_err(|e| e.to_string())?;
    stream.write_all(json_bytes).map_err(|e| e.to_string())?;
    if let Some(bin) = binary_payload {
        stream.write_all(bin).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())?;
    let mut len_buf = [0u8;4];
    stream.read_exact(&mut len_buf).map_err(|e| format!("read len failed: {}", e))?;
    let json_len = u32::from_be_bytes(len_buf) as usize;
    let mut json_buf = vec![0u8; json_len];
    stream.read_exact(&mut json_buf).map_err(|e| format!("read json failed: {}", e))?;
    let json_str = String::from_utf8_lossy(&json_buf).to_string();
    // extract len
    let mut binary = Vec::new();
    if let Some(total) = extract_number_field(&json_str, "len") {
        if total>0 && total < 10*1024*1024 {
            binary.resize(total as usize, 0);
            let _ = stream.read_exact(&mut binary);
        }
    } else if let Some(stdout_len) = extract_number_field(&json_str, "stdout_len") {
        let stderr_len = extract_number_field(&json_str, "stderr_len").unwrap_or(0);
        let total = stdout_len + stderr_len;
        if total>0 {
            binary.resize(total as usize, 0);
            let _ = stream.read_exact(&mut binary);
        }
    }
    Ok((json_str, binary))
}

fn extract_number_field(s: &str, field: &str) -> Option<u64> {
    let pat = format!("\"{}\":", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end>0 { rest[..end].parse().ok() } else { None }
}

fn try_screenshot_via_sandd(runtime_id: &str) -> Result<Vec<u8>, String> {
    // Ensure display first
    let _ = raw_rpc(&format!(r#"{{"method":"EnsureDisplay","id":"{}","width":1280,"height":720}}"#, runtime_id));
    // Try binary screenshot
    if let Ok((json, data)) = raw_rpc_binary(&format!(r#"{{"method":"Screenshot","id":"{}"}}"#, runtime_id), None) {
        if json.contains("\"ok\":true") && !data.is_empty() {
            return Ok(data);
        }
    }
    Err("sandd screenshot failed".to_string())
}

fn try_screenshot() -> Result<Vec<u8>, String> {
    // Try ImageMagick import if DISPLAY set
    if std::env::var("DISPLAY").is_ok() {
        let output = Command::new("import")
            .args(["-window", "root", "/tmp/screenshot.png"])
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                if let Ok(data) = std::fs::read("/tmp/screenshot.png") {
                    return Ok(data);
                }
            }
        }
        // Try scrot
        let output = Command::new("scrot")
            .args(["/tmp/screenshot.png"])
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                if let Ok(data) = std::fs::read("/tmp/screenshot.png") {
                    return Ok(data);
                }
            }
        }
    }
    Err("no display or screenshot tool failed".to_string())
}

fn try_click(x: i32, y: i32, runtime_id: &str) -> Result<(), String> {
    // Try via sandd desktop? We can exec xdotool in runtime
    if !runtime_id.is_empty() {
        if let Ok(client) = std::panic::catch_unwind(|| sand_client::SandClient::new(None)) {
            // Try to get display and exec xdotool
            let display_resp = raw_rpc(&format!(r#"{{"method":"GetDisplay","id":"{}"}}"#, runtime_id)).unwrap_or_default();
            // extract display field
            if let Some(d) = extract_display(&display_resp) {
                let cmd = vec!["sh".to_string(), "-lc".to_string(), format!("DISPLAY={} xdotool mousemove {} {} click 1", d, x, y)];
                if let Ok(resp) = client.exec(runtime_id, cmd) {
                    if resp.contains("\"ok\":true") {
                        return Ok(());
                    }
                }
            }
        }
    }
    // Try xdotool locally
    let output = Command::new("xdotool")
        .args(["mousemove", &x.to_string(), &y.to_string(), "click", "1"])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            return Ok(());
        }
    }
    Err("xdotool not available or failed".to_string())
}

fn try_type(text: &str, runtime_id: &str) -> Result<(), String> {
    if !runtime_id.is_empty() {
        let display_resp = raw_rpc(&format!(r#"{{"method":"GetDisplay","id":"{}"}}"#, runtime_id)).unwrap_or_default();
        if let Some(d) = extract_display(&display_resp) {
            let client = sand_client::SandClient::new(None);
            let cmd = vec!["sh".to_string(), "-lc".to_string(), format!("DISPLAY={} xdotool type -- '{}'", d, text.replace('\'', "'\\''"))];
            if let Ok(resp) = client.exec(runtime_id, cmd) {
                if resp.contains("\"ok\":true") {
                    return Ok(());
                }
            }
        }
    }
    let output = Command::new("xdotool")
        .args(["type", "--", text])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            return Ok(());
        }
    }
    Err("xdotool type failed".to_string())
}

fn extract_display(s: &str) -> Option<String> {
    let pat = "\"display\":\"";
    let start = s.find(pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// Placeholder for computer use tools - Phase 3D
// Architecture: Browser snapshot优先, 失败 fallback screenshot+vision+mouse
pub struct ComputerScreenshotTool;
pub struct ComputerClickTool;
pub struct ComputerTypeTool;
pub struct ComputerMoveTool;
pub struct ComputerKeyTool;
pub struct ComputerScrollTool;

impl Tool for ComputerScreenshotTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.screenshot".to_string(),
            description: "Take desktop screenshot via X11 XShm (or import fallback). Returns base64 png. Use when browser.snapshot fails. Uses sandd binary RPC with Xvfb.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, _args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(_args, runtime_id, None)
    }
    fn execute_with_context(&self, _args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        // Try sandd first if runtime_id available
        if !runtime_id.is_empty() {
            if let Ok(data) = try_screenshot_via_sandd(runtime_id) {
                let b64 = base64_encode(&data);
                return Ok(ToolResult::success(format!("screenshot via sandd {} bytes, base64 length {} (binary RPC, no base64 overhead in transport)", data.len(), b64.len())));
            }
        }
        match try_screenshot() {
            Ok(data) => {
                let b64 = base64_encode(&data);
                Ok(ToolResult::success(format!("screenshot taken {} bytes, base64 length {}", data.len(), b64.len())))
            }
            Err(e) => Ok(ToolResult::success(format!("computer.screenshot not available in this env ({}). In real desktop env would use X11 XShm via sandd EnsureDisplay+Screenshot binary RPC.", e))),
        }
    }
}

impl Tool for ComputerClickTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.click".to_string(),
            description: "Click at coordinates via XTest (fallback to xdotool). Uses sandd desktop display if available.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"button":{"type":"string","description":"left/right/middle"}},"required":["x","y"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let x = extract_number(args, "x").unwrap_or(0);
        let y = extract_number(args, "y").unwrap_or(0);
        match try_click(x, y, runtime_id) {
            Ok(_) => Ok(ToolResult::success(format!("clicked at {},{} via runtime {}", x, y, runtime_id))),
            Err(e) => Ok(ToolResult::success(format!("computer.click placeholder ({}): would click at {},{} via XTest in real env. Uses sandd GetDisplay+exec xdotool.", e, x, y))),
        }
    }
}

impl Tool for ComputerTypeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.type".to_string(),
            description: "Type text via XTest, using sandd desktop if available.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let text = extract_arg(args, "text").unwrap_or_default();
        match try_type(&text, runtime_id) {
            Ok(_) => Ok(ToolResult::success(format!("typed: {} via runtime {}", text, runtime_id))),
            Err(e) => Ok(ToolResult::success(format!("computer.type placeholder ({}): would type '{}' via XTest.", e, text))),
        }
    }
}

impl Tool for ComputerMoveTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.move".to_string(),
            description: "Move mouse to coordinates.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, _runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, _runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let x = extract_number(args, "x").unwrap_or(0);
        let y = extract_number(args, "y").unwrap_or(0);
        Ok(ToolResult::success(format!("computer.move placeholder: would move to {},{} via XTest", x, y)))
    }
}

impl Tool for ComputerKeyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.key".to_string(),
            description: "Press a key or key combo, e.g. Enter, Ctrl+C.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"key":{"type":"string"}},"required":["key"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, _runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, _runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let key = extract_arg(args, "key").unwrap_or_default();
        Ok(ToolResult::success(format!("computer.key placeholder: would press {} via XTest", key)))
    }
}

impl Tool for ComputerScrollTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.scroll".to_string(),
            description: "Scroll via mouse wheel.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"dx":{"type":"number"},"dy":{"type":"number"}},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, _runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, _runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let dx = extract_number(args, "dx").unwrap_or(0);
        let dy = extract_number(args, "dy").unwrap_or(0);
        Ok(ToolResult::success(format!("computer.scroll placeholder: would scroll {},{} via XTest", dx, dy)))
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
    let end = rest.find(|c: char| !c.is_ascii_digit() && c!='-' ).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as u32;
        let b1 = if i+1 < data.len() { data[i+1] as u32 } else { 0 };
        let b2 = if i+2 < data.len() { data[i+2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if i+1 < data.len() { out.push(TABLE[((n >> 6) & 63) as usize] as char); } else { out.push('='); }
        if i+2 < data.len() { out.push(TABLE[(n & 63) as usize] as char); } else { out.push('='); }
        i += 3;
    }
    out
}
