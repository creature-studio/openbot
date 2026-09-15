use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::fs;

use sand_protocol::{RuntimeId, RuntimeKind, RuntimeState, Runtime, now_ms, RuntimeEventKind};

use crate::state::StateManager;
use crate::cgroup::CgroupManager;
use crate::events::EventBus;
use crate::process::ProcessManager;
use crate::pty::PtyManager;

pub struct RuntimeManager {
    runtimes: Arc<Mutex<HashMap<String, Runtime>>>,
    processes: Arc<Mutex<HashMap<String, Vec<i32>>>>, // runtime_id -> pids
    state_mgr: Arc<StateManager>,
    cgroup_mgr: Arc<CgroupManager>,
    event_bus: Arc<EventBus>,
    pty_mgr: Arc<PtyManager>,
    proc_mgr: Arc<ProcessManager>,
}

impl RuntimeManager {
    pub fn new(state_mgr: Arc<StateManager>, cgroup_mgr: Arc<CgroupManager>, event_bus: Arc<EventBus>) -> Self {
        let pty_mgr = Arc::new(PtyManager::new());
        let proc_mgr = Arc::new(ProcessManager::new(cgroup_mgr.clone()));
        Self {
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            processes: Arc::new(Mutex::new(HashMap::new())),
            state_mgr,
            cgroup_mgr,
            event_bus,
            pty_mgr,
            proc_mgr,
        }
    }

    pub fn recover(&self) -> std::io::Result<()> {
        // load from state file
        let loaded = self.state_mgr.load_all()?;
        let mut runtimes = self.runtimes.lock().unwrap();
        for (id, rt) in loaded {
            // check if cgroup still exists and has pids, or if processes alive
            let cgroup_path = self.cgroup_mgr.runtime_cgroup_path(&id);
            let pids = self.cgroup_mgr.list_pids(&id);
            let mut state = rt.state.clone();
            if !cgroup_path.exists() && pids.is_empty() {
                // dead
                state = RuntimeState::Stopped;
            } else if !pids.is_empty() {
                // still running
                state = RuntimeState::Running;
            } else {
                state = RuntimeState::Stopped;
            }
            let mut rt = rt;
            rt.state = state;
            rt.cgroup_path = Some(cgroup_path);
            rt.process_count = pids.len();
            runtimes.insert(id.clone(), rt);
        }
        Ok(())
    }

    pub fn create_runtime(&self, kind: RuntimeKind, workspace: Option<PathBuf>) -> Result<Runtime, String> {
        let id = RuntimeId::new();
        let now = now_ms();
        let ws = workspace.unwrap_or_else(|| {
            let base = if Path::new("/workspace").exists() {
                PathBuf::from("/workspace")
            } else {
                PathBuf::from("/tmp")
            };
            base.join(format!("sand-runtime-{}", id.0))
        });

        let _ = fs::create_dir_all(&ws);

        // create cgroup
        let cgroup_path = self.cgroup_mgr.create_cgroup(&id.0).map_err(|e| format!("cgroup create failed: {}", e))?;

        let rt = Runtime {
            id: id.clone(),
            kind,
            state: RuntimeState::Running,
            workspace: ws,
            cgroup_path: Some(cgroup_path),
            created_at_ms: now,
            started_at_ms: Some(now),
            capabilities: vec![
                format!("runtime:{}:exec", id.0),
                format!("runtime:{}:pty", id.0),
                format!("runtime:{}:fs", id.0),
            ],
            process_count: 0,
            pty_count: 0,
        };

        {
            let mut runtimes = self.runtimes.lock().unwrap();
            runtimes.insert(id.0.clone(), rt.clone());
        }

        // persist
        let _ = self.state_mgr.save_runtime(&rt);

        self.event_bus.emit(id.clone(), RuntimeEventKind::Created);
        self.event_bus.emit(id.clone(), RuntimeEventKind::Started);

        Ok(rt)
    }

