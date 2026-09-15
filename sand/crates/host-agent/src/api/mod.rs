// API for host-agent - Bot -> Session -> Runtime (not Runtime=Agent), supports handoff and Lease
// Bot is long-lived, Session is per-conversation, Runtime is per-execution env
// Lease: owner Task|Workbench|Bot, leases Session A/B, Session only gets lease, Session close -> release lease, Task complete+confirmed -> destroy runtime, Workbench long-lived

use crate::agent::session::{AgentSession, AgentSessionId};
use crate::runtime::RuntimeManager;
use crate::workbench::WorkbenchManager;

pub struct Bot {
    pub id: String,
    pub sessions: Vec<String>,
    pub workbench_id: Option<String>,
    pub runtime_leases: Vec<String>, // lease_ids
}

pub struct HostAgentApi {
    runtime_mgr: RuntimeManager,
    workbench_mgr: std::sync::Mutex<WorkbenchManager>,
    bots: std::sync::Mutex<Vec<Bot>>,
}

impl HostAgentApi {
    pub fn new() -> Self {
        Self {
            runtime_mgr: RuntimeManager::new(),
            workbench_mgr: std::sync::Mutex::new(WorkbenchManager::new()),
            bots: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn create_bot(&self) -> String {
        let bot_id = format!("bot-{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
        let mut bots = self.bots.lock().unwrap();
        bots.push(Bot { id: bot_id.clone(), sessions: vec![], workbench_id: None, runtime_leases: vec![] });
        bot_id
    }

    pub fn create_session(&self, model: String) -> Result<AgentSession, String> {
        let runtime_id = self.runtime_mgr.create_runtime("assistant")?;
        Ok(AgentSession::new(runtime_id, model))
    }

    // New: Create Bot -> Create Session -> Acquire Runtime with Lease
    pub fn create_bot_session_with_lease(&self, bot_id: &str, model: String, owner: &str) -> Result<(AgentSession, String), String> {
        // Create runtime
        let runtime_id = self.runtime_mgr.create_runtime("assistant")?;
        // Create session
        let session = AgentSession::new(runtime_id.clone(), model);
        // Acquire lease: owner Task|Workbench|Bot, session gets lease
        let lease_id = self.runtime_mgr.acquire_lease(&runtime_id, owner, &session.id)?;
        // Update bot
        {
            let mut bots = self.bots.lock().unwrap();
            if let Some(bot) = bots.iter_mut().find(|b| b.id == bot_id) {
                bot.sessions.push(session.id.clone());
                bot.runtime_leases.push(lease_id.clone());
            }
        }
        Ok((session, lease_id))
    }

    pub fn create_session_with_runtime(&self, runtime_id: String, model: String) -> AgentSession {
        AgentSession::new(runtime_id, model)
    }

    pub fn create_session_in_workbench(&self, model: String) -> Result<AgentSession, String> {
        let mut wb = self.workbench_mgr.lock().unwrap();
        let wb_id = wb.ensure_workbench()?;
        Ok(AgentSession::new(wb_id, model))
    }

    pub fn handoff_session(&self, old_session: &AgentSession, new_model: Option<String>) -> AgentSession {
        old_session.handoff(new_model)
    }

    pub fn list_runtimes(&self) -> Result<String, String> {
        self.runtime_mgr.list_runtimes()
    }

    pub fn ensure_workbench(&self) -> Result<String, String> {
        let mut wb = self.workbench_mgr.lock().unwrap();
        wb.ensure_workbench()
    }

    // Lease management
    pub fn acquire_lease(&self, runtime_id: &str, owner: &str, session_id: &str) -> Result<String, String> {
        self.runtime_mgr.acquire_lease(runtime_id, owner, session_id)
    }

    pub fn release_lease(&self, lease_id: &str) -> Result<(), String> {
        self.runtime_mgr.release_lease(lease_id)
    }

    pub fn release_lease_by_session(&self, session_id: &str) -> Result<(), String> {
        self.runtime_mgr.release_lease_by_session(session_id)
    }

    pub fn list_leases(&self, runtime_id: Option<&str>) -> String {
        self.runtime_mgr.list_leases(runtime_id)
    }

    pub fn task_complete(&self, runtime_id: &str, owner: &str) -> Result<bool, String> {
        self.runtime_mgr.task_complete(runtime_id, owner)
    }

    // Bot E2E: close session -> release lease, Task complete+confirmed -> destroy runtime
    pub fn close_session(&self, session_id: &str) -> Result<(), String> {
        // Session close -> release lease
        let _ = self.release_lease_by_session(session_id);
        Ok(())
    }
}
