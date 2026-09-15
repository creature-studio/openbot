use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuntimeId(pub String);

impl RuntimeId {
    pub fn new() -> Self {
        // simple random id using /dev/urandom or time+pid
        let mut buf = [0u8; 16];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut buf);
        } else {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let pid = std::process::id() as u128;
            let combined = now ^ (pid << 32);
            buf[..16].copy_from_slice(&combined.to_le_bytes()[..16.min(16)]);
        }
        // format as hex
        let hex: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
        Self(format!("rt-{}", &hex[..12]))
    }

    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RuntimeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Assistant,
    Task,
    Workbench,
    Eval,
}

impl RuntimeKind {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "task" => Self::Task,
            "workbench" => Self::Workbench,
            "eval" => Self::Eval,
            _ => Self::Assistant,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::Task => "task",
            Self::Workbench => "workbench",
            Self::Eval => "eval",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    Creating,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed { reason: String },
    Killed,
    Oom,
    Crashed { code: Option<i32>, signal: Option<i32> },
}

impl RuntimeState {
    pub fn as_str(&self) -> String {
        match self {
            Self::Creating => "creating".to_string(),
            Self::Starting => "starting".to_string(),
            Self::Running => "running".to_string(),
            Self::Stopping => "stopping".to_string(),
            Self::Stopped => "stopped".to_string(),
            Self::Failed { reason } => format!("failed:{}", reason),
            Self::Killed => "killed".to_string(),
            Self::Oom => "oom".to_string(),
            Self::Crashed { code, signal } => format!("crashed code={:?} signal={:?}", code, signal),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed { .. } | Self::Killed | Self::Oom | Self::Crashed { .. })
    }
}

#[derive(Debug, Clone)]
pub struct Runtime {
    pub id: RuntimeId,
    pub kind: RuntimeKind,
    pub state: RuntimeState,
    pub workspace: PathBuf,
    pub cgroup_path: Option<PathBuf>,
    pub created_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub capabilities: Vec<String>,
    pub process_count: usize,
    pub pty_count: usize,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: i32,
    pub command: Vec<String>,
    pub cwd: String,
    pub started_at_ms: u64,
    pub state: String, // running, exited, etc.
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub runtime_id: RuntimeId,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: Option<u64>,
    pub stdin_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub pid: i32,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub enum PtyMessageKind {
    Open { cols: u16, rows: u16, shell: String },
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Signal { signal: i32 },
    Close,
    Exit { code: Option<i32>, signal: Option<i32> },
}

#[derive(Debug, Clone)]
pub struct PtyMessage {
    pub runtime_id: RuntimeId,
    pub pty_id: String,
    pub kind: PtyMessageKind,
}

#[derive(Debug, Clone)]
pub enum RuntimeEventKind {
    Created,
    Started,
    Stopped,
    Destroyed,
    ProcessStarted { pid: i32 },
    ProcessExited { pid: i32, code: Option<i32> },
    PtyOpened { pty_id: String },
    PtyClosed { pty_id: String },
    Oom,
    Failed { reason: String },
}

impl RuntimeEventKind {
    pub fn as_str(&self) -> String {
        match self {
            Self::Created => "created".to_string(),
            Self::Started => "started".to_string(),
            Self::Stopped => "stopped".to_string(),
            Self::Destroyed => "destroyed".to_string(),
            Self::ProcessStarted { pid } => format!("process_started:{}", pid),
            Self::ProcessExited { pid, code } => format!("process_exited:{}:{:?}", pid, code),
            Self::PtyOpened { pty_id } => format!("pty_opened:{}", pty_id),
            Self::PtyClosed { pty_id } => format!("pty_closed:{}", pty_id),
            Self::Oom => "oom".to_string(),
            Self::Failed { reason } => format!("failed:{}", reason),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeEvent {
    pub runtime_id: RuntimeId,
    pub kind: RuntimeEventKind,
    pub at_ms: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

// Simple JSON-like serialization without serde for persistence
impl Runtime {
    pub fn to_json_line(&self) -> String {
        format!(
            "{{\"id\":\"{}\",\"kind\":\"{}\",\"state\":\"{}\",\"workspace\":\"{}\",\"cgroup\":\"{}\",\"created\":{},\"started\":{},\"caps\":[{}],\"procs\":{},\"ptys\":{}}}",
            self.id.0,
            self.kind.as_str(),
            self.state.as_str().replace('\"', "'"),
            self.workspace.display(),
            self.cgroup_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            self.created_at_ms,
            self.started_at_ms.unwrap_or(0),
            self.capabilities.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(","),
            self.process_count,
            self.pty_count
        )
    }
}
