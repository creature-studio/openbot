//! Legacy runtime/lease facade retained for API compatibility. It is backed by
//! RuntimeTransport, never by a second raw UDS client, so local and remote
//! callers share the same lifecycle semantics.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use spark_model::MachineId;
use spark_transport::{CreateRuntimeRequest, RuntimeTransport};

pub struct RuntimeManager {
    transport: Arc<dyn RuntimeTransport>,
    machine_id: MachineId,
    leases: Mutex<HashMap<String, (String, String, String)>>,
    runtimes: Mutex<HashSet<String>>,
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self::with_transport(Arc::new(spark_transport::LocalTransport::new()), MachineId::local())
    }

    pub fn with_transport(transport: Arc<dyn RuntimeTransport>, machine_id: MachineId) -> Self {
        Self { transport, machine_id, leases: Mutex::new(HashMap::new()), runtimes: Mutex::new(HashSet::new()) }
    }

    pub fn create_runtime(&self, kind: &str) -> Result<String, String> {
        let workspace = std::env::var("HOME").map(|home| std::path::PathBuf::from(home).join("spark").join(kind)).unwrap_or_else(|_| std::path::PathBuf::from("/tmp").join("spark").join(kind));
        let runtime = crate::tools::context::block_on_transport(self.transport.create_runtime(CreateRuntimeRequest::new(kind, workspace).on_machine(self.machine_id.clone())))
            .map_err(|e| e.to_string())?;
        if let Ok(mut runtimes) = self.runtimes.lock() { runtimes.insert(runtime.id.clone()); }
        Ok(runtime.id)
    }

    pub fn destroy_runtime(&self, id: &str) -> Result<(), String> {
        crate::tools::context::block_on_transport(self.transport.destroy_runtime(id)).map_err(|e| e.to_string())?;
        if let Ok(mut runtimes) = self.runtimes.lock() { runtimes.remove(id); }
        Ok(())
    }

    pub fn list_runtimes(&self) -> Result<String, String> {
        let runtimes = crate::tools::context::block_on_transport(self.transport.list_runtimes()).map_err(|e| e.to_string())?;
        serde_json::to_string(&runtimes).map_err(|e| e.to_string())
    }

    pub fn acquire_lease(&self, runtime_id: &str, owner: &str, session_id: &str) -> Result<String, String> {
        if !self.runtimes.lock().map_err(|_| "runtime state unavailable")?.contains(runtime_id) {
            // Recovered runtimes may not have been created by this facade. Verify
            // through the same selected transport before leasing them.
            crate::tools::context::block_on_transport(self.transport.get_runtime(runtime_id)).map_err(|e| e.to_string())?;
        }
        let lease_id = format!("lease-{runtime_id}-{}", now_ms());
        self.leases.lock().map_err(|_| "lease state unavailable")?.insert(lease_id.clone(), (runtime_id.into(), owner.into(), session_id.into()));
        Ok(lease_id)
    }

    pub fn release_lease(&self, lease_id: &str) -> Result<(), String> {
        self.leases.lock().map_err(|_| "lease state unavailable")?.remove(lease_id).map(|_| ()).ok_or_else(|| format!("lease not found: {lease_id}"))
    }

    pub fn release_lease_by_session(&self, session_id: &str) -> Result<(), String> {
        if let Ok(mut leases) = self.leases.lock() { leases.retain(|_, (_, _, session)| session != session_id); }
        Ok(())
    }

    pub fn list_leases(&self, runtime_id: Option<&str>) -> String {
        let leases = self.leases.lock().map(|leases| leases.iter().filter(|(_, (runtime, _, _))| runtime_id.map(|id| id == runtime).unwrap_or(true)).map(|(id, (runtime, owner, session))| format!("{{\"lease_id\":\"{}\",\"runtime_id\":\"{}\",\"owner\":\"{}\",\"session_id\":\"{}\"}}", id, runtime, owner, session)).collect::<Vec<_>>().join(",")).unwrap_or_default();
        format!("[{}]", leases)
    }

    pub fn task_complete(&self, runtime_id: &str, owner: &str) -> Result<bool, String> {
        let lease = self.leases.lock().map_err(|_| "lease state unavailable")?.iter().find(|(_, (runtime, lease_owner, _))| runtime == runtime_id && lease_owner == owner).map(|(id, _)| id.clone());
        if let Some(lease) = lease { let _ = self.release_lease(&lease); }
        Ok(false)
    }
}

fn now_ms() -> u128 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default() }
