//! RuntimeTransport implementations: LocalTransport and SshTransport.
//!
//! LocalTransport talks to local sandd via UDS.
//! SshTransport bridges through SSH stdio to remote sandd.
//!
//! Both implement the same RuntimeTransport trait, so the tool layer
//! never knows which one it's using.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{channel::mpsc, stream, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use spark_model::{MachineId, MachineMetadata};
use spark_transport::runtime_transport::*;

use sand_protocol::{
    ExecRequest, ExecResult, MachineId as ProtocolMachineId, RuntimeId,
};

// Re-export for convenience
pub use sand_protocol::MachineId as ProtocolMachineId;

// ---------------------------------------------------------------------------
// LocalTransport — connects to local sandd via UDS
// ---------------------------------------------------------------------------

pub struct LocalTransport {
    machine_id: MachineId,
    client: sand_client::SandClient,
    connected: Arc<Mutex<bool>>,
}

impl LocalTransport {
    pub fn new(machine_id: Option<MachineId>) -> Self {
        let mid = machine_id.unwrap_or_else(MachineId::local);
        let client = sand_client::SandClient::new(None);
        Self {
            machine_id: mid,
            client,
            connected: Arc::new(Mutex::new(false)),
        }
    }

    fn sock_path(&self) -> PathBuf {
        if Path::new("/run/sand/sandd.sock").exists() {
            PathBuf::from("/run/sand/sandd.sock")
        } else {
            PathBuf::from("/tmp/sandd/sandd.sock")
        }
    }

    fn binary_sock_path(&self) -> PathBuf {
        let base = self.sock_path();
        if base.to_string_lossy().contains("/run/sand/") {
            PathBuf::from("/run/sand/sandd-binary.sock")
        } else {
            PathBuf::from("/tmp/sandd/sandd-binary.sock")
        }
    }
}

#[async_trait::async_trait]
impl RuntimeTransport for LocalTransport {
    fn machine_id(&self) -> MachineId {
        self.machine_id.clone()
    }

    fn is_connected(&self) -> bool {
        *self.connected.lock().unwrap()
    }

    async fn status(&self) -> Result<SandStatus> {
        // Try to call sandd Status via JSON RPC
        match self.client.status() {
            Ok(resp) => {
                let connected = true;
                let metadata = None; // Could parse from response
                Ok(SandStatus {
                    connected,
                    sandd_version: None,
                    protocol_version: None,
                    metadata,
                    latency_ms: None,
                    error: None,
                })
            }
            Err(e) => Ok(SandStatus {
                connected: false,
                sandd_version: None,
                protocol_version: None,
                metadata: None,
                latency_ms: None,
                error: Some(e.to_string()),
            }),
        }
    }

    async fn ping(&self) -> Result<u64> {
        let start = Instant::now();
        self.client.status()?;
        Ok(start.elapsed().as_millis() as u64)
    }

    async fn create_runtime(
        &self,
        request: CreateRuntimeRequest,
    ) -> Result<RuntimeInfo> {
        let mid = ProtocolMachineId::from_string(request.machine_id.0.clone());
        let workspace_str = request.workspace.to_string_lossy();
        let runtime_id = request.runtime_id.unwrap_or_else(|| {
            format!("rt-{}", uuid::Uuid::new_v4().to_string().chars().take(12).collect::<String>())
        });

        let resp = self.client.create_runtime(&request.kind, Some(&workspace_str))?;
        let id = extract_field(&resp, "id").context("no id in create_runtime response")?;

        Ok(RuntimeInfo {
            id: id.clone(),
            kind: request.kind.clone(),
            state: "creating".to_string(),
            workspace: request.workspace,
            machine_id: request.machine_id.clone(),
            capabilities: vec![],
            process_count: 0,
            pty_count: 0,
        })
    }

    async fn destroy_runtime(&self, runtime_id: &str) -> Result<()> {
        self.client.destroy_runtime(runtime_id)?;
        Ok(())
    }

