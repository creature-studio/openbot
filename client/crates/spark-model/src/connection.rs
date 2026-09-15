use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub host: String,
    pub port: u16,
    pub version: Option<String>,
    pub runtime_count: usize,
}
