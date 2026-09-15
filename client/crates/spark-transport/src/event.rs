//! Transport events: the real-time stream from host-agent to client.
//!
//! These map closely to the AgentEvent / LoopEvent types in host-agent,
//! but are normalized for the client. The spark-ui stores consume these
//! and update Entity<T> state.

use serde::{Deserialize, Serialize};

use spark_model::{Machine, MachineId, MachineStatus, MachineKind};

/// What the "+ Machine" form shows after its Test connection button.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineTestOutcome {
    pub ok: bool,
    pub message: String,
    /// Host key fingerprint, when the failure is an unknown/changed host key.
    pub fingerprint: Option<String>,
    /// True when the user has to decide ([取消] / [信任并连接]).
    pub requires_user_action: bool,
}

/// Bootstrap result, flattened so the UI can show it without the transport type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapSummary {
    pub platform: String,
    pub version: String,
    pub installed_binary: String,
    pub socket: String,
    pub data_dir: String,
    pub log_file: String,
    pub already_running: bool,
}

impl BootstrapSummary {
    pub fn from_report(report: &crate::ssh::bootstrap::BootstrapReport) -> Self {
        Self {
            platform: report.platform.target(),
            version: report.version.clone(),
            installed_binary: report.installed_binary.clone(),
            socket: report.socket.clone(),
            data_dir: report.data_dir.clone(),
            log_file: report.log_file.clone(),
            already_running: report.already_running,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransportEvent {
    // ----- Connection -----
    Connected,
    Disconnected { reason: Option<String> },

    // ----- Task lifecycle -----
    TaskCreated {
        task_id: String,
        goal: String,
    },
    TaskStatusChanged {
        task_id: String,
        status: String,
    },

    // ----- Timeline items (from agent loop) -----
    UserMessage {
        task_id: String,
        message_id: String,
        content: String,
    },
    AssistantMessage {
        task_id: String,
        message_id: String,
        content: String,
        streaming: bool,
    },
    AssistantStreaming {
        task_id: String,
        message_id: String,
        delta: String,
    },

    // ----- Tool calls -----
    ToolStarted {
        task_id: String,
        call_id: String,
        tool_name: String,
        args_json: String,
    },
    ToolOutput {
        task_id: String,
        call_id: String,
        delta: String,
    },
    ToolFinished {
        task_id: String,
        call_id: String,
        tool_name: String,
        status: String, // "success" | "error" | "permission_required" | "cancelled"
        result: String,
        duration_ms: u64,
    },

    // ----- Permission -----
    PermissionRequired {
        task_id: String,
        permission_id: String,
        tool_name: String,
        reason: String,
    },
    PermissionResolved {
        task_id: String,
        permission_id: String,
        allowed: bool,
    },

    // ----- ReadyForCheck -----
    ReadyForCheck {
        task_id: String,
        summary: String,
        files_changed: u32,
        tests_passed: bool,
        browser_verified: bool,
    },
    CheckConfirmed {
        task_id: String,
    },

    // ----- Browser -----
    BrowserScreenshot {
        task_id: String,
        frame: Vec<u8>,
    },
    BrowserAction {
        task_id: String,
        action: String, // "click @e12", "fill @e8 \"hello\"", etc.
    },

    // ----- Terminal -----
    TerminalOutput {
        task_id: String,
        terminal_id: String,
        data: Vec<u8>,
    },
    TerminalExit {
        task_id: String,
        terminal_id: String,
        code: Option<i32>,
    },

    // ----- Machines -----
    /// Full list after `ListMachines`, `AddMachine` or `RemoveMachine`.
    MachinesUpdated { machines: Vec<Machine> },
    /// Cheap status update for one machine (the sidebar's status dot).
    MachineStatusChanged {
        machine_id: MachineId,
        status: MachineStatus,
        detail: Option<String>,
    },
    /// Handshake metadata: hostname, OS, kernel, CPU, memory, GPU, sandd version.
    MachineMetadataUpdated { machine: Machine },
    /// Host key or auth problem. The UI shows the fingerprint plus
    /// [取消] / [信任并连接]; only an explicit confirmation trusts the key.
    MachineAttentionRequired {
        machine_id: MachineId,
        message: String,
        fingerprint: Option<String>,
    },
    /// `ssh host true` answered (the form's Test connection button).
    MachineTested { machine: MachineKind, outcome: MachineTestOutcome },
    /// Bootstrap progress line ("uploading sandd", "starting sandd", ...).
    MachineBootstrapProgress { machine_id: MachineId, message: String },
    MachineBootstrapFinished {
        machine_id: MachineId,
        report: BootstrapSummary,
    },
    /// Runtimes on a machine — after a reconnect this is the recovery list.
    RuntimesUpdated {
        machine_id: MachineId,
        runtimes: Vec<crate::runtime_transport::RuntimeInfo>,
    },

    // ----- Errors -----
    Error {
        task_id: Option<String>,
        message: String,
        recoverable: bool,
    },
}