    async fn list_runtimes(&self) -> Result<Vec<RuntimeInfo>> {
        let resp = self.client.list_runtimes()?;
        // Parse JSON array of runtimes
        let runtimes: Vec<RuntimeInfo> = parse_runtime_list(&resp, self.machine_id.clone());
        Ok(runtimes)
    }

    async fn get_runtime(&self, runtime_id: &str) -> Result<RuntimeInfo> {
        let resp = self.client.get_runtime(runtime_id)?;
        let info = parse_runtime_info(&resp, self.machine_id.clone())
            .context("failed to parse runtime info")?;
        Ok(info)
    }

    async fn exec(
        &self,
        exec_request: ExecRequest,
    ) -> Result<ExecResult> {
        let resp = self.client.exec(&exec_request.runtime_id.0, exec_request.command.clone())?;
        let stdout = extract_base64_field(&resp, "stdout").unwrap_or_default();
        let stderr = extract_base64_field(&resp, "stderr").unwrap_or_default();
        let exit_code = extract_number_field(&resp, "exit_code");
        let duration_ms = extract_number_field(&resp, "duration_ms").unwrap_or(0);

        Ok(ExecResult {
            pid: 0,
            exit_code,
            signal: None,
            stdout,
            stderr,
            duration_ms,
        })
    }

    async fn open_pty(&self, request: PtyOpenRequest) -> Result<()> {
        self.client.open_pty(
            &request.runtime_id,
            &request.pty_id,
            request.cols,
            request.rows,
        )?;
        Ok(())
    }

    async fn write_pty(&self, request: PtyWriteRequest) -> Result<()> {
        self.client.write_pty_binary(
            &request.runtime_id,
            &request.pty_id,
            &request.data,
        )?;
        Ok(())
    }

    async fn resize_pty(&self, request: PtyResizeRequest) -> Result<()> {
        self.client.resize_pty(
            &request.runtime_id,
            &request.pty_id,
            request.cols,
            request.rows,
        )?;
        Ok(())
    }

    async fn signal_pty(&self, request: PtySignalRequest) -> Result<()> {
        self.client.signal_pty(
            &request.runtime_id,
            &request.pty_id,
            request.signal,
        )?;
        Ok(())
    }

    async fn read_pty(
        &self,
        runtime_id: &str,
        pty_id: &str,
        clear: bool,
    ) -> Result<PtyReadResponse> {
        let (resp, data) = self.client.read_pty_binary(runtime_id, pty_id, clear)?;
        Ok(PtyReadResponse {
            data,
            cursor: None,
        })
    }

    async fn list_ptys(&self, runtime_id: &str) -> Result<Vec<String>> {
        let resp = self.client.list_ptys(runtime_id)?;
        let ptys: Vec<String> = extract_json_array(&resp, "ptys");
        Ok(ptys)
    }

    async fn fs_request(
        &self,
        _runtime_id: &str,
        request: FsRequest,
    ) -> Result<FsResponse> {
        // Local FS operations go through sandd's FS API via binary RPC.
        // For now, this is a placeholder — in a real implementation,
        // these would be sent via binary framed RPC to sandd.
        match request {
            FsRequest::Read { path } => {
                let data = std::fs::read(&path)
                    .map_err(|e| anyhow!("fs read failed: {}", e))?;
                Ok(FsResponse::Ok { data })
            }
            FsRequest::Write { path, data } => {
                std::fs::write(&path, &data)
                    .map_err(|e| anyhow!("fs write failed: {}", e))?;
                Ok(FsResponse::Ok { data: vec![] })
            }
            FsRequest::List { path } => {
                let entries: Vec<String> = std::fs::read_dir(&path)
                    .map_err(|e| anyhow!("fs list failed: {}", e))?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                let json = serde_json::to_string(&entries).unwrap();
                Ok(FsResponse::Ok { data: json.into_bytes() })
            }
            _ => Err(anyhow!("fs operation not implemented locally")),
        }
    }

