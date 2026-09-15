use std::os::unix::net::UnixStream;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct SandClient {
    sock_path: PathBuf,
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
        Self { sock_path: path }
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
