//! RuntimeTransport: the trait that abstracts over local and remote (SSH) machines.
//!
//! This is the key abstraction that lets host-agent tools (shell.exec, file.*, terminal.*,
//! browser.*, computer.*) remain completely ignorant of whether a Runtime is local or remote.
//!
//! ```text
//! Tool call
//!   ↓
//! Session → Runtime → machine_id
//!   ↓
//! MachineManager.transport(machine_id)
//!   ↓
//! RuntimeTransport::exec()        ← LocalTransport or SshTransport
//! ```
//!
//! GPUI-agnostic: this crate is usable from CLI, mobile, web, tests.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;

use spark_model::MachineId;

// ---------------------------------------------------------------------------
// CreateRuntimeRequest — what we need to create a Runtime on a machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CreateRuntimeRequest {
    /// The machine to create the runtime on.
    pub machine_id: MachineId,
    /// Runtime kind: task, workbench, assistant, eval.
    pub kind: String,
    /// Workspace path on the target machine.
    pub workspace: PathBuf,
    /// Optional: specific runtime id to use (otherwise auto-generated).
    pub runtime_id: Option<String>,
    /// Optional: cgroup configuration for resource isolation.
    pub cgroup_config: Option<CgroupConfig>,
}

#[derive(Debug, Clone)]
pub struct CgroupConfig {
    /// CPU limit in cores (e.g. 2.0 = 2 full cores).
    pub cpu_limit: Option<f64>,
    /// Memory limit in bytes.
    pub memory_limit: Option<u64>,
    /// Process group name for isolation.
    pub pgroup: Option<String>,
}

// ... (full RuntimeTransport trait + types elided for brevity — see runtime_transport.rs)
