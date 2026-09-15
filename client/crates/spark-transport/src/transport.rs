//! Core Transport: connects to host-agent, subscribes to events, sends commands.
//!
//! Design:
//! - Transport is a background task that owns the WebSocket/SSE connection.
//! - It receives TransportCommand from the UI (send message, stop, approve, etc.)
//! - It emits TransportEvent to the UI (via a channel or callback).
//! - It handles reconnection internally.
//!
//! GPUI integration: spark-ui creates a Transport, wraps the command sender
//! in a ConnectionStore entity, and forwards events into AppState.

use crate::event::TransportEvent;
use anyhow::Result;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Commands from UI → server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TransportCommand {
    /// Send a user message to a task.
    SendMessage {
        task_id: String,
        content: String,
    },

    /// Stop/abort a running task.
    StopTask {
        task_id: String,
    },

    /// Approve a permission request.
    ApprovePermission {
        task_id: String,
        permission_id: String,
    },

    /// Deny a permission request.
    DenyPermission {
        task_id: String,
        permission_id: String,
    },

    /// Confirm ReadyForCheck.
    ConfirmCheck {
        task_id: String,
    },

    /// Request more work after ReadyForCheck.
    RequestMoreWork {
        task_id: String,
        message: String,
    },

    /// Create a new task.
    CreateTask {
        bot_id: String,
        goal: String,
    },

    /// Fetch full task detail (timeline, etc.).
    FetchTaskDetail {
        task_id: String,
    },

    /// Fetch browser screenshot.
    FetchScreenshot {
        task_id: String,
    },

    /// Disconnect.
    Disconnect,
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

pub struct Transport {
    command_tx: mpsc::UnboundedSender<TransportCommand>,
    event_rx: mpsc::UnboundedReceiver<TransportEvent>,
}

impl Transport {
    /// Create a new transport that connects to the given URL.
    ///
    /// Returns the Transport handle and spawns a background task.
    pub fn new(base_url: String) -> Self {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel::<TransportCommand>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<TransportEvent>();

        // Spawn the background connection task.
        tokio::spawn(async move {
            tracing::info!(url = %base_url, "transport: connecting");
            let _ = event_tx.send(TransportEvent::Connected);

            // In a real implementation, this would:
            // 1. Establish WebSocket to base_url/ws
            // 2. Receive events from the WS and forward to event_tx
            // 3. Receive commands from command_rx and send to the WS
            // 4. Handle reconnection

            while let Some(cmd) = command_rx.recv().await {
                match cmd {
                    TransportCommand::Disconnect => {
                        let _ = event_tx.send(TransportEvent::Disconnected {
                            reason: Some("user disconnect".to_string()),
                        });
                        break;
                    }
                    TransportCommand::SendMessage { task_id, content } => {
                        tracing::debug!(task_id = %task_id, content = %content, "sending message");
                        // Forward to WS
                        // For now, echo back a mock event
                        let _ = event_tx.send(TransportEvent::UserMessage {
                            task_id: task_id.clone(),
                            message_id: format!("msg-{}", uuid::Uuid::new_v4()),
                            content: content.clone(),
                        });
                    }
                    TransportCommand::StopTask { task_id } => {
                        tracing::debug!(task_id = %task_id, "stopping task");
                    }
                    TransportCommand::ApprovePermission {
                        task_id,
                        permission_id,
                    } => {
                        tracing::debug!(
                            task_id = %task_id,
                            permission_id = %permission_id,
                            "approving permission"
                        );
                    }
                    TransportCommand::DenyPermission {
                        task_id,
                        permission_id,
                    } => {
                        tracing::debug!(
                            task_id = %task_id,
                            permission_id = %permission_id,
                            "denying permission"
                        );
                    }
                    TransportCommand::ConfirmCheck { task_id } => {
                        tracing::debug!(task_id = %task_id, "confirming check");
                    }
                    TransportCommand::CreateTask { bot_id, goal } => {
                        tracing::debug!(bot_id = %bot_id, goal = %goal, "creating task");
                        let _ = event_tx.send(TransportEvent::TaskCreated {
                            task_id: format!("task-{}", uuid::Uuid::new_v4()),
                            goal,
                        });
                    }
                    _ => {
                        tracing::debug!("unhandled command: {:?}", cmd);
                    }
                }
            }

            tracing::info!("transport: background task exiting");
        });

        Self {
            command_tx,
            event_rx,
        }
    }

    /// Send a command to the server.
    pub fn send(&self, cmd: TransportCommand) -> Result<()> {
        self.command_tx
            .send(cmd)
            .map_err(|e| anyhow::anyhow!("transport send failed: {}", e))
    }

    /// Try to receive the next event (non-blocking).
    pub fn try_recv(&mut self) -> Option<TransportEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Get a clone of the command sender (for sharing across stores).
    pub fn command_sender(&self) -> mpsc::UnboundedSender<TransportCommand> {
        self.command_tx.clone()
    }
}
