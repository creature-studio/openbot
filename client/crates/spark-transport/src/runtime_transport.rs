//! RuntimeTransport: the single abstraction the whole agent stack talks to.
//!
//! ```text
//! Tool call (shell.exec / file.* / terminal.* / browser.* / computer.*)
//!   ↓
//! Session → Runtime → machine_id                       (fixed at creation)
//!   ↓
//! MachineManager.transport(machine_id)
//!   ↓
//! RuntimeTransport::exec()                             ← LocalTransport | SshTransport
//! ```
//!
//! Nothing above this trait may branch on "local vs remote": the trait is the
//! only contract, and it carries *both* the sandd runtime RPCs (create/exec/pty/
//! fs) and the machine-level ones (status/ping/handshake/artifact transfer).
//!
//! GPUI-agnostic: usable from the CLI, from tests, and from host-agent.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use futures::Stream;

use sand_protocol::frame::FramePayload;

use spark_model::{MachineCapabilities, MachineId, MachineMetadata};

/// Boxed stream of runtime events (used for `subscribe_events`).
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send + 'static>>;

// ---------------------------------------------------------------------------
// CreateRuntime
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CreateRuntimeRequest {
    /// The machine to create the runtime on. When `None` the transport uses
    /// its own machine (locals default to the local machine id).
    pub machine_id: Option<MachineId>,
    /// Runtime kind: `task`, `workbench`, `assistant`, `eval`.
    pub kind: String,
    /// Workspace path **on the target machine**.
    pub workspace: PathBuf,
    /// Optional explicit runtime id.
    pub runtime_id: Option<String>,
    /// Optional resource limits.
    pub cgroup_config: Option<CgroupConfig>,
}