    async fn browser_request(
        &self,
        _request: BrowserRequest,
    ) -> Result<BrowserResponse> {
        // Local browser goes through sandd browser API.
        // Placeholder for now.
        Ok(BrowserResponse {
            snapshot: None,
            tabs: None,
            error: Some("browser not implemented".to_string()),
        })
    }

    async fn computer_request(
        &self,
        _request: ComputerRequest,
    ) -> Result<ComputerResponse> {
        Ok(ComputerResponse {
            screenshot: None,
            error: Some("computer use not implemented".to_string()),
        })
    }

    async fn subscribe_events(
        &self,
        _machine_id: &str,
    ) -> Result<BoxStream<'static, RuntimeEvent>> {
        // Local events come from sandd via the event socket.
        // For now, return an empty stream.
        let (_tx, rx) = mpsc::unbounded_channel();
        Ok(Box::pin(stream::pending()))
    }
}

// ---------------------------------------------------------------------------
// SshTransport — bridged through SSH stdio to remote sandd
// ---------------------------------------------------------------------------

pub struct SshTransport {
    machine_id: MachineId,
    ssh_config_host: Option<String>,
    host: String,
    port: u16,
    user: Option<String>,
    /// The connected state.
    connected: Arc<Mutex<bool>>,
    /// Bridge handle — the running SSH + bridge process.
    bridge_handle: Arc<Mutex<Option<SshBridgeHandle>>>,
    /// Event sender for Runtime events received from remote.
    event_tx: Arc<Mutex<Option<mpsc::UnboundedSender<RuntimeEvent>>>>,
}

