use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use crate::runtime::RuntimeManager;
use sand_protocol::{RuntimeId, RuntimeKind};

#[derive(Debug, Clone)]
pub struct DesiredState {
    pub runtime_id: String,
    pub kind: RuntimeKind,
    pub should_exist: bool,
    pub should_running: bool,
    pub min_procs: usize,
}

#[derive(Debug, Clone)]
pub struct ObservedState {
    pub exists: bool,
    pub running: bool,
    pub procs: usize,
    pub ptys: usize,
    pub last_check_ms: u64,
}

pub struct Supervisor {
    runtime_mgr: Arc<RuntimeManager>,
    desired: Arc<Mutex<HashMap<String, DesiredState>>>,
}

impl Supervisor {
    pub fn new(runtime_mgr: Arc<RuntimeManager>) -> Self {
        Self {
            runtime_mgr,
            desired: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn set_desired(&self, desired: DesiredState) {
        let mut map = self.desired.lock().unwrap();
        map.insert(desired.runtime_id.clone(), desired);
    }

    pub fn remove_desired(&self, runtime_id: &str) {
        let mut map = self.desired.lock().unwrap();
        map.remove(runtime_id);
    }

    pub fn get_observed(&self, runtime_id: &str) -> ObservedState {
        let id = RuntimeId::from_string(runtime_id.to_string());
        if let Some(rt) = self.runtime_mgr.get_runtime(&id) {
            ObservedState {
                exists: true,
                running: rt.state.as_str() == "running",
                procs: rt.process_count,
                ptys: rt.pty_count,
                last_check_ms: sand_protocol::now_ms(),
            }
        } else {
            ObservedState {
                exists: false,
                running: false,
                procs: 0,
                ptys: 0,
                last_check_ms: sand_protocol::now_ms(),
            }
        }
    }

    pub fn reconcile(&self) -> Vec<String> {
        let desired_map = self.desired.lock().unwrap().clone();
        let mut actions = Vec::new();
        for (id, desired) in desired_map.iter() {
            let observed = self.get_observed(id);
            if desired.should_exist && !observed.exists {
                // Should create
                eprintln!("[supervisor] desired {} exists but observed missing, would create", id);
                actions.push(format!("create {}", id));
                // In real system, we'd create here
                // For MVP, just log
            } else if !desired.should_exist && observed.exists {
                eprintln!("[supervisor] desired {} not exist but observed exists, destroying", id);
                let rid = RuntimeId::from_string(id.clone());
                if self.runtime_mgr.destroy_runtime(&rid).is_ok() {
                    actions.push(format!("destroyed {}", id));
                }
            } else if desired.should_running && !observed.running && observed.exists {
                eprintln!("[supervisor] desired {} running but observed not, restarting?", id);
                actions.push(format!("restart {}", id));
            }
            // Check procs
            if observed.exists && desired.min_procs > 0 && observed.procs < desired.min_procs {
                eprintln!("[supervisor] runtime {} has {} procs < desired {}", id, observed.procs, desired.min_procs);
                actions.push(format!("low procs {}: {} < {}", id, observed.procs, desired.min_procs));
            }
        }
        actions
    }

    pub fn run_loop(self: Arc<Self>) {
        std::thread::spawn(move || {
            loop {
                let actions = self.reconcile();
                if !actions.is_empty() {
                    eprintln!("[supervisor] reconcile actions: {:?}", actions);
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        });
    }
}
