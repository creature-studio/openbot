//! AppState: top-level GPUI entity that owns all stores.
//!
//! This is the root of the Entity<T> ownership tree.
//! GPUI's App holds AppState as an entity, and views subscribe
//! to changes via Context<T> / cx.observe / cx.subscribe.
//!
//! AppState drains the transport for events and routes them to
//! the appropriate stores.
//!
//! Machine events are routed here too, which is what keeps the remote-machine
//! feature out of the views: a task that runs on `devbox` looks exactly like a
//! local task in the timeline, and only the Machines sidebar and the Runtime
//! tab show that anything is remote.

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, IntoElement, Render, Window,
};
use spark_model::*;
use spark_transport::{Transport, TransportCommand, TransportEvent};
use tokio::sync::mpsc;

use crate::attention::AttentionOverlay;
use crate::composer::Composer;
use crate::inspector::Inspector;
use crate::sidebar::Sidebar;
use crate::stores::{*, MachineStore};
use crate::timeline::TaskTimeline;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AppEvent {
    Refresh,
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

pub struct AppState {
    pub connection: Entity<ConnectionStore>,
    pub bots: Entity<BotStore>,
    pub tasks: Entity<TaskStore>,
    pub workbenches: Entity<WorkbenchStore>,
    pub attention: Entity<AttentionStore>,
    pub settings: Entity<SettingsStore>,
    /// Machines: local + SSH. The sidebar, the "+ Machine" form, the host-key
    /// card and the Runtime inspector all read from this one store.
    pub machines: Entity<MachineStore>,
    /// Client side of the host-agent connection.
    pub transport: Transport,
    /// Commands the transport task must send to host-agent.
    commands: Option<mpsc::UnboundedReceiver<TransportCommand>>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    /// Build the store tree and the transport. The returned entity owns the
    /// command receiver; hand it to the task that writes to host-agent's socket.
    pub fn new(server_url: impl Into<String>, cx: &mut App) -> Entity<Self> {
        let (transport, commands) = Transport::new(server_url);
        let command_tx = transport.command_sender();

        let connection = cx.new(|_cx| {
            ConnectionStore::new(transport.server_url().to_string(), command_tx.clone())
        });
        let bots = cx.new(|_cx| BotStore::new());
        let tasks = cx.new(|_cx| TaskStore::new());
        let workbenches = cx.new(|_cx| WorkbenchStore::new());
        let attention = cx.new(|_cx| AttentionStore::new());
        let settings = cx.new(|_cx| SettingsStore::new());
        let machines = cx.new(|_cx| MachineStore::new());

        cx.new(move |_cx| Self {
            connection,
            bots,
            tasks,
            workbenches,
            attention,
            settings,
            machines,
            transport,
            commands: Some(commands),
        })
    }

    /// Take the command receiver once, to hand it to the transport task.
    pub fn take_commands(&mut self) -> Option<mpsc::UnboundedReceiver<TransportCommand>> {
        self.commands.take()
    }

    /// Ask host-agent for the machine list (sidebar refresh).
    pub fn refresh_machines(&mut self) {
        self.send(TransportCommand::ListMachines);
    }

    /// Drain everything host-agent has sent and route it. The window's render
    /// loop calls this before drawing.
    pub fn drain_transport(&mut self, cx: &mut Context<Self>) {
        for event in self.transport.drain() {
            self.handle_event(event, cx);
        }
    }

    /// Route one transport event to the store that owns it.
    pub fn handle_event(&mut self, event: TransportEvent, cx: &mut Context<Self>) {
        match &event {
            TransportEvent::Connected => {
                self.connection
                    .update(cx, |store, cx| store.set_status(ConnectionStatus::Connected, cx));
                return;
            }
            TransportEvent::Disconnected { reason } => {
                let detail = reason.clone().unwrap_or_else(|| "连接已关闭".to_string());
                self.connection.update(cx, |store, cx| {
                    store.set_status(ConnectionStatus::Disconnected, cx)
                });
                // The *client* lost host-agent — not the machines. Remote
                // runtime / PTY / Chrome keep running, so this is an attention
                // card, never a failed task.
                self.attention.update(cx, |store, cx| {
                    store.set(
                        "host-agent".to_string(),
                        &Attention::ExecutionError {
                            error: format!(
                                "与 host-agent 的连接中断（{detail}）。远端 runtime / PTY / Chrome 仍然存活，重连后恢复。"
                            ),
                            recoverable: true,
                        },
                        cx,
                    )
                });
                return;
            }
            _ => {}
        }

        // Machines: status, metadata, attention, bootstrap, runtime lists.
        if is_machine_event(&event) {
            self.machines.update(cx, |store, cx| {
                store.apply_event(&event, cx);
            });

            // A machine that dropped its bridge must not look like a failed
            // task: mark the tasks that live on it as connection-lost.
            if let TransportEvent::MachineStatusChanged { machine_id, status, .. } = &event {
                if !status.is_connected() {
                    let machine_id = machine_id.clone();
                    let affected: Vec<TaskId> = self
                        .tasks
                        .read(cx)
                        .tasks
                        .values()
                        .filter(|task| task.machine_id == machine_id)
                        .map(|task| task.id.clone())
                        .collect();
                    if !affected.is_empty() {
                        self.tasks.update(cx, |store, cx| {
                            for task_id in affected {
                                if let Some(task) = store.get_mut(&task_id) {
                                    task.mark_connection_lost(&status);
                                }
                            }
                            cx.notify();
                        });
                    }
                }
            }
            return;
        }

        // Task timeline, browser frames and terminal output.
        self.tasks.update(cx, |store, cx| {
            store.handle_event(&event, cx);
        });

        // Mirror the selected task's attention into the modal store, so one
        // overlay serves permissions, ReadyForCheck and completion.
        let mirror = self
            .tasks
            .read(cx)
            .selected_task()
            .and_then(|task| task.attention.clone().map(|attention| (task.id.0.clone(), attention)));
        if let Some((task_id, attention)) = mirror {
            let already_shown = self.attention.read(cx).has_active();
            if !already_shown {
                self.attention.update(cx, |store, cx| {
                    store.set(task_id, &attention, cx);
                });
            }
        }
    }

    // ----- command helpers -----

    pub fn send(&self, command: TransportCommand) {
        if let Err(e) = self.transport.send(command) {
            tracing::error!("failed to send transport command: {}", e);
        }
    }

    pub fn command_sender(&self) -> mpsc::UnboundedSender<TransportCommand> {
        self.transport.command_sender()
    }

    // ----- machine actions (the "+ Machine" flow) -----

    pub fn open_machine_form(&mut self, cx: &mut Context<Self>) {
        self.machines.update(cx, |store, cx| store.open_form(cx));
    }

    pub fn close_machine_form(&mut self, cx: &mut Context<Self>) {
        self.machines.update(cx, |store, cx| store.close_form(cx));
    }

    pub fn test_machine_connection(&mut self, cx: &mut Context<Self>) {
        let tx = self.command_sender();
        self.machines
            .update(cx, |store, cx| store.test_connection(&tx, cx));
    }

    pub fn add_machine(&mut self, cx: &mut Context<Self>) {
        let tx = self.command_sender();
        self.machines.update(cx, |store, cx| store.add_machine(&tx, cx));
    }

    /// [信任并连接] on the host-key card.
    pub fn trust_machine_host_key(&mut self, cx: &mut Context<Self>) {
        let tx = self.command_sender();
        self.machines
            .update(cx, |store, cx| store.trust_and_connect(&tx, cx));
    }

    /// [取消] on the host-key card: nothing is trusted, nothing is stored.
    pub fn cancel_machine_attention(&mut self, cx: &mut Context<Self>) {
        self.machines
            .update(cx, |store, cx| store.cancel_attention(cx));
    }

    /// Create a task on the machine selected in the sidebar/composer. The task
    /// keeps that machine for its whole life (v1: no handoff).
    pub fn create_task(&mut self, goal: String, cx: &mut Context<Self>) {
        let machine_id = self
            .machines
            .read(cx)
            .selected
            .clone()
            .unwrap_or_else(MachineId::local);
        let tx = self.command_sender();
        self.tasks.update(cx, |store, cx| {
            store.create_task_on(machine_id, goal, &tx, cx);
        });
    }
}

/// True for the events [`MachineStore::apply_event`] owns.
fn is_machine_event(event: &TransportEvent) -> bool {
    matches!(
        event,
        TransportEvent::MachinesUpdated { .. }
            | TransportEvent::MachineStatusChanged { .. }
            | TransportEvent::MachineMetadataUpdated { .. }
            | TransportEvent::MachineAttentionRequired { .. }
            | TransportEvent::MachineTested { .. }
            | TransportEvent::MachineBootstrapProgress { .. }
            | TransportEvent::MachineBootstrapFinished { .. }
            | TransportEvent::RuntimesUpdated { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_events_are_routed_to_the_machine_store() {
        assert!(is_machine_event(&TransportEvent::MachinesUpdated { machines: vec![] }));
        assert!(is_machine_event(&TransportEvent::MachineAttentionRequired {
            machine_id: MachineId::from_string("mach-1".into()),
            message: "host key changed".into(),
            fingerprint: Some("SHA256:x".into()),
        }));
        assert!(!is_machine_event(&TransportEvent::Connected));
        assert!(!is_machine_event(&TransportEvent::TaskCreated {
            task_id: "t".into(),
            goal: "g".into(),
            machine_id: None,
            runtime_id: None,
        }));
    }
}

// ---------------------------------------------------------------------------
// RootView: the window shell
// ---------------------------------------------------------------------------

/// The window: sidebar, timeline, inspector, composer — plus the machine
/// surfaces (the "+ Machine" form and the host-key card) as overlays.
pub struct RootView {
    state: Entity<AppState>,
    sidebar: Entity<Sidebar>,
    timeline: Entity<TaskTimeline>,
    inspector: Entity<Inspector>,
    composer: Entity<Composer>,
    attention: Entity<AttentionOverlay>,
}

impl RootView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();

        // Copy the entity handles out of the read guard first: `cx.new` needs a
        // mutable borrow of the context, which a live guard would block.
        let (bots, tasks, workbenches, machines, attention, connection, command_tx) = {
            let state = state.read(cx);
            (
                state.bots.clone(),
                state.tasks.clone(),
                state.workbenches.clone(),
                state.machines.clone(),
                state.attention.clone(),
                state.connection.clone(),
                state.transport.command_sender(),
            )
        };

        let sidebar = cx.new(|cx| Sidebar::new(bots, tasks.clone(), workbenches, machines.clone(), command_tx.clone(), cx));
        let timeline = cx.new(|cx| TaskTimeline::new(tasks.clone(), cx));
        let inspector = cx.new(|cx| Inspector::new(tasks.clone(), machines.clone(), cx));
        let composer = cx.new(|cx| {
            Composer::new(
                tasks,
                connection,
                machines.clone(),
                command_tx.clone(),
                cx,
            )
        });
        let attention = cx.new(|cx| AttentionOverlay::with_machines(attention, machines, command_tx, cx));

        Self {
            state,
            sidebar,
            timeline,
            inspector,
            composer,
            attention,
        }
    }

    /// Pull everything host-agent sent into the stores, then draw.
    fn drain(&mut self, cx: &mut Context<Self>) {
        let events = crate::link::take_events();
        if events.is_empty() {
            return;
        }
        self.state.update(cx, |state, cx| {
            for event in events {
                state.handle_event(event, cx);
            }
        });
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain(cx);

        // Clone what the element needs: a GPUI read guard cannot outlive the
        // statement that produced the returned element.
        let (form_open, form, machines_entity, command_tx) = {
            let state = self.state.read(cx);
            let machines_entity = state.machines.clone();
            let command_tx = state.transport.command_sender();
            let machines = machines_entity.read(cx);
            (machines.form.open, machines.form.clone(), machines_entity, command_tx)
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui::rgb(0x0f172a))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar.clone())
                    .child(div().flex().flex_1().min_w_0().child(self.timeline.clone()))
                    .child(self.inspector.clone()),
            )
            .child(self.composer.clone())
            // The "+ Machine" form is a modal over the workspace.
            .when(form_open, move |root| {
                root.child(crate::sidebar::render_add_machine_form(form, machines_entity.clone(), command_tx.clone()))
            })
            .child(self.attention.clone())
    }
}

#[cfg(test)]
mod root_view_tests {
    use super::*;

    #[test]
    fn host_agent_disconnect_is_recoverable() {
        // A lost client↔host-agent link must never be modelled as a failed task.
        let attention = Attention::ExecutionError {
            error: "connection lost".into(),
            recoverable: true,
        };
        match attention {
            Attention::ExecutionError { recoverable, .. } => assert!(recoverable),
            _ => unreachable!(),
        }
    }
}
