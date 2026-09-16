//! Workbench state is transport-backed just like a task. It never owns a
//! SandClient or a local socket: the selected RuntimeTransport decides whether
//! the workbench runtime is local or on an SSH machine.

use std::sync::Arc;

use spark_model::MachineId;
use spark_transport::{CreateRuntimeRequest, RuntimeInfo, RuntimeTransport};

pub struct WorkbenchManager {
    transport: Arc<dyn RuntimeTransport>,
    machine_id: MachineId,
    workbench_id: Option<String>,
}

impl WorkbenchManager {
    /// Legacy/local constructor retained for API callers that have no machine
    /// registry. It still uses the LocalTransport abstraction, never SandClient.
    pub fn new() -> Self {
        let transport: Arc<dyn RuntimeTransport> = Arc::new(spark_transport::LocalTransport::new());
        Self::with_transport(transport, MachineId::local())
    }

    pub fn with_transport(transport: Arc<dyn RuntimeTransport>, machine_id: MachineId) -> Self {
        Self { transport, machine_id, workbench_id: None }
    }

    /// Ensure the long-lived workbench runtime exists on this fixed machine.
    /// The synchronous API is used by the legacy HostAgentApi, so bridge the
    /// transport future on a scoped executor just as the tool layer does.
    pub fn ensure_workbench(&mut self) -> Result<String, String> {
        if let Some(id) = &self.workbench_id {
            if crate::tools::context::block_on_transport(self.transport.get_runtime(id)).is_ok() {
                return Ok(id.clone());
            }
        }
        let runtimes = crate::tools::context::block_on_transport(self.transport.list_runtimes()).map_err(|e| e.to_string())?;
        if let Some(runtime) = runtimes.into_iter().find(|runtime| runtime.kind == "workbench") {
            self.workbench_id = Some(runtime.id.clone());
            return Ok(runtime.id);
        }
        let workspace = if self.machine_id == MachineId::local() {
            crate::machine::default_workspace(&None, "workbench")
        } else {
            std::path::PathBuf::from("~/spark/workbench")
        };
        let runtime = crate::tools::context::block_on_transport(self.transport.create_runtime(
            CreateRuntimeRequest::new("workbench", workspace).on_machine(self.machine_id.clone()),
        )).map_err(|e| e.to_string())?;
        self.workbench_id = Some(runtime.id.clone());
        Ok(runtime.id)
    }

    pub fn get_workbench_id(&self) -> Option<String> {
        self.workbench_id.clone()
    }

    pub fn machine_id(&self) -> &MachineId { &self.machine_id }

    pub fn destroy_workbench(&self) -> Result<(), String> {
        if let Some(id) = &self.workbench_id {
            crate::tools::context::block_on_transport(self.transport.destroy_runtime(id)).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
