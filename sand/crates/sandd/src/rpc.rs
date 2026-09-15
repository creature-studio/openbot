use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::os::unix::net::{UnixListener, UnixStream};
use std::io::{BufRead, BufReader, Write};
use std::collections::HashMap;

use crate::runtime::RuntimeManager;
use sand_protocol::{RuntimeId, RuntimeKind};

pub struct RpcServer {
    sock_path: PathBuf,
    runtime_mgr: Arc<RuntimeManager>,
}

impl RpcServer {
    pub fn new(sock_path: PathBuf, runtime_mgr: Arc<RuntimeManager>) -> Self {
        Self { sock_path, runtime_mgr }
    }

    pub fn run(&self) -> std::io::Result<()> {
        let listener = UnixListener::bind(&self.sock_path)?;
        // set permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.sock_path, std::fs::Permissions::from_mode(0o777));
        }

        eprintln!("[rpc] listening on {}", self.sock_path.display());

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let mgr = self.runtime_mgr.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = handle_client(stream, mgr) {
                            eprintln!("[rpc] client error: {:?}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[rpc] accept error: {:?}", e);
                }
            }
        }

        Ok(())
    }
}

fn handle_client(stream: UnixStream, mgr: Arc<RuntimeManager>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        let response = handle_request(trimmed, &mgr);
        writeln!(writer, "{}", response)?;
        writer.flush()?;
        line.clear();
    }

    Ok(())
}

