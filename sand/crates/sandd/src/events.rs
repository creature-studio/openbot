use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::sync::mpsc::{Sender, channel};
use sand_protocol::{RuntimeEvent, RuntimeId, RuntimeEventKind, now_ms};

#[derive(Debug, Clone)]
pub struct EventBus {
    events: Arc<Mutex<VecDeque<RuntimeEvent>>>,
    subscribers: Arc<Mutex<Vec<Sender<RuntimeEvent>>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(VecDeque::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn emit(&self, runtime_id: RuntimeId, kind: RuntimeEventKind) {
        let ev = RuntimeEvent {
            runtime_id,
            kind,
            at_ms: now_ms(),
        };
        if let Ok(mut q) = self.events.lock() {
            q.push_back(ev.clone());
            while q.len() > 1000 {
                q.pop_front();
            }
        }
        // broadcast to subscribers, prune dead
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.retain(|s| s.send(ev.clone()).is_ok());
        }
        eprintln!("[event] {:?}", ev);
    }

    pub fn list(&self) -> Vec<RuntimeEvent> {
        self.events.lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn list_for(&self, runtime_id: &RuntimeId) -> Vec<RuntimeEvent> {
        self.events.lock().map(|q| q.iter().filter(|e| e.runtime_id.0 == runtime_id.0).cloned().collect()).unwrap_or_default()
    }

    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<RuntimeEvent> {
        let (tx, rx) = channel();
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.push(tx);
        }
        rx
    }
}
