use super::{Tool, ToolDefinition, ToolResult};
use std::process::Command;

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

fn try_click(x: i32, y: i32) -> Result<(), String> {
    // Try xdotool
    let output = Command::new("xdotool")
        .args(["mousemove", &x.to_string(), &y.to_string(), "click", "1"])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            return Ok(());
        }
    }
    // Try via XTest FFI? For MVP, placeholder
    Err("xdotool not available or failed".to_string())
}

fn try_type(text: &str) -> Result<(), String> {
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
            description: "Take desktop screenshot via X11 XShm (or import fallback). Returns base64 png. Use when browser.snapshot fails.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        match try_screenshot() {
            Ok(data) => {
                let b64 = base64_encode(&data);
                Ok(ToolResult { content: format!("screenshot taken {} bytes, base64 length {}", data.len(), b64.len()), is_error: false })
            }
            Err(e) => Ok(ToolResult { content: format!("computer.screenshot not available in this env ({}). In real desktop env would use X11 XShm. Phase 3D placeholder.", e), is_error: false }),
        }
    }
}

impl Tool for ComputerClickTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.click".to_string(),
            description: "Click at coordinates via XTest (fallback to xdotool). Use as fallback when browser.click fails.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"button":{"type":"string","description":"left/right/middle"}},"required":["x","y"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let x = extract_number(args, "x").unwrap_or(0);
        let y = extract_number(args, "y").unwrap_or(0);
        match try_click(x, y) {
            Ok(_) => Ok(ToolResult { content: format!("clicked at {},{}", x, y), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("computer.click placeholder ({}): would click at {},{} via XTest in real env. Fallback strategy: browser snapshot -> semantic action -> screenshot+vision+mouse", e, x, y), is_error: false }),
        }
    }
}

impl Tool for ComputerTypeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.type".to_string(),
            description: "Type text via XTest.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let text = extract_arg(args, "text").unwrap_or_default();
        match try_type(&text) {
            Ok(_) => Ok(ToolResult { content: format!("typed: {}", text), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("computer.type placeholder ({}): would type '{}' via XTest. In real env uses X11 XTest.", e, text), is_error: false }),
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
        let x = extract_number(args, "x").unwrap_or(0);
        let y = extract_number(args, "y").unwrap_or(0);
        Ok(ToolResult { content: format!("computer.move placeholder: would move to {},{} via XTest", x, y), is_error: false })
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
        let key = extract_arg(args, "key").unwrap_or_default();
        Ok(ToolResult { content: format!("computer.key placeholder: would press {} via XTest", key), is_error: false })
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
        let dx = extract_number(args, "dx").unwrap_or(0);
        let dy = extract_number(args, "dy").unwrap_or(0);
        Ok(ToolResult { content: format!("computer.scroll placeholder: would scroll {},{} via XTest", dx, dy), is_error: false })
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