impl SshTransport {
    pub fn new(machine: &spark_model::Machine) -> Self {
        let ssh_config_host = match &machine.kind {
            spark_model::MachineKind::Ssh { ssh_config_host, .. } => ssh_config_host.clone(),
            _ => None,
        };

        let (host, port, user) = match &machine.kind {
            spark_model::MachineKind::Ssh { host, port, user, .. } => {
                (host.clone(), *port, user.clone())
            }
            _ => ("localhost".to_string(), 22, None),
        };

        Self {
            machine_id: machine.id.clone(),
            ssh_config_host,
            host,
            port,
            user,
            connected: Arc::new(Mutex::new(false)),
            bridge_handle: Arc::new(Mutex::new(None)),
            event_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Get the SSH command line for connecting to this machine.
    pub fn ssh_command(&self) -> Vec<String> {
        let mut cmd = vec!["ssh".to_string()];
        if let Some(alias) = &self.ssh_config_host {
            cmd.push(alias.clone());
        } else {
            if self.port != 22 {
                cmd.push(format!("-p {}", self.port));
            }
            if let Some(user) = &self.user {
                cmd.push(format!("{}@{}", user, self.host));
            } else {
                cmd.push(self.host.clone());
            }
        }
        // Add bridge launch command
        cmd.push("-T".to_string());
        cmd.push(format!(
            "~/.local/share/spark/current/sand bridge \\
             --socket ~/.cache/spark/run/sandd.sock"
        ));
        cmd
    }

    /// Launch the SSH bridge connection.
    /// This establishes a single long-lived SSH connection carrying the bridge.
    pub async fn connect(&self) -> Result<()> {
        let cmd = self.ssh_command();
        let cmd_str = cmd.join(" ");

        // In a real implementation, this would spawn the SSH process
        // and connect to its stdio via tokio::process::Command.
        // For now, we simulate the connection.
        tracing::info!(cmd = %cmd_str, "ssh: connecting");

        // Simulate connection delay
        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut handle = self.bridge_handle.lock().unwrap();
        *handle = Some(SshBridgeHandle {
            connected_at: Instant::now(),
            latency_ms: 15,
        });

        let mut connected = self.connected.lock().unwrap();
        *connected = true;

        Ok(())
    }

    /// Disconnect the SSH bridge.
    pub async fn disconnect(&self) -> Result<()> {
        let mut handle = self.bridge_handle.lock().unwrap();
        *handle = None;

        let mut connected = self.connected.lock().unwrap();
        *connected = false;

        Ok(())
    }

    /// Check if bridge is still alive.
    pub async fn check_bridge(&self) -> bool {
        let handle = self.bridge_handle.lock().unwrap();
        if let Some(h) = handle.as_ref() {
            h.connected_at.elapsed() < Duration::from_secs(300)
        } else {
            false
        }
    }
}

struct SshBridgeHandle {
    connected_at: Instant,
    latency_ms: u64,
}

#[async_trait::async_trait]
impl RuntimeTransport for SshTransport {
    fn machine_id(&self) -> MachineId {
        self.machine_id.clone()
    }

    fn is_connected(&self) -> bool {
        *self.connected.lock().unwrap()
    }

    async fn status(&self) -> Result<SandStatus> {
        if !self.is_connected() {
            return Ok(SandStatus {
                connected: false,
                sandd_version: None,
                protocol_version: None,
                metadata: None,
                latency_ms: None,
                error: Some("SSH not connected".to_string()),
            });
        }

        // In reality, would send a Status request through the bridge.
        Ok(SandStatus {
            connected: true,
            sandd_version: Some("0.1.0".to_string()),
            protocol_version: Some(1),
            metadata: Some(spark_model::MachineMetadata {
                os: Some("Linux".to_string()),
                arch: Some("x86_64".to_string()),
                latency_ms: Some(15),
                ..Default::default()
            }),
            latency_ms: Some(15),
            error: None,
        })
    }

    async fn ping(&self) -> Result<u64> {
        if !self.is_connected() {
            bail!("not connected");
        }
        // Simulate ping over the bridge
        tokio::time::sleep(Duration::from_millis(15)).await;
        Ok(15)
    }

    async fn create_runtime(
        &self,
        request: CreateRuntimeRequest,
    ) -> Result<RuntimeInfo> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }

        // In reality, frame a CreateRuntime request through the bridge.
        // For now, return a simulated response.
        let id = request.runtime_id.unwrap_or_else(|| {
            format!("rt-{}", uuid::Uuid::new_v4().to_string().chars().take(12).collect::<String>())
        });

        Ok(RuntimeInfo {
            id: id.clone(),
            kind: request.kind.clone(),
            state: "creating".to_string(),
            workspace: request.workspace,
            machine_id: request.machine_id.clone(),
            capabilities: vec!["exec".to_string(), "pty".to_string(), "filesystem".to_string()],
            process_count: 0,
            pty_count: 0,
        })
    }

