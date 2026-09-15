//! MachineManager: manages local and remote (SSH) machines.
//!
//! This is the central coordinator for machine lifecycle:
//! - Add/remove machines
//! - Connect/disconnect (SSH bridge for remote, UDS for local)
//! - Bootstrap remote machines (detect platform, install sandd, start sandd)
//! - Route Runtime operations to the correct transport
//! - Periodic health pings
//!
//! The key invariant: tools NEVER know whether a machine is local or remote.
//! They call through the RuntimeTransport trait, and MachineManager dispatches.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use tokio::sync::Mutex;
use tokio::time::interval;

use sand_protocol::{MachineId, RuntimeId};
use spark_model::{Machine, MachineStatus, MachineCapabilities, MachineMetadata};
use spark_transport::runtime_transport::{
    RuntimeTransport, LocalTransport, SshTransport,
    CreateRuntimeRequest, SandStatus, RuntimeInfo,
    RuntimeEvent, BridgeConfig, BridgeHandshake,
};

// ---------------------------------------------------------------------------
// MachineHandle — per-machine state
// ---------------------------------------------------------------------------

struct MachineHandle {
    /// The Machine record.
    machine: Machine,
    /// The active transport (LocalTransport or SshTransport).
    transport: Option<Arc<dyn RuntimeTransport>>,
    /// Whether we're currently trying to reconnect.
    reconnect_attempt: Arc<Mutex<u32>>,
    /// Last status update time.
    last_status_at: Arc<Mutex<Option<Instant>>>,
}

impl MachineHandle {
    fn new(machine: Machine) -> Self {
        let transport = match &machine.kind {
            spark_model::MachineKind::Local => {
                Some(Arc::new(LocalTransport::new(Some(MachineId::local()))))
            }
            spark_model::MachineKind::Ssh { .. } => {
                let ssh = SshTransport::new(&machine);
                Some(Arc::new(ssh))
            }
        };

        Self {
            machine,
            transport,
            reconnect_attempt: Arc::new(Mutex::new(0)),
            last_status_at: Arc::new(Mutex::new(None)),
        }
    }

    fn machine_id(&self) -> MachineId {
        self.machine.id.clone()
    }
}

// ---------------------------------------------------------------------------
// MachineManager
// ---------------------------------------------------------------------------

pub struct MachineManager {
    /// All registered machines.
    machines: HashMap<MachineId, MachineHandle>,
    /// All active transport event streams.
    event_streams: Mutex<Vec<(MachineId, tokio_stream::StreamExt<RuntimeEvent>)>>,
    /// Shutdown signal.
    shutdown: Mutex<bool>,
}

impl MachineManager {
    pub fn new() -> Self {
        // Start with the local machine
        let local = Machine::local(Some("Local".to_string()));
        let mut machines = HashMap::new();
        machines.insert(local.id.clone(), MachineHandle::new(local));

        Self {
            machines,
            event_streams: Mutex::new(vec![]),
            shutdown: Mutex::new(false),
        }
    }

    // -----------------------------------------------------------------------
    // Machine registration
    // -----------------------------------------------------------------------

    /// Add a new machine (local or SSH).
    pub async fn add_machine(&self, machine: Machine) -> Result<()> {
        let id = machine.id.clone();

        if self.machines.contains_key(&id) {
            bail!("machine already exists: {}", id);
        }

        let handle = MachineHandle::new(machine);
        self.machines.insert(id.clone(), handle);

        // Auto-connect if local, or if SSH with ssh_config_host
        let handle = self.machines.get(&id).unwrap();
        if handle.machine.kind.is_local() {
            // Local is always "connected" — it uses UDS directly.
            let _ = self.update_machine_status(&id, MachineStatus::Connected).await;
        } else {
            // Try to connect
            let _ = self.connect_machine(&id).await;
        }

        tracing::info!(machine_id = %id, "machine added");
        Ok(())
    }

