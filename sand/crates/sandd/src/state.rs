use std::path::{Path, PathBuf};
use std::fs;
use std::io::{Write, BufRead};
use std::collections::HashMap;
use sand_protocol::{MachineId, RuntimeId, Runtime, RuntimeKind, RuntimeState, now_ms};

use crate::state_sqlite::SqliteState;

#[derive(Debug)]
pub struct StateManager {
    path: PathBuf,
    sqlite: Option<SqliteState>,
}

impl StateManager {
    pub fn new(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let sqlite_path = if path.to_string_lossy().contains("state.json") {
            path.with_file_name("state.db")
        } else {
            PathBuf::from(format!("{}.db", path.display()))
        };

        let sqlite = match SqliteState::new(sqlite_path.clone()) {
            Ok(db) => {
                eprintln!("[state] using SQLite WAL at {}", sqlite_path.display());
                Some(db)
            }
            Err(e) => {
                eprintln!("[state] SQLite init failed ({}), fallback to JSON at {}", e, path.display());
                None
            }
        };

        Self { path, sqlite }
    }

    /// Apply additive schema migrations. SQLite has no
    /// `ADD COLUMN IF NOT EXISTS`, so a duplicate-column error is expected and
    /// ignored on databases that already have the column.
    pub fn ensure_schema(&self) -> std::io::Result<()> {
        if let Some(sqlite) = &self.sqlite {
            let _ = sqlite.ensure_column("runtime", "machine_id", "TEXT");
        }
        Ok(())
    }

    pub fn save_runtime(&self, rt: &Runtime) -> std::io::Result<()> {
        if let Some(sqlite) = &self.sqlite {
            let caps = rt.capabilities.join(",");
            let cgroup_str = rt.cgroup_path.as_ref().map(|p| p.display().to_string());
            let state_str = rt.state.as_str();
            let ws_str = rt.workspace.display().to_string();
            if let Err(e) = sqlite.save_runtime(
                &rt.id.0,
                rt.kind.as_str(),
                &state_str,
                &ws_str,
                cgroup_str.as_deref(),
                rt.created_at_ms,
                rt.started_at_ms,
                &caps,
                rt.process_count,
                rt.pty_count,
                rt.machine_id.as_str(),
            ) {
                eprintln!("[state] sqlite save failed: {}, fallback to json", e);
                let mut runtimes = self.load_all_json().unwrap_or_default();
                runtimes.insert(rt.id.0.clone(), rt.clone());
                return self.save_all_json(&runtimes);
            }
            let mut runtimes = self.load_all_json().unwrap_or_default();
            runtimes.insert(rt.id.0.clone(), rt.clone());
            let _ = self.save_all_json(&runtimes);
            Ok(())
        } else {
            let mut runtimes = self.load_all().unwrap_or_default();
            runtimes.insert(rt.id.0.clone(), rt.clone());
            self.save_all(&runtimes)
        }
    }

    pub fn remove_runtime(&self, id: &RuntimeId) -> std::io::Result<()> {
        if let Some(sqlite) = &self.sqlite {
            let _ = sqlite.remove_runtime(&id.0);
        }
        let mut runtimes = self.load_all_json().unwrap_or_default();
        runtimes.remove(&id.0);
        self.save_all_json(&runtimes)
    }

    pub fn load_all(&self) -> std::io::Result<HashMap<String, Runtime>> {
        if let Some(sqlite) = &self.sqlite {
            match sqlite.load_runtimes() {
                Ok(rows) => {
                    let mut map = HashMap::new();
                    for row in rows {
                        let kind = RuntimeKind::from_str(&row.kind);
                        let state = match row.state.as_str() {
                            "stopped" => RuntimeState::Stopped,
                            "stopping" => RuntimeState::Stopping,
                            "starting" => RuntimeState::Starting,
                            "creating" => RuntimeState::Creating,
                            "killed" => RuntimeState::Killed,
                            "oom" => RuntimeState::Oom,
                            s if s.starts_with("failed") => RuntimeState::Failed { reason: s.to_string() },
                            s if s.starts_with("crashed") => RuntimeState::Crashed { code: None, signal: None },
                            _ => RuntimeState::Running,
                        };
                        let rt = Runtime {
                            id: RuntimeId::from_string(row.id.clone()),
                            kind,
                            state,
                            workspace: PathBuf::from(row.workspace),
                            cgroup_path: row.cgroup_path.map(PathBuf::from),
                            created_at_ms: row.created_at,
                            started_at_ms: row.started_at,
                            capabilities: if row.capabilities.is_empty() { vec![] } else { row.capabilities.split(',').map(|s| s.to_string()).collect() },
                            process_count: row.procs,
                            pty_count: row.ptys,
                            machine_id: row
                                .machine_id
                                .clone()
                                .map(MachineId::from_string)
                                .unwrap_or_else(sand_protocol::local_host_machine_id),
                        };
                        map.insert(row.id, rt);
                    }
                    return Ok(map);
                }
                Err(e) => {
                    eprintln!("[state] sqlite load failed: {}, fallback to json", e);
                }
            }
        }
        self.load_all_json()
    }

    fn load_all_json(&self) -> std::io::Result<HashMap<String, Runtime>> {
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
            if let Some(id) = extract_field(&line, "id") {
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

                let machine_id = extract_field(&line, "machine_id")
                    .map(MachineId::from_string)
                    .unwrap_or_else(sand_protocol::local_host_machine_id);
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
                    machine_id,
                };
                map.insert(id, rt);
            }
        }
        Ok(map)
    }

    fn save_all(&self, runtimes: &HashMap<String, Runtime>) -> std::io::Result<()> {
        self.save_all_json(runtimes)
    }

    fn save_all_json(&self, runtimes: &HashMap<String, Runtime>) -> std::io::Result<()> {
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

    pub fn sqlite_path(&self) -> Option<PathBuf> {
        self.sqlite.as_ref().map(|s| s.path().to_path_buf())
    }
}

fn extract_field(line: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\":", field);
    if let Some(start) = line.find(&pattern) {
        let rest = &line[start + pattern.len()..];
        let rest = rest.trim_start();
        if rest.starts_with('\"') {
            let end = rest[1..].find('\"')?;
            Some(rest[1..1+end].to_string())
        } else {
            let end = rest.find(|c| c == ',' || c == '}').unwrap_or(rest.len());
            Some(rest[..end].trim().trim_matches('\"').to_string())
        }
    } else {
        None
    }
}
