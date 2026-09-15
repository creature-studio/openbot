//! LocalTransport — the machine host-agent runs on.
//!
//! Behaviour is exactly the same as a remote machine from the caller's point of
//! view: everything goes through sandd (JSON socket for control, framed socket
//! for bulk data). There is deliberately **no** direct `std::fs`/`Command` path
//! here — if the local transport could bypass sandd, local and remote runtimes
//! would drift (architecture §三十一 / Phase R1).
//!
//! Socket discovery matches `sand-client`: `/run/sand` when it exists, else
//! `/tmp/sandd`, plus `SAND_SOCKET_DIR` for bootstrapped/test setups.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use crate::runtime_transport::BoxStream;

use sand_client::SandClient;
use sand_protocol::SAND_PROTOCOL_VERSION;
// The transport speaks `spark_model::MachineId`: it is the registry key that
// MachineManager, persistence and the UI all use. `sand_protocol::MachineId`
// stays inside the sand workspace, where sandd writes it on the wire.
use spark_model::MachineId;

use crate::runtime_transport::*;

pub struct LocalTransport {
    machine_id: MachineId,
    socket: PathBuf,
    binary_socket: PathBuf,
    connected: AtomicBool,
    last_latency_ms: std::sync::atomic::AtomicU64,
}

impl LocalTransport {
    /// Transport bound to the local sandd.
    pub fn new() -> Self {
        Self::with_socket_dir(None)
    }

