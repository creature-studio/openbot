//! AppState: top-level GPUI entity that owns all stores.
//!
//! This is the root of the Entity<T> ownership tree.
//! GPUI's App holds AppState as an entity, and views subscribe
//! to changes via Context<T> / cx.observe / cx.subscribe.
//!
//! AppState polls the transport for events and routes them to
//! the appropriate stores.

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, Task as GpuiTask,
};
use spark_model::*;
use spark_transport::{Transport, TransportCommand, TransportEvent};

use crate::stores::*;

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

    transport: Option<Transport>,
    poll_task: Option<GpuiTask<()>>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    /// Create a new AppState. Call once during app startup.
    pub fn new(cx: &mut AppContext) -> Entity<Self> {
        let settings = cx.new_entity(|_| SettingsStore::new());

        // Read server URL from settings.
        let server_url = settings.read(cx).settings.server_url.clone();

        // Create transport.
        let transport = Transport::new(server_url.clone());
        let command_tx = transport.command_sender();

        // Create stores.
        let connection = cx.new_entity(|_| ConnectionStore::new(server_url, command_tx.clone()));
        let bots = cx.new_entity(|_| BotStore::new());
        let tasks = cx.new_entity(|_| TaskStore::new());
        let workbenches = cx.new_entity(|_| WorkbenchStore::new());
        let attention = cx.new_entity(|_| AttentionStore::new());

        // Build AppState.
        let app = cx.new_entity(|cx| {
            let mut state = AppState {
                connection,
                bots,
                tasks,
                workbenches,
                attention,
                settings,
                transport: Some(transport),
                poll_task: None,
            };

            // Start the event polling loop.
            state.start_polling(cx);

            state
        });

        app
    }

    /// Start polling transport events in a background GPUI task.
    fn start_polling(&mut self, cx: &mut Context<Self>) {
        // In a real GPUI app, this would use cx.spawn or a timer-based
        // approach to poll the transport channel and route events.
        //
        // The pattern is:
        // 1. transport.try_recv() to get events
        // 2. Route each event to the appropriate store via cx.update_entity()
        // 3. Schedule next poll via cx.spawn or cx.background_executor()
        //
        // For now, this is a placeholder showing the routing logic.

        tracing::info!("app: event polling started");
    }

    /// Process a single transport event, routing it to the right store.
    pub fn handle_transport_event(
        &mut self,
        event: TransportEvent,
        cx: &mut Context<Self>,
    ) {
        match &event {
            TransportEvent::Connected => {
                self.connection.update(cx, |conn, cx| {
                    conn.set_status(ConnectionStatus::Connected, cx);
                });
            }

            TransportEvent::Disconnected { reason } => {
                self.connection.update(cx, |conn, cx| {
                    conn.set_status(
                        ConnectionStatus::Error {
                            message: reason.clone().unwrap_or_default(),
                        },
                        cx,
                    );
                });
            }

            TransportEvent::PermissionRequired { task_id, .. } => {
                // Update task
                self.tasks.update(cx, |tasks, cx| {
                    tasks.handle_event(&event, cx);
                });
                // Surface attention
                if let Some(task_id) = extract_task_id(&event) {
                    if let Some(task) = self.tasks.read(cx).tasks.get(&TaskId(task_id.clone())) {
                        if let Some(ref att) = task.attention {
                            self.attention.update(cx, |att_store, cx| {
                                att_store.set(task_id, att, cx);
                            });
                        }
                    }
                }
            }

            TransportEvent::ReadyForCheck { task_id, .. } => {
                self.tasks.update(cx, |tasks, cx| {
                    tasks.handle_event(&event, cx);
                });
                if let Some(task) = self.tasks.read(cx).tasks.get(&TaskId(task_id.clone())) {
                    if let Some(ref att) = task.attention {
                        self.attention.update(cx, |att_store, cx| {
                            att_store.set(task_id.clone(), att, cx);
                        });
                    }
                }
            }

            _ => {
                // Route everything else to TaskStore.
                self.tasks.update(cx, |tasks, cx| {
                    tasks.handle_event(&event, cx);
                });
            }
        }
    }

    // ----- Convenience commands -----

    pub fn send_message(&self, task_id: &TaskId, content: &str) {
        self.connection.read_with(
            &*self.connection.clone(),
            |conn, _| {
                conn.send_command(TransportCommand::SendMessage {
                    task_id: task_id.0.clone(),
                    content: content.to_string(),
                });
            },
        );
    }

    pub fn stop_task(&self, task_id: &TaskId) {
        self.connection.read_with(
            &*self.connection.clone(),
            |conn, _| {
                conn.send_command(TransportCommand::StopTask {
                    task_id: task_id.0.clone(),
                });
            },
        );
    }

    pub fn create_task(&self, bot_id: &str, goal: &str) {
        self.connection.read_with(
            &*self.connection.clone(),
            |conn, _| {
                conn.send_command(TransportCommand::CreateTask {
                    bot_id: bot_id.to_string(),
                    goal: goal.to_string(),
                });
            },
        );
    }

    pub fn approve_permission(&self, task_id: &TaskId, permission_id: &str) {
        self.connection.read_with(
            &*self.connection.clone(),
            |conn, _| {
                conn.send_command(TransportCommand::ApprovePermission {
                    task_id: task_id.0.clone(),
                    permission_id: permission_id.to_string(),
                });
            },
        );
    }

    pub fn deny_permission(&self, task_id: &TaskId, permission_id: &str) {
        self.connection.read_with(
            &*self.connection.clone(),
            |conn, _| {
                conn.send_command(TransportCommand::DenyPermission {
                    task_id: task_id.0.clone(),
                    permission_id: permission_id.to_string(),
                });
            },
        );
    }

    pub fn confirm_check(&self, task_id: &TaskId) {
        self.attention.update(
            &mut self.attention.clone(),
            |att, cx| att.clear(cx),
        );
        self.connection.read_with(
            &*self.connection.clone(),
            |conn, _| {
                conn.send_command(TransportCommand::ConfirmCheck {
                    task_id: task_id.0.clone(),
                });
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_task_id(event: &TransportEvent) -> Option<String> {
    match event {
        TransportEvent::TaskCreated { task_id, .. }
        | TransportEvent::TaskStatusChanged { task_id, .. }
        | TransportEvent::UserMessage { task_id, .. }
        | TransportEvent::AssistantMessage { task_id, .. }
        | TransportEvent::ToolStarted { task_id, .. }
        | TransportEvent::ToolOutput { task_id, .. }
        | TransportEvent::ToolFinished { task_id, .. }
        | TransportEvent::PermissionRequired { task_id, .. }
        | TransportEvent::ReadyForCheck { task_id, .. }
        | TransportEvent::BrowserScreenshot { task_id, .. }
        | TransportEvent::BrowserAction { task_id, .. } => Some(task_id.clone()),
        _ => None,
    }
}
