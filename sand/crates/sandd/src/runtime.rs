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
use crate::browser::BrowserManager;
use crate::computer::ComputerManager;
use crate::browser::BrowserManager;
use crate::computer::ComputerManager;
use crate::desktop::DesktopManager;

fn expand_home(path: PathBuf) -> PathBuf {
    if path == Path::new("~") || path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            if path == Path::new("~") {
                return PathBuf::from(home);
            }
            let suffix = path.strip_prefix("~/").unwrap_or_else(|_| Path::new(""));
            return PathBuf::from(home).join(suffix);
        }
    }
    path
}

#[derive(Debug, Clone, PartialEq)]
pub enum LeaseOwner {
    Task(String),      // task_id
    Workbench(String), // workbench_id or user
    Bot(String),       // bot_id
}

impl LeaseOwner {
    pub fn from_str(s: &str) -> Self {
        if s.starts_with("task:") {
            Self::Task(s.trim_start_matches("task:").to_string())
        } else if s.starts_with("workbench:") {
            Self::Workbench(s.trim_start_matches("workbench:").to_string())
        } else if s.starts_with("bot:") {
            Self::Bot(s.trim_start_matches("bot:").to_string())
        } else {
            Self::Task(s.to_string())
        }
    }
    pub fn as_str(&self) -> String {
        match self {
            Self::Task(id) => format!("task:{}", id),
            Self::Workbench(id) => format!("workbench:{}", id),
            Self::Bot(id) => format!("bot:{}", id),
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Task(_) => "Task",
            Self::Workbench(_) => "Workbench",
            Self::Bot(_) => "Bot",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeLease {
    pub lease_id: String,
    pub runtime_id: String,
    pub owner: LeaseOwner,
    pub session_id: String, // Session A/B that holds the lease
    pub created_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub active: bool,
}

impl RuntimeLease {
    pub fn new(runtime_id: String, owner: LeaseOwner, session_id: String) -> Self {
        let now = now_ms();
        let lease_id = format!("lease-{}-{}", runtime_id, now);
        Self {
            lease_id,
            runtime_id,
            owner,
            session_id,
            created_at_ms: now,
            expires_at_ms: None,
            active: true,
        }
    }
}

pub struct RuntimeManager {
    runtimes: Arc<Mutex<HashMap<String, Runtime>>>,
    processes: Arc<Mutex<HashMap<String, Vec<i32>>>>, // runtime_id -> pids
    leases: Arc<Mutex<HashMap<String, RuntimeLease>>>, // lease_id -> lease
    runtime_leases: Arc<Mutex<HashMap<String, Vec<String>>>>, // runtime_id -> lease_ids
    session_leases: Arc<Mutex<HashMap<String, String>>>, // session_id -> lease_id
    state_mgr: Arc<StateManager>,
    cgroup_mgr: Arc<CgroupManager>,
    event_bus: Arc<EventBus>,
    pty_mgr: Arc<PtyManager>,
    proc_mgr: Arc<ProcessManager>,
    desktop_mgr: Arc<DesktopManager>,
    browser_mgr: Arc<BrowserManager>,
    computer_mgr: Arc<ComputerManager>,
}

impl RuntimeManager {
    pub fn new(state_mgr: Arc<StateManager>, cgroup_mgr: Arc<CgroupManager>, event_bus: Arc<EventBus>) -> Self {
        let pty_mgr = Arc::new(PtyManager::new());
        let proc_mgr = Arc::new(ProcessManager::new(cgroup_mgr.clone()));
        let desktop_mgr = Arc::new(DesktopManager::new());
        // Browser and computer use share the runtime's desktop: Chrome renders
        // into the runtime's Xvfb and computer use injects into the same
        // display, so they must not each own a DesktopManager.
        let browser_mgr = Arc::new(BrowserManager::new(desktop_mgr.clone()));
        let computer_mgr = Arc::new(ComputerManager::new(desktop_mgr.clone()));
        Self {
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            processes: Arc::new(Mutex::new(HashMap::new())),
            leases: Arc::new(Mutex::new(HashMap::new())),
            runtime_leases: Arc::new(Mutex::new(HashMap::new())),
            session_leases: Arc::new(Mutex::new(HashMap::new())),
            state_mgr,
            cgroup_mgr,
            event_bus,
            pty_mgr,
            proc_mgr,
            desktop_mgr,
            browser_mgr,
            computer_mgr,
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
        self.create_runtime_on_machine(kind, workspace, None)
    }

    /// Create a runtime and record which machine it lives on.
    ///
    /// `machine_id == None` means "this machine" — the id is derived from the
    /// host fingerprint so that a runtime keeps the same machine id across
    /// sandd restarts and reconnects.
    pub fn create_runtime_on_machine(
        &self,
        kind: RuntimeKind,
        workspace: Option<PathBuf>,
        machine_id: Option<MachineId>,
    ) -> Result<Runtime, String> {
        let machine_id = machine_id.unwrap_or_else(sand_protocol::local_host_machine_id);
        let id = RuntimeId::new();
        let now = now_ms();
        let ws = workspace
            .map(expand_home)
            .unwrap_or_else(|| {
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
            machine_id,
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

        // destroy desktop
        self.desktop_mgr.destroy(&id.0);

        // remove from memory and state
        {
            let mut runtimes = self.runtimes.lock().unwrap();
            runtimes.remove(&id.0);
        }
        // Chrome + worker live in the runtime cgroup (already killed above);
        // this releases manager bookkeeping and stops the runtime's Xvfb.
        self.browser_mgr.destroy(&id.0);
        self.desktop_mgr.destroy(&id.0);

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

    pub fn browser_manager(&self) -> Arc<BrowserManager> {
        self.browser_mgr.clone()
    }

    pub fn computer_manager(&self) -> Arc<ComputerManager> {
        self.computer_mgr.clone()
    }

    pub fn desktop_manager(&self) -> Arc<DesktopManager> {
        self.desktop_mgr.clone()
    }

    // --- Lease model: owner Task|Workbench|Bot, leases Session A/B, Session only gets lease ---
    pub fn acquire_lease(&self, runtime_id: &str, owner_str: &str, session_id: &str) -> Result<RuntimeLease, String> {
        // Check runtime exists
        let rid = RuntimeId::from_string(runtime_id.to_string());
        if self.get_runtime(&rid).is_none() {
            return Err(format!("runtime {} not found", runtime_id));
        }
        let owner = LeaseOwner::from_str(owner_str);
        let lease = RuntimeLease::new(runtime_id.to_string(), owner, session_id.to_string());
        {
            let mut leases = self.leases.lock().unwrap();
            leases.insert(lease.lease_id.clone(), lease.clone());
        }
        {
            let mut rl = self.runtime_leases.lock().unwrap();
            rl.entry(runtime_id.to_string()).or_default().push(lease.lease_id.clone());
        }
        {
            let mut sl = self.session_leases.lock().unwrap();
            sl.insert(session_id.to_string(), lease.lease_id.clone());
        }
        eprintln!("[lease] acquired {} for runtime {} owner {} session {}", lease.lease_id, runtime_id, owner_str, session_id);
        Ok(lease)
    }

    pub fn release_lease(&self, lease_id: &str) -> Result<(), String> {
        let lease_opt = {
            let mut leases = self.leases.lock().unwrap();
            leases.remove(lease_id)
        };
        if let Some(lease) = lease_opt {
            {
                let mut rl = self.runtime_leases.lock().unwrap();
                if let Some(list) = rl.get_mut(&lease.runtime_id) {
                    list.retain(|id| id != lease_id);
                }
            }
            {
                let mut sl = self.session_leases.lock().unwrap();
                sl.retain(|_, v| v != lease_id);
            }
            eprintln!("[lease] released {} for runtime {} session {}", lease_id, lease.runtime_id, lease.session_id);
            Ok(())
        } else {
            Err(format!("lease {} not found", lease_id))
        }
    }

    pub fn release_lease_by_session(&self, session_id: &str) -> Result<(), String> {
        let lease_id_opt = {
            let sl = self.session_leases.lock().unwrap();
            sl.get(session_id).cloned()
        };
        if let Some(lease_id) = lease_id_opt {
            self.release_lease(&lease_id)
        } else {
            Err(format!("no lease for session {}", session_id))
        }
    }

    pub fn list_leases(&self, runtime_id: Option<&str>) -> Vec<RuntimeLease> {
        let leases = self.leases.lock().unwrap();
        if let Some(rid) = runtime_id {
            leases.values().filter(|l| l.runtime_id == rid && l.active).cloned().collect()
        } else {
            leases.values().filter(|l| l.active).cloned().collect()
        }
    }

    pub fn get_lease_for_session(&self, session_id: &str) -> Option<RuntimeLease> {
        let sl = self.session_leases.lock().unwrap();
        if let Some(lease_id) = sl.get(session_id) {
            let leases = self.leases.lock().unwrap();
            leases.get(lease_id).cloned()
        } else {
            None
        }
    }

    // Task complete+confirmed -> destroy runtime, Workbench long-lived
    pub fn try_destroy_if_task_complete(&self, runtime_id: &str, owner_str: &str) -> Result<bool, String> {
        // If owner is Task and task is complete+confirmed, destroy runtime
        // For now, we check if owner is Task and no active leases for that runtime, then destroy
        // Workbench leases are long-lived, so we don't auto-destroy them
        let owner = LeaseOwner::from_str(owner_str);
        match owner {
            LeaseOwner::Task(_) => {
                // Check if any active leases remain for this runtime
                let active_leases = self.list_leases(Some(runtime_id));
                if active_leases.is_empty() {
                    let rid = RuntimeId::from_string(runtime_id.to_string());
                    self.destroy_runtime(&rid)?;
                    eprintln!("[lease] Task {} complete+confirmed, destroyed runtime {}", owner_str, runtime_id);
                    Ok(true)
                } else {
                    eprintln!("[lease] Task {} complete but {} leases still active, not destroying runtime {}", owner_str, active_leases.len(), runtime_id);
                    Ok(false)
                }
            }
            LeaseOwner::Workbench(_) => {
                // Workbench long-lived, do not destroy
                eprintln!("[lease] Workbench {} owns runtime {}, keeping long-lived", owner_str, runtime_id);
                Ok(false)
            }
            LeaseOwner::Bot(_) => {
                // Bot: check if task associated? For now, same as Task but allow explicit destroy
                let active_leases = self.list_leases(Some(runtime_id));
                if active_leases.is_empty() {
                    let rid = RuntimeId::from_string(runtime_id.to_string());
                    self.destroy_runtime(&rid)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }
}
