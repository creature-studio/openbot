//! MachineManager — everything Spark knows about machines.
//!
//! ```text
//! tool call (shell.exec / file.* / terminal.* / browser.* / computer.*)
//!   │  runtime_id
//!   ▼
//! Session → Runtime{ machine_id }                  (fixed at creation, v1)
//!   │  machine_id
//!   ▼
//! MachineManager.transport(machine_id)             ← the only routing decision
//!   │
//!   ▼
//! Arc<dyn RuntimeTransport>                        LocalTransport | SshTransport
//! ```
//!
//! Responsibilities (architecture §二十三):
//!
//! * keep the machine registry (add / remove / persist);
//! * own each machine's transport and its lifecycle (connect, disconnect,
//!   reconnect with backoff, bootstrap);
//! * convert transport failures into `MachineStatus` + `Attention`, never into a
//!   failed task;
//! * route by `machine_id` — nothing above this module branches on local/remote.
//!
//! Locking rule: the registry is a `std::sync::RwLock` and **no lock is held
//! across an `.await`**. Health checks and reconnects clone the `Arc` handles
//! they need and release the lock immediately, so a slow SSH machine can never
//! block the UI or a tool call on another machine.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use tokio::sync::Mutex as AsyncMutex;

use spark_model::{Machine, MachineId, MachineKind, MachineStatus, SparkPaths};
use spark_transport::{
    ConnectError, MachineDraft, ReconnectConfig, ReconnectOutcome, RuntimeTransport, SshTransport,
    LocalTransport,
};

use crate::persistence::SqlitePersistence;

/// How often a connected machine is pinged.
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
/// Latency thresholds that define "slow" and "degraded" (§二十七).
const LATENCY_OK_MS: u64 = 100;
const LATENCY_DEGRADED_MS: u64 = 500;

/// What the UI needs to know about a machine right now.
#[derive(Debug, Clone, Default)]
pub struct MachineHealth {
    pub latency_ms: Option<u64>,
    pub checked_at: Option<Instant>,
    pub last_error: Option<String>,
    /// Consecutive failed health checks (drives the reconnect decision).
    pub failures: u32,
    /// Reconnect attempt counter (0 when healthy).
    pub attempts: u32,
}

/// Map a measured latency to a status (§二十七: <100 Connected, 100–500 slow,
/// >500 Degraded, ssh broken Disconnected).
pub fn status_for_latency(latency_ms: Option<u64>) -> MachineStatus {
    match latency_ms {
        Some(ms) if ms < LATENCY_OK_MS => MachineStatus::Connected,
        Some(ms) if ms <= LATENCY_DEGRADED_MS => MachineStatus::Degraded,
        Some(_) => MachineStatus::Degraded,
        None => MachineStatus::Disconnected,
    }
}

/// Per-machine state: the record, its transport and its health.
struct MachineHandle {
    machine: Machine,
    transport: Arc<dyn RuntimeTransport>,
    /// Present for SSH machines: bootstrap and host-key trust need the SSH
    /// specific API, which deliberately is not on `RuntimeTransport`.
    ssh: Option<Arc<SshTransport>>,
    health: Arc<AsyncMutex<MachineHealth>>,
    /// Set while a connect/reconnect attempt is in flight, so two callers do
    /// not start two ssh processes for the same machine.
    connecting: Arc<AsyncMutex<()>>,
}

impl MachineHandle {
    fn new(machine: Machine) -> Self {
        let (transport, ssh): (Arc<dyn RuntimeTransport>, Option<Arc<SshTransport>>) =
            match &machine.kind {
                MachineKind::Local => (Arc::new(LocalTransport::new()), None),
                MachineKind::Ssh { .. } => {
                    let ssh = Arc::new(SshTransport::new(&machine));
                    (ssh.clone(), Some(ssh))
                }
            };
        Self {
            machine,
            transport,
            ssh,
            health: Arc::new(AsyncMutex::new(MachineHealth::default())),
            connecting: Arc::new(AsyncMutex::new(())),
        }
    }

    fn ssh(&self) -> Option<Arc<SshTransport>> {
        self.ssh.clone()
    }

    fn id(&self) -> MachineId {
        self.machine.id.clone()
    }
}

/// Cheap clone of a handle, used when the registry lock must be released
/// before awaiting.
struct HandleSnapshot {
    machine: Machine,
    transport: Arc<dyn RuntimeTransport>,
    ssh: Option<Arc<SshTransport>>,
    health: Arc<AsyncMutex<MachineHealth>>,
    #[allow(dead_code)]
    connecting: Arc<AsyncMutex<()>>,
}

impl HandleSnapshot {
    fn id(&self) -> MachineId {
        self.machine.id.clone()
    }
}

/// Events the manager publishes for the UI / host-agent event bus.
#[derive(Debug, Clone)]
pub enum MachineEvent {
    Added(Machine),
    Removed(MachineId),
    StatusChanged {
        machine_id: MachineId,
        status: MachineStatus,
        detail: Option<String>,
    },
    /// The user must confirm a host key fingerprint.
    AttentionRequired {
        machine_id: MachineId,
        issue: Box<spark_transport::HostKeyIssue>,
    },
    MetadataUpdated(Machine),
}

