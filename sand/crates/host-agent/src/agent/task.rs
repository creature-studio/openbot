use super::state::TaskStatus;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub goal: String,
    pub session_id: AgentSessionId,
    pub runtime_id: String,
    pub status: TaskStatus,
    pub artifacts: Vec<String>,
    pub result: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Task {
    pub fn new(goal: String, session_id: String, runtime_id: String) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        Self {
            id: format!("task-{:x}-{}", now, std::process::id()),
            goal,
            session_id,
            runtime_id,
            status: TaskStatus::Pending,
            artifacts: Vec::new(),
            result: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn complete(&mut self, result: String) {
        self.result = Some(result);
        self.status = TaskStatus::Completed;
        self.updated_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    }

    pub fn fail(&mut self, reason: String) {
        self.result = Some(reason);
        self.status = TaskStatus::Failed;
        self.updated_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    }

    pub fn add_artifact(&mut self, artifact: String) {
        self.artifacts.push(artifact);
        self.updated_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    }
}

pub struct TaskManager {
    tasks: std::collections::HashMap<String, Task>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self { tasks: std::collections::HashMap::new() }
    }

    pub fn create(&mut self, goal: String, session_id: String, runtime_id: String) -> String {
        let task = Task::new(goal, session_id, runtime_id);
        let id = task.id.clone();
        self.tasks.insert(id.clone(), task);
        id
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.get_mut(id)
    }

    pub fn list(&self) -> Vec<&Task> {
        self.tasks.values().collect()
    }

    pub fn complete(&mut self, id: &str, result: String) -> Result<(), String> {
        if let Some(task) = self.tasks.get_mut(id) {
            task.complete(result);
            Ok(())
        } else {
            Err(format!("task {} not found", id))
        }
    }

    pub fn fail(&mut self, id: &str, reason: String) -> Result<(), String> {
        if let Some(task) = self.tasks.get_mut(id) {
            task.fail(reason);
            Ok(())
        } else {
            Err(format!("task {} not found", id))
        }
    }
}