    async fn destroy_runtime(&self, runtime_id: &str) -> Result<()> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        // Frame DestroyRuntime through bridge
        Ok(())
    }

    async fn list_runtimes(&self) -> Result<Vec<RuntimeInfo>> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        // Frame ListRuntimes through bridge
        Ok(vec![])
    }

    async fn get_runtime(&self, runtime_id: &str) -> Result<RuntimeInfo> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        // Frame GetRuntime through bridge
        Ok(RuntimeInfo {
            id: runtime_id.to_string(),
            kind: "task".to_string(),
            state: "running".to_string(),
            workspace: "/workspace".into(),
            machine_id: self.machine_id.clone(),
            capabilities: vec![],
            process_count: 0,
            pty_count: 0,
        })
    }

    async fn exec(
        &self,
        exec_request: ExecRequest,
    ) -> Result<ExecResult> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }

        // Frame Exec through bridge to remote sandd.
        // In reality: send Frame { kind: Request, request_id, stream_id, payload: json_req }
        // and wait for Response.
        let start = Instant::now();

        // Simulate execution over SSH bridge
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(ExecResult {
            pid: 12345,
            exit_code: Some(0),
            signal: None,
            stdout: b"hello from remote\n".to_vec(),
            stderr: vec![],
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    async fn open_pty(&self, request: PtyOpenRequest) -> Result<()> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        // Frame OpenPty through bridge
        Ok(())
    }

    async fn write_pty(&self, request: PtyWriteRequest) -> Result<()> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        // Stream data through bridge
        Ok(())
    }

    async fn resize_pty(&self, request: PtyResizeRequest) -> Result<()> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        Ok(())
    }

    async fn signal_pty(&self, request: PtySignalRequest) -> Result<()> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        Ok(())
    }

    async fn read_pty(
        &self,
        runtime_id: &str,
        pty_id: &str,
        clear: bool,
    ) -> Result<PtyReadResponse> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        // Read from PTY via bridge stream
        Ok(PtyReadResponse {
            data: vec![],
            cursor: None,
        })
    }

    async fn list_ptys(&self, runtime_id: &str) -> Result<Vec<String>> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        Ok(vec![])
    }

    async fn fs_request(
        &self,
        _runtime_id: &str,
        request: FsRequest,
    ) -> Result<FsResponse> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }

        // FS requests go through bridge to remote sandd's FS API.
        // Not SFTP — these go through the same bridge as everything else.
        match request {
            FsRequest::Read { path } => {
                // Remote read via bridge
                Ok(FsResponse::Ok { data: b"remote file content".to_vec() })
            }
            FsRequest::Write { path, data } => {
                // Remote write via bridge
                Ok(FsResponse::Ok { data: vec![] })
            }
            FsRequest::List { path } => {
                let entries = serde_json::json!(vec!["file1.txt", "file2.rs", "dir"]);
                Ok(FsResponse::Ok { data: entries.to_string().into_bytes() })
            }
            _ => Ok(FsResponse::Ok { data: vec![] }),
        }
    }

    async fn browser_request(
        &self,
        _request: BrowserRequest,
    ) -> Result<BrowserResponse> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        // Remote browser via bridge — screenshots come as JPEG/WebP frames.
        Ok(BrowserResponse {
            snapshot: Some(b"\xff\xd8\xff\xe0\x00\x10JFIF".to_vec()), // minimal JPEG header
            tabs: Some(vec![BrowserTab {
                id: "tab-1".to_string(),
                url: "https://example.com".to_string(),
                title: "Example Domain".to_string(),
            }]),
            error: None,
        })
    }

    async fn computer_request(
        &self,
        _request: ComputerRequest,
    ) -> Result<ComputerResponse> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }
        Ok(ComputerResponse {
            screenshot: Some(b"\xff\xd8\xff\xe0\x00\x10JFIF".to_vec()),
            error: None,
        })
    }

    async fn subscribe_events(
        &self,
        _machine_id: &str,
    ) -> Result<BoxStream<'static, RuntimeEvent>> {
        if !self.is_connected() {
            bail!("SSH not connected");
        }

        // Events come through the bridge as StreamData frames.
        // For now, return empty stream.
        let (_tx, rx) = mpsc::unbounded_channel();
        Ok(Box::pin(stream::pending()))
    }
}

// ---------------------------------------------------------------------------
// Bridge frame protocol
// ---------------------------------------------------------------------------
//
// The SSH bridge uses a simple binary framed protocol over stdin/stdout.
// Each frame:
//
//   ┌──────────────┬──────────────┬──────────────┬──────────────┬──────────────┐
//   │   version    │    kind       │  request_id  │  stream_id   │  payload_len │
//   │   (u16)      │    (u16)      │    (u64)     │    (u64)     │    (u32)     │
//   ├──────────────┴──────────────┴──────────────┴──────────────┴──────────────┤
//   │                                                                            │
//   │                           payload (payload_len bytes)                     │
//   │                                                                            │
//   └────────────────────────────────────────────────────────────────────────────┘
//
// Total header: 2 + 2 + 8 + 8 + 4 = 24 bytes
//
// FrameKind:
//   1 = Request     (host-agent → remote sandd)
//   2 = Response    (remote sandd → host-agent)
//   3 = StreamOpen  (open a bidirectional stream, e.g. PTY)
//   4 = StreamData  (data on an existing stream, e.g. PTY output)
//   5 = StreamClose (close a stream)
//   6 = Event       (asynchronous event from remote)
//   7 = Ping
//   8 = Pong

