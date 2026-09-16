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
//! This crate is the *client* side of the model and deliberately does not
//! depend on the `sand` workspace: `MachineId` is duplicated here (and in
//! `sand-protocol`) as an opaque newtype. The two crates stay in sync through
//! `machine_ids_match` / the "machine-local" constant rather than a shared
//! dependency, which keeps the build graph of the GPUI client independent from
//! the runtime kernel.

use serde::{Deserialize, Serialize};

/// Opaque machine identifier.
///
/// * `machine-local` — the well known id of the machine running host-agent.
/// * `mach-<hex>` — stable fingerprint id of a host (`/etc/machine-id` or
///   hostname hashed), which is what remote machines report.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MachineId(pub String);

/// Id of the machine that runs host-agent itself.
pub const LOCAL_MACHINE_ID: &str = "machine-local";

/// Fingerprint an arbitrary host seed into a machine id. Mirrors
/// `sand_protocol::host_machine_id` so both sides agree on the same string.
pub fn fingerprint_machine_id(seed: &str) -> MachineId {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in seed.trim().as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    MachineId(format!("mach-{:012x}", hash & 0xffff_ffff_ffff))
}

/// Opaque comparison that does not require the `sand` workspace: ids are only
/// ever compared as strings.
pub fn machine_ids_match(a: &MachineId, b: &MachineId) -> bool {
    a.0 == b.0
}

impl MachineId {
    /// Generate a fresh random id (used for ad-hoc machines in tests/UI).
    pub fn new() -> Self {
        Self(format!("mach-{}", uuid::Uuid::new_v4().simple()))
    }

    /// The local machine's well known id.
    pub fn local() -> Self {
        Self(LOCAL_MACHINE_ID.to_string())
    }

    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Human label for the sidebar when nothing better is known.
    pub fn short(&self) -> String {
        if self.is_local() {
            "Local".to_string()
        } else {
            self.0.clone()
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_local(&self) -> bool {
        self.0 == LOCAL_MACHINE_ID
    }
}

impl Default for MachineId {
    fn default() -> Self {
        Self::local()
    }
}

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// MachineKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineKind {
    #[default]
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

    pub fn is_ssh(&self) -> bool {
        matches!(self, MachineKind::Ssh { .. })
    }

    /// SSH target as it should be handed to `ssh(1)` on the command line.
    /// The `~/.ssh/config` alias wins when present.
    pub fn ssh_target(&self) -> Option<String> {
        match self {
            MachineKind::Local => None,
            MachineKind::Ssh { host, user, ssh_config_host, .. } => Some(
                ssh_config_host
                    .clone()
                    .unwrap_or_else(|| match user {
                        Some(user) => format!("{}@{}", user, host),
                        None => host.clone(),
                    }),
            ),
        }
    }

    /// Port to pass to `ssh -p`, unless the alias already carries it.
    pub fn ssh_port(&self) -> Option<u16> {
        match self {
            MachineKind::Local => None,
            MachineKind::Ssh { port, ssh_config_host, .. } => {
                if ssh_config_host.is_some() {
                    None // resolved by ~/.ssh/config
                } else if *port == 22 {
                    None
                } else {
                    Some(*port)
                }
            }
        }
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
    /// Permanent error (protocol mismatch, sandd failure).
    Error,
    /// Only the user can move this forward: unknown host key, key mismatch,
    /// authentication failure. Automatic reconnect must stop here.
    RequiresUserAction,
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
            MachineStatus::RequiresUserAction => "requires_user_action",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, MachineStatus::Connected | MachineStatus::Degraded)
    }

    /// True when retrying automatically would be pointless or harmful.
    pub fn requires_user_action(&self) -> bool {
        matches!(self, MachineStatus::RequiresUserAction)
    }

