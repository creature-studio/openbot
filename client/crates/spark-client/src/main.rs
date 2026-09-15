//! Spark GPUI Client
//!
//! A native Rust AI agent workbench built on GPUI (Zed's GPU-accelerated UI).
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ Spark                                      Connected ●    ⌘K   │
//! ├──────────────┬──────────────────────────────┬───────────────────┤
//! │              │                              │                   │
//! │ BOT          │     修复登录页面              │   Browser         │
//! │ ● Coding Bot │                              │                   │
//! │              │ 用户：                       │  localhost:3000   │
//! │ TASKS        │ 帮我修复登录异常              │                   │
//! │              │                              │  [browser view]   │
//! │ ▶ Login Bug  │ ● 正在工作                   │                   │
//! │ ✓ API Fix    │                              ├───────────────────┤
//! │ ! Deploy     │ ▼ 搜索文件        16ms       │  Files            │
//! │              │   src/login...               │                   │
//! │ WORKBENCH    │                              │  Changes           │
//! │              │ ▼ 读取文件        3ms        │                   │
//! │ Project A    │                              │  Terminal         │
//! │              │ ▼ 修改文件        12ms       │  Runtime          │
//! │ MACHINES     │   +12 -3                     │                   │
//! │ ● Local      │                              │  devbox ●         │
//! │ ◐ devbox     │ ▶ cargo test                 │  Ubuntu 24.04     │
//! │ + Machine    │                              │  8 cores 31.2 GB  │
//! ├──────────────┴──────────────────────────────┴───────────────────┤
//! │  Ask Spark...                    machine: [devbox ▾]  Stop  Send │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Where the machines live
//!
//! This binary never speaks SSH (architecture §一). It owns one connection to
//! `host-agent serve` over a Unix socket, sends [`TransportCommand`]s and folds
//! the events back into [`AppState`]. Adding a machine, trusting a host key,
//! bootstrapping sandd, creating a runtime on `devbox` — all of it happens
//! behind that socket.

use std::path::PathBuf;

mod app;

use anyhow::{Context, Result};
use spark_model::*;
use spark_transport::{TransportCommand, TransportEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Where host-agent listens, mirroring `host_agent::serve::default_socket_path`.
fn host_agent_socket() -> PathBuf {
    if let Ok(path) = std::env::var("SPARK_HOST_AGENT_SOCKET") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".cache")
        .join("spark")
        .join("run")
        .join("host-agent.sock")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spark_client=info,spark_transport=info".into()),
        )
        .init();

    let socket = host_agent_socket();
    println!("Spark client → host-agent at {}", socket.display());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?;

    // The link to host-agent runs on its own runtime; the GPUI app runs on the
    // main thread below.
    runtime.spawn(async move {
        if let Err(e) = run_host_agent_link(socket).await {
            tracing::error!("host-agent link failed: {e:#}");
        }
    });

    // GPUI owns the main thread; `AppState` is the entity tree the views read.
    spark_ui::run_app();
    Ok(())
}

/// One long-lived connection to host-agent: commands out, events in.
async fn run_host_agent_link(socket: PathBuf) -> Result<()> {
    let stream = UnixStream::connect(&socket)
        .await
        .with_context(|| format!("connecting to {} (is `host-agent serve` running?)", socket.display()))?;
    let (reader, mut writer) = stream.into_split();

    // The UI side of the same link.
    let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel::<TransportCommand>();
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Commands → JSON lines.
    tokio::spawn(async move {
        while let Some(command) = command_rx.recv().await {
            let Some(line) = command_to_json(&command) else {
                continue;
            };
            if writer.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    // JSON lines → events.
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if event_tx.send(line).is_err() {
                break;
            }
        }
    });

    // Fan out: refresh_machines needs a follow-up command, everything else is a
    // straight translation into `TransportEvent`.
    while let Some(line) = event_rx.recv().await {
        match event_to_transport_event(&line) {
            EventAction::Emit(event) => {
                spark_ui::push_event(event);
            }
            EventAction::RefreshMachines => {
                let _ = command_tx.send(TransportCommand::ListMachines);
            }
            EventAction::Ignore => {}
        }
    }
    Ok(())
}

enum EventAction {
    Emit(TransportEvent),
    RefreshMachines,
    Ignore,
}