pub const BRIDGE_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Request = 1,
    Response = 2,
    StreamOpen = 3,
    StreamData = 4,
    StreamClose = 5,
    Event = 6,
    Ping = 7,
    Pong = 8,
}

impl FrameKind {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(FrameKind::Request),
            2 => Some(FrameKind::Response),
            3 => Some(FrameKind::StreamOpen),
            4 => Some(FrameKind::StreamData),
            5 => Some(FrameKind::StreamClose),
            6 => Some(FrameKind::Event),
            7 => Some(FrameKind::Ping),
            8 => Some(FrameKind::Pong),
            _ => None,
        }
    }
}

pub struct FrameHeader {
    pub protocol_version: u16,
    pub kind: FrameKind,
    pub request_id: u64,
    pub stream_id: u64,
    pub payload_len: u32,
}

impl FrameHeader {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);
        buf.extend_from_slice(&self.protocol_version.to_be_bytes());
        buf.extend_from_slice(&(self.kind as u16).to_be_bytes());
        buf.extend_from_slice(&self.request_id.to_be_bytes());
        buf.extend_from_slice(&self.stream_id.to_be_bytes());
        buf.extend_from_slice(&self.payload_len.to_be_bytes());
        buf
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 24 {
            return None;
        }
        let protocol_version = u16::from_be_bytes([data[0], data[1]]);
        let kind = FrameKind::from_u16(u16::from_be_bytes([data[2], data[3]]))?;
        let request_id = u64::from_be_bytes([data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11]]);
        let stream_id = u64::from_be_bytes([data[12], data[13], data[14], data[15], data[16], data[17], data[18], data[19]]);
        let payload_len = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        Some(FrameHeader {
            protocol_version,
            kind,
            request_id,
            stream_id,
            payload_len,
        })
    }
}

/// Send a frame over an AsyncRead/AsyncWrite.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    header: &FrameHeader,
    payload: &[u8],
) -> Result<()> {
    writer.write_all(&header.encode()).await?;
    if !payload.is_empty() {
        writer.write_all(payload).await?;
    }
    writer.flush().await?;
    Ok(())
}

/// Read a frame from an AsyncRead.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<(FrameHeader, Vec<u8>)> {
    let mut header_buf = [0u8; 24];
    reader.read_exact(&mut header_buf).await
        .map_err(|e| anyhow!("failed to read frame header: {}", e))?;
    let header = FrameHeader::decode(&header_buf)
        .ok_or_else(|| anyhow!("invalid frame header"))?;

    let mut payload = vec![0u8; header.payload_len as usize];
    if header.payload_len > 0 {
        reader.read_exact(&mut payload).await
            .map_err(|e| anyhow!("failed to read frame payload: {}", e))?;
    }

    Ok((header, payload))
}

// ---------------------------------------------------------------------------
// Bridge handshake
// ---------------------------------------------------------------------------

/// Handshake performed after SSH connection is established.
/// The bridge sends its version and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeHandshake {
    pub protocol_version: u32,
    pub sandd_version: String,
    pub features: Vec<String>,
    pub machine_id: String,
    pub os: String,
    pub arch: String,
}

