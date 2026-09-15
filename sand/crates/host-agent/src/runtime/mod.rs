use sand_client::SandClient;

pub struct RuntimeManager {
    client: SandClient,
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self { client: SandClient::new(None) }
    }

    pub fn create_runtime(&self, kind: &str) -> Result<String, String> {
        let resp = self.client.create_runtime(kind, None).map_err(|e| e.to_string())?;
        extract_field(&resp, "id").ok_or("no id in response".to_string())
    }

    pub fn destroy_runtime(&self, id: &str) -> Result<(), String> {
        self.client.destroy_runtime(id).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_runtimes(&self) -> Result<String, String> {
        self.client.list_runtimes().map_err(|e| e.to_string())
    }

    // Lease model: owner Task|Workbench|Bot, leases Session A/B, Session only gets lease
    pub fn acquire_lease(&self, runtime_id: &str, owner: &str, session_id: &str) -> Result<String, String> {
        // Call sandd RPC AcquireLease
        let req = format!(r#"{{"method":"AcquireLease","runtime_id":"{}","owner":"{}","session_id":"{}"}}"#, runtime_id, owner, session_id);
        let resp = raw_rpc(&req)?;
        extract_field(&resp, "lease_id").ok_or_else(|| format!("no lease_id in response: {}", resp))
    }

    pub fn release_lease(&self, lease_id: &str) -> Result<(), String> {
        let req = format!(r#"{{"method":"ReleaseLease","lease_id":"{}"}}"#, lease_id);
        let resp = raw_rpc(&req)?;
        if resp.contains("\"ok\":true") { Ok(()) } else { Err(resp) }
    }

    pub fn release_lease_by_session(&self, session_id: &str) -> Result<(), String> {
        let req = format!(r#"{{"method":"ReleaseLease","session_id":"{}"}}"#, session_id);
        let resp = raw_rpc(&req)?;
        if resp.contains("\"ok\":true") { Ok(()) } else { Err(resp) }
    }

    pub fn list_leases(&self, runtime_id: Option<&str>) -> String {
        let req = if let Some(rid) = runtime_id {
            format!(r#"{{"method":"ListLeases","runtime_id":"{}"}}"#, rid)
        } else {
            r#"{"method":"ListLeases"}"#.to_string()
        };
        raw_rpc(&req).unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"{}\"}}", e))
    }

    pub fn task_complete(&self, runtime_id: &str, owner: &str) -> Result<bool, String> {
        let req = format!(r#"{{"method":"TaskComplete","runtime_id":"{}","owner":"{}"}}"#, runtime_id, owner);
        let resp = raw_rpc(&req)?;
        if resp.contains("\"destroyed\":true") { Ok(true) } else { Ok(false) }
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

fn extract_field(s: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
