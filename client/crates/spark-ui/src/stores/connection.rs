//! ConnectionStore: manages connection state to host-agent.

use gpui::{Context, Entity, EventEmitter};
use spark_model::ConnectionStatus;
use spark_transport::TransportCommand;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Events emitted by ConnectionStore
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    StatusChanged(ConnectionStatus),
}

// ---------------------------------------------------------------------------
// ConnectionStore
// ---------------------------------------------------------------------------

/// GPUI Entity that tracks connection status and holds the transport
/// command sender. Other stores clone `command_tx` to send commands.
pub struct ConnectionStore {
    pub status: ConnectionStatus,
    pub server_url: String,
    pub command_tx: mpsc::UnboundedSender<TransportCommand>,
}

impl EventEmitter<ConnectionEvent> for ConnectionStore {}

impl ConnectionStore {
    pub fn new(
        server_url: String,
        command_tx: mpsc::UnboundedSender<TransportCommand>,
    ) -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            server_url,
            command_tx,
        }
    }

    /// Send a command through the transport.
    pub fn send_command(&self, cmd: TransportCommand) {
        if let Err(e) = self.command_tx.send(cmd) {
            tracing::error!("failed to send transport command: {}", e);
        }
    }

    /// Get a clone of the command sender for sharing with other stores.
    pub fn command_sender(&self) -> mpsc::UnboundedSender<TransportCommand> {
        self.command_tx.clone()
    }

    /// Update connection status (called when processing TransportEvents).
    pub fn set_status(&mut self, status: ConnectionStatus, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }
}
