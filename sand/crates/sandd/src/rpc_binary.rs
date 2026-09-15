use std::path::PathBuf;
use std::sync::Arc;
use std::os::unix::net::{UnixListener, UnixStream};
use std::io::{Read, Write};
use std::collections::HashMap;

use crate::runtime::RuntimeManager;
use sand_protocol::{RuntimeId, RuntimeKind};

/// Workspace root of a runtime, used to confine filesystem operations.
fn runtime_workspace(mgr: &RuntimeManager, runtime_id: &str) -> Option<std::path::PathBuf> {
    let id = RuntimeId::from_string(runtime_id.to_string());
    mgr.get_runtime(&id).map(|rt| rt.workspace)
}

pub struct BinaryRpcServer {
    sock_path: PathBuf,
    runtime_mgr: Arc<RuntimeManager>,
}

impl BinaryRpcServer {
    pub fn new(sock_path: PathBuf, runtime_mgr: Arc<RuntimeManager>) -> Self {
        Self { sock_path, runtime_mgr }
    }

    pub fn run(&self) -> std::io::Result<()> {
        let _ = std::fs::remove_file(&self.sock_path);
        let listener = UnixListener::bind(&self.sock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.sock_path, std::fs::Permissions::from_mode(0o660));
            let _ = std::process::Command::new("chgrp").args(["sand", &self.sock_path.to_string_lossy()]).output();
        }
        eprintln!("[binary-rpc] listening on {} (binary framed, no base64)", self.sock_path.display());

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let mgr = self.runtime_mgr.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = handle_binary_client(stream, mgr) {
                            eprintln!("[binary-rpc] client error: {:?}", e);
                        }
                    });
                }
                Err(e) => eprintln!("[binary-rpc] accept error: {:?}", e),
            }
        }
        Ok(())
    }
}

fn handle_binary_client(mut stream: UnixStream, mgr: Arc<RuntimeManager>) -> std::io::Result<()> {
    loop {
        // Read 4-byte BE length of JSON header
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf) {
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break, // client closed
            Err(e) => return Err(e),
        }
        let json_len = u32::from_be_bytes(len_buf) as usize;
        if json_len == 0 || json_len > 10*1024*1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid json len"));
        }
        let mut json_buf = vec![0u8; json_len];
        stream.read_exact(&mut json_buf)?;
        let json_str = String::from_utf8_lossy(&json_buf);

        // Extract binary_len if present
        let binary_len = extract_number_field(&json_str, "binary_len").unwrap_or(0) as usize;
        let mut binary_payload = Vec::new();
        if binary_len > 0 {
            if binary_len > 100*1024*1024 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "binary too large"));
            }
            binary_payload.resize(binary_len, 0);
            stream.read_exact(&mut binary_payload)?;
        }

        // Handle request
        let (resp_json, resp_binary) = handle_binary_request(&json_str, binary_payload, &mgr);

        // Write response: 4-byte len + json + binary
        let resp_json_bytes = resp_json.as_bytes();
        let len_be = (resp_json_bytes.len() as u32).to_be_bytes();
        stream.write_all(&len_be)?;
        stream.write_all(resp_json_bytes)?;
        if !resp_binary.is_empty() {
            stream.write_all(&resp_binary)?;
        }
        stream.flush()?;
    }
    Ok(())
}

fn extract_command_array(s: &str) -> Vec<String> {
    // Find "command":[ ... ] and parse JSON strings properly
    let key = "\"command\":";
    let start = match s.find(key) {
        Some(idx) => idx + key.len(),
        None => return vec![],
    };
    let rest = s[start..].trim_start();
    if !rest.starts_with('[') {
        // single string command?
        if let Some(cmd) = extract_string_field(s, "command") {
            return vec![cmd];
        }
        return vec![];
    }
    // Parse array content until matching ]
    let mut result = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut current = String::new();
    let mut depth = 0;
    let chars: Vec<char> = rest.chars().collect();
    // rest starts with '['
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !in_string {
            if c == '[' {
                depth += 1;
                if depth == 1 {
                    // start array
                    i += 1;
                    continue;
                }
            } else if c == ']' {
                depth -= 1;
                if depth == 0 {
                    // end array, push last if any?
                    if !current.trim().is_empty() {
                        // shouldn't happen outside string
                    }
                    break;
                }
            } else if c == '"' {
                in_string = true;
                current = String::new();
            } else if c == ',' || c.is_whitespace() {
                // skip
            }
        } else {
            if escaped {
                current.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                // end string
                result.push(current.clone());
                in_string = false;
                current = String::new();
            } else {
                current.push(c);
            }
        }
        i += 1;
    }
    result
}