    /// Remove a machine and disconnect if connected.
    pub async fn remove_machine(&self, machine_id: &MachineId) -> Result<()> {
        if let Some(handle) = self.machines.get(machine_id) {
            // Disconnect if connected
            if let Some(transport) = handle.transport.as_ref() {
                if transport.is_connected() {
                    // For SSH, disconnect the bridge
                    if let Some(ssh) = transport.downcast_ref::<SshTransport>() {
                        let _ = ssh.disconnect().await;
                    }
                }
            }
        }

        self.machines.remove(machine_id);
        tracing::info!(machine_id = %machine_id, "machine removed");
        Ok(())
    }

    /// Get a machine by ID.
    pub fn get_machine(&self, machine_id: &MachineId) -> Option<&Machine> {
        self.machines.get(machine_id).map(|h| &h.machine)
    }

    /// Get all machines.
    pub fn list_machines(&self) -> Vec<Machine> {
        self.machines.values().map(|h| h.machine.clone()).collect()
    }

    // -----------------------------------------------------------------------
    // Connection management
    // -----------------------------------------------------------------------

    /// Connect to a machine (establish SSH bridge for remote, verify UDS for local).
    pub async fn connect_machine(&self, machine_id: &MachineId) -> Result<()> {
        let handle = self.machines.get(machine_id)
            .ok_or_else(|| anyhow!("machine not found: {}", machine_id))?;

        // Update status to connecting
        self.update_machine_status(machine_id, MachineStatus::Connecting).await;

        match &handle.machine.kind {
            spark_model::MachineKind::Local => {
                // Local is always available — just check UDS
                let transport = handle.transport.as_ref().unwrap();
                let status = transport.status().await?;
                if status.connected {
                    self.update_machine_status(machine_id, MachineStatus::Connected).await;
                } else {
                    self.update_machine_status(machine_id, MachineStatus::Error).await;
                    bail!("local sandd not reachable");
                }
            }
            spark_model::MachineKind::Ssh { .. } => {
                // For SSH, the transport is SshTransport
                let transport = handle.transport.as_ref().unwrap();

                // Try to connect via SSH
                match transport.connect().await {
                    Ok(()) => {
                        // After SSH connect, perform handshake via bridge
                        match self.perform_handshake(machine_id).await {
                            Ok(handshake) => {
                                // Update machine metadata from handshake
                                self.update_machine_from_handshake(machine_id, &handshake).await;
                                self.update_machine_status(machine_id, MachineStatus::Connected).await;
                                tracing::info!(machine_id = %machine_id, "machine connected via SSH");
                            }
                            Err(e) => {
                                tracing::warn!(machine_id = %machine_id, error = %e, "SSH connected but handshake failed");
                                self.update_machine_status(machine_id, MachineStatus::Bootstrapping).await;
                                // Try to bootstrap
                                let _ = self.bootstrap_machine(machine_id).await;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(machine_id = %machine_id, error = %e, "SSH connection failed");
                        self.update_machine_status(machine_id, MachineStatus::Unreachable).await;
                        bail!("SSH connection failed: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Disconnect from a machine.
    pub async fn disconnect_machine(&self, machine_id: &MachineId) -> Result<()> {
        let handle = self.machines.get(machine_id)
            .ok_or_else(|| anyhow!("machine not found: {}", machine_id))?;

        if let Some(transport) = handle.transport.as_ref() {
            if let Some(ssh) = transport.downcast_ref::<SshTransport>() {
                let _ = ssh.disconnect().await;
            }
        }

        self.update_machine_status(machine_id, MachineStatus::Disconnected).await;
        Ok(())
    }

    /// Reconnect to a machine (with backoff).
    pub async fn reconnect_machine(&self, machine_id: &MachineId) -> Result<()> {
        let mut attempt = self.machines.get(machine_id)
            .ok_or_else(|| anyhow!("machine not found"))?
            .reconnect_attempt.lock().unwrap();
        *attempt += 1;

        // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s, 30s...
        let delay = match *attempt {
            1 => Duration::from_secs(1),
            2 => Duration::from_secs(2),
            3 => Duration::from_secs(4),
            4 => Duration::from_secs(8),
            5 => Duration::from_secs(16),
            _ => Duration::from_secs(30),
        };

        tracing::info!(machine_id = %machine_id, attempt = *attempt, "reconnecting in {:?}", delay);
        tokio::time::sleep(delay).await;

        // Check if we should abort — not for auth errors
        let handle = self.machines.get(machine_id).unwrap();
        let status = handle.machine.status.clone();
        if matches!(status, MachineStatus::Error) {
            // Auth errors need user action — don't reconnect automatically
            bail!("machine in Error state, manual intervention required");
        }

        self.connect_machine(machine_id).await
    }

    // -----------------------------------------------------------------------
    // Bootstrap (remote machines)
    // -----------------------------------------------------------------------

    /// Bootstrap a remote machine: detect platform, install sandd, start sandd.
    async fn bootstrap_machine(&self, machine_id: &MachineId) -> Result<()> {
        let handle = self.machines.get(machine_id)
            .ok_or_else(|| anyhow!("machine not found"))?;

        self.update_machine_status(machine_id, MachineStatus::Bootstrapping).await;

        // Step 1: Detect platform
        let platform = self.detect_platform(machine_id).await?;
        tracing::info!(machine_id = %machine_id, platform = %platform, "detected platform");

        // Step 2: Check sandd version
        match self.check_sandd_version(machine_id).await {
            Ok(version) => {
                tracing::info!(machine_id = %machine_id, version = %version, "sandd already installed");
                // sandd exists, just connect bridge
                let _ = self.connect_machine(machine_id).await;
                return Ok(());
            }
            Err(_) => {
                // sandd not found, need to install
            }
        }

        // Step 3: Install sandd (scp the binary)
        let arch = platform.arch.clone();
        let binary_path = self.download_and_upload_binary(machine_id, &arch).await?;
        tracing::info!(machine_id = %machine_id, path = %binary_path, "sandd uploaded");

        // Step 4: Start sandd
        self.start_sandd_remote(machine_id).await?;
        tracing::info!(machine_id = %machine_id, "sandd started");

        // Step 5: Connect bridge
        let _ = self.connect_machine(machine_id).await;

        Ok(())
    }

    /// Detect the remote platform (uname -s, uname -m).
    async fn detect_platform(&self, machine_id: &MachineId) -> Result<PlatformInfo> {
        let handle = self.machines.get(machine_id).unwrap();
        let transport = handle.transport.as_ref().unwrap();

        // Execute uname commands through the transport
        let uname_s = transport.exec(ExecRequest {
            runtime_id: RuntimeId::new(),
            command: vec!["uname".to_string(), "-s".to_string()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: Some(10000),
            stdin_data: None,
        }).await?;

        let uname_m = transport.exec(ExecRequest {
            runtime_id: RuntimeId::new(),
            command: vec!["uname".to_string(), "-m".to_string()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: Some(10000),
            stdin_data: None,
        }).await?;

        let os = String::from_utf8_lossy(&uname_s.stdout).trim().to_string();
        let arch = String::from_utf8_lossy(&uname_m.stdout).trim().to_string();

        Ok(PlatformInfo {
            os,
            arch,
            os_alias: map_os(&os),
            arch_alias: map_arch(&arch),
        })
    }

    async fn check_sandd_version(&self, machine_id: &MachineId) -> Result<String> {
        let handle = self.machines.get(machine_id).unwrap();
        let transport = handle.transport.as_ref().unwrap();

        let result = transport.exec(ExecRequest {
            runtime_id: RuntimeId::new(),
            command: vec!["~/.local/share/spark/current/sandd".to_string(), "--version".to_string()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: Some(5000),
            stdin_data: None,
        }).await?;

        if result.exit_code != Some(0) {
            bail!("sandd not found or not executable");
        }

        Ok(String::from_utf8_lossy(&result.stdout).trim().to_string())
    }

    async fn download_and_upload_binary(
        &self,
        machine_id: &MachineId,
        arch: &str,
    ) -> Result<String> {
        // In a real implementation:
        // 1. Look up the right binary for (os, arch) in versions/
        // 2. scp it to the remote machine
        // 3. Verify SHA256
        // 4. Make executable

        // For now, simulate success
        Ok("~/.local/share/spark/current/sandd".to_string())
    }

    async fn start_sandd_remote(&self, machine_id: &MachineId) -> Result<()> {
        let handle = self.machines.get(machine_id).unwrap();
        let transport = handle.transport.as_ref().unwrap();

        // Start sandd on remote
        transport.exec(ExecRequest {
            runtime_id: RuntimeId::new(),
            command: vec![
                "~/.local/share/spark/current/sandd".to_string(),
                "--daemon".to_string(),
            ],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: Some(30000),
            stdin_data: None,
        }).await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Handshake
    // -----------------------------------------------------------------------

    /// Perform bridge handshake to get remote machine info.
    async fn perform_handshake(&self, machine_id: &MachineId) -> Result<BridgeHandshake> {
        let handle = self.machines.get(machine_id).unwrap();
        let transport = handle.transport.as_ref().unwrap();

        // For SSH transport, we'd read/write through the bridge's stdio.
        // This is a placeholder — real implementation would use
        // tokio::process::ChildStdout/ChildStdin.

        Ok(BridgeHandshake {
            protocol_version: 1,
            sandd_version: "0.1.0".to_string(),
            features: vec!["exec".to_string(), "pty".to_string(), "fs".to_string()],
            machine_id: machine_id.0.clone(),
            os: "Linux".to_string(),
            arch: "x86_64".to_string(),
        })
    }

    /// Update machine metadata from a handshake response.
    async fn update_machine_from_handshake(
        &self,
        machine_id: &MachineId,
        handshake: &BridgeHandshake,
    ) {
        if let Some(handle) = self.machines.get(machine_id) {
            let mut machine = handle.machine.clone();
            machine.metadata = MachineMetadata {
                os: Some(handshake.os.clone()),
                arch: Some(handshake.arch.clone()),
                sandd_version: Some(handshake.sandd_version.clone()),
                latency_ms: Some(15),
                ..Default::default()
            };
            machine.last_seen_at = Some(chrono::Utc::now());
            machine.status = MachineStatus::Connected;

            // Update the handle
            // (in real code, use interior mutability)
        }
    }

    // -----------------------------------------------------------------------
    // Runtime routing
    // -----------------------------------------------------------------------

    /// Get the transport for a given machine.
    pub fn transport(&self, machine_id: &MachineId) -> Option<Arc<dyn RuntimeTransport>> {
        self.machines.get(machine_id)
            .and_then(|h| h.transport.clone())
    }

    /// Create a Runtime on a specific machine.
    pub async fn create_runtime_on_machine(
        &self,
        machine_id: &MachineId,
        request: CreateRuntimeRequest,
    ) -> Result<RuntimeInfo> {
        let transport = self.transport(machine_id)
            .ok_or_else(|| anyhow!("machine not found or not connected: {}", machine_id))?;

        if !transport.is_connected() {
            bail!("machine not connected: {}", machine_id);
        }

        transport.create_runtime(request).await
    }

    /// Execute a command on a Runtime on a specific machine.
    pub async fn exec_on_machine(
        &self,
        machine_id: &MachineId,
        exec_request: sand_protocol::ExecRequest,
    ) -> Result<sand_protocol::ExecResult> {
        let transport = self.transport(machine_id)
            .ok_or_else(|| anyhow!("machine not found or not connected: {}", machine_id))?;

        if !transport.is_connected() {
            bail!("machine not connected: {}", machine_id);
        }

        transport.exec(exec_request).await
    }

    /// Destroy a Runtime on a specific machine.
    pub async fn destroy_runtime_on_machine(
        &self,
        machine_id: &MachineId,
        runtime_id: &str,
    ) -> Result<()> {
        let transport = self.transport(machine_id)
            .ok_or_else(|| anyhow!("machine not found or not connected: {}", machine_id))?;

        transport.destroy_runtime(runtime_id).await
    }

    // -----------------------------------------------------------------------
    // Health / Ping
    // -----------------------------------------------------------------------

    /// Ping all connected machines and update status.
    pub async fn ping_all(&self) {
        let machines: Vec<MachineId> = self.machines.keys().cloned().collect();

        for machine_id in machines {
            let handle = match self.machines.get(&machine_id) {
                Some(h) => h,
                None => continue,
            };

            if !handle.machine.status.is_connected() {
                continue;
            }

            match handle.transport.as_ref() {
                Some(transport) => {
                    match transport.ping().await {
                        Ok(latency) => {
                            // Update metadata with latency
                            let mut metadata = handle.machine.metadata.clone();
                            metadata.latency_ms = Some(latency);

                            // Classify: <100ms good, 100-500ms slow, >500ms degraded
                            let new_status = if latency < 100 {
                                MachineStatus::Connected
                            } else if latency < 500 {
                                MachineStatus::Connected // still connected but slow
                            } else {
                                MachineStatus::Degraded
                            };

                            drop(transport);
                            let _ = self.update_machine_status(&machine_id, new_status).await;
                        }
                        Err(e) => {
                            tracing::warn!(machine_id = %machine_id, error = %e, "ping failed");
                            let _ = self.update_machine_status(&machine_id, MachineStatus::Degraded).await;
                        }
                    }
                }
                None => {}
            }
        }
    }

    // -----------------------------------------------------------------------
    // Status update helper
    // -----------------------------------------------------------------------

    async fn update_machine_status(
        &self,
        machine_id: &MachineId,
        status: MachineStatus,
    ) -> Result<()> {
        if let Some(handle) = self.machines.get(machine_id) {
            let mut machine = handle.machine.clone();
            machine.status = status;
            machine.last_seen_at = Some(chrono::Utc::now());

            // In real code, this would update via interior mutability.
            // For now, we just log.
            tracing::debug!(machine_id = %machine_id, status = %status, "machine status updated");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Platform detection helpers
// ---------------------------------------------------------------------------

struct PlatformInfo {
    os: String,
    arch: String,
    os_alias: String,
    arch_alias: String,
}

fn map_os(os: &str) -> String {
    match os.to_lowercase().as_str() {
        "linux" => "linux".to_string(),
        "darwin" => "macos".to_string(),
        "freebsd" => "freebsd".to_string(),
        _ => os.to_lowercase(),
    }
}

fn map_arch(arch: &str) -> String {
    match arch.to_lowercase().as_str() {
        "x86_64" | "amd64" => "x86_64".to_string(),
        "aarch64" | "arm64" => "aarch64".to_string(),
        "armv7l" => "armv7".to_string(),
        _ => arch.to_lowercase(),
    }
}

// ---------------------------------------------------------------------------
// Unit tests (basic)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_local_machine() {
        let mgr = MachineManager::new();
        let machines = mgr.list_machines();
        assert_eq!(machines.len(), 1);
        assert!(machines[0].kind.is_local());
    }

    #[tokio::test]
    async fn test_add_ssh_machine() {
        let mgr = MachineManager::new();

        let ssh_machine = Machine::ssh(
            MachineId::new(),
            "devbox".to_string(),
            "10.0.0.42".to_string(),
            22,
            Some("dev".to_string()),
            Some("devbox".to_string()), // SSH config host alias
        );

        mgr.add_machine(ssh_machine).await.unwrap();
        let machines = mgr.list_machines();
        assert_eq!(machines.len(), 2); // local + ssh
    }
}
