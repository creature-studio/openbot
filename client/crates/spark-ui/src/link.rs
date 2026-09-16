//! link: the bridge between the host-agent connection and the GPUI entity tree.
//!
//! The connection task (see `spark-client`) owns the Unix socket. It:
//!
//! * pushes every host-agent event with [`push_event`], where the running app
//!   picks it up on its next frame;
//! * takes commands the UI produced through [`send`] (the app installs its
//!   sender once with [`install_command_sender`]).
//!
//! Two directions, one queue each, no locks held while drawing.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use spark_transport::{TransportCommand, TransportEvent};

static PENDING_EVENTS: Mutex<VecDeque<TransportEvent>> = Mutex::new(VecDeque::new());
static PENDING_COMMANDS: Mutex<VecDeque<TransportCommand>> = Mutex::new(VecDeque::new());
static COMMAND_SENDER: Mutex<Option<Arc<dyn Fn(TransportCommand) + Send + Sync>>> =
    Mutex::new(None);

/// Queue an event from host-agent for the UI thread.
pub fn push_event(event: TransportEvent) {
    PENDING_EVENTS.lock().push_back(event);
}

/// Take everything queued so far. Called once per frame.
pub fn take_events() -> Vec<TransportEvent> {
    let mut pending = PENDING_EVENTS.lock();
    pending.drain(..).collect()
}

/// How many events are waiting (used to decide whether to request a redraw).
pub fn has_pending_events() -> bool {
    !PENDING_EVENTS.lock().is_empty()
}

/// Install the outbound side: the connection task registers a closure that
/// writes commands to host-agent.
pub fn install_command_sender<F>(sender: F)
where
    F: Fn(TransportCommand) + Send + Sync + 'static,
{
    let sender: Arc<dyn Fn(TransportCommand) + Send + Sync> = Arc::new(sender);
    *COMMAND_SENDER.lock() = Some(sender.clone());
    // GPUI can be constructed before the socket task finishes connecting. Do
    // not lose a click made during that window; flush queued commands only
    // after the sender is installed and without holding the queue lock.
    let pending: Vec<_> = PENDING_COMMANDS.lock().drain(..).collect();
    for command in pending {
        sender(command);
    }
}

/// Send a command to host-agent. Commands are retained until the link is
/// installed, so early UI actions are not silently discarded.
pub fn send(command: TransportCommand) {
    if let Some(sender) = COMMAND_SENDER.lock().clone() {
        sender(command);
    } else {
        PENDING_COMMANDS.lock().push_back(command);
        tracing::debug!("host-agent link not installed yet; queued command");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spark_model::MachineId;

    #[test]
    fn events_queue_and_drain() {
        push_event(TransportEvent::Connected);
        push_event(TransportEvent::Disconnected { reason: None });
        assert!(has_pending_events());
        let events = take_events();
        assert_eq!(events.len(), 2);
        assert!(!has_pending_events(), "queue must be empty after draining");
        assert!(take_events().is_empty());
    }

    #[test]
    fn installed_sender_receives_commands() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        install_command_sender(|_command| {
            COUNT.fetch_add(1, Ordering::Relaxed);
        });
        send(TransportCommand::ListMachines);
        send(TransportCommand::RefreshMachineStatus(MachineId::local()));
        assert_eq!(COUNT.load(Ordering::Relaxed), 2);
    }
}
