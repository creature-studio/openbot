use std::os::unix::net::UnixStream;
use std::io::{BufRead, BufReader, Write, Read};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct SandClient {
    sock_path: PathBuf,
    binary_sock_path: PathBuf,
}

impl SandClient {
    pub fn new(sock_path: Option<PathBuf>) -> Self {
        let path = sock_path.unwrap_or_else(|| {
            if Path::new("/run/sand/sandd.sock").exists() {
                PathBuf::from("/run/sand/sandd.sock")
            } else {
                PathBuf::from("/tmp/sandd/sandd.sock")
            }
        });
        let binary_path = if path.to_string_lossy().contains("/run/sand/") {
            PathBuf::from("/run/sand/sandd-binary.sock")
        } else {
            PathBuf::from("/tmp/sandd/sandd-binary.sock")
        };
        Self { sock_path: path, binary_sock_path: binary_path }
    }

    pub fn new_with_paths(sock_path: PathBuf, binary_sock_path: PathBuf) -> Self {
        Self { sock_path, binary_sock_path }
    }

    pub fn call(&self, req: &str) -> std::io::Result<String> {
        let mut stream = UnixStream::connect(&self.sock_path)?;
        writeln!(stream, "{}", req)?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader.read_line(&mut response)?;
        Ok(response.trim().to_string())
    }

    pub fn create_runtime(&self, kind: &str, workspace: Option<&str>) -> std::io::Result<String> {
        let ws_part = if let Some(ws) = workspace {
            format!(",\"workspace\":\"{}\"", ws)
        } else {
            "".to_string()
        };
        let req = format!("{{\"method\":\"CreateRuntime\",\"kind\":\"{}\"{} }}", kind, ws_part);
        self.call(&req)
    }

    pub fn list_runtimes(&self) -> std::io::Result<String> {
        self.call("{\"method\":\"ListRuntimes\"}")
    }

    pub fn get_runtime(&self, id: &str) -> std::io::Result<String> {
        self.call(&format!("{{\"method\":\"GetRuntime\",\"id\":\"{}\"}}", id))
    }

    pub fn destroy_runtime(&self, id: &str) -> std::io::Result<String> {
        self.call(&format!("{{\"method\":\"DestroyRuntime\",\"id\":\"{}\"}}", id))
    }

    pub fn exec(&self, id: &str, command: Vec<String>) -> std::io::Result<String> {
        let cmd_json = command.iter().map(|c| format!("\"{}\"", c.replace('"', "\\\""))).collect::<Vec<_>>().join(",");
        let req = format!("{{\"method\":\"Exec\",\"id\":\"{}\",\"command\":[{}]}}", id, cmd_json);
        self.call(&req)
    }

    pub fn exec_str(&self, id: &str, cmd_str: &str) -> std::io::Result<String> {
        // cmd_str like "pwd" or "echo hello"
        // split for simple cases
        let parts: Vec<String> = cmd_str.split_whitespace().map(|s| s.to_string()).collect();
        self.exec(id, parts)
    }

    pub fn spawn_background(&self, id: &str, command: Vec<String>) -> std::io::Result<String> {
        let cmd_json = command.iter().map(|c| format!("\"{}\"", c.replace('"', "\\\""))).collect::<Vec<_>>().join(",");
        let req = format!("{{\"method\":\"SpawnBackground\",\"id\":\"{}\",\"command\":[{}]}}", id, cmd_json);
        self.call(&req)
    }

    pub fn open_pty(&self, id: &str, pty_id: &str, cols: u16, rows: u16) -> std::io::Result<String> {
        let req = format!("{{\"method\":\"OpenPty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"cols\":{},\"rows\":{}}}", id, pty_id, cols, rows);
        self.call(&req)
    }

    pub fn write_pty(&self, id: &str, pty_id: &str, data: &[u8]) -> std::io::Result<String> {
        let b64 = base64_encode(data);
        let req = format!("{{\"method\":\"WritePty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"data_b64\":\"{}\"}}", id, pty_id, b64);
        self.call(&req)
    }

    pub fn resize_pty(&self, id: &str, pty_id: &str, cols: u16, rows: u16) -> std::io::Result<String> {
        let req = format!("{{\"method\":\"ResizePty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"cols\":{},\"rows\":{}}}", id, pty_id, cols, rows);
        self.call(&req)
    }

    pub fn close_pty(&self, id: &str, pty_id: &str) -> std::io::Result<String> {
        let req = format!("{{\"method\":\"ClosePty\",\"id\":\"{}\",\"pty_id\":\"{}\"}}", id, pty_id);
        self.call(&req)
    }

    pub fn list_ptys(&self, id: &str) -> std::io::Result<String> {
        let req = format!("{{\"method\":\"ListPtys\",\"id\":\"{}\"}}", id);
        self.call(&req)
    }

