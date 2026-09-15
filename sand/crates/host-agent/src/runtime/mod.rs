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
        // Parse id from JSON
        extract_field(&resp, "id").ok_or("no id in response".to_string())
    }

    pub fn destroy_runtime(&self, id: &str) -> Result<(), String> {
        self.client.destroy_runtime(id).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_runtimes(&self) -> Result<String, String> {
        self.client.list_runtimes().map_err(|e| e.to_string())
    }
}

fn extract_field(s: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
