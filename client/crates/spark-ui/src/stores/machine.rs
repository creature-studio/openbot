//! MachineStore: the Machines sidebar, the "+ Machine" form and the runtime
//! inspector data.
//!
//! The store never talks to SSH. It sends [`TransportCommand`]s to host-agent
//! and folds the resulting [`TransportEvent`]s back into state — which is why
//! the GPUI client can show a remote machine without knowing what SSH is.

use gpui::{Context, EventEmitter};
use spark_model::*;
use spark_transport::{BootstrapSummary, MachineTestOutcome, TransportCommand, TransportEvent};

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum MachineStoreEvent {
    MachineAdded(MachineId),
    MachineRemoved(MachineId),
    MachineStatusChanged(MachineId),
    MachineSelected(Option<MachineId>),
    /// A machine is blocked on the user (host key / auth).
    AttentionRaised(MachineId),
    AttentionCleared(MachineId),
}

/// Which prompt the user must answer before a machine can connect.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineAttention {
    pub machine_id: MachineId,
    /// Human text ("需要信任主机密钥" / "认证失败").
    pub message: String,
    /// `SHA256:...` — what the user compares against the machine's real key.
    pub fingerprint: Option<String>,
    /// True when the machine changed its host key: the dangerous case, never
    /// auto-accepted, and the user is told it is a different key than before.
    pub changed: bool,
}

/// The "+ Machine" form the user is filling in.
#[derive(Debug, Clone, Default)]
pub struct MachineForm {
    /// Display name ("devbox").
    pub name: String,
    /// Literal hostname/IP.
    pub host: String,
    /// `~/.ssh/config` alias, when the user wants OpenSSH to resolve the host
    /// (ProxyJump, IdentityFile, User… all come from the config, not from us).
    pub ssh_config_host: String,
    pub user: String,
    pub port: String,
    pub use_ssh_config: bool,
    /// Result of the Test connection button.
    pub test: Option<MachineTestOutcome>,
    /// True while `ssh host true` is in flight.
    pub testing: bool,
    /// Visible when the form is open.
    pub open: bool,
}

impl MachineForm {
    pub fn open() -> Self {
        Self {
            port: "22".to_string(),
            open: true,
            ..Self::default()
        }
    }

    pub fn close(&mut self) {
        *self = Self::default();
    }

