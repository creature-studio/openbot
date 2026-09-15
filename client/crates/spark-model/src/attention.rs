use serde::{Deserialize, Serialize};

/// Attention represents something that requires the user's immediate input.
/// It drives the overlay/modal UI at the bottom or center of the screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Attention {
    /// Agent needs permission to use a tool.
    PermissionRequired {
        tool: String,
        reason: String,
        detail: Option<String>,
    },

    /// Agent needs free-form input from the user.
    WaitingInput {
        prompt: String,
    },

    /// Agent has finished and is ready for human review.
    ReadyForCheck {
        summary: String,
        files_changed: u32,
        tests_passed: bool,
        browser_verified: bool,
    },

    /// An error that the agent cannot recover from on its own.
    ExecutionError {
        error: String,
        recoverable: bool,
    },

    /// Task completed successfully.
    Completed {
        summary: String,
    },
}

impl Attention {
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::PermissionRequired { .. } => "Permission Required",
            Self::WaitingInput { .. } => "Input Needed",
            Self::ReadyForCheck { .. } => "Ready for Check",
            Self::ExecutionError { .. } => "Error",
            Self::Completed { .. } => "Completed",
        }
    }
}