impl CreateRuntimeRequest {
    pub fn new(kind: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            machine_id: None,
            kind: kind.into(),
            workspace: workspace.into(),
            runtime_id: None,
            cgroup_config: None,
        }
    }

    pub fn on_machine(mut self, machine_id: MachineId) -> Self {
        self.machine_id = Some(machine_id);
        self
    }

    pub fn with_runtime_id(mut self, runtime_id: impl Into<String>) -> Self {
        self.runtime_id = Some(runtime_id.into());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct CgroupConfig {
    /// CPU limit in cores (2.0 == two full cores).
    pub cpu_limit: Option<f64>,
    /// Memory limit in bytes.
    pub memory_limit: Option<u64>,
    /// Optional process group name.
    pub pgroup: Option<String>,
}

// ---------------------------------------------------------------------------
// Status / handshake
// ---------------------------------------------------------------------------

/// Result of `RuntimeTransport::status()`.
#[derive(Debug, Clone, Default)]
pub struct SandStatus {
    /// The transport is usable (socket reachable / bridge alive).
    pub connected: bool,
    /// sandd binary version as reported by the handshake.
    pub sandd_version: Option<String>,
    /// sand protocol version (independent of the binary version).
    pub protocol_version: Option<u32>,
    /// True when the remote protocol version matches ours.
    pub protocol_compatible: bool,
    /// Machine facts collected on connect.
    pub metadata: MachineMetadata,
    /// Capabilities advertised by the handshake.
    pub capabilities: MachineCapabilities,
    /// Round trip time of the last probe.
    pub latency_ms: Option<u64>,
    /// Populated when `connected == false`.
    pub error: Option<String>,
}

/// The version-negotiation payload exchanged with a machine on connect.
#[derive(Debug, Clone, Default)]
pub struct HandshakeResponse {
    /// Protocol version implemented by the remote sandd.
    pub protocol_version: u32,
    /// Our protocol version, for comparison.
    pub client_protocol_version: u32,
    pub sandd_version: String,
    /// True when protocol versions match.
    pub compatible: bool,
    pub features: Vec<String>,
    pub machine_id: String,
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub kernel: String,
    pub cpu_cores: u64,
    pub memory_total: u64,
    pub uptime_seconds: u64,
    pub gpu: Option<String>,
    pub latency_ms: Option<u64>,
}

impl HandshakeResponse {
    /// Capabilities implied by the advertised feature list.
    pub fn capabilities(&self) -> MachineCapabilities {
        MachineCapabilities::from_features(
            &self.features,
            self.features.iter().any(|f| f == "desktop"),
            self.gpu.is_some(),
        )
    }

    /// Machine facts, ready to store on the `Machine` record.
    pub fn metadata(&self) -> MachineMetadata {
        MachineMetadata {
            os: Some(self.os.clone()),
            kernel: Some(self.kernel.clone()),
            arch: Some(self.arch.clone()),
            cpu_cores: Some(self.cpu_cores as u32),
            memory_total: Some(self.memory_total),
            gpu: self.gpu.clone(),
            sandd_version: Some(self.sandd_version.clone()),
            uptime_seconds: Some(self.uptime_seconds),
            latency_ms: self.latency_ms,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeInfo {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub workspace: PathBuf,
    /// Machine the runtime lives on — the routing key for every later call.
    pub machine_id: MachineId,
    pub capabilities: Vec<String>,
    pub process_count: usize,
    pub pty_count: usize,
}

// ---------------------------------------------------------------------------
// Exec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub runtime_id: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: Option<u64>,
    pub stdin_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    pub pid: i32,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Raw bytes: never assume UTF-8 (commands legitimately emit arbitrary bytes).
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration_ms: u64,
}

impl ExecResult {
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

// ---------------------------------------------------------------------------
// PTY
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PtyOpenRequest {
    pub runtime_id: String,
    pub pty_id: String,
    pub cols: u16,
    pub rows: u16,
    pub shell: String,
}

#[derive(Debug, Clone)]
pub struct PtyWriteRequest {
    pub runtime_id: String,
    pub pty_id: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PtyResizeRequest {
    pub runtime_id: String,
    pub pty_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone)]
pub struct PtySignalRequest {
    pub runtime_id: String,
    pub pty_id: String,
    /// Numeric signal (2 == SIGINT, 15 == SIGTERM, ...).
    pub signal: i32,
}

#[derive(Debug, Clone, Default)]
pub struct PtyReadResponse {
    pub data: Vec<u8>,
    /// Cursor for incremental reads, when the backend supports one.
    pub cursor: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct PtyReadRequest {
    pub runtime_id: String,
    pub pty_id: String,
    pub clear: bool,
}

// ---------------------------------------------------------------------------
// Filesystem
// ---------------------------------------------------------------------------

/// All filesystem traffic for the agent goes through here — **never SFTP**.
/// SFTP/SCP is reserved for bootstrap and artifact transfer only.
#[derive(Debug, Clone)]
pub enum FsRequest {
    Read { path: PathBuf },
    Write { path: PathBuf, data: Vec<u8> },
    List { path: PathBuf },
    Stat { path: PathBuf },
    Mkdir { path: PathBuf, recursive: bool },
    Remove { path: PathBuf, recursive: bool },
    Rename { from: PathBuf, to: PathBuf },
    Search { path: PathBuf, query: String },
    Glob { path: PathBuf, pattern: String },
    Patch { path: PathBuf, patch: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsResponse {
    /// Raw bytes for `Read`; JSON for listing-shaped responses.
    Ok { data: Vec<u8> },
    Error { message: String },
}

impl FsResponse {
    pub fn text(&self) -> String {
        match self {
            FsResponse::Ok { data } => String::from_utf8_lossy(data).to_string(),
            FsResponse::Error { message } => message.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Browser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    Open { url: String },
    Snapshot,
    Click { reference: String },
    Fill { reference: String, text: String },
    Press { key: String },
    Screenshot { format: String, quality: u8 },
    Tabs,
    Close,
}

#[derive(Debug, Clone)]
pub struct BrowserRequest {
    pub runtime_id: String,
    pub action: BrowserAction,
}

#[derive(Debug, Clone, Default)]
pub struct BrowserTab {
    pub id: String,
    pub url: String,
    pub title: String,
}

/// One screenshot frame. Frames are latest-wins: the client drops stale frames
/// rather than queueing them.
#[derive(Debug, Clone, Default)]
pub struct BrowserFrame {
    /// Monotonic frame id, used to drop out-of-order frames.
    pub frame_id: u64,
    pub width: u32,
    pub height: u32,
    pub timestamp_ms: u64,
    /// JPEG or WebP bytes.
    pub data: Vec<u8>,
    pub format: String,
}

#[derive(Debug, Clone, Default)]
pub struct BrowserResponse {
    pub snapshot: Option<String>,
    pub url: Option<String>,
    pub tabs: Option<Vec<BrowserTab>>,
    pub frame: Option<BrowserFrame>,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Computer use
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerAction {
    Screenshot,
    Click { x: i32, y: i32, button: String },
    Type { text: String },
    Move { x: i32, y: i32 },
    Key { key: String },
    Scroll { x: i32, y: i32, delta: i32 },
}

#[derive(Debug, Clone)]
pub struct ComputerRequest {
    pub runtime_id: String,
    pub action: ComputerAction,
}

#[derive(Debug, Clone, Default)]
pub struct ComputerResponse {
    pub frame: Option<BrowserFrame>,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct RuntimeEvent {
    pub runtime_id: String,
    pub kind: String,
    pub at_ms: u64,
    pub machine_id: Option<MachineId>,
}

// ---------------------------------------------------------------------------
// Machine-level operations (lifecycle + bootstrap + artifacts)
// ---------------------------------------------------------------------------

/// Facts a transport learned about the machine it points at.
#[derive(Debug, Clone, Default)]
pub struct TransportStatus {
    pub connected: bool,
    pub detail: Option<String>,
}

/// One artifact transfer request (bootstrap binaries, artifact download).
#[derive(Debug, Clone)]
pub struct FileTransferRequest {
    /// Path on the remote machine.
    pub remote_path: String,
    /// Local path.
    pub local_path: PathBuf,
    /// Expected SHA-256 (hex). Verification is mandatory for uploads.
    pub sha256: Option<String>,
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// Everything the upper layers may do to a machine.
///
/// Implementors: [`crate::LocalTransport`] (local sandd over UDS) and
/// [`crate::SshTransport`] (remote sandd over an SSH stdio bridge).
#[async_trait::async_trait]
pub trait RuntimeTransport: Send + Sync {
    /// Machine this transport is bound to.
    fn machine_id(&self) -> MachineId;

    /// True when the transport currently has a usable connection.
    fn is_connected(&self) -> bool;

    // ----- connection lifecycle -----

    /// Establish the connection (no-op for local). Idempotent.
    async fn connect(&self) -> Result<()> {
        Ok(())
    }

    /// Tear the connection down. Must **never** destroy remote state: dropping
    /// a bridge leaves the remote sandd, its runtimes, PTYs and browsers alive.
    async fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    /// Version negotiation with the machine. Run on every connect.
    async fn handshake(&self) -> Result<HandshakeResponse>;

    /// Cheap liveness + latency probe over the existing connection.
    async fn ping(&self) -> Result<u64>;

    /// Machine/daemon status, including capabilities and metadata.
    async fn status(&self) -> Result<SandStatus>;

    // ----- runtime lifecycle -----

    async fn create_runtime(&self, request: CreateRuntimeRequest) -> Result<RuntimeInfo>;

    async fn destroy_runtime(&self, runtime_id: &str) -> Result<()>;

    async fn list_runtimes(&self) -> Result<Vec<RuntimeInfo>>;

    async fn get_runtime(&self, runtime_id: &str) -> Result<RuntimeInfo>;

    // ----- execution -----

    async fn exec(&self, request: ExecRequest) -> Result<ExecResult>;

    // ----- pty -----

    async fn open_pty(&self, request: PtyOpenRequest) -> Result<()>;
    async fn write_pty(&self, request: PtyWriteRequest) -> Result<()>;
    async fn resize_pty(&self, request: PtyResizeRequest) -> Result<()>;
    async fn signal_pty(&self, request: PtySignalRequest) -> Result<()>;
    async fn read_pty(&self, request: PtyReadRequest) -> Result<PtyReadResponse>;
    async fn close_pty(&self, runtime_id: &str, pty_id: &str) -> Result<()>;
    async fn list_ptys(&self, runtime_id: &str) -> Result<Vec<String>>;

    // ----- filesystem -----

    /// Agent filesystem API: same code path locally and remotely.
    async fn fs_request(&self, runtime_id: &str, request: FsRequest) -> Result<FsResponse>;

    // ----- browser / computer -----

    async fn browser_request(&self, request: BrowserRequest) -> Result<BrowserResponse>;

    async fn computer_request(&self, request: ComputerRequest) -> Result<ComputerResponse>;

    // ----- events -----

    async fn subscribe_events(&self, runtime_id: Option<&str>) -> Result<BoxStream<RuntimeEvent>>;

    // ----- machine-level extras -----

    /// Run a command for *bootstrap/diagnostic* purposes only (no runtime).
    ///
    /// This is the one place a transport may run something outside a runtime:
    /// probing `uname`, checking `--version`, starting sandd. It must never be
    /// used by agent tools.
    async fn machine_exec(&self, command: Vec<String>) -> Result<ExecResult>;

    /// Copy a local file to the machine (bootstrap, artifact upload).
    async fn upload_file(&self, request: FileTransferRequest) -> Result<()>;

    /// Copy a file from the machine (artifact download).
    async fn download_file(&self, request: FileTransferRequest) -> Result<Vec<u8>>;

    /// Human readable description of the connection, for logs and UI.
    fn describe(&self) -> String {
        format!("machine {}", self.machine_id())
    }
}

// ---------------------------------------------------------------------------
// Raw RPC access
// ---------------------------------------------------------------------------

/// The `json + binary` body of one sandd call, exactly as it travels on the
/// framed socket (and therefore as it travels inside a bridge frame).
pub type RpcPayload = FramePayload;

/// Minimal raw RPC access.
///
/// `RuntimeTransport` covers the *agent* surface. Browser and computer helpers
/// need to send their own method names (`BrowserSnapshot`,
/// `ComputerScreenshot`, …) without teaching the trait every panel the UI may
/// grow, so they talk to this smaller trait instead. Both transports implement
/// it over the same socket the rest of their calls use — local UDS or the SSH
/// bridge — which is why the helpers are written once.
#[async_trait::async_trait]
pub trait RawRpc: Send + Sync {
    /// Send one sandd method. `method_json` is a complete
    /// `{"method":"...", ...}` body; `binary` is the raw byte section.
    async fn rpc_raw(&self, method_json: String, binary: Option<Vec<u8>>) -> Result<RpcPayload>;
}

// ---------------------------------------------------------------------------
// Filesystem request serialization
// ---------------------------------------------------------------------------

/// Serialize an [`FsRequest`] into a sandd method body.
///
/// `Read` and `Write` carry their bytes separately (see the transports);
/// everything else is pure JSON and goes through here so the local and remote
/// transports cannot drift apart — the whole point of one trait.
pub fn fs_request_json(runtime_id: &str, request: &FsRequest) -> String {
    let id = spark_json::escape(runtime_id);
    match request {
        FsRequest::Read { path } => format!(
            "{{\"method\":\"FsRead\",\"id\":\"{id}\",\"path\":\"{}\"}}",
            spark_json::escape(&path.display().to_string())
        ),
        FsRequest::Write { path, data } => format!(
            "{{\"method\":\"FsWrite\",\"id\":\"{id}\",\"path\":\"{}\",\"binary_len\":{}}}",
            spark_json::escape(&path.display().to_string()),
            data.len()
        ),
        FsRequest::List { path } => format!(
            "{{\"method\":\"FsList\",\"id\":\"{id}\",\"path\":\"{}\"}}",
            spark_json::escape(&path.display().to_string())
        ),
        FsRequest::Stat { path } => format!(
            "{{\"method\":\"FsStat\",\"id\":\"{id}\",\"path\":\"{}\"}}",
            spark_json::escape(&path.display().to_string())
        ),
        FsRequest::Mkdir { path, recursive } => format!(
            "{{\"method\":\"FsMkdir\",\"id\":\"{id}\",\"path\":\"{}\",\"recursive\":{recursive}}}",
            spark_json::escape(&path.display().to_string())
        ),
        FsRequest::Remove { path, recursive } => format!(
            "{{\"method\":\"FsRemove\",\"id\":\"{id}\",\"path\":\"{}\",\"recursive\":{recursive}}}",
            spark_json::escape(&path.display().to_string())
        ),
        FsRequest::Rename { from, to } => format!(
            "{{\"method\":\"FsRename\",\"id\":\"{id}\",\"path\":\"{}\",\"to\":\"{}\"}}",
            spark_json::escape(&from.display().to_string()),
            spark_json::escape(&to.display().to_string())
        ),
        FsRequest::Search { path, query } => format!(
            "{{\"method\":\"FsSearch\",\"id\":\"{id}\",\"path\":\"{}\",\"query\":\"{}\"}}",
            spark_json::escape(&path.display().to_string()),
            spark_json::escape(query)
        ),
        FsRequest::Glob { path, pattern } => format!(
            "{{\"method\":\"FsGlob\",\"id\":\"{id}\",\"path\":\"{}\",\"pattern\":\"{}\"}}",
            spark_json::escape(&path.display().to_string()),
            spark_json::escape(pattern)
        ),
        // Patches are text: send them as text when they are valid UTF-8
        // (readable in logs, and the applier takes a string), base64 otherwise
        // so no byte is lost.
        FsRequest::Patch { path, patch } => {
            let payload = match std::str::from_utf8(patch) {
                Ok(text) => format!("\"patch\":\"{}\"", spark_json::escape(text)),
                Err(_) => format!(
                    "\"patch_b64\":\"{}\"",
                    spark_json::escape(&crate::hash::base64(patch))
                ),
            };
            format!(
                "{{\"method\":\"FsPatch\",\"id\":\"{id}\",\"path\":\"{}\",{payload}}}",
                spark_json::escape(&path.display().to_string())
            )
        }
    }
}

/// The binary section an [`FsRequest`] must carry (only `Write` has one).
pub fn fs_request_binary(request: &FsRequest) -> Option<Vec<u8>> {
    match request {
        FsRequest::Write { data, .. } => Some(data.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Shared helpers for handshake JSON parsing
// ---------------------------------------------------------------------------

/// Parse a `Handshake` response JSON (sandd's `{"method":"Handshake"}` reply).
pub fn parse_handshake_json(json: &str, client_protocol: u32, latency_ms: Option<u64>) -> HandshakeResponse {
    let get = |field: &str| spark_json::get_str(json, field);
    let get_num = |field: &str| spark_json::get_u64(json, field).unwrap_or(0);

    let protocol_version = get_num("protocol_version") as u32;
    let features = spark_json::get_str_array(json, "features");

    HandshakeResponse {
        protocol_version,
        client_protocol_version: client_protocol,
        sandd_version: get("sandd_version").unwrap_or_default(),
        compatible: spark_json::get_bool(json, "compatible")
            .unwrap_or(protocol_version == client_protocol),
        features,
        machine_id: get("machine_id").unwrap_or_default(),
        os: get("os").unwrap_or_default(),
        arch: get("arch").unwrap_or_default(),
        hostname: get("hostname").unwrap_or_default(),
        kernel: get("kernel").unwrap_or_default(),
        cpu_cores: get_num("cpu_cores"),
        memory_total: get_num("memory_total"),
        uptime_seconds: get_num("uptime_seconds"),
        gpu: get("gpu"),
        latency_ms,
    }
}

/// Hand-rolled JSON helpers used by every transport (sandd's JSON layer is
/// hand rolled too; no serde in the runtime hot path).
pub mod spark_json {
    /// `"field":"value"` → `value`.
    pub fn get_str(json: &str, field: &str) -> Option<String> {
        let patterns = [
            format!("\"{}\":\"", field),
            format!("\"{}\": \"", field),
            format!("\"{}\" : \"", field),
        ];
        for pat in &patterns {
            if let Some(start) = json.find(pat.as_str()) {
                let rest = &json[start + pat.len()..];
                if let Some(end) = rest.find('"') {
                    return Some(unescape(&rest[..end]));
                }
            }
        }
        None
    }

    /// `"field":123` → 123.
    pub fn get_u64(json: &str, field: &str) -> Option<u64> {
        let pat = format!("\"{}\":", field);
        let start = json.find(pat.as_str())?;
        let rest = json[start + pat.len()..].trim_start();
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        rest[..end].parse().ok()
    }

    /// `"field":true|false` → bool.
    pub fn get_bool(json: &str, field: &str) -> Option<bool> {
        let pat = format!("\"{}\":", field);
        let start = json.find(pat.as_str())?;
        let rest = json[start + pat.len()..].trim_start();
        if rest.starts_with("true") {
            Some(true)
        } else if rest.starts_with("false") {
            Some(false)
        } else {
            None
        }
    }

    /// `"field":["a","b"]` → `["a","b"]`. Empty when the field is missing or
    /// is not an array of strings.
    pub fn get_str_array(json: &str, field: &str) -> Vec<String> {
        let Some(body) = array_body(json, field) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                let mut value = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            match chars.next() {
                                Some('n') => value.push('\n'),
                                Some('t') => value.push('\t'),
                                Some('r') => value.push('\r'),
                                Some(other) => value.push(other),
                                None => break,
                            }
                        }
                        '"' => break,
                        other => value.push(other),
                    }
                }
                out.push(value);
            }
        }
        out
    }

    /// Extract a JSON array of objects as raw object strings.
    pub fn get_object_array(json: &str, field: &str) -> Vec<String> {
        let pat = format!("\"{}\":[", field);
        let alt = format!("\"{}\": [", field);
        let start = match json.find(pat.as_str()) {
            Some(pos) => pos + pat.len(),
            None => match json.find(alt.as_str()) {
                Some(pos) => pos + alt.len(),
                None => return Vec::new(),
            },
        };
        let rest = &json[start..];
        // scan for the matching close bracket, tracking braces and strings
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escaped = false;
        let mut end = None;
        for (idx, c) in rest.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => depth -= 1,
                ']' if depth == 0 => {
                    end = Some(idx);
                    break;
                }
                _ => {}
            }
        }
        let body = match end {
            Some(end) => &rest[..end],
            None => rest,
        };

        let mut objects = Vec::new();
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escaped = false;
        let mut start_idx: Option<usize> = None;
        for (idx, c) in body.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => {
                    if depth == 0 {
                        start_idx = Some(idx);
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(s) = start_idx {
                            objects.push(body[s..=idx].to_string());
                        }
                        start_idx = None;
                    }
                }
                _ => {}
            }
        }
        objects
    }

    /// The raw text between `"field":[` and its matching `]`.
    fn array_body(json: &str, field: &str) -> Option<String> {
        let pat = format!("\"{}\":", field);
        let start = json.find(pat.as_str())?;
        let rest = json[start + pat.len()..].trim_start();
        let rest = rest.strip_prefix('[')?;
        let mut depth = 1i32;
        let mut in_string = false;
        let mut escaped = false;
        for (idx, c) in rest.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(rest[..idx].to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The raw JSON value of a field (object/array kept verbatim, strings
    /// unescaped). Used to forward `data` from list/stat style responses.
    pub fn raw_field(json: &str, field: &str) -> Option<String> {
        let pat = format!("\"{}\":", field);
        let start = json.find(pat.as_str())?;
        let rest = json[start + pat.len()..].trim_start();
        let first = rest.chars().next()?;
        if first == '"' {
            return get_str(json, field);
        }
        if first == '{' || first == '[' {
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escaped = false;
            for (idx, c) in rest.char_indices() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        in_string = false;
                    }
                    continue;
                }
                match c {
                    '"' => in_string = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(rest[..=idx].to_string());
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
        // number / bool / null
        let end = rest
            .find(|c: char| c == ',' || c == '}')
            .unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }

    /// True when a response says `"ok":true`.
    pub fn is_ok(json: &str) -> bool {
        get_bool(json, "ok").unwrap_or(false)
    }

    /// `"error":"..."` when present.
    pub fn error_of(json: &str) -> Option<String> {
        get_str(json, "error")
    }

    pub fn escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    fn unescape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                }
                Some(other) => out.push(other),
                None => break,
            }
        }
        out
    }

    /// Serialize `[String]` into a JSON array of strings.
    pub fn str_array(items: &[String]) -> String {
        let inner: Vec<String> = items
            .iter()
            .map(|s| format!("\"{}\"", escape(s)))
            .collect();
        format!("[{}]", inner.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_handshake_response() {
        let json = r#"{"ok":true,"protocol_version":2,"sandd_version":"0.1.0","compatible":true,"features":["exec","pty","fs","browser"],"machine_id":"mach-abc","os":"Ubuntu 24.04","arch":"x86_64","hostname":"devbox","kernel":"6.5.0","uptime_seconds":1234,"cpu_cores":12,"memory_total":17179869184,"gpu":"NVIDIA GPU"}"#;
        let hs = parse_handshake_json(json, 2, Some(31));
        assert_eq!(hs.protocol_version, 2);
        assert!(hs.compatible);
        assert_eq!(hs.sandd_version, "0.1.0");
        assert_eq!(hs.machine_id, "mach-abc");
        assert_eq!(hs.cpu_cores, 12);
        assert_eq!(hs.memory_total, 17_179_869_184);
        assert_eq!(hs.latency_ms, Some(31));
        assert!(hs.features.contains(&"browser".to_string()));
        assert_eq!(hs.gpu.as_deref(), Some("NVIDIA GPU"));

        let caps = hs.capabilities();
        assert!(caps.exec && caps.pty && caps.filesystem && caps.browser);
        assert!(caps.gpu);
    }

    #[test]
    fn extracts_object_arrays() {
        let json = r#"{"ok":true,"runtimes":[{"id":"rt-1","kind":"task","state":"running","workspace":"/home/dev/p","procs":2,"ptys":1,"machine_id":"mach-x"},{"id":"rt-2","kind":"workbench","state":"running","workspace":"/tmp/w","procs":0,"ptys":0,"machine_id":"mach-x"}]}"#;
        let objects = spark_json::get_object_array(json, "runtimes");
        assert_eq!(objects.len(), 2, "objects: {objects:?}");
        assert_eq!(spark_json::get_str(&objects[0], "id").as_deref(), Some("rt-1"));
        assert_eq!(spark_json::get_str(&objects[1], "kind").as_deref(), Some("workbench"));
        assert_eq!(spark_json::get_u64(&objects[0], "procs"), Some(2));
    }

    #[test]
    fn parses_string_arrays() {
        let json = r#"{"ok":true,"ptys":["term-1","term-2"]}"#;
        let ptys = spark_json::get_str_array(json, "ptys");
        assert_eq!(ptys, vec!["term-1".to_string(), "term-2".to_string()]);
    }

    #[test]
    fn escapes_json() {
        assert_eq!(spark_json::escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        assert_eq!(spark_json::str_array(&["x".into(), "y\"z".into()]), "[\"x\",\"y\\\"z\"]");
    }
}
