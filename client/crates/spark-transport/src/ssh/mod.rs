//! SshTransport — a remote machine reached over a single long-lived SSH
//! connection carrying the binary framed bridge.
//!
//! ```text
//! host-agent
//!    │  frames (24-byte header + json/binary payload)
//!    ▼
//! ssh -T devbox  "sand bridge --socket ~/.cache/spark/run/sandd.sock"
//!    │
//!    ▼
//! sand bridge ──UDS──▶ remote sandd ──▶ runtimes / PTYs / browser / files
//! ```
//!
//! Properties this module is responsible for:
//!
//! * **one** ssh process per machine, never one per tool call (architecture §七);
//! * request/response correlation by `request_id`, so concurrent tool calls do
//!   not serialize behind each other;
//! * `Ping`/`Pong` answered by the bridge, giving a real latency number (§二十七);
//! * push frames (events, streams) delivered to subscribers;
//! * disconnect ≠ destroy: dropping the connection leaves the remote sandd, its
//!   runtimes, PTYs and browsers running; reconnecting re-attaches by listing
//!   runtimes and resuming (§十五/§十六);
//! * host key verification against the user's `known_hosts`, never
//!   `StrictHostKeyChecking=no` (§十).

pub mod bootstrap;
pub mod hostkey;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crate::runtime_transport::BoxStream;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use sand_protocol::frame::{FrameHeader, FrameKind, FramePayload, MAX_PAYLOAD_LEN};
use sand_protocol::SAND_PROTOCOL_VERSION;

use spark_model::{Machine, MachineId, MachineStatus};

use crate::events::EventSink;
use crate::runtime_transport::*;
use crate::wire::{read_frame, write_frame};

/// Request timeout. Generous: bootstrap, snapshot generation and screenshots on
/// a loaded remote machine are all slow, and a timeout here is indistinguishable
/// from "the machine died" for the user.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Ping timeout, deliberately short: this drives the health indicator.
const PING_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Errors the UI must react to
// ---------------------------------------------------------------------------

/// Connection failures that need a human decision. MachineManager converts these
/// into an `Attention` card / `RequiresUserAction` status.
#[derive(Debug)]
pub enum ConnectError {
    /// Host key not in `known_hosts` yet — needs a first-use confirmation.
    HostKeyUnknown(hostkey::HostKeyIssue),
    /// Host key differs from `known_hosts` — high risk, never automatic.
    HostKeyChanged(hostkey::HostKeyIssue),
    /// Authentication failed / no usable key.
    AuthFailed(String),
    /// Transient network problem: safe to retry with backoff.
    Unreachable(String),
    /// Connected, but the machine cannot run sandd (missing sandd, bad arch).
    Bootstrap(String),
    /// Protocol versions do not line up.
    IncompatibleProtocol { client: u32, remote: u32 },
    Other(String),
}

impl ConnectError {
    pub fn message(&self) -> String {
        match self {
            ConnectError::HostKeyUnknown(issue) => issue.attention_message(),
            ConnectError::HostKeyChanged(issue) => issue.attention_message(),
            ConnectError::AuthFailed(m) => format!("SSH 认证失败: {m}"),
            ConnectError::Unreachable(m) => m.clone(),
            ConnectError::Bootstrap(m) => m.clone(),
            ConnectError::IncompatibleProtocol { client, remote } => format!(
                "protocol 不兼容: 本机 v{client}, 远端 sandd v{remote}. 请升级远端 sandd（Spark 会尝试 bootstrap）"
            ),
            ConnectError::Other(m) => m.clone(),
        }
    }

    /// Status the machine record should move to.
    pub fn status(&self) -> MachineStatus {
        match self {
            ConnectError::HostKeyUnknown(_)
            | ConnectError::HostKeyChanged(_)
            | ConnectError::AuthFailed(_) => MachineStatus::RequiresUserAction,
            ConnectError::Unreachable(_) => MachineStatus::Unreachable,
            ConnectError::Bootstrap(_)
            | ConnectError::IncompatibleProtocol { .. }
            | ConnectError::Other(_) => MachineStatus::Error,
        }
    }