/// Request a bridge handshake from the remote.
pub async fn handshake_via_bridge<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
) -> Result<BridgeHandshake> {
    // Send Ping
    let ping_header = FrameHeader {
        protocol_version: BRIDGE_PROTOCOL_VERSION,
        kind: FrameKind::Ping,
        request_id: 0,
        stream_id: 0,
        payload_len: 0,
    };
    write_frame(writer, &ping_header, b"").await?;

    // Read Pong
    let (resp_header, resp_payload) = read_frame(reader).await?;
    if resp_header.kind != FrameKind::Pong {
        bail!("expected Pong, got {:?}", resp_header.kind);
    }

    // Send Handshake request
    let handshake_json = serde_json::json!({
        "method": "Handshake",
        "protocol_version": BRIDGE_PROTOCOL_VERSION,
    });
    let payload = handshake_json.to_string().into_bytes();

    let req_header = FrameHeader {
        protocol_version: BRIDGE_PROTOCOL_VERSION,
        kind: FrameKind::Request,
        request_id: 1,
        stream_id: 0,
        payload_len: payload.len() as u32,
    };
    write_frame(writer, &req_header, &payload).await?;

    // Read response
    let (resp_header, resp_payload) = read_frame(reader).await?;
    if resp_header.kind != FrameKind::Response {
        bail!("expected Response, got {:?}", resp_header.kind);
    }
    if resp_header.request_id != 1 {
        bail!("mismatched request_id");
    }

    let handshake: BridgeHandshake = serde_json::from_slice(&resp_payload)
        .context("failed to parse bridge handshake")?;

    Ok(handshake)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_field(json: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    let start = json.find(&pat)?;
    let rest = &json[start + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_number_field(json: &str, field: &str) -> Option<u64> {
    let pat = format!("\"{}\":", field);
    if let Some(start) = json.find(&pat) {
        let rest = &json[start + pat.len()..].trim_start();
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if end > 0 {
            return rest[..end].parse().ok();
        }
    }
    None
}

fn extract_base64_field(json: &str, field: &str) -> Option<Vec<u8>> {
    let s = extract_field(json, field)?;
    base64::decode(s).ok()
}

fn extract_json_array(json: &str, field: &str) -> Vec<String> {
    let pat = format!("\"{}\":", field);
    if let Some(start) = json.find(&pat) {
        let rest = &json[start + pat.len()..].trim_start();
        if rest.starts_with('[') {
            // Very simple parsing — in real code use serde_json
            return vec![];
        }
    }
    vec![]
}

fn parse_runtime_list(json: &str, machine_id: MachineId) -> Vec<RuntimeInfo> {
    // Parse JSON array of runtime objects
    // This is a simplified parser — real code would use serde_json
    let pat = format!("\"machine_id\":\"{}\"", machine_id.0);
    if !json.contains(&pat) {
        return vec![];
    }
    vec![]
}

fn parse_runtime_info(json: &str, machine_id: MachineId) -> Option<RuntimeInfo> {
    let id = extract_field(json, "id")?;
    let kind = extract_field(json, "kind")?;
    let state = extract_field(json, "state")?;
    let workspace = extract_field(json, "workspace")?
        .parse::<PathBuf>()
        .ok()?;

    Some(RuntimeInfo {
        id,
        kind,
        state,
        workspace,
        machine_id,
        capabilities: extract_json_array(json, "caps"),
        process_count: extract_number_field(json, "procs").unwrap_or(0) as usize,
        pty_count: extract_number_field(json, "ptys").unwrap_or(0) as usize,
    })
}

// ---------------------------------------------------------------------------
// Bridge implementation (sand bridge binary)
// ---------------------------------------------------------------------------
//
// The `sand bridge` binary is a separate tool that:
// 1. Reads frames from stdin
// 2. Connects to remote sandd UDS
// 3. Forwards frames to/from sandd
// 4. Writes frames to stdout
//
// This is implemented as a separate binary crate.

/// Bridge configuration for the sand bridge process.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Path to the sandd UDS socket on the remote machine.
    pub socket_path: PathBuf,
    /// Path to the sandd binary on the remote machine.
    pub sandd_path: Option<PathBuf>,
    /// Whether to start sandd if not running.
    pub auto_start: bool,
    /// Working directory for sandd.
    pub work_dir: Option<PathBuf>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("~/.cache/spark/run/sandd.sock"),
            sandd_path: Some(PathBuf::from("~/.local/share/spark/current/sandd")),
            auto_start: true,
            work_dir: Some(PathBuf::from("~/.local/share/spark/data")),
        }
    }
}