    /// Map an SSH/sandd failure onto a machine status.
    ///
    /// Auth failures and host-key problems are terminal until the user acts;
    /// timeouts and connection resets are retryable.
    pub fn from_ssh_error(message: &str) -> MachineStatus {
        let lower = message.to_lowercase();
        let needs_user = [
            "permission denied",
            "host key verification failed",
            "remote host identification has changed",
            "no such identity",
            "offending key",
            "too many authentication failures",
            "authentication failed",
        ];
        if needs_user.iter().any(|needle| lower.contains(needle)) {
            return MachineStatus::RequiresUserAction;
        }
        if lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("connection refused")
            || lower.contains("could not resolve")
            || lower.contains("no route to host")
            || lower.contains("connection closed")
            || lower.contains("broken pipe")
        {
            return MachineStatus::Unreachable;
        }
        MachineStatus::Error
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
    /// Build capabilities from the feature list a remote sandd reported
    /// (`Handshake.features`). A feature we do not recognise is simply off.
    pub fn from_features(features: &[String], has_display: bool, gpu: bool) -> Self {
        let has = |name: &str| features.iter().any(|f| f == name);
        Self {
            exec: has("exec"),
            pty: has("pty"),
            filesystem: has("fs"),
            // Browser support needs a sandd that advertises it *and* is not a
            // bare remote host; the remote side spawns Xvfb + Chrome itself.
            browser: has("browser"),
            computer_use: (has("computer") || has("computer.desktop")) && has_display,
            desktop: has_display,
            gpu,
        }
    }

    pub fn with_gpu(mut self, gpu: bool) -> Self {
        self.gpu = gpu;
        self
    }

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
            MachineKind::Ssh { host, port: _, user, ssh_config_host } => {
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

    /// Argument vector for `ssh(1)` **without** the remote command, e.g.
    /// `["-T", "-p", "2222", "dev@10.0.0.42"]`.
    ///
    /// `~/.ssh/config`, `ssh-agent`, `known_hosts`, `ProxyJump` and
    /// `ProxyCommand` are all honoured because we drive the system `ssh`
    /// binary rather than reimplementing the protocol. Nothing here ever
    /// disables host key checking.
    pub fn ssh_args(&self) -> Option<Vec<String>> {
        let target = self.kind.ssh_target()?;
        let mut args = vec!["-T".to_string()];
        if let Some(port) = self.kind.ssh_port() {
            args.push("-p".to_string());
            args.push(port.to_string());
        }
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
        args.push("--".to_string());
        args.push(target);
        Some(args)
    }

    /// The remote command that starts the bridge on the machine.
    ///
    /// Falls back from `sand bridge` to `sandd bridge` so that a machine which
    /// only has the sandd binary bootstrapped still works.
    pub fn bridge_remote_command(socket: &str) -> String {
        // Preserve `~/.cache/...`: single-quoting a tilde would make the
        // bridge look for a literal directory named `~` on the remote host.
        let socket_arg = if let Some(suffix) = socket.strip_prefix("~/") {
            format!("\"$HOME/{suffix}\"")
        } else {
            format!("'{}'", socket.replace('\'', "'\\''"))
        };
        // Prefer the `sand` wrapper on PATH; fall back to the bootstrapped
        // binaries, so a machine that only has sandd still works.
        let candidates = [
            "sand".to_string(),
            "\"$HOME/.local/share/spark/current/sand\"".to_string(),
            "\"$HOME/.local/share/spark/current/sandd\"".to_string(),
        ];
        let mut script = String::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let keyword = if index == 0 { "if" } else { "elif" };
            script.push_str(&format!(
                "{keyword} command -v {candidate} >/dev/null 2>&1; then {candidate} bridge --socket {socket_arg}; "
            ));
        }
        script.push_str(
            "else echo 'spark: no sand/sandd bridge binary on this machine' >&2; exit 127; fi",
        );
        script
    }

    /// Install layout on a machine (§十二): binaries live under
    /// `~/.local/share/spark`, sockets under `~/.cache/spark/run`.
    pub fn spark_paths() -> SparkPaths {
        SparkPaths::default()
    }

    /// Refresh the volatile parts of a machine record after a successful ping.
    pub fn touch(&mut self, latency_ms: u64) {
        self.last_seen_at = Some(chrono::Utc::now());
        self.metadata.latency_ms = Some(latency_ms);
    }
}

/// Remote install layout.
#[derive(Debug, Clone)]
pub struct SparkPaths {
    pub root: String,
    pub current: String,
    pub socket: String,
    pub data: String,
    pub log: String,
}

impl SparkPaths {
    /// Directory that holds the daemon socket (created 0700 by bootstrap).
    pub fn socket_dir(&self) -> String {
        match self.socket.rsplit_once('/') {
            Some((dir, _)) => dir.to_string(),
            None => "~/.cache/spark/run".to_string(),
        }
    }

    /// Log directory (`~/.local/state/spark`).
    pub fn logs(&self) -> &str {
        &self.log
    }

    /// Versioned directory for a sandd version.
    pub fn version_dir(&self, version: &str) -> String {
        format!("{}/versions/{}", self.root, version)
    }

    /// Where the machine's artifact cache lives (lazy artifact download).
    pub fn artifacts_dir(&self) -> String {
        format!("{}/artifacts", self.data)
    }
}

impl Default for SparkPaths {
    fn default() -> Self {
        Self {
            root: "~/.local/share/spark".to_string(),
            current: "~/.local/share/spark/current".to_string(),
            socket: "~/.cache/spark/run/sandd.sock".to_string(),
            data: "~/.local/share/spark/data".to_string(),
            log: "~/.local/state/spark".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Machine connection parameters (for UI forms)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            kind: MachineKind::Ssh {
                host: host.clone(),
                port,
                user: user.clone(),
                ssh_config_host: ssh_config_host.clone(),
            },
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