/// Translate one host-agent line into a client event.
fn event_to_transport_event(line: &str) -> EventAction {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return EventAction::Ignore,
    };
    let event = value.get("event").and_then(|v| v.as_str()).unwrap_or_default();
    match event {
        "machines" => match serde_json::from_value::<Vec<Machine>>(value["machines"].clone()) {
            Ok(machines) => EventAction::Emit(TransportEvent::MachinesUpdated { machines }),
            Err(_) => EventAction::Ignore,
        },
        "machine_metadata" => match serde_json::from_value::<Machine>(value["machine"].clone()) {
            Ok(machine) => EventAction::Emit(TransportEvent::MachineMetadataUpdated { machine }),
            Err(_) => EventAction::Ignore,
        },
        "machine_status" => {
            let machine_id = MachineId::from_string(
                value
                    .get("machine_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            );
            let status = match value.get("status").and_then(|v| v.as_str()) {
                Some("connected") => MachineStatus::Connected,
                Some("degraded") => MachineStatus::Degraded,
                Some("connecting") => MachineStatus::Connecting,
                Some("bootstrapping") => MachineStatus::Bootstrapping,
                Some("unreachable") => MachineStatus::Unreachable,
                Some("requires_user_action") => MachineStatus::RequiresUserAction,
                Some("error") => MachineStatus::Error,
                Some("disconnected") | _ => MachineStatus::Disconnected,
            };
            EventAction::Emit(TransportEvent::MachineStatusChanged {
                machine_id,
                status,
                detail: value
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            })
        }
        "machine_attention" => EventAction::Emit(TransportEvent::MachineAttentionRequired {
            machine_id: MachineId::from_string(
                value
                    .get("machine_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            message: value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            fingerprint: value
                .get("fingerprint")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }),
        "machine_tested" => EventAction::Emit(TransportEvent::MachineTested {
            machine: MachineKind::Local, // the draft is still a draft: no id yet
            outcome: spark_transport::MachineTestOutcome {
                ok: value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
                message: value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                fingerprint: value
                    .get("fingerprint")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                requires_user_action: value
                    .get("requires_user_action")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            },
        }),
        "bootstrap_finished" => EventAction::Emit(TransportEvent::MachineBootstrapFinished {
            machine_id: MachineId::from_string(
                value
                    .get("machine_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            report: serde_json::from_value(value["report"].clone()).unwrap_or(
                spark_transport::BootstrapSummary {
                    platform: "unknown".into(),
                    version: "unknown".into(),
                    installed_binary: String::new(),
                    socket: String::new(),
                    data_dir: String::new(),
                    log_file: String::new(),
                    already_running: false,
                },
            ),
        }),
        "runtimes" => {
            let machine_id = MachineId::from_string(
                value
                    .get("machine_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            );
            match serde_json::from_value::<Vec<spark_transport::RuntimeInfo>>(
                value["runtimes"].clone(),
            ) {
                Ok(runtimes) => {
                    EventAction::Emit(TransportEvent::RuntimesUpdated { machine_id, runtimes })
                }
                Err(_) => EventAction::Ignore,
            }
        }
        "reconnected" => {
            // Recovery: the remote runtimes survived, so the client re-attaches
            // rather than reporting a failure.
            let machine_id = MachineId::from_string(
                value
                    .get("machine_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            );
            let count = value["runtimes"].as_array().map(|a| a.len()).unwrap_or(0);
            EventAction::Emit(TransportEvent::Error {
                task_id: None,
                message: format!(
                    "{} 已重新连接，远端仍有 {} 个 runtime 存活，已重新挂载。",
                    machine_id.as_str(),
                    count
                ),
                recoverable: true,
            })
        }
        "refresh_machines" => EventAction::RefreshMachines,
        "error" => EventAction::Emit(TransportEvent::Error {
            task_id: None,
            message: value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown host-agent error")
                .to_string(),
            recoverable: true,
        }),
        _ => EventAction::Ignore,
    }
}

/// Translate a UI command into the host-agent line protocol.
fn command_to_json(command: &TransportCommand) -> Option<String> {
    let line = match command {
        TransportCommand::ListMachines => r#"{"cmd":"list_machines"}"#.to_string(),
        TransportCommand::AddMachine(draft) => format!(
            r#"{{"cmd":"add_machine","name":"{}","host":"{}","alias":{},"user":{},"port":{}}}"#,
            escape(&draft.name),
            escape(&draft.host),
            json_string_or_null(draft.ssh_config_host.as_deref()),
            json_string_or_null(draft.user.as_deref()),
            draft.port
        ),
        TransportCommand::TestMachineConnection(draft) => format!(
            r#"{{"cmd":"test_machine","name":"{}","host":"{}","alias":{},"user":{},"port":{}}}"#,
            escape(&draft.name),
            escape(&draft.host),
            json_string_or_null(draft.ssh_config_host.as_deref()),
            json_string_or_null(draft.user.as_deref()),
            draft.port
        ),
        TransportCommand::RemoveMachine(id) => machine_command("remove_machine", id),
        TransportCommand::ConnectMachine(id) => machine_command("connect_machine", id),
        TransportCommand::DisconnectMachine(id) => machine_command("disconnect_machine", id),
        TransportCommand::ReconnectMachine(id) => machine_command("reconnect_machine", id),
        TransportCommand::BootstrapMachine(id) => machine_command("bootstrap_machine", id),
        TransportCommand::TrustMachineHostKey(id) => machine_command("trust_host_key", id),
        TransportCommand::RefreshMachineStatus(id) => machine_command("refresh_status", id),
        TransportCommand::ListRuntimes { machine_id } => machine_command("list_runtimes", machine_id),
        TransportCommand::DestroyRuntime {
            machine_id,
            runtime_id,
        } => format!(
            r#"{{"cmd":"destroy_runtime","machine_id":"{}","runtime_id":"{}"}}"#,
            escape(machine_id.as_str()),
            escape(runtime_id)
        ),
        TransportCommand::CreateRuntime {
            machine_id,
            kind,
            workspace,
        } => format!(
            r#"{{"cmd":"create_runtime","machine_id":"{}","kind":"{}","workspace":"{}"}}"#,
            escape(machine_id.as_str()),
            escape(kind),
            escape(workspace)
        ),
        TransportCommand::Connect => r#"{"cmd":"list_machines"}"#.to_string(),
        // Task / terminal / browser commands travel on the same socket once the
        // session layer is wired in (they need a runtime id, which arrives with
        // the MachinesUpdated reply).
        _ => return None,
    };
    Some(line)
}

fn machine_command(cmd: &str, machine_id: &MachineId) -> String {
    format!(
        r#"{{"cmd":"{}","machine_id":"{}"}}"#,
        cmd,
        escape(machine_id.as_str())
    )
}

fn json_string_or_null(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", escape(value)),
        None => "null".to_string(),
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_machine_command_carries_coordinates_only() {
        let draft = spark_transport::MachineDraft::new("devbox", "10.0.0.42")
            .with_user("ubuntu")
            .with_port(2222);
        let line = command_to_json(&TransportCommand::AddMachine(draft)).expect("line");
        assert!(line.contains("\"cmd\":\"add_machine\""));
        assert!(line.contains("\"port\":2222"));
        // No key material, ever.
        assert!(!line.contains("key"));
        assert!(!line.contains("password"));
    }

    #[test]
    fn machine_status_maps_onto_the_model() {
        let line = r#"{"event":"machine_status","machine_id":"mach-1","status":"degraded","detail":null}"#;
        match event_to_transport_event(line) {
            EventAction::Emit(TransportEvent::MachineStatusChanged { status, .. }) => {
                assert_eq!(status, MachineStatus::Degraded)
            }
            _ => panic!("expected a status event"),
        }
    }

    #[test]
    fn attention_events_keep_the_fingerprint() {
        let line = r#"{"event":"machine_attention","machine_id":"mach-1","message":"unknown host key","fingerprint":"SHA256:abc","changed":false}"#;
        match event_to_transport_event(line) {
            EventAction::Emit(TransportEvent::MachineAttentionRequired { fingerprint, .. }) => {
                assert_eq!(fingerprint.as_deref(), Some("SHA256:abc"))
            }
            _ => panic!("expected an attention event"),
        }
    }

    #[test]
    fn added_machine_triggers_a_refresh() {
        match event_to_transport_event(r#"{"event":"refresh_machines"}"#) {
            EventAction::RefreshMachines => {}
            _ => panic!("expected a refresh"),
        }
    }
}
