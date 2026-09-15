//! ToolExecutionContext: provides machines/routing for tools.

use std::sync::Arc;
use sand_protocol::MachineId;
use spark_model::Machine;
use spark_transport::RuntimeTransport;

/// Context passed to tools so they can route operations to the correct machine.
/// Tools never know whether a machine is local or remote — they call through
/// the RuntimeTransport trait.
pub struct ToolExecutionContext {
    /// Get the transport for a given machine.
    pub get_transport: Arc<dyn Fn(&MachineId) -> Option<Arc<dyn RuntimeTransport>> + Send + Sync>,
    /// Get a machine by ID.
    pub get_machine: Arc<dyn Fn(&MachineId) -> Option<spark_model::Machine> + Send + Sync>,
    /// Get all machines.
    pub list_machines: Arc<dyn Fn() -> Vec<spark_model::Machine> + Send + Sync>,
    /// Default machine ID (usually local).
    pub default_machine_id: MachineId,
}

impl ToolExecutionContext {
    pub fn new(
        get_transport: Arc<dyn Fn(&MachineId) -> Option<Arc<dyn RuntimeTransport>> + Send + Sync>,
        get_machine: Arc<dyn Fn(&MachineId) -> Option<spark_model::Machine> + Send + Sync>,
        list_machines: Arc<dyn Fn() -> Vec<spark_model::Machine> + Send + Sync>,
        default_machine_id: MachineId,
    ) -> Self {
        Self {
            get_transport,
            get_machine,
            list_machines,
            default_machine_id,
        }
    }

    pub fn transport_for(&self, machine_id: &MachineId) -> Option<Arc<dyn RuntimeTransport>> {
        (self.get_transport)(machine_id)
    }

    pub fn machine(&self, machine_id: &MachineId) -> Option<spark_model::Machine> {
        (self.get_machine)(machine_id)
    }

    pub fn machines(&self) -> Vec<spark_model::Machine> {
        (self.list_machines)()
    }

    pub fn default_machine(&self) -> MachineId {
        self.default_machine_id.clone()
    }
}
