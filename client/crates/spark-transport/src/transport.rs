//! `Transport` — the GPUI client's handle on host-agent.
//!
//! ```text
//! GPUI stores ──send(TransportCommand)──▶ host-agent ──▶ MachineManager
//!      ▲                                                   │
//!      └────────── poll(TransportEvent) ◀──────────────────┘
//! ```
//!
//! Two rules shape this type:
//!
//! * the client never talks to a machine directly — every machine command
//!   (connect, bootstrap, exec, browser, computer use) goes to host-agent and
//!   comes back as an event, so there is exactly one place that owns SSH
//!   connections and sandd sockets (architecture §三);
//! * the client does not decide whether a machine is local or remote — it sends
//!   a `machine_id` and host-agent routes it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::mpsc;

use spark_model::{Machine, MachineId, MachineKind};

use crate::event::TransportEvent;

// ---------------------------------------------------------------------------
// Commands: client → host-agent
// ---------------------------------------------------------------------------

/// A machine the user wants host-agent to remember.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineDraft {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    /// `Host` alias from `~/.ssh/config`. When set, OpenSSH resolves the rest.
    pub ssh_config_host: Option<String>,
}

impl MachineDraft {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            port: 22,
            user: None,
            ssh_config_host: None,
        }
    }

    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.ssh_config_host = Some(alias.into());
        self
    }

    /// Materialize the record host-agent will store. Never contains a secret:
    /// keys and passwords stay with OpenSSH/ssh-agent (architecture §九).
    pub fn to_machine(&self, id: MachineId) -> Machine {
        Machine::ssh(
            id,
            self.name.clone(),
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.ssh_config_host.clone(),
        )
    }

    /// What the sidebar shows while the machine is still only a draft.
    pub fn target(&self) -> String {
        if let Some(alias) = &self.ssh_config_host {
            return alias.clone();
        }
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.name.trim().is_empty()
            && (!self.host.trim().is_empty() || self.ssh_config_host.is_some())
    }
}

/// Commands the UI sends to host-agent.
#[derive(Debug, Clone)]
pub enum TransportCommand {
    // ----- connection -----
    Connect,
    Disconnect,

    // ----- machines -----
    /// List machines (answer arrives as `TransportEvent::MachinesUpdated`).
    ListMachines,
    AddMachine(MachineDraft),
    /// A machine the user cancelled before it was ever saved.
    RemoveMachine(MachineId),
    /// `ssh host true` — the "+ Machine" form's Test connection button.
    TestMachineConnection(MachineDraft),
    /// Connect + handshake (bootstrap only when the sandd is missing).
    ConnectMachine(MachineId),
    DisconnectMachine(MachineId),
    ReconnectMachine(MachineId),
    /// Force bootstrap (re-install sandd) and report the result.
    BootstrapMachine(MachineId),
    /// User confirmed a host key fingerprint in the Attention card.
    TrustMachineHostKey(MachineId),
    /// Re-check health now instead of waiting for the next tick.
    RefreshMachineStatus(MachineId),

    // ----- runtimes -----
    /// Create a runtime on a machine (`None` == the machine the UI is showing).
    CreateRuntime {
        machine_id: MachineId,
        kind: String,
        workspace: String,
    },
    DestroyRuntime {
        machine_id: MachineId,
        runtime_id: String,
    },
    ListRuntimes {
        machine_id: MachineId,
    },

