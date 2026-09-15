use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentSessionId(pub String);

impl AgentSessionId {
    pub fn new() -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
        let rand = std::process::id() as u128 + now % 1000000;
        Self(format!("sess-{:x}-{:x}", now, rand))
    }
    pub fn from_string(s: String) -> Self { Self(s) }
}

#[derive(Debug, Clone)]
pub struct AgentSession {
    pub id: String, // use String for simplicity, compatible with AgentSessionId
    pub runtime_id: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub tool_state: HashMap<String, String>,
    pub metadata: HashMap<String, String>, // for checkpoint/recovery
    pub cwd: String,
    pub status: super::state::AgentStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub goal: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "system" => Role::System,
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => Role::User,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String, // JSON string
}

impl AgentSession {
    pub fn new(runtime_id: String, model: String) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        Self {
            id: AgentSessionId::new().0,
            runtime_id,
            model,
            messages: Vec::new(),
            tool_state: HashMap::new(),
            metadata: HashMap::new(),
            cwd: "/workspace".to_string(),
            status: super::state::AgentStatus::Idle,
            created_at: now,
            updated_at: now,
            goal: None,
        }
    }

    pub fn with_goal(mut self, goal: String) -> Self {
        self.goal = Some(goal);
        self
    }

    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        self.updated_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    }

    pub fn add_user_message(&mut self, content: String) {
        self.add_message(Message {
            role: Role::User,
            content,
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
        });
    }

    pub fn add_system_message(&mut self, content: String) {
        self.add_message(Message {
            role: Role::System,
            content,
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
        });
    }

    // Handoff: Bot -> Session -> Runtime separation
    // Allow transferring runtime to new session or sharing runtime
    pub fn handoff(&self, new_model: Option<String>) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        Self {
            id: AgentSessionId::new().0,
            runtime_id: self.runtime_id.clone(), // same runtime, new session
            model: new_model.unwrap_or_else(|| self.model.clone()),
            messages: vec![], // fresh messages, but could copy context
            tool_state: self.tool_state.clone(),
            metadata: self.metadata.clone(),
            cwd: self.cwd.clone(),
            status: super::state::AgentStatus::Idle,
            created_at: now,
            updated_at: now,
            goal: self.goal.clone(),
        }
    }

    pub fn handoff_with_messages(&self, new_model: Option<String>, keep_last_n: usize) -> Self {
        let mut new_sess = self.handoff(new_model);
        // Keep last N messages for context
        if keep_last_n > 0 && !self.messages.is_empty() {
            let start = if self.messages.len() > keep_last_n { self.messages.len() - keep_last_n } else { 0 };
            new_sess.messages = self.messages[start..].to_vec();
        }
        new_sess
    }

    pub fn transfer_runtime(&mut self, new_runtime_id: String) {
        // Transfer session to new runtime (e.g., workbench -> task)
        self.runtime_id = new_runtime_id;
        self.updated_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    }
}
