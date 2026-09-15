use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use sand_protocol::{RuntimeEvent, RuntimeId, RuntimeEventKind, now_ms};

#[derive(Debug, Clone)]
pub struct EventBus {
    events: Arc<Mutex<VecDeque<RuntimeEvent>>>,
    // simple broadcast via list of senders? For now just store
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn emit(&self, runtime_id: RuntimeId, kind: RuntimeEventKind) {
        let ev = RuntimeEvent {
            runtime_id,
            kind,
            at_ms: now_ms(),
        };
        if let Ok(mut q) = self.events.lock() {
            q.push_back(ev);
            // keep last 1000
            while q.len() > 1000 {
                q.pop_front();
            }
        }
        // also log
        // eprintln!("[event] {:?}", ev);
    }

    pub fn list(&self) -> Vec<RuntimeEvent> {
        self.events.lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn list_for(&self, runtime_id: &RuntimeId) -> Vec<RuntimeEvent> {
        self.events.lock().map(|q| q.iter().filter(|e| e.runtime_id.0 == runtime_id.0).cloned().collect()).unwrap_or_default()
    }
}
