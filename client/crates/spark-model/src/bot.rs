use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BotId(pub String);

impl BotId {
    pub fn new() -> Self {
        Self(format!("bot-{}", uuid::Uuid::new_v4()))
    }
}

impl std::fmt::Display for BotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bot {
    pub id: BotId,
    pub name: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub session_ids: Vec<String>,
    pub runtime_ids: Vec<String>,
}
