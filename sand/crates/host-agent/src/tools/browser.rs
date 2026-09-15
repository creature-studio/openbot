use super::{Tool, ToolDefinition, ToolResult};
use std::process::Command;

// Browser tools now call browser-worker Node + Playwright via HTTP
// browser-worker is expected to be running at 127.0.0.1:port from /tmp/browser-worker.port
// If not running, tools will try to spawn it or return placeholder

fn get_browser_worker_port() -> Option<u16> {
    // Try env, then port file
    if let Ok(port_str) = std::env::var("BROWSER_WORKER_PORT") {
        if let Ok(port) = port_str.parse::<u16>() {
            return Some(port);
        }
    }
    let port_file = std::env::var("BROWSER_WORKER_PORT_FILE").unwrap_or_else(|_| "/tmp/browser-worker.port".to_string());
    if let Ok(content) = std::fs::read_to_string(&port_file) {
        if let Ok(port) = content.trim().parse::<u16>() {
            return Some(port);
        }
    }
    None
}

fn call_browser_worker(endpoint: &str, method: &str, body: Option<&str>) -> Result<String, String> {
    let port = get_browser_worker_port().ok_or("browser-worker not running, no port found")?;
    let url = format!("http://127.0.0.1:{}{}", port, endpoint);
    
    let mut curl_args = vec!["-s", "-X", method, &url, "-H", "Content-Type: application/json"];
    let body_string;
    if let Some(b) = body {
        body_string = b.to_string();
        curl_args.push("-d");
        curl_args.push(&body_string);
    }
    
    let output = Command::new("curl")
        .args(&curl_args)
        .output()
        .map_err(|e| format!("curl failed: {}", e))?;
    
    if !output.status.success() {
        return Err(format!("browser-worker request failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    
    let resp = String::from_utf8_lossy(&output.stdout).to_string();
    // Check if response contains error
    if resp.contains("\"ok\":false") {
        // Extract error field
        if let Some(err) = extract_field(&resp, "error") {
            return Err(err);
        }
        return Err(resp);
    }
    Ok(resp)
}

fn extract_field(s: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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

pub struct BrowserOpenTool;
pub struct BrowserSnapshotTool;
pub struct BrowserClickTool;
pub struct BrowserFillTool;
pub struct BrowserScreenshotTool;
pub struct BrowserTabsTool;
pub struct BrowserPressTool;

impl Tool for BrowserOpenTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser.open".to_string(),
            description: "Open a URL in browser via Playwright.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"url":{"type":"string","description":"URL to open"}},"required":["url"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let url = extract_arg(args, "url").ok_or("missing url")?;
        let body = format!(r#"{{"url":"{}"}}"#, url.replace('"', "\\\""));
        match call_browser_worker("/open", "POST", Some(&body)) {
            Ok(resp) => Ok(ToolResult { content: format!("opened {}: {}", url, resp), is_error: false }),
            Err(e) => {
                // Fallback: try to spawn browser-worker if not running
                if e.contains("not running") {
                    Ok(ToolResult { content: format!("browser-worker not running ({}), would need to start Node + Playwright. For MVP, please run: cd sand/browser-worker && npm install && npm start", e), is_error: false })
                } else {
                    Ok(ToolResult { content: format!("browser.open failed: {}", e), is_error: true })
                }
            }
        }
    }
}

impl Tool for BrowserSnapshotTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser.snapshot".to_string(),
            description: "Snapshot current page accessibility tree with refs. Returns [ref] role \"name\" list. Use ref for click/fill.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        match call_browser_worker("/snapshot", "GET", None) {
            Ok(resp) => {
                if let Some(snap) = extract_field(&resp, "snapshot") {
                    // snapshot field is escaped, need unescape
                    let unescaped = snap.replace("\\n", "\n").replace("\\\"", "\"");
                    Ok(ToolResult { content: format!("snapshot:\n{}", unescaped), is_error: false })
                } else {
                    Ok(ToolResult { content: resp, is_error: false })
                }
            }
            Err(e) => Ok(ToolResult { content: format!("browser.snapshot failed: {} - Phase 3C placeholder: would return [1] button \"Login\" etc.", e), is_error: false }),
        }
    }
}

impl Tool for BrowserClickTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser.click".to_string(),
            description: "Click element by ref from snapshot, e.g. ref=1.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"ref":{"type":"string","description":"Ref id from snapshot"}},"required":["ref"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let r = extract_arg(args, "ref").or_else(|| extract_arg(args, "ref_id")).ok_or("missing ref")?;
        let body = format!(r#"{{"ref":"{}"}}"#, r);
        match call_browser_worker("/click", "POST", Some(&body)) {
            Ok(resp) => Ok(ToolResult { content: format!("clicked {}: {}", r, resp), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("browser.click failed: {}", e), is_error: true }),
        }
    }
}

impl Tool for BrowserFillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser.fill".to_string(),
            description: "Fill input by ref with value.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"ref":{"type":"string"},"value":{"type":"string"}},"required":["ref","value"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let r = extract_arg(args, "ref").ok_or("missing ref")?;
        let value = extract_arg(args, "value").ok_or("missing value")?;
        let body = format!(r#"{{"ref":"{}","value":"{}"}}"#, r, value.replace('"', "\\\""));
        match call_browser_worker("/fill", "POST", Some(&body)) {
            Ok(resp) => Ok(ToolResult { content: format!("filled {}: {}", r, resp), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("browser.fill failed: {}", e), is_error: true }),
        }
    }
}

impl Tool for BrowserScreenshotTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser.screenshot".to_string(),
            description: "Take screenshot of current page, returns base64.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        match call_browser_worker("/screenshot", "POST", None) {
            Ok(resp) => Ok(ToolResult { content: format!("screenshot: {}", resp.chars().take(200).collect::<String>()), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("browser.screenshot failed: {}", e), is_error: true }),
        }
    }
}

impl Tool for BrowserTabsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser.tabs".to_string(),
            description: "List browser tabs.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        match call_browser_worker("/tabs", "GET", None) {
            Ok(resp) => Ok(ToolResult { content: resp, is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("browser.tabs failed: {}", e), is_error: true }),
        }
    }
}

impl Tool for BrowserPressTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser.press".to_string(),
            description: "Press a key, e.g. Enter, Escape, ArrowDown.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"key":{"type":"string"}},"required":["key"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let key = extract_arg(args, "key").ok_or("missing key")?;
        let body = format!(r#"{{"key":"{}"}}"#, key);
        match call_browser_worker("/press", "POST", Some(&body)) {
            Ok(resp) => Ok(ToolResult { content: format!("pressed {}: {}", key, resp), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("browser.press failed: {}", e), is_error: true }),
        }
    }
}
