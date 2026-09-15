use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionCreated { session_id: String, runtime_id: String },
    SessionMessage { session_id: String, role: String, content: String },
    ToolCall { session_id: String, tool: String, args: String },
    ToolResult { session_id: String, tool: String, result: String },
    Attention { session_id: String, attention: String },
    Completed { session_id: String, result: String },
    Failed { session_id: String, reason: String },
}

pub struct EventBus {
    events: Arc<Mutex<Vec<AgentEvent>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self { events: Arc::new(Mutex::new(Vec::new())) }
    }

    pub fn push(&self, event: AgentEvent) {
        let mut ev = self.events.lock().unwrap();
        ev.push(event);
        if ev.len() > 1000 {
            ev.remove(0);
        }
    }

    pub fn list(&self) -> Vec<AgentEvent> {
        self.events.lock().unwrap().clone()
    }
}
