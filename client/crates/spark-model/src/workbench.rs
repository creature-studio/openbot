use serde::{Deserialize, Serialize};

/// A workbench is a long-lived runtime that persists across tasks.
/// It holds the user's project workspace, files, browser, and terminals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workbench {
    pub id: String,
    pub name: String,
    /// Machine the workbench runtime lives on. A workbench is long lived
    /// (workspace + runtime + PTYs + browser survive across sessions), which is
    /// exactly why the machine choice must be explicit.
    pub machine_id: super::machine::MachineId,
    pub runtime_id: Option<String>,
    pub project_path: Option<String>,
    pub active: bool,
}

impl Workbench {
    pub fn new(id: impl Into<String>, name: impl Into<String>, machine_id: super::machine::MachineId) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            machine_id,
            runtime_id: None,
            project_path: None,
            active: false,
        }
    }
}
