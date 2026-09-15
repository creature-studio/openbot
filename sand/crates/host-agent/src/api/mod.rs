// API for host-agent - Bot -> Session -> Runtime (not Runtime=Agent), supports handoff
// Bot is long-lived, Session is per-conversation, Runtime is per-execution env

use crate::agent::session::{AgentSession, AgentSessionId};
use crate::runtime::RuntimeManager;
use crate::workbench::WorkbenchManager;

pub struct Bot {
    pub id: String,
    pub sessions: Vec<AgentSessionId>,
    pub workbench_id: Option<String>,
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
        bots.push(Bot { id: bot_id.clone(), sessions: vec![], workbench_id: None });
        bot_id
    }

    pub fn create_session(&self, model: String) -> Result<AgentSession, String> {
        let runtime_id = self.runtime_mgr.create_runtime("assistant")?;
        Ok(AgentSession::new(runtime_id, model))
    }

    pub fn create_session_with_runtime(&self, runtime_id: String, model: String) -> AgentSession {
        // Bot -> Session -> Runtime: reuse existing runtime for new session (handoff)
        AgentSession::new(runtime_id, model)
    }

    pub fn create_session_in_workbench(&self, model: String) -> Result<AgentSession, String> {
        // Create session in workbench long-lived runtime
        let mut wb = self.workbench_mgr.lock().unwrap();
        let wb_id = wb.ensure_workbench()?;
        Ok(AgentSession::new(wb_id, model))
    }

    pub fn handoff_session(&self, old_session: &AgentSession, new_model: Option<String>) -> AgentSession {
        // Handoff: same runtime, new session (Bot -> Session -> Runtime separation)
        old_session.handoff(new_model)
    }

    pub fn list_runtimes(&self) -> Result<String, String> {
        self.runtime_mgr.list_runtimes()
    }

    pub fn ensure_workbench(&self) -> Result<String, String> {
        let mut wb = self.workbench_mgr.lock().unwrap();
        wb.ensure_workbench()
    }
}
