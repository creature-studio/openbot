//! spark-model: Machine abstraction — local and remote (SSH) machines.
//!
//! Machine is a first-class concept: a place where Runtimes can be created.
//! Local and Remote machines are treated uniformly — the agent/tool layer
//! never knows whether a Runtime is local or remote.
//!
//! ```text
//! Machine
//!   └─ Runtime
//!        ├─ Task A
//!        ├─ Task B
//!        └─ Workbench
//! ```
//!
//! MachineId is defined in sand-protocol (the shared protocol crate) and
//! re-exported here. All other machine types are defined here.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Re-export MachineId from sand-protocol — the single source of truth.
pub use sand_protocol::MachineId;

// ---------------------------------------------------------------------------
// MachineKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineKind {
    /// The local machine where host-agent runs (and where sandd talks via UDS).
    Local,

    /// An SSH-accessible remote machine.
    /// The `ssh_config_host` is an optional Host alias from ~/.ssh/config.
    /// If set, `ssh devbox` is used; otherwise `ssh -p PORT user@host` is used.
    Ssh {
        /// Hostname or IP.
        host: String,
        /// SSH port.
        port: u16,
        /// SSH username. If None, uses the local username or SSH config.
        user: Option<String>,
        /// SSH config Host alias (e.g. "devbox"). If set, this is preferred
        /// over explicit host/port/user — the system's ssh will resolve everything
        /// from ~/.ssh/config including IdentityFile, ProxyJump, etc.
        ssh_config_host: Option<String>,
    },
}

impl MachineKind {
    pub fn display_name(&self) -> &str {
        match self {
            MachineKind::Local => "Local",
            MachineKind::Ssh { .. } => "SSH",
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, MachineKind::Local)
    }
}

// ---------------------------------------------------------------------------
// MachineStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineStatus {
    /// No active connection. Machine exists but we're not connected.
    Disconnected,
    /// SSH handshake in progress.
    Connecting,
    /// SSH connected, bootstrapping sandd/bridge (detect platform, install, start).
    Bootstrapping,
    /// Fully connected, bridge active, sandd reachable.
    Connected,
    /// Connected but degraded (high latency, some features unavailable).
    Degraded,
    /// Could not reach the machine (SSH timeout, network error).
    Unreachable,
    /// Permanent error (auth failure, host key mismatch requiring user action).
    Error,
}

impl MachineStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MachineStatus::Disconnected => "disconnected",
            MachineStatus::Connecting => "connecting",
            MachineStatus::Bootstrapping => "bootstrapping",
            MachineStatus::Connected => "connected",
            MachineStatus::Degraded => "degraded",
            MachineStatus::Unreachable => "unreachable",
            MachineStatus::Error => "error",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, MachineStatus::Connected | MachineStatus::Degraded)
    }
}

// ---------------------------------------------------------------------------
// MachineCapabilities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MachineCapabilities {
    /// Can execute shell commands via sandd Exec.
    pub exec: bool,
    /// Can open/use PTY sessions.
    pub pty: bool,
    /// Can access filesystem via sandd FS API.
    pub filesystem: bool,
    /// Can run a browser (Xvfb + Chrome) on this machine.
    pub browser: bool,
    /// Can run computer-use tools (screenshot, mouse, keyboard).
    pub computer_use: bool,
    /// Has a desktop environment / display server.
    pub desktop: bool,
    /// Has a GPU available.
    pub gpu: bool,
}

impl MachineCapabilities {
    /// Default capabilities for a local machine (everything on).
    pub fn local() -> Self {
        Self {
            exec: true,
            pty: true,
            filesystem: true,
            browser: true,
            computer_use: true,
            desktop: true,
            gpu: true,
        }
    }

    /// Default capabilities for a remote SSH machine — conservative.
    /// We only know for sure after bootstrap.
    pub fn ssh_default() -> Self {
        Self {
            exec: true,
            pty: true,
            filesystem: true,
            browser: false,
            computer_use: false,
            desktop: false,
            gpu: false,
        }
    }
}