    /// Turn the fields into the draft host-agent stores (coordinates only —
    /// never a key or a password).
    pub fn draft(&self) -> Option<spark_transport::MachineDraft> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        let alias = self.ssh_config_host.trim();
        let host = if !alias.is_empty() {
            alias
        } else if !self.host.trim().is_empty() {
            self.host.trim()
        } else {
            return None;
        };
        let mut draft = spark_transport::MachineDraft::new(name, host);
        if !alias.is_empty() {
            draft = draft.with_alias(alias);
        }
        if !self.user.trim().is_empty() {
            draft = draft.with_user(self.user.trim());
        }
        // A hand-written port is only meaningful for a literal host; an alias
        // carries its own port in ~/.ssh/config.
        if alias.is_empty() {
            if let Ok(port) = self.port.trim().parse::<u16>() {
                draft = draft.with_port(port);
            }
        }
        Some(draft)
    }

    /// Client-side validation, mirroring what host-agent will accept.
    pub fn error(&self) -> Option<&'static str> {
        if self.name.trim().is_empty() {
            return Some("请填写机器名称");
        }
        if self.use_ssh_config && self.ssh_config_host.trim().is_empty() {
            return Some("请填写 ~/.ssh/config 里的 Host 别名");
        }
        if !self.use_ssh_config && self.host.trim().is_empty() {
            return Some("请填写主机名或 IP");
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct MachineStore {
    pub machines: Vec<Machine>,
    pub selected: Option<MachineId>,
    pub is_connecting: bool,
    /// The "+ Machine" form.
    pub form: MachineForm,
    /// Machine waiting on the user (host key / auth).
    pub attention: Option<MachineAttention>,
    /// Runtimes per machine, refreshed after a reconnect (`RuntimesUpdated`).
    pub runtimes: Vec<(MachineId, Vec<spark_transport::RuntimeInfo>)>,
    /// Last bootstrap report, shown in the Runtime inspector.
    pub last_bootstrap: Option<(MachineId, BootstrapSummary)>,
}

impl EventEmitter<MachineStoreEvent> for MachineStore {}

impl MachineStore {
    pub fn new() -> Self {
        // Start with the local machine: a user with no machines configured can
        // still run everything locally.
        Self {
            machines: vec![Machine::local(Some("Local".to_string()))],
            selected: Some(MachineId::local()),
            is_connecting: false,
            form: MachineForm::default(),
            attention: None,
            runtimes: Vec::new(),
            last_bootstrap: None,
        }
    }

    // ----- selection / lookup -----

    pub fn select(&mut self, id: Option<MachineId>, cx: &mut Context<Self>) {
        self.selected = id.clone();
        cx.emit(MachineStoreEvent::MachineSelected(id));
        cx.notify();
    }

    pub fn selected(&self) -> Option<&Machine> {
        self.selected
            .as_ref()
            .and_then(|id| self.machines.iter().find(|m| &m.id == id))
    }

    pub fn get(&self, id: &MachineId) -> Option<&Machine> {
        self.machines.iter().find(|m| &m.id == id)
    }

    pub fn list_machines(&self) -> Vec<Machine> {
        self.machines.clone()
    }

    /// Machines that can actually run something right now.
    pub fn connected(&self) -> Vec<&Machine> {
        self.machines
            .iter()
            .filter(|m| m.status.is_connected())
            .collect()
    }

    pub fn runtimes_on(&self, machine_id: &MachineId) -> &[spark_transport::RuntimeInfo] {
        self.runtimes
            .iter()
            .find(|(id, _)| id == machine_id)
            .map(|(_, runtimes)| runtimes.as_slice())
            .unwrap_or(&[])
    }

    // ----- user actions: they only send commands -----

    /// Open the "+ Machine" form.
    pub fn open_form(&mut self, cx: &mut Context<Self>) {
        self.form = MachineForm::open();
        self.form.use_ssh_config = false;
        cx.notify();
    }

    pub fn close_form(&mut self, cx: &mut Context<Self>) {
        self.form.close();
        cx.notify();
    }

    /// "+ Machine" → Test connection: `ssh host true` through host-agent.
    pub fn test_connection(&mut self, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        let Some(draft) = self.form.draft() else {
            return;
        };
        self.form.testing = true;
        self.form.test = None;
        let _ = command_tx.send(TransportCommand::TestMachineConnection(draft));
        cx.notify();
    }

    /// Save the machine and connect it (host-agent bootstraps sandd if needed).
    pub fn add_machine(&mut self, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        let Some(draft) = self.form.draft() else {
            return;
        };
        self.is_connecting = true;
        let _ = command_tx.send(TransportCommand::AddMachine(draft));
        self.form.close();
        cx.notify();
    }

    pub fn connect(&mut self, id: &MachineId, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        self.is_connecting = true;
        let _ = command_tx.send(TransportCommand::ConnectMachine(id.clone()));
        cx.notify();
    }

    pub fn disconnect(&mut self, id: &MachineId, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        let _ = command_tx.send(TransportCommand::DisconnectMachine(id.clone()));
        cx.notify();
    }

    pub fn reconnect(&mut self, id: &MachineId, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        self.is_connecting = true;
        let _ = command_tx.send(TransportCommand::ReconnectMachine(id.clone()));
        cx.notify();
    }

    pub fn bootstrap(&mut self, id: &MachineId, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        let _ = command_tx.send(TransportCommand::BootstrapMachine(id.clone()));
        cx.notify();
    }

    pub fn remove(&mut self, id: &MachineId, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        // The local machine cannot be removed: it is where host-agent runs.
        if id.is_local() {
            return;
        }
        let _ = command_tx.send(TransportCommand::RemoveMachine(id.clone()));
        cx.notify();
    }

    /// The user pressed [信任并连接]: write the fingerprint to known_hosts and
    /// retry. This is the *only* path that trusts a host key.
    pub fn trust_and_connect(&mut self, command_tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>, cx: &mut Context<Self>) {
        let Some(attention) = self.attention.clone() else {
            return;
        };
        let _ = command_tx.send(TransportCommand::TrustMachineHostKey(attention.machine_id.clone()));
        self.attention = None;
        cx.notify();
    }

    /// The user pressed [取消].
    pub fn cancel_attention(&mut self, cx: &mut Context<Self>) {
        self.attention = None;
        cx.notify();
    }

    // ----- attention -----

    /// Raise the "needs the user" card for a machine (host key / auth failure).
    ///
    /// `changed` is the dangerous variant: the machine is presenting a
    /// *different* key than before, so the UI must warn instead of offering a
    /// plain accept (architecture §十).
    pub fn raise_attention(
        &mut self,
        machine_id: &MachineId,
        message: &str,
        fingerprint: Option<String>,
    ) -> bool {
        self.attention = Some(MachineAttention {
            machine_id: machine_id.clone(),
            message: message.to_string(),
            fingerprint,
            changed: message.contains("changed") || message.contains("变更"),
        });
        if let Some(machine) = self.machines.iter_mut().find(|m| &m.id == machine_id) {
            machine.status = MachineStatus::RequiresUserAction;
        }
        true
    }

    pub fn clear_attention_for(&mut self, machine_id: &MachineId) -> bool {
        if self.attention.as_ref().map(|a| &a.machine_id) == Some(machine_id) {
            self.attention = None;
            return true;
        }
        false
    }

    // ----- event folding -----

    /// Apply one transport event. Returns true when the store changed.
    pub fn apply_event(&mut self, event: &TransportEvent, cx: &mut Context<Self>) -> bool {
        match event {
            TransportEvent::MachinesUpdated { machines } => {
                let selected = self.selected.clone();
                self.machines = machines.clone();
                // The local machine is always registered by host-agent; if the
                // list somehow misses the selection, keep the UI usable.
                if selected.as_ref().map_or(true, |id| self.get(id).is_none()) {
                    self.selected = self.machines.first().map(|m| m.id.clone());
                }
                self.is_connecting = false;
                cx.notify();
                true
            }
            TransportEvent::MachineStatusChanged { machine_id, status, .. } => {
                if let Some(machine) = self.machines.iter_mut().find(|m| &m.id == machine_id) {
                    machine.status = status.clone();
                }
                if status.is_connected() {
                    self.is_connecting = false;
                    // Connected again: the attention card is stale.
                    if self.attention.as_ref().map(|a| &a.machine_id) == Some(machine_id) {
                        self.attention = None;
                    }
                }
                cx.emit(MachineStoreEvent::MachineStatusChanged(machine_id.clone()));
                cx.notify();
                true
            }
            TransportEvent::MachineMetadataUpdated { machine } => {
                if let Some(slot) = self.machines.iter_mut().find(|m| m.id == machine.id) {
                    *slot = machine.clone();
                } else {
                    self.machines.push(machine.clone());
                }
                cx.notify();
                true
            }
            TransportEvent::MachineAttentionRequired { machine_id, message, fingerprint } => {
                self.raise_attention(machine_id, message, fingerprint.clone());
                cx.emit(MachineStoreEvent::AttentionRaised(machine_id.clone()));
                cx.notify();
                true
            }
            TransportEvent::MachineTested { outcome, .. } => {
                self.form.testing = false;
                self.form.test = Some(outcome.clone());
                cx.notify();
                true
            }
            TransportEvent::MachineBootstrapFinished { machine_id, report } => {
                self.last_bootstrap = Some((machine_id.clone(), report.clone()));
                cx.notify();
                true
            }
            TransportEvent::MachineBootstrapProgress { .. } => true,
            TransportEvent::RuntimesUpdated { machine_id, runtimes } => {
                self.runtimes
                    .retain(|(id, _)| id != machine_id);
                self.runtimes.push((machine_id.clone(), runtimes.clone()));
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// Convenience for command senders stored elsewhere (composer, task view).
    pub fn command_sender(
        &self,
        tx: &tokio::sync::mpsc::UnboundedSender<TransportCommand>,
    ) -> tokio::sync::mpsc::UnboundedSender<TransportCommand> {
        tx.clone()
    }
}

impl Default for MachineStore {
    fn default() -> Self {
        Self::new()
    }
}

/// `●`/`◐`/`✗`/`…` — the sidebar's status column.
pub fn status_icon(status: &MachineStatus) -> &'static str {
    match status {
        MachineStatus::Connected => "●",
        MachineStatus::Degraded => "◐",
        MachineStatus::Connecting | MachineStatus::Bootstrapping => "…",
        MachineStatus::RequiresUserAction => "!",
        MachineStatus::Unreachable => "○",
        MachineStatus::Disconnected | MachineStatus::Error => "✗",
    }
}

/// Human-readable status, matching `docs/remote-machine-architecture.md`.
pub fn status_label(status: &MachineStatus) -> &'static str {
    match status {
        MachineStatus::Connected => "已连接",
        MachineStatus::Degraded => "延迟较高",
        MachineStatus::Connecting => "连接中",
        MachineStatus::Bootstrapping => "正在 bootstrap",
        MachineStatus::Disconnected => "未连接",
        MachineStatus::Unreachable => "不可达",
        MachineStatus::Error => "错误",
        MachineStatus::RequiresUserAction => "等待确认",
    }
}

/// Latency text for the runtime inspector / sidebar tooltip.
pub fn latency_label(machine: &Machine) -> String {
    match machine.metadata.latency_ms {
        Some(ms) if ms < 100 => format!("{ms}ms"),
        Some(ms) if ms <= 500 => format!("{ms}ms (slow)"),
        Some(ms) => format!("{ms}ms (degraded)"),
        None => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_requires_a_name_and_a_target() {
        let mut form = MachineForm::open();
        assert!(form.draft().is_none());
        form.name = "devbox".into();
        assert!(form.error().is_some());
        form.host = "10.0.0.42".into();
        let draft = form.draft().expect("draft");
        assert_eq!(draft.name, "devbox");
        assert_eq!(draft.port, 22);
    }

    #[test]
    fn alias_form_ignores_the_port_field() {
        let form = MachineForm {
            name: "devbox".into(),
            ssh_config_host: "devbox".into(),
            host: "".into(),
            port: "2222".into(),
            use_ssh_config: true,
            ..MachineForm::default()
        };
        let draft = form.draft().expect("draft");
        // ~/.ssh/config owns the port for an alias; sending -p would override it.
        assert_eq!(draft.ssh_config_host.as_deref(), Some("devbox"));
        assert_eq!(draft.port, 22);
    }

    #[test]
    fn status_icons_cover_every_variant() {
        for status in [
            MachineStatus::Connected,
            MachineStatus::Degraded,
            MachineStatus::Connecting,
            MachineStatus::Bootstrapping,
            MachineStatus::Disconnected,
            MachineStatus::Unreachable,
            MachineStatus::Error,
            MachineStatus::RequiresUserAction,
        ] {
            assert!(!status_icon(&status).is_empty());
            assert!(!status_label(&status).is_empty());
        }
    }

    #[test]
    fn store_starts_local_and_only_the_local_machine_is_protected() {
        let store = MachineStore::new();
        assert_eq!(store.machines.len(), 1);
        assert!(store.machines[0].id.is_local());
        assert_eq!(store.selected.as_ref().map(|id| id.is_local()), Some(true));
        assert!(store.connected().is_empty());
    }

    #[test]
    fn attention_marks_the_machine_as_blocked_and_a_changed_key_as_dangerous() {
        let mut store = MachineStore::new();
        let id = MachineId::from_string("mach-1".into());
        store.machines.push(Machine::ssh(
            id.clone(),
            "devbox".into(),
            "10.0.0.42".into(),
            22,
            None,
            None,
        ));

        store.raise_attention(&id, "主机密钥变更：与 known_hosts 记录不一致", Some("SHA256:abc".into()));
        let attention = store.attention.clone().expect("attention");
        assert!(attention.changed, "a changed key must be flagged");
        assert_eq!(attention.fingerprint.as_deref(), Some("SHA256:abc"));
        assert_eq!(
            store.get(&id).map(|m| m.status.clone()),
            Some(MachineStatus::RequiresUserAction)
        );

        // Cancelling the card does not silently trust anything.
        assert!(store.clear_attention_for(&id));
        assert!(store.attention.is_none());

        // An unknown key is a normal (non-dangerous) confirmation.
        store.raise_attention(&id, "未知主机密钥，需要确认", Some("SHA256:def".into()));
        assert!(!store.attention.as_ref().unwrap().changed);
    }
}
