use serde::{Deserialize, Serialize};

/// A workbench is a long-lived runtime that persists across tasks.
/// It holds the user's project workspace, files, browser, and terminals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workbench {
    pub id: String,
    pub name: String,
    pub runtime_id: Option<String>,
    pub project_path: Option<String>,
    pub active: bool,
}
