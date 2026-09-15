//! Synchronous API client for snapshot fetches.
//!
//! For real-time streaming, use the Transport struct which handles
//! WebSocket/SSE connections and emits TransportEvent.

use anyhow::{Context, Result};
use spark_model::*;

// ---------------------------------------------------------------------------
// ApiClient
// ---------------------------------------------------------------------------

/// Stateless HTTP client. One-shot calls to fetch data.
/// In production, this would use reqwest or hyper. For now, we define
/// the interface and provide a mock-friendly abstraction.
pub struct ApiClient {
    base_url: String,
}

impl ApiClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // ----- Bots -----

    pub fn list_bots(&self) -> Result<Vec<Bot>> {
        // GET /api/bots
        // For now, return mock data.
        Ok(vec![Bot {
            id: BotId("bot-default".to_string()),
            name: "Coding Bot".to_string(),
            model: "gpt-4o".to_string(),
            system_prompt: None,
            session_ids: vec![],
            runtime_ids: vec![],
        }])
    }

    // ----- Tasks -----

    pub fn list_tasks(&self, _bot_id: &BotId) -> Result<Vec<TaskSummary>> {
        // GET /api/bots/{id}/tasks
        Ok(vec![])
    }

    pub fn get_task(&self, task_id: &TaskId) -> Result<TaskDetail> {
        // GET /api/tasks/{id}
        Err(anyhow::anyhow!("task not found: {}", task_id))
    }

    pub fn create_task(&self, _bot_id: &BotId, goal: &str) -> Result<TaskSummary> {
        // POST /api/tasks { goal }
        Ok(TaskSummary {
            id: TaskId::new(),
            goal: goal.to_string(),
            status: TaskStatus::Pending,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
    }

    pub fn send_message(&self, _task_id: &TaskId, _content: &str) -> Result<()> {
        // POST /api/tasks/{id}/messages { content }
        Ok(())
    }

    pub fn stop_task(&self, _task_id: &TaskId) -> Result<()> {
        // POST /api/tasks/{id}/stop
        Ok(())
    }

    pub fn approve_permission(&self, _task_id: &TaskId, _permission_id: &str) -> Result<()> {
        // POST /api/tasks/{id}/permissions/{pid}/approve
        Ok(())
    }

    pub fn deny_permission(&self, _task_id: &TaskId, _permission_id: &str) -> Result<()> {
        // POST /api/tasks/{id}/permissions/{pid}/deny
        Ok(())
    }

    pub fn confirm_check(&self, _task_id: &TaskId) -> Result<()> {
        // POST /api/tasks/{id}/confirm
        Ok(())
    }

    // ----- Workbenches -----

    pub fn list_workbenches(&self) -> Result<Vec<Workbench>> {
        // GET /api/workbenches
        Ok(vec![])
    }

    // ----- Browser -----

    pub fn get_browser_screenshot(&self, _task_id: &TaskId) -> Result<Vec<u8>> {
        // GET /api/tasks/{id}/browser/screenshot
        Ok(vec![])
    }

    // ----- Files / Diff -----

    pub fn get_file_changes(&self, _task_id: &TaskId) -> Result<Vec<FileChange>> {
        // GET /api/tasks/{id}/files
        Ok(vec![])
    }

    pub fn get_file_diff(&self, _task_id: &TaskId, _path: &str) -> Result<String> {
        // GET /api/tasks/{id}/files/{path}/diff
        Ok(String::new())
    }
}