    pub fn get_runtime(&self, id: &RuntimeId) -> Option<Runtime> {
        let mut runtimes = self.runtimes.lock().unwrap();
        if let Some(rt) = runtimes.get(&id.0) {
            let mut rt = rt.clone();
            // update counts
            let pids = self.cgroup_mgr.list_pids(&id.0);
            rt.process_count = pids.len();
            rt.pty_count = self.pty_mgr.count_for_runtime(&id.0);
            // check cgroup stats for OOM?
            let stats = self.cgroup_mgr.read_stats(&id.0);
            if stats.oom_kills > 0 {
                rt.state = RuntimeState::Oom;
            }
            Some(rt)
        } else {
            None
        }
    }

    pub fn list_runtimes(&self) -> Vec<Runtime> {
        let runtimes = self.runtimes.lock().unwrap();
        let mut list = Vec::new();
        for rt in runtimes.values() {
            let mut rt = rt.clone();
            rt.process_count = self.cgroup_mgr.list_pids(&rt.id.0).len();
            rt.pty_count = self.pty_mgr.count_for_runtime(&rt.id.0);
            list.push(rt);
        }
        list
    }

    pub fn destroy_runtime(&self, id: &RuntimeId) -> Result<(), String> {
        // set stopping
        {
            let mut runtimes = self.runtimes.lock().unwrap();
            if let Some(rt) = runtimes.get_mut(&id.0) {
                rt.state = RuntimeState::Stopping;
            } else {
                return Err(format!("runtime {} not found", id.0));
            }
        }

        // kill PTYs
        self.pty_mgr.destroy_for_runtime(&id.0);

        // kill cgroup
        let _ = self.cgroup_mgr.kill_cgroup(&id.0);

        // wait a bit for procs to die
        for _ in 0..10 {
            if self.cgroup_mgr.is_empty(&id.0) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // force kill remaining via proc manager
        self.proc_mgr.kill_all_for_runtime(&id.0);

        // destroy cgroup
        let _ = self.cgroup_mgr.destroy_cgroup(&id.0);

        // remove workspace? keep for now, but clean?
        // let _ = fs::remove_dir_all(&rt.workspace);

        // remove from memory and state
        {
            let mut runtimes = self.runtimes.lock().unwrap();
            runtimes.remove(&id.0);
        }
        let _ = self.state_mgr.remove_runtime(id);
        self.event_bus.emit(id.clone(), RuntimeEventKind::Destroyed);

        Ok(())
    }

    pub fn exec(&self, runtime_id: &RuntimeId, command: Vec<String>, cwd: Option<String>, env: HashMap<String, String>, timeout_ms: Option<u64>) -> Result<crate::process::ExecResult, String> {
        // check runtime exists
        let rt = self.get_runtime(runtime_id).ok_or_else(|| format!("runtime {} not found", runtime_id.0))?;

        // use process manager robust
        let result = self.proc_mgr.exec_robust(runtime_id, &rt.workspace, command, cwd, env, timeout_ms)?;

        // track pid
        {
            let mut procs = self.processes.lock().unwrap();
            procs.entry(runtime_id.0.clone()).or_default().push(result.pid);
        }

        Ok(result)
    }

    pub fn spawn_background(&self, runtime_id: &RuntimeId, command: Vec<String>, cwd: Option<String>, env: HashMap<String, String>) -> Result<i32, String> {
        let rt = self.get_runtime(runtime_id).ok_or_else(|| format!("runtime {} not found", runtime_id.0))?;
        let pid = self.proc_mgr.spawn_background(runtime_id, &rt.workspace, command, cwd, env)?;
        {
            let mut procs = self.processes.lock().unwrap();
            procs.entry(runtime_id.0.clone()).or_default().push(pid);
        }
        Ok(pid)
    }

    pub fn list_processes(&self, runtime_id: &RuntimeId) -> Vec<i32> {
        self.cgroup_mgr.list_pids(&runtime_id.0)
    }

    pub fn pty_manager(&self) -> Arc<PtyManager> {
        self.pty_mgr.clone()
    }

    pub fn event_bus(&self) -> Arc<EventBus> {
        self.event_bus.clone()
    }

    pub fn cgroup_manager(&self) -> Arc<CgroupManager> {
        self.cgroup_mgr.clone()
    }
}
