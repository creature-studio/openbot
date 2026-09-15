//! ToolExecutionContext: provides machines/routing for tools.

use std::sync::Arc;
use spark_model::{Machine, MachineId};
use spark_transport::RuntimeTransport;

/// Drive a transport future from synchronous tool code.
///
/// Tools are synchronous (`fn execute(&self, args, runtime_id)`) while the
/// transports are async, so something has to bridge the two *correctly*:
///
/// * inside host-agent's runtime (the normal case) the future is driven on the
///   runtime handle from a scoped thread — calling `block_on` on the runtime's
///   own thread would deadlock the worker;
/// * outside any runtime (CLI paths, unit tests) a plain executor drives it.
///
/// Returns whatever the transport call returned; a panic in the transport is
/// propagated rather than silently swallowed.
pub fn block_on_transport<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => std::thread::scope(|scope| {
            scope
                .spawn(move || handle.block_on(future))
                .join()
                .expect("a transport call panicked")
        }),
        Err(_) => futures::executor::block_on(future),
    }
}

/// Context passed to tools so they can route operations to the correct machine.
/// Tools never know whether a machine is local or remote — they call through
/// the RuntimeTransport trait.
pub struct ToolExecutionContext {
    /// Get the transport for a given machine.
    pub get_transport: Arc<dyn Fn(&MachineId) -> Option<Arc<dyn RuntimeTransport>> + Send + Sync>,
    /// Get a machine by ID.
    pub get_machine: Arc<dyn Fn(&MachineId) -> Option<Machine> + Send + Sync>,
    /// Get all machines.
    pub list_machines: Arc<dyn Fn() -> Vec<Machine> + Send + Sync>,
    /// Default machine ID (usually local).
    pub default_machine_id: MachineId,
    /// Which machine a runtime lives on. This is what makes routing correct:
    /// a task created on `devbox` must run its tools on `devbox`, not on the
    /// default machine (architecture §十九).
    pub runtime_machine: Arc<dyn Fn(&str) -> Option<MachineId> + Send + Sync>,
}

impl ToolExecutionContext {
    pub fn new(
        get_transport: Arc<dyn Fn(&MachineId) -> Option<Arc<dyn RuntimeTransport>> + Send + Sync>,
        get_machine: Arc<dyn Fn(&MachineId) -> Option<Machine> + Send + Sync>,
        list_machines: Arc<dyn Fn() -> Vec<Machine> + Send + Sync>,
        default_machine_id: MachineId,
    ) -> Self {
        // Legacy constructor: without a runtime→machine resolver every tool
        // call lands on the default machine, which is what the old code did.
        Self {
            get_transport,
            get_machine,
            list_machines,
            default_machine_id,
            runtime_machine: Arc::new(|_runtime_id| None),
        }
    }

    /// Full constructor: the session knows which machine a runtime belongs to.
    pub fn with_runtime_machine(
        get_transport: Arc<dyn Fn(&MachineId) -> Option<Arc<dyn RuntimeTransport>> + Send + Sync>,
        get_machine: Arc<dyn Fn(&MachineId) -> Option<spark_model::Machine> + Send + Sync>,
        list_machines: Arc<dyn Fn() -> Vec<spark_model::Machine> + Send + Sync>,
        default_machine_id: MachineId,
        runtime_machine: Arc<dyn Fn(&str) -> Option<MachineId> + Send + Sync>,
    ) -> Self {
        Self {
            get_transport,
            get_machine,
            list_machines,
            default_machine_id,
            runtime_machine,
        }
    }

    /// Machine a runtime lives on, falling back to the default machine (a
    /// runtime created before the machine registry existed is local).
    pub fn machine_for_runtime(&self, runtime_id: &str) -> MachineId {
        (self.runtime_machine)(runtime_id).unwrap_or_else(|| self.default_machine_id.clone())
    }

    /// **The tool-facing call.** Tools hand in the runtime they were given and
    /// get back the transport of whatever machine that runtime lives on —
    /// local or SSH, they cannot tell the difference.
    pub fn transport_for_runtime(&self, runtime_id: &str) -> Option<Arc<dyn RuntimeTransport>> {
        let machine_id = self.machine_for_runtime(runtime_id);
        self.transport_for(&machine_id)
            .or_else(|| self.transport_for(&self.default_machine_id))
    }

    pub fn transport_for(&self, machine_id: &MachineId) -> Option<Arc<dyn RuntimeTransport>> {
        (self.get_transport)(machine_id)
    }

    pub fn machine(&self, machine_id: &MachineId) -> Option<Machine> {
        (self.get_machine)(machine_id)
    }

    pub fn machines(&self) -> Vec<Machine> {
        (self.list_machines)()
    }

    pub fn default_machine(&self) -> MachineId {
        self.default_machine_id.clone()
    }
}
