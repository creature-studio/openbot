// API for host-agent - will be HTTP/UDS in future
// For now, simple struct for handling requests

use crate::agent::session::{AgentSession, AgentSessionId};
use crate::runtime::RuntimeManager;

pub struct HostAgentApi {
    runtime_mgr: RuntimeManager,
}

impl HostAgentApi {
    pub fn new() -> Self {
        Self { runtime_mgr: RuntimeManager::new() }
    }

    pub fn create_session(&self, model: String) -> Result<AgentSession, String> {
        let runtime_id = self.runtime_mgr.create_runtime("assistant")?;
        Ok(AgentSession::new(runtime_id, model))
    }

    pub fn list_runtimes(&self) -> Result<String, String> {
        self.runtime_mgr.list_runtimes()
    }
}