    pub fn status(&self) -> std::io::Result<String> {
        self.call("{\"method\":\"Status\"}")
    }

    // Binary framed RPC: no base64, efficient for PTY/screenshot
    pub fn call_binary(&self, json_req: &str, binary_payload: Option<&[u8]>) -> std::io::Result<(String, Vec<u8>)> {
        let mut stream = UnixStream::connect(&self.binary_sock_path)?;
        let json_bytes = json_req.as_bytes();
        let json_len = json_bytes.len() as u32;
        // If binary_payload provided, we need to include binary_len in json? For simplicity, caller should include binary_len field in json
        stream.write_all(&json_len.to_be_bytes())?;
        stream.write_all(json_bytes)?;
        if let Some(bin) = binary_payload {
            stream.write_all(bin)?;
        }
        stream.flush()?;

        // Read response
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf)?;
        let resp_json_len = u32::from_be_bytes(len_buf) as usize;
        if resp_json_len == 0 || resp_json_len > 10*1024*1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid resp json len"));
        }
        let mut json_buf = vec![0u8; resp_json_len];
        stream.read_exact(&mut json_buf)?;
        let resp_json = String::from_utf8_lossy(&json_buf).to_string();

        // Try to extract stdout_len/stderr_len or len for binary size
        let mut binary = Vec::new();
        if let Some(stdout_len) = extract_number_field(&resp_json, "stdout_len") {
            let stderr_len = extract_number_field(&resp_json, "stderr_len").unwrap_or(0);
            let total = stdout_len + stderr_len;
            if total > 0 {
                binary.resize(total as usize, 0);
                stream.read_exact(&mut binary)?;
            }
        } else if let Some(len) = extract_number_field(&resp_json, "len") {
            if len > 0 && len < 100*1024*1024 {
                binary.resize(len as usize, 0);
                stream.read_exact(&mut binary)?;
            }
        }
        Ok((resp_json, binary))
    }

    pub fn exec_binary(&self, id: &str, command: Vec<String>) -> std::io::Result<(String, Vec<u8>, Vec<u8>)> {
        let cmd_json = command.iter().map(|c| format!("\"{}\"", c.replace('"', "\\\""))).collect::<Vec<_>>().join(",");
        let req = format!(r#"{{"method":"Exec","id":"{}","command":[{}]}}"#, id, cmd_json);
        let (json_resp, binary) = self.call_binary(&req, None)?;
        // binary = stdout+stderr
        let stdout_len = extract_number_field(&json_resp, "stdout_len").unwrap_or(0) as usize;
        let stderr_len = extract_number_field(&json_resp, "stderr_len").unwrap_or(0) as usize;
        let stdout = if binary.len() >= stdout_len { binary[..stdout_len].to_vec() } else { binary.clone() };
        let stderr = if binary.len() > stdout_len {
            let end = std::cmp::min(stdout_len+stderr_len, binary.len());
            binary[stdout_len..end].to_vec()
        } else { vec![] };
        Ok((json_resp, stdout, stderr))
    }

    pub fn write_pty_binary(&self, id: &str, pty_id: &str, data: &[u8]) -> std::io::Result<String> {
        let req = format!(r#"{{"method":"WritePty","id":"{}","pty_id":"{}","binary_len":{}}}"#, id, pty_id, data.len());
        let (json_resp, _) = self.call_binary(&req, Some(data))?;
        Ok(json_resp)
    }

    pub fn read_pty_binary(&self, id: &str, pty_id: &str, clear: bool) -> std::io::Result<(String, Vec<u8>)> {
        let req = format!(r#"{{"method":"ReadPty","id":"{}","pty_id":"{}","clear":{}}}"#, id, pty_id, clear);
        self.call_binary(&req, None)
    }

    pub fn signal_pty(&self, id: &str, pty_id: &str, signal: i32) -> std::io::Result<String> {
        let req = format!(r#"{{"method":"SignalPty","id":"{}","pty_id":"{}","signal":{}}}"#, id, pty_id, signal);
        self.call(&req)
    }

    pub fn signal_pty_str(&self, id: &str, pty_id: &str, signal_str: &str) -> std::io::Result<String> {
        let req = format!(r#"{{"method":"SignalPty","id":"{}","pty_id":"{}","signal_str":"{}"}}"#, id, pty_id, signal_str);
        self.call(&req)
    }

    pub fn set_pty_raw(&self, id: &str, pty_id: &str, raw: bool) -> std::io::Result<String> {
        let req = format!(r#"{{"method":"SetPtyRaw","id":"{}","pty_id":"{}","raw":{}}}"#, id, pty_id, raw);
        self.call(&req)
    }
}

fn extract_number_field(s: &str, field: &str) -> Option<u64> {
    let pat = format!("\"{}\":", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start+pat.len()..].trim_start();
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if end > 0 { return rest[..end].parse().ok(); }
    }
    None
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