    // ----- terminal -----
    OpenTerminal {
        machine_id: MachineId,
        runtime_id: String,
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    WriteTerminal {
        machine_id: MachineId,
        runtime_id: String,
        terminal_id: String,
        data: Vec<u8>,
    },
    ResizeTerminal {
        machine_id: MachineId,
        runtime_id: String,
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    CloseTerminal {
        machine_id: MachineId,
        runtime_id: String,
        terminal_id: String,
    },

    // ----- tasks / workbench / bots -----
    CreateTask {
        machine_id: MachineId,
        goal: String,
    },
    SendTaskMessage {
        task_id: String,
        content: String,
    },
    StopTask {
        task_id: String,
    },
    ApprovePermission {
        task_id: String,
        permission_id: String,
    },
    DenyPermission {
        task_id: String,
        permission_id: String,
    },
    /// ReadyForCheck → user confirmed the work.
    ConfirmComplete {
        task_id: String,
    },

    // ----- browser / computer -----
    /// Browser actions are addressed to a *runtime*, not a machine: the runtime
    /// is what owns Chrome.
    BrowserAction {
        runtime_id: String,
        action: crate::runtime_transport::BrowserAction,
    },
    ComputerAction {
        runtime_id: String,
        action: crate::runtime_transport::ComputerAction,
    },
    /// Register interest in screenshot frames for a runtime.
    SubscribeBrowser {
        runtime_id: String,
    },
    UnsubscribeBrowser {
        runtime_id: String,
    },

    Shutdown,
}

impl TransportCommand {
    /// Machine this command concerns, when it concerns one.
    pub fn machine_id(&self) -> Option<&MachineId> {
        match self {
            TransportCommand::RemoveMachine(id)
            | TransportCommand::ConnectMachine(id)
            | TransportCommand::DisconnectMachine(id)
            | TransportCommand::ReconnectMachine(id)
            | TransportCommand::BootstrapMachine(id)
            | TransportCommand::TrustMachineHostKey(id)
            | TransportCommand::RefreshMachineStatus(id)
            | TransportCommand::CreateRuntime { machine_id: id, .. }
            | TransportCommand::DestroyRuntime { machine_id: id, .. }
            | TransportCommand::ListRuntimes { machine_id: id }
            | TransportCommand::OpenTerminal { machine_id: id, .. }
            | TransportCommand::WriteTerminal { machine_id: id, .. }
            | TransportCommand::ResizeTerminal { machine_id: id, .. }
            | TransportCommand::CloseTerminal { machine_id: id, .. }
            | TransportCommand::CreateTask { machine_id: id, .. } => Some(id),
            _ => None,
        }
    }

    /// Short label for logs.
    pub fn label(&self) -> &'static str {
        match self {
            TransportCommand::Connect => "connect",
            TransportCommand::Disconnect => "disconnect",
            TransportCommand::ListMachines => "list_machines",
            TransportCommand::AddMachine(_) => "add_machine",
            TransportCommand::RemoveMachine(_) => "remove_machine",
            TransportCommand::TestMachineConnection(_) => "test_machine_connection",
            TransportCommand::ConnectMachine(_) => "connect_machine",
            TransportCommand::DisconnectMachine(_) => "disconnect_machine",
            TransportCommand::ReconnectMachine(_) => "reconnect_machine",
            TransportCommand::BootstrapMachine(_) => "bootstrap_machine",
            TransportCommand::TrustMachineHostKey(_) => "trust_host_key",
            TransportCommand::RefreshMachineStatus(_) => "refresh_machine_status",
            TransportCommand::CreateRuntime { .. } => "create_runtime",
            TransportCommand::DestroyRuntime { .. } => "destroy_runtime",
            TransportCommand::ListRuntimes { .. } => "list_runtimes",
            TransportCommand::OpenTerminal { .. } => "open_terminal",
            TransportCommand::WriteTerminal { .. } => "write_terminal",
            TransportCommand::ResizeTerminal { .. } => "resize_terminal",
            TransportCommand::CloseTerminal { .. } => "close_terminal",
            TransportCommand::CreateTask { .. } => "create_task",
            TransportCommand::SendTaskMessage { .. } => "send_task_message",
            TransportCommand::StopTask { .. } => "stop_task",
            TransportCommand::ApprovePermission { .. } => "approve_permission",
            TransportCommand::DenyPermission { .. } => "deny_permission",
            TransportCommand::ConfirmComplete { .. } => "confirm_complete",
            TransportCommand::BrowserAction { .. } => "browser_action",
            TransportCommand::ComputerAction { .. } => "computer_action",
            TransportCommand::SubscribeBrowser { .. } => "subscribe_browser",
            TransportCommand::UnsubscribeBrowser { .. } => "unsubscribe_browser",
            TransportCommand::Shutdown => "shutdown",
        }
    }
}

// ---------------------------------------------------------------------------
// Transport handle
// ---------------------------------------------------------------------------

/// The GPUI-facing handle: a command channel plus a stream of events.
///
/// The wire implementation (HTTP/WebSocket to host-agent) lives in spark-client;
/// this type is deliberately cheap and free of I/O so the UI can hold it inside
/// an Entity without async plumbing (architecture §十七).
pub struct Transport {
    server_url: String,
    command_tx: mpsc::UnboundedSender<TransportCommand>,
    event_tx: mpsc::UnboundedSender<TransportEvent>,
    /// Events not yet consumed by the UI.
    event_rx: Arc<Mutex<mpsc::UnboundedReceiver<TransportEvent>>>,
    connected: Arc<AtomicBool>,
}

impl Transport {
    /// Create a transport with an internal command queue.
    ///
    /// Returns the handle plus the command receiver host-agent's link should
    /// drive, so the UI never has to know how commands travel.
    pub fn new(server_url: impl Into<String>) -> (Self, mpsc::UnboundedReceiver<TransportCommand>) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let transport = Self {
            server_url: server_url.into(),
            command_tx,
            event_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            connected: Arc::new(AtomicBool::new(false)),
        };
        (transport, command_rx)
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    /// Sender other stores clone to enqueue commands.
    pub fn command_sender(&self) -> mpsc::UnboundedSender<TransportCommand> {
        self.command_tx.clone()
    }

