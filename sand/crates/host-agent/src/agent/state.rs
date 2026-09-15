#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatus {
    Idle,
    Running,
    WaitingTool,
    WaitingInput,
    ReadyForCheck,
    Failed { reason: String },
    Completed,
}

impl AgentStatus {
    pub fn as_str(&self) -> &str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Running => "running",
            AgentStatus::WaitingTool => "waiting_tool",
            AgentStatus::WaitingInput => "waiting_input",
            AgentStatus::ReadyForCheck => "ready_for_check",
            AgentStatus::Failed { .. } => "failed",
            AgentStatus::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Attention {
    WaitingInput,
    ReadyForCheck,
    ExecutionError { error: String },
    PermissionRequired { tool: String, reason: String },
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub goal: String,
    pub session_id: String, // was AgentSessionId, now String for simplicity
    pub runtime_id: String,
    pub status: TaskStatus,
    pub artifacts: Vec<String>,
    pub result: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Task {
    pub fn new(goal: String, session_id: String, runtime_id: String) -> Self {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        Self {
            id: format!("task-{:x}", now),
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
}
