//! AttentionStore: tracks the global attention state across all tasks.
//!
//! When any task has an active attention (permission, ready-for-check, etc.),
//! the AttentionStore surfaces it so the UI can show overlays/modals.

use gpui::{Context, EventEmitter};
use spark_model::Attention;

#[derive(Debug, Clone)]
pub enum AttentionSnapshot {
    None,
    PermissionRequired {
        task_id: String,
        tool: String,
        reason: String,
    },
    WaitingInput {
        task_id: String,
        prompt: String,
    },
    ReadyForCheck {
        task_id: String,
        summary: String,
        files_changed: u32,
        tests_passed: bool,
        browser_verified: bool,
    },
    Error {
        task_id: String,
        error: String,
    },
    Completed {
        task_id: String,
        summary: String,
    },
}

#[derive(Debug, Clone)]
pub enum AttentionEvent {
    Changed,
}

pub struct AttentionStore {
    pub active: AttentionSnapshot,
    pub history: Vec<(String, AttentionSnapshot)>, // (task_id, snapshot)
}

impl EventEmitter<AttentionEvent> for AttentionStore {}

impl AttentionStore {
    pub fn new() -> Self {
        Self {
            active: AttentionSnapshot::None,
            history: Vec::new(),
        }
    }

    pub fn set(
        &mut self,
        task_id: String,
        attention: &Attention,
        cx: &mut Context<Self>,
    ) {
        let snapshot = match attention {
            Attention::PermissionRequired {
                tool,
                reason,
                detail: _,
            } => AttentionSnapshot::PermissionRequired {
                task_id: task_id.clone(),
                tool: tool.clone(),
                reason: reason.clone(),
            },
            Attention::WaitingInput { prompt } => AttentionSnapshot::WaitingInput {
                task_id: task_id.clone(),
                prompt: prompt.clone(),
            },
            Attention::ReadyForCheck {
                summary,
                files_changed,
                tests_passed,
                browser_verified,
            } => AttentionSnapshot::ReadyForCheck {
                task_id: task_id.clone(),
                summary: summary.clone(),
                files_changed: *files_changed,
                tests_passed: *tests_passed,
                browser_verified: *browser_verified,
            },
            Attention::ExecutionError { error, .. } => AttentionSnapshot::Error {
                task_id: task_id.clone(),
                error: error.clone(),
            },
            Attention::Completed { summary } => AttentionSnapshot::Completed {
                task_id: task_id.clone(),
                summary: summary.clone(),
            },
        };
        self.history.push((task_id, snapshot.clone()));
        self.active = snapshot;
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.active = AttentionSnapshot::None;
        cx.notify();
    }

    pub fn has_active(&self) -> bool {
        !matches!(self.active, AttentionSnapshot::None)
    }
}
