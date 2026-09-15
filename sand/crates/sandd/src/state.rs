use std::path::{Path, PathBuf};
use std::fs;
use std::io::{Write, BufRead};
use std::collections::HashMap;
use sand_protocol::{RuntimeId, Runtime, RuntimeKind, RuntimeState, now_ms};

#[derive(Debug)]
pub struct StateManager {
    path: PathBuf,
}

impl StateManager {
    pub fn new(path: PathBuf) -> Self {
        // ensure dir exists
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self { path }
    }

    pub fn save_runtime(&self, rt: &Runtime) -> std::io::Result<()> {
        // append or update? For simplicity, we store all runtimes in file as json lines, rewrite file each time
        // read existing, update, write
        let mut runtimes = self.load_all().unwrap_or_default();
        runtimes.insert(rt.id.0.clone(), rt.clone());
        self.save_all(&runtimes)
    }

    pub fn remove_runtime(&self, id: &RuntimeId) -> std::io::Result<()> {
        let mut runtimes = self.load_all().unwrap_or_default();
        runtimes.remove(&id.0);
        self.save_all(&runtimes)
    }

    pub fn load_all(&self) -> std::io::Result<HashMap<String, Runtime>> {
        let mut map = HashMap::new();
        if !self.path.exists() {
            return Ok(map);
        }
        let file = fs::File::open(&self.path)?;
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            // simple parsing: we stored via to_json_line which is not full json but we can parse id
            // For now, parse id field
            if let Some(id) = extract_field(&line, "id") {
                // try to parse kind, state, workspace etc.
                let kind_str = extract_field(&line, "kind").unwrap_or_else(|| "assistant".to_string());
                let state_str = extract_field(&line, "state").unwrap_or_else(|| "running".to_string());
                let workspace_str = extract_field(&line, "workspace").unwrap_or_else(|| "/tmp".to_string());
                let created = extract_field(&line, "created").and_then(|s| s.parse::<u64>().ok()).unwrap_or(now_ms());
                let started = extract_field(&line, "started").and_then(|s| s.parse::<u64>().ok());

                let kind = RuntimeKind::from_str(&kind_str);
                let state = if state_str.starts_with("failed") {
                    RuntimeState::Failed { reason: state_str }
                } else if state_str == "killed" {
                    RuntimeState::Killed
                } else if state_str == "oom" {
                    RuntimeState::Oom
                } else if state_str.starts_with("crashed") {
                    RuntimeState::Crashed { code: None, signal: None }
                } else if state_str == "stopped" {
                    RuntimeState::Stopped
                } else if state_str == "stopping" {
                    RuntimeState::Stopping
                } else if state_str == "starting" {
                    RuntimeState::Starting
                } else if state_str == "creating" {
                    RuntimeState::Creating
                } else {
                    RuntimeState::Running
                };

                let rt = Runtime {
                    id: RuntimeId::from_string(id.clone()),
                    kind,
                    state,
                    workspace: PathBuf::from(workspace_str),
                    cgroup_path: None,
                    created_at_ms: created,
                    started_at_ms: if started.unwrap_or(0) == 0 { None } else { started },
                    capabilities: vec![],
                    process_count: 0,
                    pty_count: 0,
                };
                map.insert(id, rt);
            }
        }
        Ok(map)
    }

    fn save_all(&self, runtimes: &HashMap<String, Runtime>) -> std::io::Result<()> {
        let tmp_path = self.path.with_extension("tmp");
        let mut file = fs::File::create(&tmp_path)?;
        for rt in runtimes.values() {
            writeln!(file, "{}", rt.to_json_line())?;
        }
        file.flush()?;
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn extract_field(line: &str, field: &str) -> Option<String> {
    // very naive: look for "field":"value" or "field":value
    let pattern = format!("\"{}\":", field);
    if let Some(start) = line.find(&pattern) {
        let rest = &line[start + pattern.len()..];
        let rest = rest.trim_start();
        if rest.starts_with('"') {
            // string value
            let end = rest[1..].find('"')?;
            Some(rest[1..1+end].to_string())
        } else {
            // number or until , or }
            let end = rest.find(|c| c == ',' || c == '}').unwrap_or(rest.len());
            Some(rest[..end].trim().trim_matches('"').to_string())
        }
    } else {
        None
    }
}
