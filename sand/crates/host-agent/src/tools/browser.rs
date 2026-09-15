use super::{Tool, ToolDefinition, ToolResult};
use std::process::Command;

// Browser tools now call browser-worker Node + Playwright via HTTP
// browser-worker is expected to be running at 127.0.0.1:port from /tmp/browser-worker.port
// If not running, tools will try to spawn it via sandd SpawnBackground with cgroup + display + profile

fn get_browser_worker_port() -> Option<u16> {
    if let Ok(port_str) = std::env::var("BROWSER_WORKER_PORT") {
        if let Ok(port) = port_str.parse::<u16>() {
            return Some(port);
        }
    }
    let port_file = std::env::var("BROWSER_WORKER_PORT_FILE").unwrap_or_else(|_| "/tmp/browser-worker.port".to_string());
    if let Ok(content) = std::fs::read_to_string(&port_file) {
        if let Ok(port) = content.trim().parse::<u16>() {
            if std::net::TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
                return Some(port);
            }
        }
    }
    None
}

fn get_runtime_workspace(runtime_id: &str) -> Option<String> {
    let req = format!(r#"{{"method":"GetRuntime","id":"{}"}}"#, runtime_id);
    if let Ok(resp) = raw_rpc(&req) {
        if let Some(ws) = extract_field(&resp, "workspace") {
            return Some(ws);
        }
    }
    None
}

fn try_spawn_browser_worker(runtime_id: &str) -> Result<(), String> {
    // Ensure display first for headful Chrome via Xvfb
    let _ = raw_rpc(&format!(r#"{{"method":"EnsureDisplay","id":"{}","width":1280,"height":720}}"#, runtime_id));
    let display_resp = raw_rpc(&format!(r#"{{"method":"GetDisplay","id":"{}"}}"#, runtime_id)).unwrap_or_default();
    let display = extract_field(&display_resp, "display").unwrap_or_else(|| ":99".to_string());

    let worker_path = "/home/user/openbot/sand/browser-worker/src/index.js";
    if !std::path::Path::new(worker_path).exists() {
        return Err(format!("worker path not found: {}", worker_path));
    }

    // Chrome profile per runtime workspace
    let profile_dir = if let Some(ws) = get_runtime_workspace(runtime_id) {
        format!("{}/chrome-profile", ws)
    } else {
        format!("/tmp/chrome-profile-{}", runtime_id)
    };
    let _ = std::fs::create_dir_all(&profile_dir);

    // Spawn with DISPLAY and CHROME_PROFILE env
    // Use SpawnBackground via sandd so it's in runtime cgroup
    let req = format!(r#"{{"method":"SpawnBackground","id":"{}","command":["sh","-lc","DISPLAY={} CHROME_PROFILE={} BROWSER_WORKER_PORT_FILE=/tmp/browser-worker-{}.port node {}"]}}"#, runtime_id, display, profile_dir, runtime_id, worker_path);
    match raw_rpc_spawn(&req) {
        Ok(resp) => {
            if resp.contains("\"ok\":true") {
                // Wait for port file specific to this runtime
                let port_file = format!("/tmp/browser-worker-{}.port", runtime_id);
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if let Ok(content) = std::fs::read_to_string(&port_file) {
                        if let Ok(port) = content.trim().parse::<u16>() {
                            if std::net::TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
                                // Also write to global port file for backward compat
                                let _ = std::fs::write("/tmp/browser-worker.port", content);
                                return Ok(());
                            }
                        }
                    }
                    // Also check global
                    if get_browser_worker_port().is_some() {
                        return Ok(());
                    }
                }
                Err("spawned but port not found after 10s".to_string())
            } else {
                Err(format!("spawn failed: {}", resp))
            }
        }
        Err(e) => Err(e),
    }
}

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

fn raw_rpc_spawn(req: &str) -> Result<String, String> {
    raw_rpc(req)
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
    // Handle escaped quotes: find closing " that is not preceded by \
    let mut end = None;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            end = Some(i);
            break;
        }
    }
    let end = end?;
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
            description: "Open a URL in browser via Playwright. Auto-spawns browser-worker via sandd if not running.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"url":{"type":"string","description":"URL to open"}},"required":["url"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let url = extract_arg(args, "url").ok_or("missing url")?;
        let body = format!(r#"{{"url":"{}"}}"#, url.replace('"', "\\\""));

        // Try existing worker, else auto-spawn via runtime
        if get_browser_worker_port().is_none() && !runtime_id.is_empty() {
            println!("[browser] worker not running, trying to spawn via runtime {}", runtime_id);
            let _ = try_spawn_browser_worker(runtime_id);
        }

        match call_browser_worker("/open", "POST", Some(&body)) {
            Ok(resp) => Ok(ToolResult::success(format!("opened {}: {}", url, resp))),
            Err(e) => {
                if e.contains("not running") {
                    Ok(ToolResult::success(format!("browser-worker not running ({}). Auto-spawn attempted but needs Node+Playwright. Manual: cd sand/browser-worker && npm install && BROWSER_WORKER_PORT=9222 node src/index.js &", e)))
                } else {
                    Ok(ToolResult::error(format!("browser.open failed: {}", e), None))
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
                    Ok(ToolResult::success(format!("snapshot:\n{}", unescaped)))
                } else {
                    Ok(ToolResult::success(resp))
                }
            }
            Err(e) => Ok(ToolResult::success(format!("browser.snapshot failed: {} - Phase 3C placeholder: would return [1] button \"Login\" etc.", e))),
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
            Ok(resp) => Ok(ToolResult::success(format!("clicked {}: {}", r, resp))),
            Err(e) => Ok(ToolResult::success(format!("browser.click failed: {}", e))),
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
            Ok(resp) => Ok(ToolResult::success(format!("filled {}: {}", r, resp))),
            Err(e) => Ok(ToolResult::success(format!("browser.fill failed: {}", e))),
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
            Ok(resp) => Ok(ToolResult::success(format!("screenshot: {}", resp.chars().take(200).collect::<String>()))),
            Err(e) => Ok(ToolResult::success(format!("browser.screenshot failed: {}", e))),
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
            Ok(resp) => Ok(ToolResult::success(resp)),
            Err(e) => Ok(ToolResult::success(format!("browser.tabs failed: {}", e))),
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
            Ok(resp) => Ok(ToolResult::success(format!("pressed {}: {}", key, resp))),
            Err(e) => Ok(ToolResult::error(format!("browser.press failed: {}", e), None)),
        }
    }
}
