use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::sync::mpsc::{Sender, channel};
use sand_protocol::{RuntimeEvent, RuntimeId, RuntimeEventKind, now_ms};

#[derive(Debug, Clone)]
pub struct EventBus {
    /// `(event_id, event)`. The id is monotonic for the life of the daemon so
    /// clients can poll incrementally (`since`) instead of diffing text.
    events: Arc<Mutex<VecDeque<(u64, RuntimeEvent)>>>,
    subscribers: Arc<Mutex<Vec<Sender<RuntimeEvent>>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(VecDeque::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    pub fn emit(&self, runtime_id: RuntimeId, kind: RuntimeEventKind) {
        let ev = RuntimeEvent {
            runtime_id,
            kind,
            at_ms: now_ms(),
        };
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut q) = self.events.lock() {
            q.push_back((id, ev.clone()));
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
        self.events
            .lock()
            .map(|q| q.iter().map(|(_, e)| e.clone()).collect())
            .unwrap_or_default()
    }

    pub fn list_for(&self, runtime_id: &RuntimeId) -> Vec<RuntimeEvent> {
        self.events
            .lock()
            .map(|q| {
                q.iter()
                    .filter(|(_, e)| e.runtime_id.0 == runtime_id.0)
                    .map(|(_, e)| e.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Events newer than `since_id`, optionally limited to one runtime.
    ///
    /// This is what the bridge polls: idempotent, cheap, and it lets a client
    /// that was disconnected for an hour catch up without replaying everything
    /// it already saw.
    pub fn list_since(&self, runtime_id: Option<&RuntimeId>, since_id: u64) -> Vec<(u64, RuntimeEvent)> {
        self.events
            .lock()
            .map(|q| {
                q.iter()
                    .filter(|(id, _)| *id > since_id)
                    .filter(|(_, e)| match runtime_id {
                        Some(rid) => e.runtime_id.0 == rid.0,
                        None => true,
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Highest event id handed out so far (0 when nothing happened yet).
    pub fn last_id(&self) -> u64 {
        self.next_id.load(std::sync::atomic::Ordering::Relaxed) - 1
    }

    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<RuntimeEvent> {
        let (tx, rx) = channel();
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.push(tx);
        }
        rx
    }
}