fn handle_request(req_str: &str, mgr: &RuntimeManager) -> String {
    // Very simple JSON parsing without serde: expect {"method":"...","params":{...}}
    // We'll do naive extraction

    let method = extract_string_field(req_str, "method").unwrap_or_default();

    match method.as_str() {
        "CreateRuntime" => {
            let kind_str = extract_string_field(req_str, "kind").unwrap_or_else(|| "assistant".to_string());
            let workspace = extract_string_field(req_str, "workspace");
            let kind = RuntimeKind::from_str(&kind_str);
            let ws_path = workspace.map(|s| PathBuf::from(s));
            match mgr.create_runtime(kind, ws_path) {
                Ok(rt) => {
                    format!("{{\"ok\":true,\"id\":\"{}\",\"kind\":\"{}\",\"workspace\":\"{}\"}}", rt.id.0, rt.kind.as_str(), rt.workspace.display())
                }
                Err(e) => {
                    format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e))
                }
            }
        }
        "GetRuntime" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let id = RuntimeId::from_string(id_str);
            if let Some(rt) = mgr.get_runtime(&id) {
                format!("{{\"ok\":true,\"id\":\"{}\",\"kind\":\"{}\",\"state\":\"{}\",\"workspace\":\"{}\",\"procs\":{},\"ptys\":{}}}", rt.id.0, rt.kind.as_str(), rt.state.as_str(), rt.workspace.display(), rt.process_count, rt.pty_count)
            } else {
                format!("{{\"ok\":false,\"error\":\"runtime not found\"}}")
            }
        }
        "ListRuntimes" => {
            let list = mgr.list_runtimes();
            let mut items = Vec::new();
            for rt in list {
                items.push(format!("{{\"id\":\"{}\",\"kind\":\"{}\",\"state\":\"{}\",\"procs\":{},\"ptys\":{}}}", rt.id.0, rt.kind.as_str(), rt.state.as_str(), rt.process_count, rt.pty_count));
            }
            format!("{{\"ok\":true,\"runtimes\":[{}]}}", items.join(","))
        }
        "DestroyRuntime" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let id = RuntimeId::from_string(id_str);
            match mgr.destroy_runtime(&id) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "Exec" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let command_str = extract_string_field(req_str, "command").unwrap_or_default();
            // command is space separated? For simplicity, we expect command as string like "pwd" or "echo hello" or array? We'll parse as json array if present
            // Try to extract command array via naive method
            let command_vec = if req_str.contains("\"command\":[") {
                // extract array content
                if let Some(start) = req_str.find("\"command\":[") {
                    let rest = &req_str[start + "\"command\":[".len()..];
                    if let Some(end) = rest.find(']') {
                        let arr_str = &rest[..end];
                        // split by comma, trim quotes
                        arr_str.split(',').map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
                    } else {
                        vec![command_str.clone()]
                    }
                } else {
                    vec![command_str.clone()]
                }
            } else {
                // split by space but handle?
                vec![command_str.clone()]
            };

            // Actually better: if command_str contains spaces, split? But for echo hello we need ["echo","hello"]
            // Let's attempt to split command_str by whitespace if command_vec is single and contains space and not array
            let final_cmd = if command_vec.len() == 1 && command_vec[0].contains(' ') && !req_str.contains("\"command\":[") {
                // naive shell split? For simplicity, split by space
                // but for "bash -lc 'echo hello'" we need better
                // We'll try to parse as shell: if starts with "bash", keep rest as is?
                // For now, split by space
                command_vec[0].split_whitespace().map(|s| s.to_string()).collect()
            } else {
                command_vec
            };

            let cwd = extract_string_field(req_str, "cwd");
            let timeout = extract_number_field(req_str, "timeout_ms");

            let id = RuntimeId::from_string(id_str);
            let env = HashMap::new(); // ignore for now

            match mgr.exec(&id, final_cmd, cwd, env, timeout) {
                Ok(res) => {
                    let stdout_b64 = base64_encode(&res.stdout);
                    let stderr_b64 = base64_encode(&res.stderr);
                    format!("{{\"ok\":true,\"pid\":{},\"exit_code\":{},\"stdout_b64\":\"{}\",\"stderr_b64\":\"{}\",\"duration_ms\":{}}}", res.pid, res.exit_code.unwrap_or(-1), stdout_b64, stderr_b64, res.duration_ms)
                }
                Err(e) => {
                    format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e))
                }
            }
        }
        "SpawnBackground" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            // parse command array similar to Exec
            let command_vec = if req_str.contains("\"command\":[") {
                if let Some(start) = req_str.find("\"command\":[") {
                    let rest = &req_str[start + "\"command\":[".len()..];
                    if let Some(end) = rest.find(']') {
                        let arr_str = &rest[..end];
                        arr_str.split(',').map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            } else {
                let command_str = extract_string_field(req_str, "command").unwrap_or_default();
                if command_str.contains(' ') {
                    command_str.split_whitespace().map(|s| s.to_string()).collect()
                } else if !command_str.is_empty() {
                    vec![command_str]
                } else {
                    vec![]
                }
            };
            let cwd = extract_string_field(req_str, "cwd");
            let id = RuntimeId::from_string(id_str);
            let env = HashMap::new();
            match mgr.spawn_background(&id, command_vec, cwd, env) {
                Ok(pid) => format!("{{\"ok\":true,\"pid\":{}}}", pid),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "OpenPty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let cols = extract_number_field(req_str, "cols").unwrap_or(80) as u16;
            let rows = extract_number_field(req_str, "rows").unwrap_or(24) as u16;
            let shell = extract_string_field(req_str, "shell").unwrap_or_else(|| "/bin/bash".to_string());

            let cgroup_path = mgr.cgroup_manager().runtime_cgroup_path(&id_str);
            let cgroup_opt = if cgroup_path.exists() { Some(cgroup_path) } else { None };

            match mgr.pty_manager().open_pty(&id_str, &pty_id, cols, rows, &shell, cgroup_opt) {
                Ok(sess) => {
                    format!("{{\"ok\":true,\"runtime_id\":\"{}\",\"pty_id\":\"{}\",\"pid\":{}}}", sess.runtime_id, sess.pty_id, sess.pid)
                }
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "WritePty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let data_b64 = extract_string_field(req_str, "data_b64").unwrap_or_default();
            let data = base64_decode(&data_b64);
            match mgr.pty_manager().write_pty(&id_str, &pty_id, data) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ResizePty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let cols = extract_number_field(req_str, "cols").unwrap_or(80) as u16;
            let rows = extract_number_field(req_str, "rows").unwrap_or(24) as u16;
            match mgr.pty_manager().resize_pty(&id_str, &pty_id, cols, rows) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ClosePty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            match mgr.pty_manager().close_pty(&id_str, &pty_id) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ListPtys" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let list = mgr.pty_manager().list_for_runtime(&id_str);
            let mut items = Vec::new();
            for sess in list {
                items.push(format!("{{\"pty_id\":\"{}\",\"pid\":{},\"cols\":{},\"rows\":{}}}", sess.pty_id, sess.pid, sess.cols, sess.rows));
            }
            format!("{{\"ok\":true,\"ptys\":[{}]}}", items.join(","))
        }
        "Status" => {
            let list = mgr.list_runtimes();
            format!("{{\"ok\":true,\"runtime_count\":{},\"version\":\"0.1.0\"}}", list.len())
        }
        _ => {
            format!("{{\"ok\":false,\"error\":\"unknown method {}\"}}", escape_json(&method))
        }
    }
}

fn extract_string_field(s: &str, field: &str) -> Option<String> {
    // look for "field":"value" or "field": "value"
    let patterns = [format!("\"{}\":\"", field), format!("\"{}\" : \"", field), format!("\"{}\": \"", field)];
    for pat in &patterns {
        if let Some(start) = s.find(pat) {
            let rest = &s[start + pat.len()..];
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
    }
    // try single quotes?
    let patterns2 = [format!("\"{}\":'", field), format!("\"{}\": '", field)];
    for pat in &patterns2 {
        if let Some(start) = s.find(pat) {
            let rest = &s[start + pat.len()..];
            if let Some(end) = rest.find('\'') {
                return Some(rest[..end].to_string());
            }
        }
    }
    // try without quotes for id that may be like "id": "rt-..."
    // also try "field":value where value is not quoted but until comma
    let pat = format!("\"{}\":", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start + pat.len()..].trim_start();
        if rest.starts_with('"') {
            // already handled
        } else {
            // until comma or }
            let end = rest.find(|c| c == ',' || c == '}' || c == ' ' || c == '\n').unwrap_or(rest.len());
            let val = rest[..end].trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn extract_number_field(s: &str, field: &str) -> Option<u64> {
    let pat = format!("\"{}\":", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start + pat.len()..].trim_start();
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if end > 0 {
            return rest[..end].parse().ok();
        }
    }
    None
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn base64_encode(data: &[u8]) -> String {
    // simple base64 without external crate
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
        if i+1 < data.len() {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if i+2 < data.len() {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }

        i += 3;
    }
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // skip whitespace
        while i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b'\r' || bytes[i] == b' ' ) { i+=1; }
        if i+3 >= bytes.len() { break; }
        let mut vals = [0u8; 4];
        let mut padding = 0;
        let mut valid = true;
        for j in 0..4 {
            if i+j >= bytes.len() { valid = false; break; }
            let b = bytes[i+j];
            if b == b'=' {
                padding += 1;
                vals[j] = 0;
            } else {
                let v = table[b as usize];
                if v == 255 { valid = false; break; }
                vals[j] = v;
            }
        }
        if !valid { i+=1; continue; }
        let n = ((vals[0] as u32) << 18) | ((vals[1] as u32) << 12) | ((vals[2] as u32) << 6) | (vals[3] as u32);
        out.push(((n >> 16) & 0xFF) as u8);
        if padding < 2 { out.push(((n >> 8) & 0xFF) as u8); }
        if padding < 1 { out.push((n & 0xFF) as u8); }
        i += 4;
        if padding > 0 { break; }
    }
    out
}