    /// May the reconnect loop retry this? Auth failures and host key changes
    /// must never spin forever waiting for a human who is not watching.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ConnectError::Unreachable(_) | ConnectError::Bootstrap(_)
        )
    }

    pub fn host_key_issue(&self) -> Option<&hostkey::HostKeyIssue> {
        match self {
            ConnectError::HostKeyUnknown(issue) | ConnectError::HostKeyChanged(issue) => Some(issue),
            _ => None,
        }
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for ConnectError {}

// ---------------------------------------------------------------------------
// Bridge connection
// ---------------------------------------------------------------------------

/// The live ssh child plus its framed stdio.
struct BridgeConnection {
    /// Owned so we can kill the ssh process on teardown.
    child: AsyncMutex<Option<Child>>,
    stdin: AsyncMutex<ChildStdin>,
    stdout: AsyncMutex<ChildStdout>,
    next_request_id: AtomicU64,
    /// In-flight requests: `request_id` → responder.
    pending: AsyncMutex<HashMap<u64, oneshot::Sender<(String, Vec<u8>)>>>,
    /// Server-push frames (events, streams) fan out here.
    sink: EventSink,
    dead: AtomicBool,
    /// Latency of the last ping.
    latency_ms: AtomicU64,
    /// ssh's stderr, drained by a background task (error classification).
    stderr: Arc<AsyncMutex<String>>,
}

impl BridgeConnection {
    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    fn latency(&self) -> Option<u64> {
        let value = self.latency_ms.load(Ordering::Relaxed);
        (value > 0).then_some(value)
    }

    async fn stderr_text(&self) -> String {
        self.stderr.lock().await.clone()
    }

    async fn mark_dead(&self, reason: &str) {
        if !self.dead.swap(true, Ordering::Relaxed) {
            tracing::warn!("ssh bridge closed: {reason}");
        }
        self.sink.clone().close();
        // Fail every in-flight request instead of leaving callers hanging.
        let pending: Vec<_> = self.pending.lock().await.drain().collect();
        for (_, tx) in pending {
            let _ = tx.send((
                "{\"ok\":false,\"error\":\"ssh bridge closed\"}".to_string(),
                Vec::new(),
            ));
        }
    }

    async fn kill(&self) {
        self.mark_dead("torn down").await;
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.start_kill();
            // Reap the process so it cannot linger as a zombie.
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
    }

    /// Send one request and wait for its response.
    async fn request(&self, method_json: String, binary: Option<Vec<u8>>) -> Result<FramePayload> {
        self.request_with_timeout(method_json, binary, REQUEST_TIMEOUT)
            .await
    }

    async fn request_with_timeout(
        &self,
        method_json: String,
        binary: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<FramePayload> {
        if self.is_dead() {
            bail!("SSH bridge is not connected");
        }

        let payload = FramePayload {
            json: method_json,
            binary: binary.unwrap_or_default(),
        }
        .encode();
        if payload.len() > MAX_PAYLOAD_LEN as usize {
            bail!("request payload too large: {} bytes", payload.len());
        }

        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id, tx);

        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = write_frame(
                &mut *stdin,
                &FrameHeader::new(FrameKind::Request, request_id, 0, 0),
                &payload,
            )
            .await
            {
                self.pending.lock().await.remove(&request_id);
                self.mark_dead(&format!("write failed: {e}")).await;
                return Err(anyhow!(e).context("writing request to ssh bridge"));
            }
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok((json, binary))) => Ok(FramePayload { json, binary }),
            Ok(Err(_)) => bail!("ssh bridge closed before responding"),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                bail!("ssh bridge request timed out after {}s", timeout.as_secs())
            }
        }
    }

    /// Ping measured across the whole `host-agent → ssh → bridge` path.
    async fn ping(&self) -> Result<u64> {
        let started = Instant::now();
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id, tx);

        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = write_frame(
                &mut *stdin,
                &FrameHeader::new(FrameKind::Ping, request_id, 0, 0),
                &[],
            )
            .await
            {
                self.pending.lock().await.remove(&request_id);
                self.mark_dead(&format!("ping write failed: {e}")).await;
                return Err(anyhow!(e).context("writing ping"));
            }
        }

        match tokio::time::timeout(PING_TIMEOUT, rx).await {
            Ok(Ok(_)) => {
                let elapsed = started.elapsed().as_millis() as u64;
                self.latency_ms.store(elapsed, Ordering::Relaxed);
                Ok(elapsed)
            }
            Ok(Err(_)) => bail!("bridge closed during ping"),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                bail!("ping timed out after {}s", PING_TIMEOUT.as_secs())
            }
        }
    }
}

/// Reads frames from ssh stdout until EOF: resolves pending requests and
/// forwards push frames to subscribers.
async fn read_loop(connection: Arc<BridgeConnection>) {
    loop {
        let frame = {
            let mut stdout = connection.stdout.lock().await;
            read_frame(&mut *stdout).await
        };

        match frame {
            Ok((header, payload)) => {
                let decoded = FramePayload::decode(&payload).unwrap_or_default();
                match header.kind {
                    FrameKind::Response | FrameKind::Pong => {
                        let responder = connection.pending.lock().await.remove(&header.request_id);
                        if let Some(tx) = responder {
                            let _ = tx.send((decoded.json, decoded.binary));
                        }
                    }
                    FrameKind::Event | FrameKind::StreamData | FrameKind::StreamOpen
                    | FrameKind::StreamClose => {
                        let mut sink = connection.sink.clone();
                        sink.set_last_id(spark_json::get_u64(&decoded.json, "event_id").unwrap_or(0));
                        sink.push(RuntimeEvent {
                            runtime_id: spark_json::get_str(&decoded.json, "runtime_id")
                                .unwrap_or_default(),
                            kind: spark_json::get_str(&decoded.json, "kind")
                                .unwrap_or_else(|| format!("frame:{}", header.kind.name())),
                            at_ms: spark_json::get_u64(&decoded.json, "at_ms")
                                .unwrap_or_else(sand_protocol::now_ms),
                            machine_id: None,
                        });
                    }
                    FrameKind::Request => {
                        // The bridge never sends requests; ignore defensively.
                    }
                    FrameKind::Ping => { /* the bridge is the only pinger */ }
                }
            }
            Err(e) => {
                let stderr = connection.stderr_text().await;
                let reason = if stderr.trim().is_empty() {
                    e.to_string()
                } else {
                    format!("{e}: {}", stderr.trim())
                };
                connection.mark_dead(&reason).await;
                return;
            }
        }
    }
}

