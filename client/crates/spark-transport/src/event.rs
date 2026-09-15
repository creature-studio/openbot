//! Transport events: the real-time stream from host-agent to client.
//!
//! These map closely to the AgentEvent / LoopEvent types in host-agent,
//! but are normalized for the client. The spark-ui stores consume these
//! and update Entity<T> state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransportEvent {
    // ----- Connection -----
    Connected,
    Disconnected { reason: Option<String> },

    // ----- Task lifecycle -----
    TaskCreated {
        task_id: String,
        goal: String,
    },
    TaskStatusChanged {
        task_id: String,
        status: String,
    },

    // ----- Timeline items (from agent loop) -----
    UserMessage {
        task_id: String,
        message_id: String,
        content: String,
    },
    AssistantMessage {
        task_id: String,
        message_id: String,
        content: String,
        streaming: bool,
    },
    AssistantStreaming {
        task_id: String,
        message_id: String,
        delta: String,
    },

    // ----- Tool calls -----
    ToolStarted {
        task_id: String,
        call_id: String,
        tool_name: String,
        args_json: String,
    },
    ToolOutput {
        task_id: String,
        call_id: String,
        delta: String,
    },
    ToolFinished {
        task_id: String,
        call_id: String,
        tool_name: String,
        status: String, // "success" | "error" | "permission_required" | "cancelled"
        result: String,
        duration_ms: u64,
    },

    // ----- Permission -----
    PermissionRequired {
        task_id: String,
        permission_id: String,
        tool_name: String,
        reason: String,
    },
    PermissionResolved {
        task_id: String,
        permission_id: String,
        allowed: bool,
    },

    // ----- ReadyForCheck -----
    ReadyForCheck {
        task_id: String,
        summary: String,
        files_changed: u32,
        tests_passed: bool,
        browser_verified: bool,
    },
    CheckConfirmed {
        task_id: String,
    },

    // ----- Browser -----
    BrowserScreenshot {
        task_id: String,
        frame: Vec<u8>,
    },
    BrowserAction {
        task_id: String,
        action: String, // "click @e12", "fill @e8 \"hello\"", etc.
    },

    // ----- Terminal -----
    TerminalOutput {
        task_id: String,
        terminal_id: String,
        data: Vec<u8>,
    },
    TerminalExit {
        task_id: String,
        terminal_id: String,
        code: Option<i32>,
    },

    // ----- Errors -----
    Error {
        task_id: Option<String>,
        message: String,
        recoverable: bool,
    },
}
