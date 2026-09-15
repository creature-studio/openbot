use std::path::PathBuf;
use sand_client::SandClient;

/// Workbench is a long-lived runtime that persists across sessions
/// It holds user's workspace, files, browser, terminal
pub struct WorkbenchManager {
    client: SandClient,
    workbench_id: Option<String>,
}

impl WorkbenchManager {
    pub fn new() -> Self {
        Self {
            client: SandClient::new(None),
            workbench_id: None,
        }
    }

    /// Ensure workbench runtime exists, create if not
    pub fn ensure_workbench(&mut self) -> Result<String, String> {
        if let Some(id) = &self.workbench_id {
            // Check if still exists
            if let Ok(resp) = self.client.get_runtime(id) {
                if resp.contains("\"ok\":true") {
                    return Ok(id.clone());
                }
            }
        }
        // Try to find existing workbench runtime from list
        if let Ok(list_resp) = self.client.list_runtimes() {
            // naive parse for workbench kind
            if let Some(id) = extract_workbench_id(&list_resp) {
                self.workbench_id = Some(id.clone());
                return Ok(id);
            }
        }
        // Create new workbench runtime
        let resp = self.client.create_runtime("workbench", None).map_err(|e| e.to_string())?;
        if let Some(id) = extract_id(&resp) {
            self.workbench_id = Some(id.clone());
            // Ensure display for workbench
            let _ = self.client.call(&format!(r#"{{"method":"EnsureDisplay","id":"{}","width":1280,"height":720}}"#, id));
            return Ok(id);
        }
        Err(format!("failed to create workbench: {}", resp))
    }

    pub fn get_workbench_id(&self) -> Option<String> {
        self.workbench_id.clone()
    }

    pub fn destroy_workbench(&self) -> Result<(), String> {
        if let Some(id) = &self.workbench_id {
            let _ = self.client.destroy_runtime(id);
        }
        Ok(())
    }
}

fn extract_id(s: &str) -> Option<String> {
    let pat = "\"id\":\"";
    let start = s.find(pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_workbench_id(list_json: &str) -> Option<String> {
    // Look for "kind":"workbench" and extract nearby id
    // Very naive: split by },{
    for chunk in list_json.split("},{") {
        if chunk.contains("\"kind\":\"workbench\"") {
            if let Some(id) = extract_id(chunk) {
                return Some(id);
            }
        }
    }
    None
}