/// Drain ssh stderr into a shared buffer so failures can be classified later.
async fn stderr_loop(connection: Arc<BridgeConnection>, mut stderr: tokio::process::ChildStderr) {
    let mut buffer = [0u8; 4096];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut guard = connection.stderr.lock().await;
                if guard.len() < 64 * 1024 {
                    guard.push_str(&String::from_utf8_lossy(&buffer[..n]));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SshTransport
// ---------------------------------------------------------------------------

pub struct SshTransport {
    /// The machine record this transport was built from.
    machine: Machine,
    machine_id: MachineId,
    /// Live bridge, when connected.
    connection: AsyncMutex<Option<Arc<BridgeConnection>>>,
    connected: AtomicBool,
    /// Remote sandd socket path.
    socket: String,
    /// Remote install layout.
    paths: spark_model::SparkPaths,
    /// Push-frame sink for events.
    events: EventSink,
    /// True while the machine is in the middle of a reconnect attempt.
    reconnecting: AtomicBool,
}

impl SshTransport {
    pub fn new(machine: &Machine) -> Self {
        let paths = Machine::spark_paths();
        let socket = paths.socket.clone();
        Self {
            machine: machine.clone(),
            machine_id: machine.id.clone(),
            connection: AsyncMutex::new(None),
            connected: AtomicBool::new(false),
            socket,
            paths,
            events: EventSink::new(),
            reconnecting: AtomicBool::new(false),
        }
    }

    /// The SSH argument vector (without the remote command).
    pub fn ssh_args(&self) -> Result<Vec<String>> {
        self.machine
            .ssh_args()
            .ok_or_else(|| anyhow!("machine {} is not an SSH machine", self.machine.id))
    }

    /// `ssh_args` with extra options inserted *before* the destination, because
    /// `ssh` stops parsing options at the target.
    fn ssh_args_with(&self, extra: &[&str]) -> Result<Vec<String>> {
        let args = self.ssh_args()?;
        let mut out: Vec<String> = Vec::with_capacity(args.len() + extra.len());
        let mut inserted = false;
        for arg in args {
            if !inserted && arg == "--" {
                out.extend(extra.iter().map(|s| s.to_string()));
                inserted = true;
            }
            out.push(arg);
        }
        if !inserted {
            out.extend(extra.iter().map(|s| s.to_string()));
        }
        Ok(out)
    }

    /// Host key checking is always on; a user's `StrictHostKeyChecking=no` in
    /// `~/.ssh/config` cannot weaken us, because this option comes last and
    /// therefore wins. First-use confirmation happens through the host-key flow
    /// instead (architecture §十).
    fn ssh_command(&self, remote_command: &str) -> Result<Command> {
        let args = self.ssh_args_with(&["-o", "StrictHostKeyChecking=yes"])?;
        let mut command = Command::new("ssh");
        command
            .args(&args)
            .arg(remote_command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(command)
    }

    /// Command line for `ssh`, including the remote bridge command. Logged and
    /// shown in the UI so a user can reproduce the connection by hand.
    pub fn bridge_command(&self) -> Result<Vec<String>> {
        let mut args = self.ssh_args_with(&["-o", "StrictHostKeyChecking=yes"])?;
        args.push(Machine::bridge_remote_command(&self.socket));
        Ok(args)
    }

    pub fn socket_path(&self) -> &str {
        &self.socket
    }

    /// Target string for `scp` (`alias` or `user@host`).
    pub fn ssh_target(&self) -> Option<String> {
        self.machine.kind.ssh_target()
    }

    pub fn ssh_port(&self) -> u16 {
        self.machine.kind.ssh_port().unwrap_or(22)
    }

    /// Machine record this transport was created from.
    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    /// Remote install layout (`~/.local/share/spark`, `~/.cache/spark/run`).
    pub fn paths(&self) -> &spark_model::SparkPaths {
        &self.paths
    }

    /// Pre-flight used by "Test connection": `ssh ... true`, with failures
    /// classified so the UI can show the right prompt.
    pub async fn test_connection(&self) -> Result<(), ConnectError> {
        let args = self
            .ssh_args_with(&["-o", "StrictHostKeyChecking=yes"])
            .map_err(|e| ConnectError::Other(e.to_string()))?;
        let output = Command::new("ssh")
            .args(&args)
            .arg("true")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| ConnectError::Unreachable(format!("cannot run ssh: {e}")))?;

        if output.status.success() {
            return Ok(());
        }
        Err(self.classify_failure(&String::from_utf8_lossy(&output.stderr)))
    }

    /// Turn ssh stderr into a typed error, inspecting the host key when needed.
    fn classify_failure(&self, stderr: &str) -> ConnectError {
        let lower = stderr.to_lowercase();
        if lower.contains("permission denied")
            || lower.contains("no such identity")
            || lower.contains("too many authentication failures")
            || lower.contains("agent refused operation")
        {
            return ConnectError::AuthFailed(stderr.trim().to_string());
        }

        if let Some(severity) = hostkey::classify_ssh_stderr(stderr) {
            let (host, port) = self.host_port();
            return match hostkey::scan_host_key(&host, port) {
                Ok(issue) => match severity {
                    hostkey::Severity::Unknown => ConnectError::HostKeyUnknown(issue),
                    hostkey::Severity::Changed => {
                        ConnectError::HostKeyChanged(hostkey::changed_issue(&host, port, issue))
                    }
                },
                Err(scan_err) => ConnectError::Other(format!(
                    "{}; 另外无法读取主机密钥: {scan_err}",
                    stderr.trim()
                )),
            };
        }

        ConnectError::Unreachable(format!("ssh failed: {}", stderr.trim()))
    }

    fn host_port(&self) -> (String, u16) {
        match &self.machine.kind {
            spark_model::MachineKind::Ssh { host, port, .. } => (host.clone(), *port),
            _ => (self.machine.name.clone(), 22),
        }
    }

    /// Run one command over a *fresh* ssh process.
    ///
    /// Bootstrap genuinely needs this: the long-lived bridge talks to sandd, and
    /// sandd is exactly what does not exist yet. Everything after bootstrap goes
    /// through the bridge, which is what keeps "one ssh process per machine"
    /// true for the agent's whole lifetime.
    pub async fn ssh_exec(&self, remote_command: &str) -> Result<(i32, String, String)> {
        let args = self.ssh_args_with(&["-o", "StrictHostKeyChecking=yes"])?;
        let output = Command::new("ssh")
            .args(&args)
            .arg(remote_command)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("ssh {remote_command:?}"))?;
        Ok((
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }

    /// Best-effort teardown helper for a `scp` transfer.
    fn scp_command(&self, local: &std::path::Path, remote: &str) -> Result<Command> {
        let target = self
            .ssh_target()
            .ok_or_else(|| anyhow!("not an ssh machine"))?;
        let mut command = Command::new("scp");
        command
            .arg("-q")
            .arg("-o")
            .arg("StrictHostKeyChecking=yes");
        if self.machine.kind.ssh_port().is_some() {
            command.arg("-P").arg(self.ssh_port().to_string());
        }
        command
            .arg("--")
            .arg(local)
            .arg(format!("{target}:{remote}"));
        Ok(command)
    }

    /// Copy a local file to the machine and verify its SHA-256 remotely.
    ///
    /// `scp` is used **only** for bootstrap binaries and artifact transfer —
    /// never as the agent's filesystem API (architecture §十三/§二十二).
    pub async fn scp_upload(&self, local: &std::path::Path, remote: &str) -> Result<()> {
        let data = std::fs::read(local).with_context(|| format!("reading {}", local.display()))?;
        let expected = crate::hash::sha256_hex(&data);

        let output = self
            .scp_command(local, remote)?
            .output()
            .await
            .context("running scp (is OpenSSH installed?)")?;
        if !output.status.success() {
            bail!(
                "scp {} failed: {}",
                local.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        // The remote hash is the contract: if it does not match, bootstrap stops
        // here rather than executing a truncated binary.
        let (code, stdout, stderr) = self
            .ssh_exec(&format!("sha256sum {} | cut -d' ' -f1", shell_quote(remote)))
            .await?;
        if code != 0 {
            bail!("remote sha256 check failed: {}", stderr.trim());
        }
        let remote_hash = stdout.trim().to_lowercase();
        if remote_hash != expected {
            bail!("sha256 mismatch after upload to {remote}: local {expected}, remote {remote_hash}");
        }
        tracing::info!("uploaded {} → {} (sha256 verified)", local.display(), remote);
        Ok(())
    }

    /// Copy a file from the machine (artifacts, logs) with optional verification.
    pub async fn scp_download(&self, remote: &str, local: &std::path::Path) -> Result<()> {
        let target = self
            .ssh_target()
            .ok_or_else(|| anyhow!("not an ssh machine"))?;
        let mut command = Command::new("scp");
        command
            .arg("-q")
            .arg("-o")
            .arg("StrictHostKeyChecking=yes");
        if self.machine.kind.ssh_port().is_some() {
            command.arg("-P").arg(self.ssh_port().to_string());
        }
        let output = command
            .arg("--")
            .arg(format!("{target}:{remote}"))
            .arg(local)
            .output()
            .await
            .context("running scp")?;
        if !output.status.success() {
            bail!(
                "scp download failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// SSH into the machine, start the bridge, hand-shake with sandd.
    ///
    /// 1. `ssh ... true` pre-flight so host key / auth errors surface clearly;
    /// 2. start the long-lived `sand bridge` connection;
    /// 3. handshake (protocol version, sandd version, features, machine info);
    /// 4. if sandd is missing or too old, bootstrap once and retry.
    pub async fn connect_machine(&self, allow_bootstrap: bool) -> Result<HandshakeResponse, ConnectError> {
        if self.is_connected() {
            if let Ok(handshake) = self.handshake().await {
                if handshake.compatible {
                    return Ok(handshake);
                }
            }
        }

        // 1. pre-flight
        self.test_connection().await?;

        // 2-3. bridge + handshake
        match self.start_bridge().await {
            Ok(handshake) => Ok(handshake),
            Err(err) => {
                let bootstrappable = matches!(
                    err,
                    ConnectError::Bootstrap(_) | ConnectError::IncompatibleProtocol { .. }
                );
                if !allow_bootstrap || !bootstrappable {
                    return Err(err);
                }
                bootstrap::bootstrap(self)
                    .await
                    .map_err(|err| ConnectError::Bootstrap(err.to_string()))?;
                self.start_bridge().await
            }
        }
    }

    /// Start the framed bridge and complete the handshake.
    async fn start_bridge(&self) -> Result<HandshakeResponse, ConnectError> {
        let remote_command = Machine::bridge_remote_command(&self.socket);
        let mut command = self
            .ssh_command(&remote_command)
            .map_err(|e| ConnectError::Other(e.to_string()))?;

        let mut child = command
            .spawn()
            .map_err(|e| ConnectError::Unreachable(format!("cannot spawn ssh: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ConnectError::Other("ssh stdin unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ConnectError::Other("ssh stdout unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ConnectError::Other("ssh stderr unavailable".to_string()))?;

        let connection = Arc::new(BridgeConnection {
            child: AsyncMutex::new(Some(child)),
            stdin: AsyncMutex::new(stdin),
            stdout: AsyncMutex::new(stdout),
            next_request_id: AtomicU64::new(1),
            pending: AsyncMutex::new(HashMap::new()),
            sink: self.events.clone(),
            dead: AtomicBool::new(false),
            latency_ms: AtomicU64::new(0),
            stderr: Arc::new(AsyncMutex::new(String::new())),
        });

        tokio::spawn(read_loop(connection.clone()));
        tokio::spawn(stderr_loop(connection.clone(), stderr));

        {
            let mut guard = self.connection.lock().await;
            *guard = Some(connection.clone());
        }
        self.connected.store(true, Ordering::Relaxed);

        match self.handshake().await {
            Ok(handshake) if handshake.compatible => Ok(handshake),
            Ok(handshake) => {
                let remote = handshake.protocol_version;
                self.teardown().await;
                Err(ConnectError::IncompatibleProtocol {
                    client: SAND_PROTOCOL_VERSION,
                    remote,
                })
            }
            Err(e) => {
                // The bridge command prints a precise message when sandd is not
                // reachable, which is exactly the bootstrap trigger.
                let stderr = connection.stderr_text().await;
                self.teardown().await;
                let lowered = stderr.to_lowercase();
                if lowered.contains("sandd")
                    || lowered.contains("cannot connect")
                    || lowered.contains("no such file")
                    || lowered.contains("command not found")
                {
                    return Err(ConnectError::Bootstrap(format!(
                        "远端 sandd 不可用: {}",
                        stderr.trim()
                    )));
                }
                if let Some(issue) = self.classify_failure(&stderr).host_key_issue().cloned() {
                    return Err(match issue {
                        issue @ hostkey::HostKeyIssue::Unknown { .. } => {
                            ConnectError::HostKeyUnknown(issue)
                        }
                        issue @ hostkey::HostKeyIssue::Changed { .. } => {
                            ConnectError::HostKeyChanged(issue)
                        }
                    });
                }
                Err(ConnectError::Unreachable(format!("{e}")))
            }
        }
    }

    /// Close the bridge. Remote state is untouched by design — this is the
    /// method that must never destroy a runtime.
    pub async fn teardown(&self) {
        let connection = { self.connection.lock().await.take() };
        if let Some(connection) = connection {
            connection.kill().await;
        }
        self.connected.store(false, Ordering::Relaxed);
    }

    async fn current(&self) -> Result<Arc<BridgeConnection>> {
        let guard = self.connection.lock().await;
        let connection = guard
            .clone()
            .ok_or_else(|| anyhow!("machine {} is not connected via SSH", self.machine_id))?;
        if connection.is_dead() {
            bail!("machine {} ssh bridge is closed", self.machine_id);
        }
        Ok(connection)
    }

    /// Number of live ssh processes this transport owns (tests assert it is 0 or 1).
    pub fn ssh_process_count(&self) -> usize {
        match self.connection.try_lock() {
            Ok(guard) => guard.as_ref().map(|c| usize::from(!c.is_dead())).unwrap_or(0),
            Err(_) => 1,
        }
    }

    /// Last drain of pushed events (used right after a reconnect).
    pub fn drain_events(&self, runtime_id: Option<&str>) -> Vec<RuntimeEvent> {
        let mut sink = self.events.clone();
        sink.drain(runtime_id)
    }
}

/// Send a `HostKeyUnknown/Changed` decision: trust the key by writing it to
/// `known_hosts` (the user confirmed the fingerprint), then reconnect.
///
/// A *changed* key replaces the old entry instead of sitting next to it.
pub fn trust_host_key(issue: &hostkey::HostKeyIssue) -> Result<()> {
    let path = hostkey::known_hosts_path();
    if issue.is_high_risk() {
        hostkey::remove_host_key(&path, issue.host(), issue.port())?;
    }
    hostkey::trust_host_key(&path, issue.key_line())
}

// ---------------------------------------------------------------------------
// RuntimeTransport implementation
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl RuntimeTransport for SshTransport {
    fn machine_id(&self) -> MachineId {
        self.machine_id.clone()
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    async fn connect(&self) -> Result<()> {
        self.connect_machine(true)
            .await
            .map(|_| ())
            .map_err(|e| anyhow!(e.message()))
    }

    async fn disconnect(&self) -> Result<()> {
        self.teardown().await;
        Ok(())
    }

    async fn handshake(&self) -> Result<HandshakeResponse> {
        let connection = self.current().await?;
        let started = Instant::now();
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"Handshake\",\"protocol_version\":{}}}",
                    SAND_PROTOCOL_VERSION
                ),
                None,
            )
            .await
            .context("bridge handshake failed")?;
        if !spark_json::is_ok(&response.json) {
            bail!(
                "handshake rejected: {}",
                spark_json::error_of(&response.json).unwrap_or_else(|| response.json.clone())
            );
        }
        let latency = connection
            .latency()
            .unwrap_or_else(|| started.elapsed().as_millis() as u64);
        Ok(parse_handshake_json(
            &response.json,
            SAND_PROTOCOL_VERSION,
            Some(latency),
        ))
    }

    async fn ping(&self) -> Result<u64> {
        let connection = match self.current().await {
            Ok(connection) => connection,
            Err(e) => {
                self.connected.store(false, Ordering::Relaxed);
                return Err(e);
            }
        };
        match connection.ping().await {
            Ok(latency) => Ok(latency),
            Err(e) => {
                self.connected.store(false, Ordering::Relaxed);
                connection.mark_dead(&e.to_string()).await;
                Err(e)
            }
        }
    }

    async fn status(&self) -> Result<SandStatus> {
        if self.is_connected() {
            if let Ok(handshake) = self.handshake().await {
                if handshake.compatible {
                    return Ok(SandStatus {
                        connected: true,
                        sandd_version: Some(handshake.sandd_version.clone()),
                        protocol_version: Some(handshake.protocol_version),
                        protocol_compatible: true,
                        metadata: handshake.metadata(),
                        capabilities: handshake.capabilities(),
                        latency_ms: handshake.latency_ms,
                        error: None,
                    });
                }
            }
        }
        let error = match self.current().await {
            Ok(connection) => connection.stderr_text().await,
            Err(e) => e.to_string(),
        };
        Ok(SandStatus {
            connected: false,
            protocol_compatible: false,
            error: Some(if error.trim().is_empty() {
                "machine is not connected".to_string()
            } else {
                error.trim().to_string()
            }),
            ..Default::default()
        })
    }

    async fn create_runtime(&self, request: CreateRuntimeRequest) -> Result<RuntimeInfo> {
        let connection = self.current().await?;
        let machine_id = request
            .machine_id
            .clone()
            .unwrap_or_else(|| self.machine_id.clone());
        let json = format!(
            "{{\"method\":\"CreateRuntime\",\"kind\":\"{}\",\"workspace\":\"{}\",\"machine_id\":\"{}\"}}",
            spark_json::escape(&request.kind),
            spark_json::escape(&request.workspace.display().to_string()),
            spark_json::escape(machine_id.as_str())
        );
        let response = connection.request(json, None).await?;
        if !spark_json::is_ok(&response.json) {
            bail!(
                "create_runtime failed on {}: {}",
                self.machine_id,
                spark_json::error_of(&response.json).unwrap_or(response.json)
            );
        }
        Ok(RuntimeInfo {
            id: spark_json::get_str(&response.json, "id")
                .ok_or_else(|| anyhow!("create_runtime returned no id"))?,
            kind: spark_json::get_str(&response.json, "kind").unwrap_or(request.kind),
            state: spark_json::get_str(&response.json, "state").unwrap_or_else(|| "running".into()),
            workspace: PathBuf::from(
                spark_json::get_str(&response.json, "workspace")
                    .unwrap_or_else(|| request.workspace.display().to_string()),
            ),
            machine_id,
            capabilities: Vec::new(),
            process_count: 0,
            pty_count: 0,
        })
    }

    async fn destroy_runtime(&self, runtime_id: &str) -> Result<()> {
        let connection = self.current().await?;
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"DestroyRuntime\",\"id\":\"{}\"}}",
                    spark_json::escape(runtime_id)
                ),
                None,
            )
            .await?;
        if !spark_json::is_ok(&response.json) {
            bail!(
                "destroy_runtime failed: {}",
                spark_json::error_of(&response.json).unwrap_or(response.json)
            );
        }
        Ok(())
    }

    async fn list_runtimes(&self) -> Result<Vec<RuntimeInfo>> {
        let connection = self.current().await?;
        let response = connection
            .request("{\"method\":\"ListRuntimes\"}".to_string(), None)
            .await?;
        if !spark_json::is_ok(&response.json) {
            bail!(
                "list_runtimes failed: {}",
                spark_json::error_of(&response.json).unwrap_or(response.json)
            );
        }
        Ok(spark_json::get_object_array(&response.json, "runtimes")
            .iter()
            .map(|obj| runtime_info(obj, &self.machine_id))
            .collect())
    }

    async fn get_runtime(&self, runtime_id: &str) -> Result<RuntimeInfo> {
        let connection = self.current().await?;
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"GetRuntime\",\"id\":\"{}\"}}",
                    spark_json::escape(runtime_id)
                ),
                None,
            )
            .await?;
        if !spark_json::is_ok(&response.json) {
            bail!(
                "get_runtime failed: {}",
                spark_json::error_of(&response.json).unwrap_or(response.json)
            );
        }
        Ok(runtime_info(&response.json, &self.machine_id))
    }

    async fn exec(&self, request: ExecRequest) -> Result<ExecResult> {
        let connection = self.current().await?;
        let mut json = format!(
            "{{\"method\":\"Exec\",\"id\":\"{}\",\"command\":{}",
            spark_json::escape(&request.runtime_id),
            spark_json::str_array(&request.command)
        );
        if let Some(cwd) = &request.cwd {
            json.push_str(&format!(",\"cwd\":\"{}\"", spark_json::escape(cwd)));
        }
        if let Some(timeout) = request.timeout_ms {
            json.push_str(&format!(",\"timeout_ms\":{timeout}"));
        }
        if let Some(stdin) = &request.stdin_data {
            json.push_str(&format!(",\"binary_len\":{}", stdin.len()));
        }
        json.push('}');

        let response = connection.request(json, request.stdin_data.clone()).await?;
        if !spark_json::is_ok(&response.json) {
            bail!(
                "exec failed on {}: {}",
                self.machine_id,
                spark_json::error_of(&response.json).unwrap_or(response.json)
            );
        }

        // stdout and stderr share the binary section; `stdout_len` splits them.
        let stdout_len = spark_json::get_u64(&response.json, "stdout_len")
            .unwrap_or(response.binary.len() as u64) as usize;
        let stdout_len = stdout_len.min(response.binary.len());
        let stdout = response.binary[..stdout_len].to_vec();
        let stderr = response.binary[stdout_len..].to_vec();

        Ok(ExecResult {
            pid: spark_json::get_u64(&response.json, "pid").unwrap_or(0) as i32,
            exit_code: spark_json::get_u64(&response.json, "exit_code").map(|v| v as i32),
            signal: None,
            stdout,
            stderr,
            duration_ms: spark_json::get_u64(&response.json, "duration_ms").unwrap_or(0),
        })
    }

    async fn open_pty(&self, request: PtyOpenRequest) -> Result<()> {
        let connection = self.current().await?;
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"OpenPty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"cols\":{},\"rows\":{},\"shell\":\"{}\"}}",
                    spark_json::escape(&request.runtime_id),
                    spark_json::escape(&request.pty_id),
                    request.cols,
                    request.rows,
                    spark_json::escape(&request.shell)
                ),
                None,
            )
            .await?;
        expect_ok(&response.json, "open_pty")
    }

    async fn write_pty(&self, request: PtyWriteRequest) -> Result<()> {
        let connection = self.current().await?;
        // Keystrokes travel as binary: a PTY stream is not text, and base64
        // inside JSON would double the cost of every byte typed.
        let json = format!(
            "{{\"method\":\"WritePty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"binary_len\":{}}}",
            spark_json::escape(&request.runtime_id),
            spark_json::escape(&request.pty_id),
            request.data.len()
        );
        let response = connection.request(json, Some(request.data)).await?;
        expect_ok(&response.json, "write_pty")
    }

    async fn resize_pty(&self, request: PtyResizeRequest) -> Result<()> {
        let connection = self.current().await?;
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"ResizePty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"cols\":{},\"rows\":{}}}",
                    spark_json::escape(&request.runtime_id),
                    spark_json::escape(&request.pty_id),
                    request.cols,
                    request.rows
                ),
                None,
            )
            .await?;
        expect_ok(&response.json, "resize_pty")
    }

    async fn signal_pty(&self, request: PtySignalRequest) -> Result<()> {
        let connection = self.current().await?;
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"SignalPty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"signal\":{}}}",
                    spark_json::escape(&request.runtime_id),
                    spark_json::escape(&request.pty_id),
                    request.signal
                ),
                None,
            )
            .await?;
        expect_ok(&response.json, "signal_pty")
    }

    async fn read_pty(&self, request: PtyReadRequest) -> Result<PtyReadResponse> {
        let connection = self.current().await?;
        let json = format!(
            "{{\"method\":\"ReadPty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"clear\":{}}}",
            spark_json::escape(&request.runtime_id),
            spark_json::escape(&request.pty_id),
            request.clear
        );
        let response = connection.request(json, None).await?;
        if !spark_json::is_ok(&response.json) {
            bail!("read_pty failed: {}", response.json);
        }
        let cursor = spark_json::get_u64(&response.json, "len");
        Ok(PtyReadResponse {
            data: response.binary,
            cursor,
        })
    }

    async fn close_pty(&self, runtime_id: &str, pty_id: &str) -> Result<()> {
        let connection = self.current().await?;
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"ClosePty\",\"id\":\"{}\",\"pty_id\":\"{}\"}}",
                    spark_json::escape(runtime_id),
                    spark_json::escape(pty_id)
                ),
                None,
            )
            .await?;
        expect_ok(&response.json, "close_pty")
    }

    async fn list_ptys(&self, runtime_id: &str) -> Result<Vec<String>> {
        let connection = self.current().await?;
        let response = connection
            .request(
                format!(
                    "{{\"method\":\"ListPtys\",\"id\":\"{}\"}}",
                    spark_json::escape(runtime_id)
                ),
                None,
            )
            .await?;
        if !spark_json::is_ok(&response.json) {
            bail!("list_ptys failed: {}", response.json);
        }
        Ok(spark_json::get_object_array(&response.json, "ptys")
            .iter()
            .filter_map(|obj| spark_json::get_str(obj, "pty_id"))
            .collect())
    }

    async fn fs_request(&self, runtime_id: &str, request: FsRequest) -> Result<FsResponse> {
        let connection = self.current().await?;
        let binary = fs_request_binary(&request);
        let json = fs_request_json(runtime_id, &request);
        let response = connection.request(json, binary).await?;

        if !spark_json::is_ok(&response.json) {
            return Ok(FsResponse::Error {
                message: fs_error(&response.json),
            });
        }

        match request {
            FsRequest::Read { .. } => Ok(FsResponse::Ok {
                data: response.binary,
            }),
            FsRequest::Write { .. } => Ok(FsResponse::Ok { data: Vec::new() }),
            // Listing-shaped responses carry their payload in `data`; hand the
            // caller exactly that JSON value (local transport does the same, so
            // tool output cannot differ between machines).
            _ => Ok(FsResponse::Ok {
                data: spark_json::raw_field(&response.json, "data")
                    .unwrap_or(response.json)
                    .into_bytes(),
            }),
        }
    }

    async fn browser_request(&self, request: BrowserRequest) -> Result<BrowserResponse> {
        crate::browser::dispatch(self, request).await
    }

    async fn computer_request(&self, request: ComputerRequest) -> Result<ComputerResponse> {
        crate::computer::dispatch(self, request).await
    }

    async fn subscribe_events(&self, runtime_id: Option<&str>) -> Result<BoxStream<RuntimeEvent>> {
        // Remote events are pushed as frames by the bridge; the sink is where
        // the reader loop puts them.
        Ok(crate::events::subscribe_sink(&self.events, runtime_id))
    }

    async fn machine_exec(&self, command: Vec<String>) -> Result<ExecResult> {
        // Diagnostics/bootstrap only — runs over a one-shot ssh invocation
        // because it must work when sandd is not running yet.
        let joined = command
            .iter()
            .map(|part| shell_quote(part))
            .collect::<Vec<_>>()
            .join(" ");
        let started = Instant::now();
        let (code, stdout, stderr) = self.ssh_exec(&joined).await?;
        Ok(ExecResult {
            pid: 0,
            exit_code: Some(code),
            signal: None,
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    async fn upload_file(&self, request: FileTransferRequest) -> Result<()> {
        self.scp_upload(&request.local_path, &request.remote_path).await
    }

    async fn download_file(&self, request: FileTransferRequest) -> Result<Vec<u8>> {
        // Preferred path: `cat` over a one-shot ssh, because artifacts may live
        // outside any runtime workspace (where FsRead's sandbox does not apply).
        let (code, stdout, stderr) = self
            .ssh_exec(&format!("cat -- {}", shell_quote(&request.remote_path)))
            .await?;
        if code != 0 {
            bail!("download failed: {}", stderr.trim());
        }
        let data = stdout.into_bytes();
        if let Some(expected) = &request.sha256 {
            crate::hash::verify_sha256(&data, expected)?;
        }
        Ok(data)
    }

    fn describe(&self) -> String {
        match self.machine.kind.ssh_target() {
            Some(target) => format!("ssh {target} (machine {})", self.machine_id),
            None => format!("machine {}", self.machine_id),
        }
    }
}

// ---------------------------------------------------------------------------
// Raw bridge access (browser/computer helpers)
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl RawRpc for SshTransport {
    async fn rpc_raw(&self, method_json: String, binary: Option<Vec<u8>>) -> Result<RpcPayload> {
        let connection = self.current().await?;
        connection.request(method_json, binary).await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expect_ok(json: &str, what: &str) -> Result<()> {
    if spark_json::is_ok(json) {
        return Ok(());
    }
    bail!(
        "{} failed: {}",
        what,
        spark_json::error_of(json).unwrap_or_else(|| json.to_string())
    )
}

fn fs_error(json: &str) -> String {
    let message = spark_json::error_of(json).unwrap_or_else(|| json.to_string());
    if spark_json::get_bool(json, "security").unwrap_or(false) {
        format!("SECURITY BLOCKED: {message}")
    } else {
        message
    }
}

fn runtime_info(json: &str, fallback_machine: &MachineId) -> RuntimeInfo {
    RuntimeInfo {
        id: spark_json::get_str(json, "id").unwrap_or_default(),
        kind: spark_json::get_str(json, "kind").unwrap_or_default(),
        state: spark_json::get_str(json, "state").unwrap_or_default(),
        workspace: PathBuf::from(spark_json::get_str(json, "workspace").unwrap_or_default()),
        machine_id: spark_json::get_str(json, "machine_id")
            .map(MachineId::from_string)
            .unwrap_or_else(|| fallback_machine.clone()),
        capabilities: Vec::new(),
        process_count: spark_json::get_u64(json, "procs").unwrap_or(0) as usize,
        pty_count: spark_json::get_u64(json, "ptys").unwrap_or(0) as usize,
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        // `kill_on_drop` on the child does the real cleanup; this makes the
        // intent explicit: dropping a transport closes the bridge, and closing
        // the bridge never touches remote runtimes.
        if let Ok(mut guard) = self.connection.try_lock() {
            if let Some(connection) = guard.take() {
                connection.dead.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh_machine() -> Machine {
        Machine::ssh(
            MachineId::from_string("mach-test".to_string()),
            "devbox".to_string(),
            "10.0.0.42".to_string(),
            2222,
            Some("dev".to_string()),
            None,
        )
    }

    #[test]
    fn builds_ssh_argument_vector() {
        let transport = SshTransport::new(&ssh_machine());
        let args = transport.ssh_args().expect("ssh args");
        assert_eq!(
            args,
            vec![
                "-T".to_string(),
                "-p".to_string(),
                "2222".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "--".to_string(),
                "dev@10.0.0.42".to_string(),
            ]
        );
    }

    #[test]
    fn extra_options_go_before_the_destination() {
        let transport = SshTransport::new(&ssh_machine());
        let args = transport
            .ssh_args_with(&["-o", "StrictHostKeyChecking=yes"])
            .unwrap();
        let separator = args.iter().position(|a| a == "--").expect("separator");
        let option = args
            .iter()
            .position(|a| a == "StrictHostKeyChecking=yes")
            .expect("option present");
        assert!(
            option < separator,
            "ssh stops parsing options at the destination: {args:?}"
        );
        assert_eq!(args.last().unwrap(), "dev@10.0.0.42");
    }

    #[test]
    fn bridge_command_never_disables_host_key_checking() {
        let transport = SshTransport::new(&ssh_machine());
        let joined = transport.bridge_command().expect("bridge command").join(" ");
        assert!(joined.contains("sand bridge --socket"));
        assert!(joined.contains("~/.cache/spark/run/sandd.sock"));
        assert!(joined.contains("StrictHostKeyChecking=yes"));
        assert!(!joined.contains("StrictHostKeyChecking=no"));
    }

    #[test]
    fn ssh_config_alias_wins_over_host_and_port() {
        let machine = Machine::ssh(
            MachineId::from_string("mach-alias".into()),
            "devbox".into(),
            "10.0.0.42".into(),
            2200,
            Some("dev".into()),
            Some("devbox".into()),
        );
        let transport = SshTransport::new(&machine);
        let args = transport.ssh_args().unwrap();
        // The alias carries user/port/ProxyJump; we must not also pass -p.
        assert_eq!(args.last().unwrap(), "devbox");
        assert!(!args.contains(&"-p".to_string()));
    }

    #[test]
    fn classifies_ssh_failures() {
        let transport = SshTransport::new(&ssh_machine());
        match transport.classify_failure("Permission denied (publickey).") {
            ConnectError::AuthFailed(_) => {}
            other => panic!("expected auth failure, got {other:?}"),
        }
        match transport.classify_failure("ssh: connect to host 10.0.0.42 port 2222: Connection refused") {
            ConnectError::Unreachable(_) => {}
            other => panic!("expected unreachable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exec_without_connection_errors_instead_of_pretending() {
        let transport = SshTransport::new(&ssh_machine());
        assert!(!transport.is_connected());
        let err = transport
            .exec(ExecRequest {
                runtime_id: "rt-x".into(),
                command: vec!["true".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: None,
                stdin_data: None,
            })
            .await
            .expect_err("must not pretend to execute");
        assert!(err.to_string().contains("not connected"), "got {err}");
    }

    #[tokio::test]
    async fn status_without_connection_is_honest() {
        let transport = SshTransport::new(&ssh_machine());
        let status = transport.status().await.expect("status");
        assert!(!status.connected);
        assert!(status.error.is_some());
    }

    #[tokio::test]
    async fn disconnect_does_not_destroy_anything() {
        // No connection: disconnect is a no-op, and crucially it must not try to
        // reach the machine to "clean up" runtimes.
        let transport = SshTransport::new(&ssh_machine());
        transport.disconnect().await.expect("disconnect");
        assert_eq!(transport.ssh_process_count(), 0);
    }

    #[test]
    fn connect_error_classification_drives_machine_status() {
        let unreachable = ConnectError::Unreachable("Connection timed out".into());
        assert_eq!(unreachable.status(), MachineStatus::Unreachable);
        assert!(unreachable.is_retryable());

        let auth = ConnectError::AuthFailed("Permission denied (publickey)".into());
        assert_eq!(auth.status(), MachineStatus::RequiresUserAction);
        assert!(
            !auth.is_retryable(),
            "an auth failure must not spin forever"
        );

        let incompatible = ConnectError::IncompatibleProtocol {
            client: 2,
            remote: 1,
        };
        assert_eq!(incompatible.status(), MachineStatus::Error);
        assert!(incompatible.message().contains("不兼容"));

        let bootstrap = ConnectError::Bootstrap("sandd missing".into());
        assert_eq!(bootstrap.status(), MachineStatus::Error);
        assert!(bootstrap.is_retryable(), "bootstrap issues can be retried");
    }

    #[test]
    fn shell_quoting_survives_spaces_and_quotes() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
