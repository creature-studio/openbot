use super::{Tool, ToolDefinition, ToolResult};

pub struct TerminalOpenTool {
    client: sand_client::SandClient,
}
pub struct TerminalWriteTool {
    client: sand_client::SandClient,
}
pub struct TerminalReadTool {
    client: sand_client::SandClient,
}

impl TerminalOpenTool {
    pub fn new() -> Self { Self { client: sand_client::SandClient::new(None) } }
}
impl TerminalWriteTool {
    pub fn new() -> Self { Self { client: sand_client::SandClient::new(None) } }
}
impl TerminalReadTool {
    pub fn new() -> Self { Self { client: sand_client::SandClient::new(None) } }
}

impl Tool for TerminalOpenTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "terminal.open".to_string(),
            description: "Open a PTY terminal in runtime with TERM=xterm-256color, ANSI support, resize, raw mode. Returns pty_id. Supports Ctrl+C/D via terminal.write with \\x03/\\x04 or signal tool.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"shell":{"type":"string"},"cols":{"type":"number"},"rows":{"type":"number"}},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").unwrap_or_else(|| format!("term-{}", std::process::id()));
        let shell = extract_arg(args, "shell").unwrap_or_else(|| "/bin/bash".to_string());
        let cols = extract_number(args, "cols").unwrap_or(80) as u16;
        let rows = extract_number(args, "rows").unwrap_or(24) as u16;
        match self.client.open_pty(runtime_id, &pty_id, cols, rows) {
            Ok(resp) => Ok(ToolResult::success(format!("opened pty {} ({}x{}) shell={} TERM=xterm-256color ANSI enabled in runtime {}: {}\nUse terminal.write with data=\"\\x03\" for Ctrl+C, \"\\x04\" for Ctrl+D, or signal tool", pty_id, cols, rows, shell, runtime_id, resp))),
            Err(e) => Ok(ToolResult::error(format!("open pty failed: {}", e), None)),
        }
    }
}

impl Tool for TerminalWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "terminal.write".to_string(),
            description: "Write data to a PTY terminal via binary RPC (no base64 overhead).".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"data":{"type":"string"}},"required":["pty_id","data"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        let data = extract_arg(args, "data").ok_or("missing data")?;
        // Try binary RPC first (efficient, no base64)
        match self.client.write_pty_binary(runtime_id, &pty_id, data.as_bytes()) {
            Ok(resp) => Ok(ToolResult::success(format!("write to {} via binary RPC: {}", pty_id, resp))),
            Err(_) => {
                // Fallback to base64 JSON RPC
                let b64 = base64_encode(data.as_bytes());
                let req = format!(r#"{{"method":"WritePty","id":"{}","pty_id":"{}","data_b64":"{}"}}"#, runtime_id, pty_id, b64);
                match raw_rpc(&req) {
                    Ok(resp) => Ok(ToolResult::success(format!("write to {}: {}", pty_id, resp))),
                    Err(e) => Ok(ToolResult::error(format!("write failed: {}", e), None)),
                }
            }
        }
    }
}

impl Tool for TerminalReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "terminal.read".to_string(),
            description: "Read output from a PTY terminal via binary RPC (raw bytes).".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pty_id":{"type":"string"},"clear":{"type":"boolean"}},"required":["pty_id"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let pty_id = extract_arg(args, "pty_id").ok_or("missing pty_id")?;
        let clear = args.contains("\"clear\":true");
        // Try binary RPC
        match self.client.read_pty_binary(runtime_id, &pty_id, clear) {
            Ok((json, data)) => {
                Ok(ToolResult::success(format!("pty {} output via binary RPC ({} bytes) json={}:\n{}", pty_id, data.len(), json, String::from_utf8_lossy(&data))))
            }
            Err(_) => {
                let req = format!(r#"{{"method":"ReadPty","id":"{}","pty_id":"{}","clear":{}}}"#, runtime_id, pty_id, clear);
                match raw_rpc(&req) {
                    Ok(resp) => {
                        let b64 = extract_field(&resp, "data_b64").unwrap_or_default();
                        let data = base64_decode(&b64);
                        Ok(ToolResult::success(format!("pty {} output ({} bytes):\n{}", pty_id, data.len(), String::from_utf8_lossy(&data))))
                    }
                    Err(e) => Ok(ToolResult::error(format!("read failed: {}", e), None)),
                }
            }
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
        Some(raw.replace("\\n","\n").replace("\\\"","\"").replace("\\\\","\\").replace("\\x03", "\x03").replace("\\x04", "\x04").replace("\\x1b", "\x1b"))
    } else { None }
}

fn extract_number(json: &str, key: &str) -> Option<i32> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)?;
    let rest = json[start+pat.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c!='-').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_field(s: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() { table[c as usize]=i as u8; }
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i=0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i]==b'\n' || bytes[i]==b'\r' || bytes[i]==b' ') { i+=1; }
        if i+3 >= bytes.len() { break; }
        let mut vals=[0u8;4];
        let mut padding=0;
        let mut valid=true;
        for j in 0..4 {
            if i+j>=bytes.len() { valid=false; break; }
            let b=bytes[i+j];
            if b==b'=' { padding+=1; vals[j]=0; } else { let v=table[b as usize]; if v==255 { valid=false; break; } vals[j]=v; }
        }
        if !valid { i+=1; continue; }
        let n=((vals[0] as u32)<<18)|((vals[1] as u32)<<12)|((vals[2] as u32)<<6)|(vals[3] as u32);
        out.push(((n>>16)&0xFF) as u8);
        if padding<2 { out.push(((n>>8)&0xFF) as u8); }
        if padding<1 { out.push((n&0xFF) as u8); }
        i+=4;
        if padding>0 { break; }
    }
    out
}