// ---------------------------------------------------------------------------
// MachineMetadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MachineMetadata {
    /// OS name (e.g. "Ubuntu 24.04", "Debian GNU/Linux 12").
    pub os: Option<String>,
    /// Kernel version (e.g. "6.5.0-15-generic").
    pub kernel: Option<String>,
    /// Architecture (e.g. "x86_64", "aarch64").
    pub arch: Option<String>,
    /// CPU model / core count.
    pub cpu_cores: Option<u32>,
    /// CPU usage percentage (0–100), updated by pings.
    pub cpu_percent: Option<f32>,
    /// Total memory in bytes.
    pub memory_total: Option<u64>,
    /// Used memory in bytes.
    pub memory_used: Option<u64>,
    /// GPU description (e.g. "NVIDIA RTX 4090").
    pub gpu: Option<String>,
    /// sandd version on the machine.
    pub sandd_version: Option<String>,
    /// Agent version on the machine.
    pub agent_version: Option<String>,
    /// Machine uptime in seconds.
    pub uptime_seconds: Option<u64>,
    /// Last ping latency in milliseconds.
    pub latency_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    pub id: MachineId,
    pub name: String,

    pub kind: MachineKind,

    pub status: MachineStatus,

    pub capabilities: MachineCapabilities,

    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,

    pub metadata: MachineMetadata,

    /// Optional display name for SSH machines — used for UI.
    /// Different from `name` which is a unique ID-friendly slug.
    pub display_name: Option<String>,
}

impl Machine {
    pub fn local(name: Option<String>) -> Self {
        let name = name.unwrap_or_else(|| "Local".to_string());
        Self {
            id: MachineId::local(),
            name: name.clone(),
            kind: MachineKind::Local,
            status: MachineStatus::Connected,
            capabilities: MachineCapabilities::local(),
            last_seen_at: Some(chrono::Utc::now()),
            metadata: MachineMetadata::default(),
            display_name: Some(name),
        }
    }

    pub fn ssh(
        id: MachineId,
        name: String,
        host: String,
        port: u16,
        user: Option<String>,
        ssh_config_host: Option<String>,
    ) -> Self {
        Self {
            id,
            name: name.clone(),
            kind: MachineKind::Ssh {
                host: host.clone(),
                port,
                user,
                ssh_config_host,
            },
            status: MachineStatus::Disconnected,
            capabilities: MachineCapabilities::ssh_default(),
            last_seen_at: None,
            metadata: MachineMetadata::default(),
            display_name: Some(name),
        }
    }

    /// Human-readable identifier for the connection target.
    pub fn connection_target(&self) -> String {
        match &self.kind {
            MachineKind::Local => "local".to_string(),
            MachineKind::Ssh { host, port, user, ssh_config_host } => {
                if let Some(alias) = ssh_config_host {
                    alias.clone()
                } else if let Some(user) = user {
                    format!("{}@{}", user, host)
                } else {
                    format!("{}", host)
                }
            }
        }
    }

    /// SSH command line for launching the bridge.
    /// e.g. `ssh -T devbox` or `ssh -p 22 user@host`.
    pub fn ssh_command(&self) -> Option<Vec<String>> {
        match &self.kind {
            MachineKind::Local => None,
            MachineKind::Ssh { ssh_config_host, port, user, host } => {
                let mut cmd = vec!["ssh".to_string()];
                if let Some(alias) = ssh_config_host {
                    // Use alias — ssh config resolves everything.
                    cmd.push(alias.clone());
                } else {
                    if *port != 22 {
                        cmd.push(format!("-p {}", port));
                    }
                    if let Some(user) = user {
                        cmd.push(format!("{}@{}", user, host));
                    } else {
                        cmd.push(host.clone());
                    }
                }
                Some(cmd)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Machine connection parameters (for UI forms)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MachineConnection {
    /// Connection type: "local" or "ssh".
    pub kind: MachineKind,

    /// Display name shown in the UI.
    pub name: String,

    // SSH fields (only relevant when kind == Ssh)
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub ssh_config_host: Option<String>,
}

impl MachineConnection {
    pub fn local(name: String) -> Self {
        Self {
            kind: MachineKind::Local,
            name,
            host: String::new(),
            port: 22,
            user: None,
            ssh_config_host: None,
        }
    }

    pub fn ssh(name: String, host: String, port: u16, user: Option<String>, ssh_config_host: Option<String>) -> Self {
        Self {
            kind: MachineKind::Ssh { host, port, user, ssh_config_host },
            name,
            host,
            port,
            user,
            ssh_config_host,
        }
    }
}

// ---------------------------------------------------------------------------
// SSH host key (for security verification)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshHostKey {
    pub host: String,
    pub port: u16,
    pub key_type: String,   // e.g. "ED25519", "RSA", "ECDSA"
    pub fingerprint: String, // e.g. "SHA256:xxxx"
    pub raw_key: String,     // Base64-encoded public key
    pub first_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_verified_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl SshHostKey {
    pub fn new(host: String, port: u16, key_type: String, fingerprint: String, raw_key: String) -> Self {
        Self {
            host,
            port,
            key_type,
            fingerprint,
            raw_key,
            first_seen_at: Some(chrono::Utc::now()),
            last_verified_at: None,
        }
    }
}