pub struct MachineManager {
    machines: RwLock<HashMap<MachineId, MachineHandle>>,
    /// Persisted machine records (host alias/name/port/user only — never keys).
    persistence: Option<Arc<SqlitePersistence>>,
    reconnect: ReconnectConfig,
    /// Subscribers (UI, event bus).
    listeners: RwLock<Vec<Arc<dyn Fn(MachineEvent) + Send + Sync>>>,
    running: RwLock<bool>,
    /// Host key issues waiting for the user, per machine. Only a confirmation
    /// through [`MachineManager::trust_host_key`] consumes one, so a fingerprint
    /// can never be trusted as a side effect of some other call.
    pending_host_keys: RwLock<HashMap<MachineId, spark_transport::HostKeyIssue>>,
    /// Runtime ownership is durable routing state: every tool call resolves
    /// runtime_id -> machine_id before touching a transport.
    runtime_machines: RwLock<HashMap<String, MachineId>>,
}

impl MachineManager {
    /// Registry with only the local machine, no persistence.
    pub fn new() -> Self {
        let manager = Self {
            machines: RwLock::new(HashMap::new()),
            persistence: None,
            reconnect: ReconnectConfig::default(),
            listeners: RwLock::new(Vec::new()),
            running: RwLock::new(false),
            pending_host_keys: RwLock::new(HashMap::new()),
            runtime_machines: RwLock::new(HashMap::new()),
        };
        manager.register(Machine::local(Some("Local".to_string())));
        manager
    }

    /// Registry that restores the machines saved in SQLite.
    pub fn with_persistence(persistence: Arc<SqlitePersistence>) -> Self {
        let manager = Self {
            machines: RwLock::new(HashMap::new()),
            persistence: Some(persistence),
            reconnect: ReconnectConfig::default(),
            listeners: RwLock::new(Vec::new()),
            running: RwLock::new(false),
            pending_host_keys: RwLock::new(HashMap::new()),
            runtime_machines: RwLock::new(HashMap::new()),
        };
        manager.register(Machine::local(Some("Local".to_string())));

        match manager.persistence.as_ref().map(|p| p.load_machines()) {
            Some(Ok(machines)) => {
                for machine in machines {
                    manager.register(machine);
                }
            }
            Some(Err(e)) => tracing::warn!("cannot load machines from sqlite: {e}"),
            None => {}
        }
        manager
    }

    // -----------------------------------------------------------------------
    // Registration
    // -----------------------------------------------------------------------

    fn register(&self, machine: Machine) {
        let handle = MachineHandle::new(machine.clone());
        if let Ok(mut machines) = self.machines.write() {
            machines.insert(machine.id.clone(), handle);
        }
        self.emit(MachineEvent::Added(machine));
    }

    /// Subscribe to machine events (UI stores, logging).
    pub fn subscribe(&self, listener: Arc<dyn Fn(MachineEvent) + Send + Sync>) {
        if let Ok(mut listeners) = self.listeners.write() {
            listeners.push(listener);
        }
    }

    fn emit(&self, event: MachineEvent) {
        if let Ok(listeners) = self.listeners.read() {
            for listener in listeners.iter() {
                listener(event.clone());
            }
        }
    }

    /// Add a machine from the "+ Machine" form and connect it.
    ///
    /// Only connection *coordinates* are stored (§九): host, port, user and the
    /// `~/.ssh/config` alias. Keys, passwords and passphrases never reach Spark.
    pub async fn add_machine(&self, draft: &MachineDraft) -> Result<Machine> {
        if !draft.is_valid() {
            bail!("machine needs a name and either a host or an ssh config alias");
        }
        let id = MachineId::new();
        let machine = draft.to_machine(id);
        self.register(machine.clone());
        self.persist(&machine)?;

        // Connecting is best-effort here: a machine that is currently down must
        // still be addable, and the UI shows why it is not connected.
        if let Err(e) = self.connect_machine(&machine.id).await {
            tracing::warn!(machine = %machine.id, "added machine failed to connect: {e}");
        }
        Ok(self
            .get_machine(&machine.id)
            .unwrap_or(machine))
    }

    /// Remove a machine, disconnecting first. Never touches remote runtimes of
    /// *other* machines.
    pub async fn remove_machine(&self, machine_id: &MachineId) -> Result<()> {
        if machine_id.is_local() {
            bail!("the local machine is permanent");
        }
        let handle = self.take_handle(machine_id)?;
        if handle.transport.is_connected() {
            let _ = handle.transport.disconnect().await;
        }
        if let Some(persistence) = &self.persistence {
            let _ = persistence.delete_machine(machine_id.as_str());
        }
        if let Ok(mut runtimes) = self.runtime_machines.write() {
            runtimes.retain(|_, owner| owner != machine_id);
        }
        self.emit(MachineEvent::Removed(machine_id.clone()));
        Ok(())
    }