fn handle_binary_request(req_json: &str, binary_payload: Vec<u8>, mgr: &RuntimeManager) -> (String, Vec<u8>) {
    let method = extract_string_field(req_json, "method").unwrap_or_default();

    match method.as_str() {
        "Exec" => {
            let id_str = extract_string_field(req_json, "id").unwrap_or_default();
            let command_vec = extract_command_array(req_json);
            let command_vec = if command_vec.is_empty() {
                // fallback to single string field
                let cmd = extract_string_field(req_json, "command").unwrap_or_default();
                if cmd.is_empty() { vec![] } else if cmd.contains(' ') { vec!["bash".to_string(), "-lc".to_string(), cmd] } else { vec![cmd] }
            } else {
                command_vec
            };
            let cwd = extract_string_field(req_json, "cwd");
            let timeout = extract_number_field(req_json, "timeout_ms");
            let id = RuntimeId::from_string(id_str.clone());
            let env = HashMap::new();
            eprintln!("[binary-rpc] Exec id={} cmd={:?}", id_str, command_vec);

            match mgr.exec(&id, command_vec, cwd, env, timeout) {
                Ok(res) => {
                    // Binary payload: stdout + stderr with lengths? For simplicity, return stdout as binary, stderr in json as b64? No, we want no base64.
                    // We'll return json with exit_code, stdout_len, stderr_len, and binary payload = stdout + stderr concatenated
                    let stdout_len = res.stdout.len();
                    let stderr_len = res.stderr.len();
                    let json = format!(r#"{{"ok":true,"pid":{},"exit_code":{},"stdout_len":{},"stderr_len":{},"duration_ms":{}}}"#, res.pid, res.exit_code.unwrap_or(-1), stdout_len, stderr_len, res.duration_ms);
                    let mut binary = Vec::new();
                    binary.extend_from_slice(&res.stdout);
                    binary.extend_from_slice(&res.stderr);
                    (json, binary)
                }
                Err(e) => (format!(r#"{{"ok":false,"error":"{}"}}"#, escape_json(&e)), vec![]),
            }
        }
        "WritePty" => {
            let id_str = extract_string_field(req_json, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_json, "pty_id").unwrap_or_else(|| "default".to_string());
            // binary_payload is data to write
            match mgr.pty_manager().write_pty(&id_str, &pty_id, binary_payload) {
                Ok(_) => (r#"{"ok":true}"#.to_string(), vec![]),
                Err(e) => (format!(r#"{{"ok":false,"error":"{}"}}"#, escape_json(&e)), vec![]),
            }
        }
        "ReadPty" => {
            let id_str = extract_string_field(req_json, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_json, "pty_id").unwrap_or_else(|| "default".to_string());
            let clear = req_json.contains("\"clear\":true");
            match mgr.pty_manager().read_pty(&id_str, &pty_id, clear) {
                Ok(data) => {
                    let json = format!(r#"{{"ok":true,"len":{}}}"#, data.len());
                    (json, data)
                }
                Err(e) => (format!(r#"{{"ok":false,"error":"{}"}}"#, escape_json(&e)), vec![]),
            }
        }
        "OpenPty" | "ResizePty" | "ClosePty" | "ListPtys" | "CreateRuntime" | "GetRuntime" | "ListRuntimes" | "DestroyRuntime" | "Status" | "SpawnBackground" | "ListEvents" | "EnsureDisplay" | "GetDisplay" | "SignalPty" | "SetDesiredState" | "GetObservedState" | "AcquireLease" | "ReleaseLease" | "ListLeases" | "TaskComplete" | "FsList" | "FsStat" | "FsSearch" | "FsGlob" | "FsMkdir" | "FsRemove" | "FsRename" | "FsPatch" | "Handshake" | "Ping" => {
            // For these, delegate to existing JSON handler (no binary)
            let json_resp = crate::rpc::handle_request_public(req_json, mgr);
            (json_resp, vec![])
        }
        // Raw file bytes: no base64 on the wire. The response declares `len`
        // so the bridge/client knows how many bytes follow the JSON.
        "FsRead" => {
            let id_str = extract_string_field(req_json, "id").unwrap_or_default();
            let path = extract_string_field(req_json, "path").unwrap_or_default();
            let root = match runtime_workspace(mgr, &id_str) {
                Some(ws) => ws,
                None => return (r#"{"ok":false,"error":"runtime not found"}"#.to_string(), vec![]),
            };
            match crate::fs::read(&root, &path) {
                Ok(data) => {
                    let json = format!(r#"{{"ok":true,"len":{}}}"#, data.len());
                    (json, data)
                }
                Err(e) => (
                    format!(r#"{{"ok":false,"security":{},"error":"{}"}}"#, e.security, escape_json(&e.message)),
                    vec![],
                ),
            }
        }
        "FsWrite" => {
            let id_str = extract_string_field(req_json, "id").unwrap_or_default();
            let path = extract_string_field(req_json, "path").unwrap_or_default();
            // binary_payload carries the file content; `data_b64` is accepted as
            // a fallback for callers on the JSON socket.
            let data = if !binary_payload.is_empty() {
                binary_payload
            } else {
                extract_string_field(req_json, "data_b64")
                    .map(|b64| base64_decode(&b64))
                    .unwrap_or_default()
            };
            let root = match runtime_workspace(mgr, &id_str) {
                Some(ws) => ws,
                None => return (r#"{"ok":false,"error":"runtime not found"}"#.to_string(), vec![]),
            };
            match crate::fs::write(&root, &path, &data) {
                Ok(len) => (format!(r#"{{"ok":true,"bytes_written":{}}}"#, len), vec![]),
                Err(e) => (
                    format!(r#"{{"ok":false,"security":{},"error":"{}"}}"#, e.security, escape_json(&e.message)),
                    vec![],
                ),
            }
        }
        "Screenshot" => {
            let id_str = extract_string_field(req_json, "id").unwrap_or_default();
            // Try desktop manager first
            if let Ok(data) = mgr.desktop_manager().screenshot(&id_str) {
                let json = format!(r#"{{"ok":true,"len":{},"format":"png"}}"#, data.len());
                return (json, data);
            }
            // Fallback to DISPLAY env
            let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
            let output = std::process::Command::new("sh")
                .args(["-c", &format!("DISPLAY={} import -window root png:- 2>/dev/null | head -c 5000000 || DISPLAY={} scrot -z -o - 2>/dev/null | head -c 5000000", display, display)])
                .output();
            if let Ok(out) = output {
                if !out.stdout.is_empty() {
                    let json = format!(r#"{{"ok":true,"len":{},"format":"png"}}"#, out.stdout.len());
                    return (json, out.stdout);
                }
            }
            (r#"{"ok":false,"error":"screenshot not available, no DISPLAY"}"#.to_string(), vec![])
        }
        _ => (format!(r#"{{"ok":false,"error":"unknown method {}"}}"#, escape_json(&method)), vec![]),
    }
}

// Reuse helpers from rpc.rs (duplicated for binary)
fn extract_string_field(s: &str, field: &str) -> Option<String> {
    let patterns = [format!("\"{}\":\"", field), format!("\"{}\" : \"", field), format!("\"{}\": \"", field)];
    for pat in &patterns {
        if let Some(start) = s.find(pat) {
            let rest = &s[start + pat.len()..];
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
    }
    let pat = format!("\"{}\":", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start + pat.len()..].trim_start();
        if !rest.starts_with('"') {
            let end = rest.find(|c| c == ',' || c == '}' || c == ' ' || c == '\n').unwrap_or(rest.len());
            let val = rest[..end].trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() { return Some(val); }
        }
    }
    None
}

fn extract_number_field(s: &str, field: &str) -> Option<u64> {
    let pat = format!("\"{}\":", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start + pat.len()..].trim_start();
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if end > 0 { return rest[..end].parse().ok(); }
    }
    None
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let mut vals = [0u8; 4];
        let mut padding = 0;
        let mut valid = true;
        for j in 0..4 {
            let b = bytes[i + j];
            if b == b'=' {
                padding += 1;
                vals[j] = 0;
            } else {
                let v = table[b as usize];
                if v == 255 {
                    valid = false;
                    break;
                }
                vals[j] = v;
            }
        }
        if !valid {
            break;
        }
        let n = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);
        out.push(((n >> 16) & 0xFF) as u8);
        if padding < 2 {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if padding < 1 {
            out.push((n & 0xFF) as u8);
        }
        i += 4;
        if padding > 0 {
            break;
        }
    }
    out
}
