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
            // Runtime kernel should only: discover, record, report, cleanup
            // It should NOT auto-create or auto-restart, that is up to Agent/Task policy
            if desired.should_exist && !observed.exists {
                // Should exist but missing -> report as failed, let upper layer decide to recreate
                eprintln!("[supervisor] OBSERVED: desired {} should_exist=true but observed missing -> report failed, NOT auto-creating (upper layer decides)", id);
                actions.push(format!("report missing {} (should_exist but not found)", id));
                // Emit event via EventBus? For now just log, upper layer will see via GetObservedState
                // Do NOT auto-create
            } else if !desired.should_exist && observed.exists {
                // Should NOT exist but exists -> cleanup (this is allowed: kernel cleans up)
                eprintln!("[supervisor] OBSERVED: desired {} should_exist=false but observed exists -> cleanup destroying", id);
                let rid = RuntimeId::from_string(id.clone());
                if self.runtime_mgr.destroy_runtime(&rid).is_ok() {
                    actions.push(format!("cleaned up destroyed {}", id));
                }
            } else if desired.should_running && !observed.running && observed.exists {
                // Should be running but not -> report failed, NOT auto-restart
                eprintln!("[supervisor] OBSERVED: desired {} should_running=true but observed running=false state={} -> report failed, NOT auto-restart (Task policy decides)", id, if observed.exists { "stopped" } else { "missing" });
                actions.push(format!("report not_running {} (should_running but stopped)", id));
                // Do NOT auto-restart
            }
            // Check procs - just report
            if observed.exists && desired.min_procs > 0 && observed.procs < desired.min_procs {
                eprintln!("[supervisor] OBSERVED: runtime {} has {} procs < desired {} -> report low", id, observed.procs, desired.min_procs);
                actions.push(format!("report low procs {}: {} < {}", id, observed.procs, desired.min_procs));
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