    /// Transport bound to a sandd whose sockets live in `dir`
    /// (`sandd.sock` + `sandd-binary.sock`).
    pub fn with_socket_dir(dir: Option<PathBuf>) -> Self {
        let socket_dir = dir.unwrap_or_else(default_socket_dir);
        Self {
            // host-agent registers the local machine under the well known
            // `machine-local` id; the host fingerprint sandd reports is machine
            // *metadata*, not the registry key.
            machine_id: MachineId::local(),
            socket: socket_dir.join("sandd.sock"),
            binary_socket: socket_dir.join("sandd-binary.sock"),
            connected: AtomicBool::new(false),
            last_latency_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Local sockets live in one of three places, in priority order.
    pub fn default_socket_paths() -> (PathBuf, PathBuf) {
        let dir = default_socket_dir();
        (dir.join("sandd.sock"), dir.join("sandd-binary.sock"))
    }

    /// Override the machine id (used by tests that run two sandds side by side).
    pub fn with_machine_id(mut self, machine_id: MachineId) -> Self {
        self.machine_id = machine_id;
        self
    }

    fn client(&self) -> SandClient {
        SandClient::new_with_paths(self.socket.clone(), self.binary_socket.clone())
    }

    /// One request over the framed socket, returning JSON + raw bytes.
    fn rpc_binary(&self, request: &str, binary: Option<&[u8]>) -> Result<(String, Vec<u8>)> {
        let client = self.client();
        client
            .call_binary(request, binary)
            .with_context(|| format!("sandd call failed: {}", request))
    }

    /// One request over the JSON socket.
    fn rpc(&self, request: &str) -> Result<String> {
        let client = self.client();
        client.call(request).context("sandd call failed")
    }

    fn latency(&self) -> Option<u64> {
        let value = self.last_latency_ms.load(Ordering::Relaxed);
        if value == 0 {
            None
        } else {
            Some(value)
        }
    }
}

impl Default for LocalTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// `/run/sand` when writable, else `$SAND_SOCKET_DIR`, else `/tmp/sandd`.
pub fn default_socket_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAND_SOCKET_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if Path::new("/run/sand/sandd.sock").exists() {
        return PathBuf::from("/run/sand");
    }
    if Path::new("/run/sand").exists() {
        return PathBuf::from("/run/sand");
    }
    PathBuf::from("/tmp/sandd")
}

#[async_trait::async_trait]
impl RuntimeTransport for LocalTransport {
    fn machine_id(&self) -> MachineId {
        self.machine_id.clone()
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    async fn connect(&self) -> Result<()> {
        // A connection is just "can we get a status back".
        let status = self.status().await?;
        if !status.connected {
            bail!(
                "local sandd unreachable at {}: {}",
                self.socket.display(),
                status.error.unwrap_or_else(|| "unknown error".to_string())
            );
        }
        self.connected.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        // Nothing to tear down: local sandd is a machine level daemon and must
        // keep its runtimes alive.
        self.connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    async fn handshake(&self) -> Result<HandshakeResponse> {
        let start = Instant::now();
        let response = self
            .rpc(&format!(
                "{{\"method\":\"Handshake\",\"protocol_version\":{}}}",
                SAND_PROTOCOL_VERSION
            ))
            .context("local sandd handshake failed")?;
        let latency = start.elapsed().as_millis() as u64;
        if !spark_json::is_ok(&response) {
            bail!(
                "handshake rejected: {}",
                spark_json::error_of(&response).unwrap_or_default()
            );
        }
        self.last_latency_ms.store(latency, Ordering::Relaxed);
        self.connected.store(true, Ordering::Relaxed);
        Ok(parse_handshake_json(&response, SAND_PROTOCOL_VERSION, Some(latency)))
    }

    async fn ping(&self) -> Result<u64> {
        let start = Instant::now();
        let response = self.rpc("{\"method\":\"Ping\"}")?;
        if !spark_json::is_ok(&response) {
            self.connected.store(false, Ordering::Relaxed);
            bail!("ping failed: {}", response);
        }
        let latency = start.elapsed().as_millis() as u64;
        self.last_latency_ms.store(latency, Ordering::Relaxed);
        Ok(latency)
    }

    async fn status(&self) -> Result<SandStatus> {
        match self.rpc(&format!(
            "{{\"method\":\"Handshake\",\"protocol_version\":{}}}",
            SAND_PROTOCOL_VERSION
        )) {
            Ok(response) if spark_json::is_ok(&response) => {
                let handshake = parse_handshake_json(&response, SAND_PROTOCOL_VERSION, self.latency());
                self.connected.store(true, Ordering::Relaxed);
                Ok(SandStatus {
                    connected: true,
                    sandd_version: Some(handshake.sandd_version.clone()),
                    protocol_version: Some(handshake.protocol_version),
                    protocol_compatible: handshake.compatible,
                    metadata: handshake.metadata(),
                    capabilities: handshake.capabilities(),
                    latency_ms: handshake.latency_ms,
                    error: None,
                })
            }
            Ok(response) => {
                self.connected.store(false, Ordering::Relaxed);
                Ok(SandStatus {
                    connected: false,
                    protocol_compatible: false,
                    error: Some(
                        spark_json::error_of(&response)
                            .unwrap_or_else(|| response.trim().to_string()),
                    ),
                    ..Default::default()
                })
            }
            Err(e) => {
                self.connected.store(false, Ordering::Relaxed);
                Ok(SandStatus {
                    connected: false,
                    protocol_compatible: false,
                    error: Some(format!("{e:#}")),
                    ..Default::default()
                })
            }
        }
    }

    async fn create_runtime(&self, request: CreateRuntimeRequest) -> Result<RuntimeInfo> {
        let workspace = request.workspace.display().to_string();
        let machine_id = request
            .machine_id
            .clone()
            .unwrap_or_else(|| self.machine_id.clone());
        let mut json = format!(
            "{{\"method\":\"CreateRuntime\",\"kind\":\"{}\",\"workspace\":\"{}\",\"machine_id\":\"{}\"",
            spark_json::escape(&request.kind),
            spark_json::escape(&workspace),
            spark_json::escape(machine_id.as_str()),
        );
        if let Some(runtime_id) = &request.runtime_id {
            json.push_str(&format!(
                ",\"runtime_id\":\"{}\"",
                spark_json::escape(runtime_id)
            ));
        }
        json.push('}');

        let response = self.rpc(&json)?;
        if !spark_json::is_ok(&response) {
            bail!(
                "create_runtime failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }
        self.connected.store(true, Ordering::Relaxed);
        Ok(RuntimeInfo {
            id: spark_json::get_str(&response, "id").ok_or_else(|| anyhow!("no id returned"))?,
            kind: spark_json::get_str(&response, "kind").unwrap_or(request.kind),
            state: spark_json::get_str(&response, "state").unwrap_or_else(|| "running".to_string()),
            workspace: PathBuf::from(
                spark_json::get_str(&response, "workspace").unwrap_or(workspace),
            ),
            machine_id,
            capabilities: vec![],
            process_count: 0,
            pty_count: 0,
        })
    }

    async fn destroy_runtime(&self, runtime_id: &str) -> Result<()> {
        let response = self.rpc(&format!(
            "{{\"method\":\"DestroyRuntime\",\"id\":\"{}\"}}",
            spark_json::escape(runtime_id)
        ))?;
        if !spark_json::is_ok(&response) {
            bail!(
                "destroy_runtime failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }
        Ok(())
    }

    async fn list_runtimes(&self) -> Result<Vec<RuntimeInfo>> {
        let response = self.rpc("{\"method\":\"ListRuntimes\"}")?;
        if !spark_json::is_ok(&response) {
            bail!(
                "list_runtimes failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }
        Ok(spark_json::get_object_array(&response, "runtimes")
            .iter()
            .map(|obj| runtime_info_from_json(obj, &self.machine_id))
            .collect())
    }

    async fn get_runtime(&self, runtime_id: &str) -> Result<RuntimeInfo> {
        let response = self.rpc(&format!(
            "{{\"method\":\"GetRuntime\",\"id\":\"{}\"}}",
            spark_json::escape(runtime_id)
        ))?;
        if !spark_json::is_ok(&response) {
            bail!(
                "get_runtime failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }
        Ok(runtime_info_from_json(&response, &self.machine_id))
    }

    async fn exec(&self, request: ExecRequest) -> Result<ExecResult> {
        let command = spark_json::str_array(&request.command);
        let mut json = format!(
            "{{\"method\":\"Exec\",\"id\":\"{}\",\"command\":{}",
            spark_json::escape(&request.runtime_id),
            command
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

        let start = Instant::now();
        let (response, binary) = self.rpc_binary(&json, request.stdin_data.as_deref())?;
        if !spark_json::is_ok(&response) {
            bail!(
                "exec failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }

        let stdout_len = spark_json::get_u64(&response, "stdout_len").unwrap_or(0) as usize;
        let stderr_len = spark_json::get_u64(&response, "stderr_len").unwrap_or(0) as usize;
        let stdout = binary.get(..stdout_len).unwrap_or(&binary).to_vec();
        let stderr = binary
            .get(stdout_len..stdout_len + stderr_len)
            .unwrap_or_default()
            .to_vec();

        Ok(ExecResult {
            pid: spark_json::get_u64(&response, "pid").unwrap_or(0) as i32,
            exit_code: spark_json::get_u64(&response, "exit_code").map(|v| v as i32),
            signal: None,
            stdout,
            stderr,
            duration_ms: spark_json::get_u64(&response, "duration_ms")
                .unwrap_or_else(|| start.elapsed().as_millis() as u64),
        })
    }

    async fn open_pty(&self, request: PtyOpenRequest) -> Result<()> {
        let response = self.rpc(&format!(
            "{{\"method\":\"OpenPty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"cols\":{},\"rows\":{},\"shell\":\"{}\"}}",
            spark_json::escape(&request.runtime_id),
            spark_json::escape(&request.pty_id),
            request.cols,
            request.rows,
            spark_json::escape(&request.shell),
        ))?;
        if !spark_json::is_ok(&response) {
            bail!(
                "open_pty failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }
        Ok(())
    }

    async fn write_pty(&self, request: PtyWriteRequest) -> Result<()> {
        let json = format!(
            "{{\"method\":\"WritePty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"binary_len\":{}}}",
            spark_json::escape(&request.runtime_id),
            spark_json::escape(&request.pty_id),
            request.data.len()
        );
        let (response, _) = self.rpc_binary(&json, Some(&request.data))?;
        if !spark_json::is_ok(&response) {
            bail!(
                "write_pty failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }
        Ok(())
    }

    async fn resize_pty(&self, request: PtyResizeRequest) -> Result<()> {
        let response = self.rpc(&format!(
            "{{\"method\":\"ResizePty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"cols\":{},\"rows\":{}}}",
            spark_json::escape(&request.runtime_id),
            spark_json::escape(&request.pty_id),
            request.cols,
            request.rows
        ))?;
        if !spark_json::is_ok(&response) {
            bail!("resize_pty failed: {}", response);
        }
        Ok(())
    }

    async fn signal_pty(&self, request: PtySignalRequest) -> Result<()> {
        let response = self.rpc(&format!(
            "{{\"method\":\"SignalPty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"signal\":{}}}",
            spark_json::escape(&request.runtime_id),
            spark_json::escape(&request.pty_id),
            request.signal
        ))?;
        if !spark_json::is_ok(&response) {
            bail!("signal_pty failed: {}", response);
        }
        Ok(())
    }

    async fn read_pty(&self, request: PtyReadRequest) -> Result<PtyReadResponse> {
        let json = format!(
            "{{\"method\":\"ReadPty\",\"id\":\"{}\",\"pty_id\":\"{}\",\"clear\":{}}}",
            spark_json::escape(&request.runtime_id),
            spark_json::escape(&request.pty_id),
            request.clear
        );
        let (response, data) = self.rpc_binary(&json, None)?;
        if !spark_json::is_ok(&response) {
            bail!(
                "read_pty failed: {}",
                spark_json::error_of(&response).unwrap_or(response)
            );
        }
        Ok(PtyReadResponse {
            data,
            cursor: spark_json::get_u64(&response, "len"),
        })
    }

    async fn close_pty(&self, runtime_id: &str, pty_id: &str) -> Result<()> {
        let response = self.rpc(&format!(
            "{{\"method\":\"ClosePty\",\"id\":\"{}\",\"pty_id\":\"{}\"}}",
            spark_json::escape(runtime_id),
            spark_json::escape(pty_id)
        ))?;
        if !spark_json::is_ok(&response) {
            bail!("close_pty failed: {}", response);
        }
        Ok(())
    }

    async fn list_ptys(&self, runtime_id: &str) -> Result<Vec<String>> {
        let response = self.rpc(&format!(
            "{{\"method\":\"ListPtys\",\"id\":\"{}\"}}",
            spark_json::escape(runtime_id)
        ))?;
        if !spark_json::is_ok(&response) {
            bail!("list_ptys failed: {}", response);
        }
        Ok(spark_json::get_object_array(&response, "ptys")
            .iter()
            .filter_map(|obj| spark_json::get_str(obj, "pty_id"))
            .collect())
    }

    async fn fs_request(&self, runtime_id: &str, request: FsRequest) -> Result<FsResponse> {
        // One serializer for both transports: identical requests on the wire
        // whether the runtime is local or on an SSH machine.
        let json = fs_request_json(runtime_id, &request);
        let binary = fs_request_binary(&request);

        let (response, data) = if binary.is_some() {
            self.rpc_binary(&json, binary.as_deref())?
        } else {
            let response = self.rpc(&json)?;
            (response, Vec::new())
        };

        if !spark_json::is_ok(&response) {
            return Ok(FsResponse::Error {
                message: fs_error_message(&response),
            });
        }

        match request {
            // File bytes come back on the framed socket.
            FsRequest::Read { .. } => Ok(FsResponse::Ok { data }),
            FsRequest::Write { .. } => Ok(FsResponse::Ok { data: Vec::new() }),
            // Listing-shaped ops return their payload in `data`; handing the raw
            // JSON value to the caller keeps remote and local output identical.
            _ => Ok(FsResponse::Ok {
                data: spark_json::raw_field(&response, "data")
                    .unwrap_or(response)
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
        // Local events are polled from sandd's ring buffer with an `event_id`
        // cursor, so a client that was away catches up instead of losing them.
        let socket = self.socket.clone();
        let binary_socket = self.binary_socket.clone();
        Ok(crate::events::poll_events_with(
            move |body| {
                let socket = socket.clone();
                let binary_socket = binary_socket.clone();
                async move {
                    let client = SandClient::new_with_paths(socket, binary_socket);
                    client
                        .call(&body)
                        .map_err(|e| anyhow!("list events failed: {e}"))
                }
            },
            runtime_id.map(|s| s.to_string()),
            0,
        ))
    }

    async fn machine_exec(&self, command: Vec<String>) -> Result<ExecResult> {
        // Diagnostics/bootstrap only. Agent tools never reach this: they need a
        // runtime, and a runtime is what this bypasses.
        let start = Instant::now();
        let (program, args) = command
            .split_first()
            .ok_or_else(|| anyhow!("empty command"))?;
        let output = tokio::process::Command::new(program)
            .args(args)
            .output()
            .await
            .with_context(|| format!("running {program}"))?;
        Ok(ExecResult {
            pid: output_pid(&output),
            exit_code: output.status.code(),
            signal: None,
            stdout: output.stdout,
            stderr: output.stderr,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    async fn upload_file(&self, request: FileTransferRequest) -> Result<()> {
        // Local: a plain copy, still verified when a hash is supplied.
        let data = std::fs::read(&request.remote_path)
            .with_context(|| format!("read {}", request.remote_path))?;
        if let Some(expected) = &request.sha256 {
            crate::hash::verify_sha256(&data, expected)
                .with_context(|| format!("sha256 mismatch for {}", request.remote_path))?;
        }
        std::fs::write(&request.local_path, data)
            .with_context(|| format!("write {}", request.local_path.display()))
    }

    async fn download_file(&self, request: FileTransferRequest) -> Result<Vec<u8>> {
        // For the local machine the "remote path" is just a path.
        let data = std::fs::read(&request.remote_path)
            .with_context(|| format!("read {}", request.remote_path))?;
        if let Some(expected) = &request.sha256 {
            crate::hash::verify_sha256(&data, expected)?;
        }
        Ok(data)
    }

    fn describe(&self) -> String {
        format!("local sandd at {}", self.socket.display())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn runtime_info_from_json(json: &str, fallback_machine: &MachineId) -> RuntimeInfo {
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

fn fs_error_message(response: &str) -> String {
    let message = spark_json::error_of(response).unwrap_or_else(|| response.to_string());
    if spark_json::get_bool(response, "security").unwrap_or(false) {
        format!("SECURITY BLOCKED: {message}")
    } else {
        message
    }
}

// ---------------------------------------------------------------------------
// Raw bridge access (browser/computer helpers)
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl RawRpc for LocalTransport {
    async fn rpc_raw(&self, method_json: String, binary: Option<Vec<u8>>) -> Result<RpcPayload> {
        // Same socket, same framing as sandd expects. Browser and computer
        // helpers therefore need no local special case.
        let (json, data) = self.rpc_binary(&method_json, binary.as_deref())?;
        Ok(RpcPayload {
            json,
            binary: data,
        })
    }
}

#[cfg(unix)]
fn output_pid(output: &std::process::Output) -> i32 {
    let _ = output;
    0
}

#[cfg(not(unix))]
fn output_pid(_output: &std::process::Output) -> i32 {
    0
}
