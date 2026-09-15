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
    pub id: AgentSessionId,
    pub runtime_id: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub tool_state: HashMap<String, String>,
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
            id: AgentSessionId::new(),
            runtime_id,
            model,
            messages: Vec::new(),
            tool_state: HashMap::new(),
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
}