    /// Emit an event into the UI (called by whoever owns the real link).
    pub fn emit(&self, event: TransportEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Send a command to host-agent.
    pub fn send(&self, command: TransportCommand) -> Result<()> {
        self.command_tx
            .send(command)
            .map_err(|_| anyhow::anyhow!("host-agent connection is closed"))
    }

    /// Take the next pending event, if any (non-blocking: GPUI renders, it does
    /// not wait on I/O).
    pub fn try_recv(&self) -> Option<TransportEvent> {
        let mut guard = self.event_rx.lock().ok()?;
        guard.try_recv().ok()
    }

    /// Drain everything pending — called once per UI tick.
    pub fn drain(&self) -> Vec<TransportEvent> {
        let mut out = Vec::new();
        while let Some(event) = self.try_recv() {
            out.push(event);
        }
        out
    }

    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("server_url", &self.server_url)
            .field("connected", &self.is_connected())
            .finish()
    }
}

/// Does this machine need the user's attention before it can connect?
pub fn needs_attention(machine: &Machine) -> bool {
    use spark_model::MachineStatus;
    matches!(
        machine.status,
        MachineStatus::RequiresUserAction | MachineStatus::Error
    )
}

/// Machines sorted for the sidebar: local first, then by name.
pub fn sidebar_order(machines: &[Machine]) -> Vec<Machine> {
    let mut sorted = machines.to_vec();
    sorted.sort_by(|a, b| {
        let local_rank = |machine: &Machine| usize::from(!matches!(machine.kind, MachineKind::Local));
        local_rank(a)
            .cmp(&local_rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_never_carries_a_secret() {
        let draft = MachineDraft::new("devbox", "10.0.0.42")
            .with_user("dev")
            .with_port(2222);
        let machine = draft.to_machine(MachineId::from_string("mach-1".into()));
        // The record holds an address, a user and an alias — nothing else.
        match machine.kind {
            MachineKind::Ssh {
                host,
                port,
                user,
                ssh_config_host,
            } => {
                assert_eq!(host, "10.0.0.42");
                assert_eq!(port, 2222);
                assert_eq!(user.as_deref(), Some("dev"));
                assert!(ssh_config_host.is_none());
            }
            MachineKind::Local => panic!("draft must produce an SSH machine"),
        }
    }

    #[test]
    fn alias_draft_uses_the_ssh_config_host() {
        let draft = MachineDraft::new("devbox", "10.0.0.42").with_alias("devbox");
        assert_eq!(draft.target(), "devbox");
        let machine = draft.to_machine(MachineId::from_string("mach-2".into()));
        // With an alias the user/port come from ~/.ssh/config, so we must not
        // pass them on the command line.
        assert!(machine.ssh_port().is_none());
        assert_eq!(machine.ssh_args().unwrap().last().unwrap(), "devbox");
    }

    #[test]
    fn validates_required_fields() {
        assert!(!MachineDraft::new("", "host").is_valid());
        assert!(!MachineDraft::new("name", "").is_valid());
        assert!(MachineDraft::new("name", "host").is_valid());
        let alias_only = MachineDraft {
            name: "name".into(),
            host: String::new(),
            port: 22,
            user: None,
            ssh_config_host: Some("devbox".into()),
        };
        assert!(alias_only.is_valid());
    }

    #[test]
    fn commands_expose_their_machine() {
        let id = MachineId::from_string("mach-9".into());
        let command = TransportCommand::ConnectMachine(id.clone());
        assert_eq!(command.machine_id(), Some(&id));
        assert_eq!(command.label(), "connect_machine");
        assert!(TransportCommand::Shutdown.machine_id().is_none());
    }

    #[test]
    fn transport_queues_commands_and_events() {
        let (transport, mut commands) = Transport::new("http://127.0.0.1:7878");
        transport
            .send(TransportCommand::Connect)
            .expect("send command");
        let received = commands.try_recv().expect("command queued");
        assert_eq!(received.label(), "connect");

        transport.emit(TransportEvent::Connected);
        match transport.try_recv() {
            Some(TransportEvent::Connected) => {}
            other => panic!("expected Connected, got {other:?}"),
        }
        assert!(transport.try_recv().is_none());

        transport.set_connected(true);
        assert!(transport.is_connected());
    }

    #[test]
    fn sidebar_puts_local_first() {
        let local = Machine::local();
        let remote_b = Machine::ssh(
            MachineId::from_string("mach-b".into()),
            "beta".into(),
            "b".into(),
            22,
            None,
            None,
        );
        let remote_a = Machine::ssh(
            MachineId::from_string("mach-a".into()),
            "Alpha".into(),
            "a".into(),
            22,
            None,
            None,
        );
        let ordered = sidebar_order(&[remote_b, local, remote_a]);
        assert!(matches!(ordered[0].kind, MachineKind::Local));
        assert_eq!(ordered[1].name, "Alpha");
        assert_eq!(ordered[2].name, "beta");
    }

    #[test]
    fn attention_detection() {
        let mut machine = Machine::local();
        assert!(!needs_attention(&machine));
        machine.status = spark_model::MachineStatus::RequiresUserAction;
        assert!(needs_attention(&machine));
    }
}