    fn take_handle(&self, machine_id: &MachineId) -> Result<MachineHandle> {
        let mut machines = self
            .machines
            .write()
            .map_err(|_| anyhow!("machine registry lock poisoned"))?;
        machines
            .remove(machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))
    }

    fn persist(&self, machine: &Machine) -> Result<()> {
        if let Some(persistence) = &self.persistence {
            persistence.save_machine(machine)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Lookup — the sync API tools use
    // -----------------------------------------------------------------------

    pub fn get_machine(&self, machine_id: &MachineId) -> Option<Machine> {
        self.machines
            .read()
            .ok()?
            .get(machine_id)
            .map(|handle| handle.machine.clone())
    }

    pub fn list_machines(&self) -> Vec<Machine> {
        self.machines
            .read()
            .map(|machines| machines.values().map(|h| h.machine.clone()).collect())
            .unwrap_or_default()
    }

    pub fn machine_ids(&self) -> Vec<MachineId> {
        self.machines
            .read()
            .map(|machines| machines.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn default_machine_id(&self) -> MachineId {
        MachineId::local()
    }

    /// Persistence is owned by the machine manager so host-agent session/task
    /// recovery uses the same database as machine coordinates.
    pub fn persistence(&self) -> Option<Arc<SqlitePersistence>> {
        self.persistence.clone()
    }

    /// **The routing call.** Every tool goes through here.
    ///
    /// Returning `Option` rather than an error keeps the tool layer simple: a
    /// missing machine is a "no transport" condition the tool reports as a tool
    /// error, not a crash.
    pub fn transport(&self, machine_id: &MachineId) -> Option<Arc<dyn RuntimeTransport>> {
        self.machines
            .read()
            .ok()?
            .get(machine_id)
            .map(|handle| handle.transport.clone())
    }

    /// Synchronous ownership lookup used by the synchronous ToolRegistry.
    pub fn runtime_machine_id(&self, runtime_id: &str) -> Option<MachineId> {
        self.runtime_machines.read().ok()?.get(runtime_id).cloned()
    }

    fn remember_runtime(&self, runtime_id: impl Into<String>, machine_id: &MachineId) {
        if let Ok(mut runtimes) = self.runtime_machines.write() {
            runtimes.insert(runtime_id.into(), machine_id.clone());
        }
    }

    /// The transport a runtime belongs to, checking the runtime's own record
    /// when the caller only has a runtime id.
    pub async fn transport_for_runtime(&self, runtime_id: &str) -> Option<Arc<dyn RuntimeTransport>> {
        if let Some(machine_id) = self.runtime_machine_id(runtime_id) {
            return self.transport(&machine_id);
        }
        for handle in self.handles() {
            if let Ok(info) = handle.transport.get_runtime(runtime_id).await {
                if info.id == runtime_id {
                    self.remember_runtime(runtime_id.to_string(), &handle.id());
                    return Some(handle.transport.clone());
                }
            }
        }
        // A legacy runtime is only safe to assume local when it was explicitly
        // discovered on the local transport.
        None
    }

    /// Snapshot of every machine as cheap clones.
    ///
    /// The registry lock is released before the caller awaits anything, so a
    /// slow SSH machine can never block a tool call on another machine.
    fn handles(&self) -> Vec<HandleSnapshot> {
        self.machines
            .read()
            .map(|machines| {
                machines
                    .values()
                    .map(|handle| HandleSnapshot {
                        machine: handle.machine.clone(),
                        transport: handle.transport.clone(),
                        ssh: handle.ssh.clone(),
                        health: handle.health.clone(),
                        connecting: handle.connecting.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// `ssh host true` + host key report, for the form's Test connection button.
    pub async fn test_connection(&self, draft: &MachineDraft) -> Result<(), ConnectError> {
        let machine = draft.to_machine(MachineId::new());
        let transport = SshTransport::new(&machine);
        transport.test_connection().await
    }

    /// Connect a machine (bootstrap included when sandd is missing).
    pub async fn connect_machine(&self, machine_id: &MachineId) -> Result<()> {
        let snapshot = self
            .handles()
            .into_iter()
            .find(|handle| handle.id() == *machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))?;
        let transport = snapshot.transport.clone();

        // One connect at a time per machine: two callers must never start two
        // ssh processes for the same machine.
        let _guard = snapshot.connecting.lock().await;
        let was_bootstrapping = matches!(
            self.get_machine(machine_id).map(|m| m.status),
            Some(MachineStatus::Bootstrapping)
        );
        self.set_status(
            machine_id,
            if was_bootstrapping {
                MachineStatus::Bootstrapping
            } else {
                MachineStatus::Connecting
            },
            None,
        );

        // SSH machines go through the typed path so a host key problem arrives
        // as a fingerprint we can show, not as a string we have to re-parse.
        if let Some(ssh) = snapshot.ssh.clone() {
            return match ssh.connect_machine(true).await {
                Ok(handshake) => {
                    let latency = ssh.ping().await.ok();
                    self.apply_handshake(machine_id, Some(handshake), latency);
                    Ok(())
                }
                Err(ConnectError::Bootstrap(_)) => {
                    // A missing/incompatible remote daemon is the one safe
                    // automatic recovery: bootstrap the user-owned binary, then
                    // establish the same long-lived bridge. Host-key and auth
                    // failures never enter this branch.
                    match self.bootstrap_machine(machine_id).await {
                        Ok(_) if snapshot.transport.is_connected() => Ok(()),
                        Ok(_) => Err(anyhow!("bootstrap completed but the sandd bridge did not connect")),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => {
                    self.record_connect_error(machine_id, &error);
                    Err(anyhow!(error.message()))
                }
            };
        }

        // Local machine: no SSH, just verify sandd answers.
        match transport.connect().await {
            Ok(()) => {
                let handshake = transport.handshake().await.ok();
                let latency = transport.ping().await.ok();
                self.apply_handshake(machine_id, handshake, latency);
                Ok(())
            }
            Err(e) => {
                let message = e.to_string();
                let status = MachineStatus::from_ssh_error(&message);
                self.set_status(machine_id.clone(), status, Some(message.clone()));
                Err(anyhow!(message))
            }
        }
    }

    /// Record a typed connection failure: status, detail, and — for host key
    /// problems — an Attention event carrying the fingerprint.
    fn record_connect_error(&self, machine_id: &MachineId, error: &ConnectError) {
        self.set_status(
            machine_id.clone(),
            error.status(),
            Some(error.message()),
        );
        if let Some(issue) = error.host_key_issue() {
            // Remember the issue so [信任并连接] can write *this* key, and show
            // the fingerprint: + [取消] [信任并连接] in the UI.
            if let Ok(mut pending) = self.pending_host_keys.write() {
                pending.insert(machine_id.clone(), issue.clone());
            }
            self.emit(MachineEvent::AttentionRequired {
                machine_id: machine_id.clone(),
                issue: Box::new(issue.clone()),
            });
        }
        tracing::warn!(machine = %machine_id, "connect failed: {}", error.message());
    }

    /// The host key issue waiting for the user, if any.
    pub fn pending_host_key(&self, machine_id: &MachineId) -> Option<spark_transport::HostKeyIssue> {
        self.pending_host_keys
            .read()
            .ok()
            .and_then(|pending| pending.get(machine_id).cloned())
    }

    /// The user pressed [信任并连接] on the Attention card.
    ///
    /// This is the **only** path that writes to `known_hosts`: it takes the
    /// remembered issue (never a fingerprint that arrived from anywhere else),
    /// writes the key line, then reconnects. A *changed* key expected a second
    /// confirmation in the UI before this is called.
    pub async fn trust_host_key(&self, machine_id: &MachineId) -> Result<()> {
        let issue = self
            .pending_host_key(machine_id)
            .ok_or_else(|| anyhow!("no host key waiting for confirmation on {machine_id}"))?;
        spark_transport::ssh::trust_host_key(&issue)
            .context("writing the host key to known_hosts")?;
        tracing::info!(
            machine = %machine_id,
            fingerprint = %issue.fingerprint(),
            high_risk = issue.is_high_risk(),
            "user confirmed the host key"
        );
        if let Ok(mut pending) = self.pending_host_keys.write() {
            pending.remove(machine_id);
        }
        self.connect_machine(machine_id).await
    }

    /// Disconnect the **bridge** only. Remote runtimes, PTYs, servers and Chrome
    /// keep running (§十五).
    pub async fn disconnect_machine(&self, machine_id: &MachineId) -> Result<()> {
        let transport = self
            .transport(machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))?;
        transport.disconnect().await?;
        self.set_status(machine_id, MachineStatus::Disconnected, None);
        Ok(())
    }

    /// Reconnect with the §二十六 backoff schedule, then re-attach to whatever
    /// runtimes are still alive on the machine.
    ///
    /// The decision to keep trying is made by the shared reconnect policy, so
    /// "host key changed" or "auth failed" stops here and asks the user instead
    /// of looping forever.
    pub async fn reconnect_machine(&self, machine_id: &MachineId) -> Result<Vec<String>> {
        let snapshot = self
            .handles()
            .into_iter()
            .find(|handle| handle.id() == *machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))?;
        let transport = snapshot.transport.clone();
        // Keep manual reconnect and the health supervisor from creating a
        // second bridge while the first attempt is still backing off.
        let _guard = snapshot.connecting.lock().await;

        let mut attempt = 0u32;
        loop {
            // Typed error for SSH (carries the host key fingerprint); the local
            // machine cannot produce these, so any error is just an error.
            let outcome = match snapshot.ssh.clone() {
                Some(ssh) => match ssh.connect_machine(true).await {
                    Ok(handshake) => {
                        let latency = ssh.ping().await.ok();
                        self.apply_handshake(machine_id, Some(handshake), latency);
                        return Ok(self.attach_after_reconnect(machine_id, &transport).await);
                    }
                    Err(error) => {
                        spark_transport::reconnect::plan(
                            &error,
                            attempt,
                            &self.reconnect,
                            entropy(),
                        )
                    }
                },
                None => match transport.connect().await {
                    Ok(()) => {
                        let handshake = transport.handshake().await.ok();
                        let latency = transport.ping().await.ok();
                        self.apply_handshake(machine_id, handshake, latency);
                        return Ok(self.attach_after_reconnect(machine_id, &transport).await);
                    }
                    Err(e) => {
                        let error = ConnectError::Unreachable(e.to_string());
                        spark_transport::reconnect::plan(
                            &error,
                            attempt,
                            &self.reconnect,
                            entropy(),
                        )
                    }
                },
            };

            match outcome {
                ReconnectOutcome::RetryAfter(delay) => {
                    self.set_status(
                        machine_id.clone(),
                        MachineStatus::Unreachable,
                        Some(format!(
                            "{} (attempt {})",
                            spark_transport::reconnect::describe_wait(delay),
                            attempt + 1
                        )),
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                ReconnectOutcome::RequiresUserAction(message) => {
                    self.set_status(
                        machine_id.clone(),
                        MachineStatus::RequiresUserAction,
                        Some(message.clone()),
                    );
                    if let Some(ssh) = snapshot.ssh.clone() {
                        if let Err(error) = ssh.connect_machine(false).await {
                            self.record_connect_error(machine_id, &error);
                        }
                    }
                    bail!("{message}");
                }
                ReconnectOutcome::GiveUp(message) => {
                    self.set_status(
                        machine_id.clone(),
                        MachineStatus::Error,
                        Some(message.clone()),
                    );
                    bail!("{message}");
                }
            }
        }
    }

    /// After a reconnect: find the runtimes that survived and report them.
    ///
    /// This is the recovery half of "disconnect ≠ destroy": the machine is
    /// reachable again, and the work that was running is still there.
    async fn attach_after_reconnect(&self, machine_id: &MachineId, transport: &Arc<dyn RuntimeTransport>) -> Vec<String> {
        match transport.list_runtimes().await {
            Ok(runtimes) => {
                let ids: Vec<String> = runtimes
                    .into_iter()
                    .map(|r| {
                        self.remember_runtime(r.id.clone(), machine_id);
                        r.id
                    })
                    .collect();
                tracing::info!("reconnected: {} runtime(s) still alive", ids.len());
                ids
            }
            Err(e) => {
                tracing::warn!("reconnected but ListRuntimes failed: {e}");
                Vec::new()
            }
        }
    }

    /// Force a bootstrap (re-install sandd) and reconnect.
    pub async fn bootstrap_machine(&self, machine_id: &MachineId) -> Result<spark_transport::BootstrapReport> {
        let snapshot = self
            .handles()
            .into_iter()
            .find(|handle| handle.id() == *machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))?;
        let transport = snapshot
            .ssh
            .ok_or_else(|| anyhow!("bootstrap is only meaningful for SSH machines"))?;

        self.set_status(machine_id.clone(), MachineStatus::Bootstrapping, None);
        let report = match spark_transport::ssh::bootstrap::bootstrap(&transport).await {
            Ok(report) => report,
            Err(error) => {
                self.set_status(machine_id.clone(), MachineStatus::Error, Some(error.to_string()));
                return Err(anyhow::Error::from(error));
            }
        };
        // Bootstrap only installs and starts sandd; the bridge is (re)started
        // by the connect that follows, so do it here and handshake for real.
        if let Ok(handshake) = transport.connect_machine(false).await {
            let latency = transport.ping().await.ok();
            self.apply_handshake(machine_id, Some(handshake), latency);
        }
        Ok(report)
    }

    // -----------------------------------------------------------------------
    // Health
    // -----------------------------------------------------------------------

    /// Ping every connected machine once (Ping/Pong over the existing bridge —
    /// never `ssh hostname`, §二十七).
    pub async fn health_check_once(&self) -> Vec<(MachineId, MachineStatus)> {
        let mut out = Vec::new();
        for handle in self.handles() {
            let machine_id = handle.id();
            if !handle.transport.is_connected() {
                let status = self
                    .get_machine(&machine_id)
                    .map(|machine| machine.status)
                    .unwrap_or(MachineStatus::Disconnected);
                out.push((machine_id, status));
                continue;
            }
            match handle.transport.ping().await {
                Ok(latency) => {
                    let status = status_for_latency(Some(latency));
                    if let Ok(mut health) = handle.health.try_lock() {
                        health.latency_ms = Some(latency);
                        health.checked_at = Some(Instant::now());
                        health.last_error = None;
                        health.failures = 0;
                        health.attempts = 0;
                    }
                    self.update_machine(machine_id.clone(), |machine| {
                        machine.touch(latency);
                        machine.status = status.clone();
                    });
                    out.push((machine_id, status));
                }
                Err(e) => {
                    let message = e.to_string();
                    if let Ok(mut health) = handle.health.try_lock() {
                        health.failures += 1;
                        health.last_error = Some(message.clone());
                    }
                    // The bridge is gone: the machine is Disconnected, *not* its
                    // runtimes. A task on it shows "connection lost" and waits.
                    self.set_status(
                        machine_id.clone(),
                        MachineStatus::Disconnected,
                        Some(message),
                    );
                    out.push((machine_id, MachineStatus::Disconnected));
                }
            }
        }
        out
    }

    /// Background health loop (started by host-agent).
    pub async fn run_health_loop(self: Arc<Self>) {
        *self.running.write().unwrap() = true;
        let mut ticker = tokio::time::interval(HEALTH_INTERVAL);
        loop {
            ticker.tick().await;
            if !*self.running.read().unwrap() {
                return;
            }
            self.health_check_once().await;
            self.schedule_reconnects().await;
        }
    }

    /// Start at most one reconnect supervisor per disconnected SSH machine.
    /// The supervisor uses the shared policy's jittered backoff and stops on
    /// authentication or host-key failures; the remote runtime is never
    /// destroyed while this bridge is being rebuilt.
    pub(crate) async fn schedule_reconnects(self: &Arc<Self>) {
        for handle in self.handles() {
            let Some(ssh) = handle.ssh.clone() else { continue; };
            let machine_id = handle.id();
            let status = self
                .get_machine(&machine_id)
                .map(|machine| machine.status)
                .unwrap_or(MachineStatus::Disconnected);
            if ssh.is_connected()
                || status == MachineStatus::RequiresUserAction
                || !ssh.begin_reconnect()
            {
                continue;
            }
            let manager = Arc::clone(self);
            tokio::spawn(async move {
                let _ = manager.reconnect_machine(&machine_id).await;
                ssh.finish_reconnect();
            });
        }
    }

    pub fn stop(&self) {
        *self.running.write().unwrap() = false;
    }

    /// Connect every saved non-local machine at startup.
    pub async fn connect_saved_machines(&self) {
        for machine in self.list_machines() {
            if matches!(machine.kind, MachineKind::Local) {
                continue;
            }
            // Best effort: a machine that is down must not stop host-agent.
            if self.connect_machine(&machine.id).await.is_ok() {
                let _ = self.list_runtimes(&machine.id).await;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Runtime creation helper (one place that knows machine + kind + workspace)
    // -----------------------------------------------------------------------

    /// Create a runtime on a machine, choosing a workspace default per kind.
    pub async fn create_runtime(
        &self,
        machine_id: &MachineId,
        kind: &str,
        workspace: Option<String>,
    ) -> Result<spark_transport::RuntimeInfo> {
        let transport = self
            .transport(machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))?;
        let workspace = match workspace {
            Some(path) => std::path::PathBuf::from(path),
            None => default_workspace(&self.get_machine(machine_id), kind),
        };
        let request = spark_transport::CreateRuntimeRequest::new(kind, workspace)
            .on_machine(machine_id.clone());
        let runtime = transport.create_runtime(request).await?;
        self.remember_runtime(runtime.id.clone(), machine_id);
        Ok(runtime)
    }

    /// Persist the session's machine pin and message checkpoint. OpenSSH
    /// credentials are deliberately absent from this record.
    pub fn persist_session(&self, session: &crate::agent::session::AgentSession) {
        let Some(persistence) = &self.persistence else { return; };
        let messages = serde_json::to_string(&session.messages).unwrap_or_else(|_| "[]".to_string());
        let _ = persistence.save_session(
            &session.id,
            &session.runtime_id,
            session.machine_id().as_str(),
            &session.model,
            session.status.as_str(),
            session.goal.as_deref(),
            &session.cwd,
            session.created_at,
            session.updated_at,
            &messages,
        );
    }

    /// Persist the task's machine pin and lifecycle record. Secrets are not
    /// part of `Task`, and the persistence layer stores only the runtime and
    /// machine coordinates needed for recovery.
    pub fn persist_task(&self, task: &crate::agent::task::Task, machine_id: &MachineId) {
        let Some(persistence) = &self.persistence else { return; };
        let status = match &task.status {
            crate::agent::state::TaskStatus::Pending => "pending",
            crate::agent::state::TaskStatus::Running => "running",
            crate::agent::state::TaskStatus::WaitingApproval => "waiting_approval",
            crate::agent::state::TaskStatus::Completed => "completed",
            crate::agent::state::TaskStatus::Failed => "failed",
            crate::agent::state::TaskStatus::Cancelled => "cancelled",
        };
        let artifacts = serde_json::to_string(&task.artifacts).unwrap_or_else(|_| "[]".to_string());
        let _ = persistence.save_task(
            &task.id,
            &task.goal,
            &task.session_id,
            &task.runtime_id,
            machine_id.as_str(),
            status,
            &artifacts,
            task.result.as_deref(),
            task.created_at,
            task.updated_at,
        );
    }

    /// Destroy a runtime (**only** the runtime — never the machine's sandd).
    pub async fn destroy_runtime(&self, machine_id: &MachineId, runtime_id: &str) -> Result<()> {
        if let Some(owner) = self.runtime_machine_id(runtime_id) {
            if &owner != machine_id {
                bail!("runtime {runtime_id} belongs to machine {owner}, not {machine_id}");
            }
        }
        let transport = self
            .transport(machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))?;
        transport.destroy_runtime(runtime_id).await?;
        if let Ok(mut runtimes) = self.runtime_machines.write() {
            runtimes.remove(runtime_id);
        }
        Ok(())
    }

    pub async fn open_pty(&self, runtime_id: &str, request: spark_transport::PtyOpenRequest) -> Result<()> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.open_pty(request).await
    }

    pub async fn write_pty(&self, runtime_id: &str, request: spark_transport::PtyWriteRequest) -> Result<()> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.write_pty(request).await
    }

    pub async fn resize_pty(&self, runtime_id: &str, request: spark_transport::PtyResizeRequest) -> Result<()> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.resize_pty(request).await
    }

    pub async fn read_pty(&self, runtime_id: &str, request: spark_transport::PtyReadRequest) -> Result<spark_transport::PtyReadResponse> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.read_pty(request).await
    }

    pub async fn signal_pty(&self, runtime_id: &str, request: spark_transport::PtySignalRequest) -> Result<()> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.signal_pty(request).await
    }

    pub async fn close_pty(&self, runtime_id: &str, pty_id: &str) -> Result<()> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.close_pty(runtime_id, pty_id).await
    }

    pub async fn browser_request(&self, runtime_id: &str, request: spark_transport::BrowserRequest) -> Result<spark_transport::BrowserResponse> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.browser_request(request).await
    }

    pub async fn computer_request(&self, runtime_id: &str, request: spark_transport::ComputerRequest) -> Result<spark_transport::ComputerResponse> {
        let transport = self.transport_for_runtime(runtime_id).await.ok_or_else(|| anyhow!("runtime is not mapped to a connected machine: {runtime_id}"))?;
        transport.computer_request(request).await
    }

    pub async fn list_runtimes(&self, machine_id: &MachineId) -> Result<Vec<spark_transport::RuntimeInfo>> {
        let transport = self
            .transport(machine_id)
            .ok_or_else(|| anyhow!("machine not found: {machine_id}"))?;
        let runtimes = transport.list_runtimes().await?;
        for runtime in &runtimes {
            self.remember_runtime(runtime.id.clone(), machine_id);
        }
        Ok(runtimes)
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    fn update_machine<F: FnOnce(&mut Machine)>(&self, machine_id: MachineId, update: F) {
        let updated = {
            let mut machines = match self.machines.write() {
                Ok(machines) => machines,
                Err(_) => return,
            };
            match machines.get_mut(&machine_id) {
                Some(handle) => {
                    update(&mut handle.machine);
                    Some(handle.machine.clone())
                }
                None => None,
            }
        };
        if let Some(machine) = updated {
            self.emit(MachineEvent::MetadataUpdated(machine));
        }
    }

    fn set_status(
        &self,
        machine_id: MachineId,
        status: MachineStatus,
        detail: Option<String>,
    ) {
        let notify = self
            .get_machine(&machine_id)
            .map(|machine| machine.status != status)
            .unwrap_or(false);
        self.update_machine(machine_id.clone(), |machine| {
            machine.status = status.clone();
            if status == MachineStatus::Connected {
                machine.last_seen_at = Some(chrono::Utc::now());
            }
        });
        if let Some(machine) = self.get_machine(&machine_id) {
            self.emit(MachineEvent::StatusChanged {
                machine_id: machine_id.clone(),
                status: machine.status,
                detail,
            });
        } else if notify {
            self.emit(MachineEvent::StatusChanged {
                machine_id,
                status,
                detail,
            });
        }
    }

    /// Fold a handshake into the machine record: metadata, capabilities, status.
    fn apply_handshake(
        &self,
        machine_id: &MachineId,
        handshake: Option<spark_transport::HandshakeResponse>,
        latency: Option<u64>,
    ) {
        let Some(handshake) = handshake else {
            self.set_status(machine_id.clone(), MachineStatus::Connected, None);
            return;
        };
        if let Ok(mut pending) = self.pending_host_keys.write() {
            pending.remove(machine_id);
        }
        let status = status_for_latency(latency.or(handshake.latency_ms));
        let metadata = handshake.metadata();
        let capabilities = handshake.capabilities();
        let version = handshake.sandd_version.clone();
        self.update_machine(machine_id.clone(), |machine| {
            machine.metadata = metadata.clone();
            machine.capabilities = capabilities.clone();
            machine.status = status.clone();
            if latency.is_some() {
                machine.metadata.latency_ms = latency;
            }
            machine.last_seen_at = Some(chrono::Utc::now());
            if machine.display_name.is_none() && !handshake.hostname.is_empty() {
                machine.display_name = Some(handshake.hostname.clone());
            }
        });
        if let Some(machine) = self.get_machine(machine_id) {
            self.emit(MachineEvent::MetadataUpdated(machine.clone()));
            if let Err(e) = self.persist(&machine) {
                tracing::debug!("cannot persist machine metadata: {e}");
            }
        }
        tracing::info!(
            machine = %machine_id,
            sandd = %version,
            latency = ?latency,
            status = status.as_str(),
            "machine handshake complete"
        );
    }

    /// Message for the Attention card when a machine is blocked on the user
    /// (unknown host key, changed key, auth failure). `None` when it is fine.
    pub fn attention_message(&self, machine_id: &MachineId) -> Option<String> {
        let machine = self.get_machine(machine_id)?;
        match machine.status {
            MachineStatus::RequiresUserAction => Some(format!(
                "{} 需要你确认才能继续连接（主机密钥或认证问题）",
                machine.display_name.clone().unwrap_or_else(|| machine.name.clone())
            )),
            MachineStatus::Unreachable => Some(format!(
                "{} 暂时无法连接，Spark 会在后台重试",
                machine.display_name.clone().unwrap_or_else(|| machine.name.clone())
            )),
            MachineStatus::Error => Some(format!(
                "{} 连接失败，可能需要重新 bootstrap",
                machine.display_name.clone().unwrap_or_else(|| machine.name.clone())
            )),
            _ => None,
        }
    }
}

impl Default for MachineManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Default workspace per runtime kind, computed **on the target machine's**
/// filesystem layout (a remote machine does not have our `$HOME`).
pub fn default_workspace(machine: &Option<Machine>, kind: &str) -> std::path::PathBuf {
    let base = match machine.as_ref().map(|m| &m.kind) {
        Some(MachineKind::Ssh { .. }) => std::path::PathBuf::from("~/spark"),
        _ => std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
            .join("spark"),
    };
    match kind {
        "task" | "workbench" | "eval" | "assistant" => base.join(kind),
        other => base.join(other),
    }
}

/// Entropy for jitter: the clock, which is plenty for de-synchronizing clients.
fn entropy() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}

/// Convenience for callers that only have an error string.
pub fn status_from_error(error: &str) -> MachineStatus {
    MachineStatus::from_ssh_error(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_thresholds_match_the_architecture() {
        assert_eq!(status_for_latency(Some(20)), MachineStatus::Connected);
        assert_eq!(status_for_latency(Some(99)), MachineStatus::Connected);
        assert_eq!(status_for_latency(Some(100)), MachineStatus::Degraded);
        assert_eq!(status_for_latency(Some(500)), MachineStatus::Degraded);
        assert_eq!(status_for_latency(Some(501)), MachineStatus::Degraded);
        assert_eq!(status_for_latency(None), MachineStatus::Disconnected);
    }

    #[test]
    fn default_workspace_is_per_machine() {
        let local = Machine::local(None);
        let path = default_workspace(&Some(local), "task");
        assert!(path.ends_with("task"));
        assert!(path.is_absolute(), "local workspace: {path:?}");

        let remote = Machine::ssh(
            MachineId::from_string("mach-x".into()),
            "devbox".into(),
            "10.0.0.42".into(),
            22,
            None,
            None,
        );
        // A remote machine does not share our $HOME: the path must be relative
        // to the user's home *there*, which sandd expands.
        let remote_path = default_workspace(&Some(remote), "workbench");
        assert_eq!(remote_path, std::path::PathBuf::from("~/spark/workbench"));
    }

    #[test]
    fn registry_starts_with_local_and_routes_by_id() {
        let manager = MachineManager::new();
        let machines = manager.list_machines();
        assert_eq!(machines.len(), 1);
        assert!(matches!(machines[0].kind, MachineKind::Local));

        let local_transport = manager.transport(&MachineId::local()).expect("local transport");
        assert_eq!(local_transport.machine_id(), MachineId::local());
        assert!(manager.transport(&MachineId::from_string("mach-nope".into())).is_none());
    }

    #[tokio::test]
    async fn adding_a_machine_registers_it_even_when_ssh_is_unavailable() {
        let manager = MachineManager::new();
        // Port 1 on localhost: nothing is listening, so the connect attempt must
        // fail without stopping the machine from being registered.
        let draft = MachineDraft::new("dead", "127.0.0.1").with_port(1);
        let machine = manager.add_machine(&draft).await.expect("machine added");
        assert_eq!(machine.name, "dead");
        assert!(manager.transport(&machine.id).is_some());
        assert!(manager.list_machines().len() >= 2);
        // Status reflects the failure rather than pretending to be connected.
        let stored = manager.get_machine(&machine.id).unwrap();
        assert!(!stored.status.is_connected());
    }

    #[tokio::test]
    async fn removing_a_machine_drops_its_transport() {
        let manager = MachineManager::new();
        let draft = MachineDraft::new("temp", "127.0.0.1").with_port(1);
        let machine = manager.add_machine(&draft).await.unwrap();
        manager.remove_machine(&machine.id).await.unwrap();
        assert!(manager.get_machine(&machine.id).is_none());
        assert!(manager.transport(&machine.id).is_none());
    }

    #[tokio::test]
    async fn health_check_skips_disconnected_machines() {
        let manager = MachineManager::new();
        let results = manager.health_check_once().await;
        // Local transport has no connection yet: nothing to ping, no crash.
        assert!(results.is_empty() || results.iter().all(|(_, s)| *s != MachineStatus::Connected));
    }

    #[test]
    fn attention_message_only_when_blocked() {
        let manager = MachineManager::new();
        assert!(manager.attention_message(&MachineId::local()).is_none());
        manager.set_status(
            MachineId::local(),
            MachineStatus::RequiresUserAction,
            Some("host key".into()),
        );
        let message = manager.attention_message(&MachineId::local()).expect("attention");
        assert!(message.contains("确认"), "got {message}");
    }

    #[tokio::test]
    async fn trusting_a_host_key_requires_a_pending_issue() {
        let manager = MachineManager::new();
        let id = MachineId::from_string("mach-unknown".into());
        // Nothing remembered: [信任并连接] must not invent a key.
        assert!(manager.pending_host_key(&id).is_none());
        let trusted = manager.trust_host_key(&id).await;
        assert!(trusted.is_err(), "trusting without an issue must fail");
    }

    #[test]
    fn events_reach_listeners() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let manager = MachineManager::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        manager.subscribe(Arc::new(move |_event| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        }));
        manager.set_status(MachineId::local(), MachineStatus::Connected, None);
        assert!(counter.load(Ordering::Relaxed) > 0);
    }
}
