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

use std::collections::VecDeque;
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

/// Ensure display environment variables are valid before GPUI initializes.
///
/// On Linux/WSL2, `WAYLAND_DISPLAY` is often set by the environment while
/// `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` does not actually exist, causing
/// GPUI's `WaylandClient::new()` to panic with `NoCompositor`. In that case,
/// falling back to X11 (or fixing `XDG_RUNTIME_DIR`) prevents the crash.
fn sanitize_display_env() {
    #[cfg(target_os = "linux")]
    {
        if let Ok(wayland_display) = std::env::var("WAYLAND_DISPLAY") {
            if !wayland_display.is_empty() {
                let socket_exists = if wayland_display.starts_with('/') {
                    std::path::Path::new(&wayland_display).exists()
                } else if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR") {
                    std::path::Path::new(&xdg_runtime).join(&wayland_display).exists()
                } else {
                    false
                };

                if !socket_exists {
                    // Wayland socket does not exist at standard XDG_RUNTIME_DIR.
                    // If DISPLAY is set (e.g. WSLg X11 server :0), unset WAYLAND_DISPLAY
                    // so GPUI smoothly falls back to X11 instead of panicking.
                    if std::env::var("DISPLAY").map_or(false, |d| !d.is_empty()) {
                        unsafe {
                            std::env::remove_var("WAYLAND_DISPLAY");
                        }
                    } else if std::path::Path::new("/mnt/wslg/runtime-dir").join(&wayland_display).exists() {
                        unsafe {
                            std::env::set_var("XDG_RUNTIME_DIR", "/mnt/wslg/runtime-dir");
                        }
                    }
                }
            }
        }

        // On WSL2 or headless systems without native DRM devices (/dev/dri), Mesa's
        // EGL loader attempts to probe hardware DRI and Zink (OpenGL-over-Vulkan)
        // which fail with fd -1 and "ZINK: failed to choose pdev" before falling back
        // to software rendering. Setting LIBGL_ALWAYS_SOFTWARE=1 silences those errors
        // and avoids failed probe cycles unless explicitly overridden.
        if !std::path::Path::new("/dev/dri").exists() && std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none() {
            unsafe {
                std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
            }
        }
    }
}

fn main() -> Result<()> {
    sanitize_display_env();

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

/// One reconnecting connection to host-agent: commands out, events in. The
/// client owns no machine transport; this loop only maintains the local UDS
/// link and retries it with bounded backoff after host-agent restarts.
async fn run_host_agent_link(socket: PathBuf) -> Result<()> {
    let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel::<TransportCommand>();
    let command_sink = command_tx.clone();
    spark_ui::install_command_sender(move |command| {
        let _ = command_sink.send(command);
    });

    type Reader = tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>;
    let mut reader: Option<Reader> = None;
    let mut writer: Option<tokio::net::unix::OwnedWriteHalf> = None;
    let mut pending_lines: VecDeque<String> = VecDeque::new();
    let mut retry: usize = 0;
    let mut ever_connected = false;

    loop {
        if reader.is_none() || writer.is_none() {
            if ever_connected {
                let seconds = [1_u64, 2, 4, 8, 16, 30][retry.min(5)];
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                retry = retry.saturating_add(1);
            }
            match UnixStream::connect(&socket).await {
                Ok(stream) => {
                    let (read_half, write_half) = stream.into_split();
                    reader = Some(BufReader::new(read_half).lines());
                    writer = Some(write_half);
                    retry = 0;
                    ever_connected = true;
                    spark_ui::push_event(TransportEvent::Connected);
                    pending_lines.push_back(r#"{"cmd":"list_machines"}"#.to_string());
                }
                Err(error) => {
                    tracing::warn!("host-agent link unavailable at {}: {error}", socket.display());
                    continue;
                }
            }
        }

        while let Some(line) = pending_lines.pop_front() {
            let Some(current_writer) = writer.as_mut() else {
                pending_lines.push_front(line);
                break;
            };
            if current_writer.write_all(line.as_bytes()).await.is_err()
                || current_writer.write_all(b"\n").await.is_err()
            {
                reader = None;
                writer = None;
                spark_ui::push_event(TransportEvent::Disconnected {
                    reason: Some("host-agent link write failed".into()),
                });
                pending_lines.push_front(line);
                break;
            }
        }
        if reader.is_none() {
            continue;
        }

        tokio::select! {
            command = command_rx.recv() => {
                let Some(command) = command else { return Ok(()); };
                let Some(line) = command_to_json(&command) else { continue; };
                pending_lines.push_back(line);
            }
            result = reader.as_mut().expect("reader present").next_line() => {
                match result {
                    Ok(Some(line)) => match event_to_transport_event(&line) {
                        EventAction::Emit(event) => spark_ui::push_event(event),
                        EventAction::RefreshMachines => pending_lines.push_back(r#"{"cmd":"list_machines"}"#.to_string()),
                        EventAction::Ignore => {}
                    },
                    Ok(None) | Err(_) => {
                        reader = None;
                        writer = None;
                        spark_ui::push_event(TransportEvent::Disconnected {
                            reason: Some("host-agent link closed".into()),
                        });
                    }
                }
            }
        }
    }
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
        "connected" => EventAction::Emit(TransportEvent::Connected),
        "disconnected" => EventAction::Emit(TransportEvent::Disconnected {
            reason: value.get("reason").and_then(|v| v.as_str()).map(str::to_string),
        }),
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
        "bootstrap_progress" => EventAction::Emit(TransportEvent::MachineBootstrapProgress {
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
        "task_created" => EventAction::Emit(TransportEvent::TaskCreated {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            goal: value.get("goal").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            machine_id: value.get("machine_id").and_then(|v| v.as_str()).map(|id| MachineId::from_string(id.to_string())),
            runtime_id: value.get("runtime_id").and_then(|v| v.as_str()).map(str::to_string),
        }),
        "task_status" => EventAction::Emit(TransportEvent::TaskStatusChanged {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            status: value.get("status").and_then(|v| v.as_str()).unwrap_or("pending").to_string(),
        }),
        "task_stopped" => EventAction::Emit(TransportEvent::TaskStatusChanged {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            status: "cancelled".to_string(),
        }),
        "assistant_streaming" => EventAction::Emit(TransportEvent::AssistantStreaming {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            message_id: value.get("message_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            delta: value.get("delta").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        }),
        "assistant_message" => EventAction::Emit(TransportEvent::AssistantMessage {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            message_id: value.get("message_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            content: value.get("content").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            streaming: value.get("streaming").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "tool_started" => EventAction::Emit(TransportEvent::ToolStarted {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            call_id: value.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            tool_name: value.get("tool_name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            args_json: value.get("args_json").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        }),
        "tool_output" => EventAction::Emit(TransportEvent::ToolOutput {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            call_id: value.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            delta: value.get("delta").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        }),
        "tool_finished" => EventAction::Emit(TransportEvent::ToolFinished {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            call_id: value.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            tool_name: value.get("tool_name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            status: value.get("status").and_then(|v| v.as_str()).unwrap_or("error").to_string(),
            result: value.get("result").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            duration_ms: value.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0),
        }),
        "permission_required" => EventAction::Emit(TransportEvent::PermissionRequired {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            permission_id: value.get("permission_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            tool_name: value.get("tool_name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            reason: value.get("reason").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        }),
        "terminal_output" => {
            let Some(data) = value.get("data").and_then(|v| v.as_str()).and_then(base64_decode) else {
                return EventAction::Ignore;
            };
            EventAction::Emit(TransportEvent::TerminalOutput {
                task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                terminal_id: value.get("terminal_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                data,
            })
        }
        "terminal_exit" => EventAction::Emit(TransportEvent::TerminalExit {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            terminal_id: value.get("terminal_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            code: value.get("code").and_then(|v| v.as_i64()).map(|code| code as i32),
        }),
        "ready_for_check" => EventAction::Emit(TransportEvent::ReadyForCheck {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            summary: value.get("summary").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            files_changed: value.get("files_changed").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            tests_passed: value.get("tests_passed").and_then(|v| v.as_bool()).unwrap_or(false),
            browser_verified: value.get("browser_verified").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "permission_resolved" => EventAction::Emit(TransportEvent::PermissionResolved {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            permission_id: value.get("permission_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            allowed: value.get("allowed").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "check_confirmed" => EventAction::Emit(TransportEvent::CheckConfirmed {
            task_id: value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        }),
        "browser_frame" | "computer_frame" => {
            let runtime_id = value
                .get("runtime_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let frame_id = value.get("frame_id").and_then(|v| v.as_u64()).unwrap_or(0);
            let width = value.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let height = value.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let format = value
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("png")
                .to_string();
            let Some(data) = value.get("data").and_then(|v| v.as_str()).and_then(base64_decode) else {
                return EventAction::Ignore;
            };
            if event == "browser_frame" {
                EventAction::Emit(TransportEvent::BrowserFrame {
                    runtime_id,
                    frame_id,
                    width,
                    height,
                    format,
                    data,
                })
            } else {
                EventAction::Emit(TransportEvent::ComputerFrame {
                    runtime_id,
                    frame_id,
                    width,
                    height,
                    format,
                    data,
                })
            }
        }
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
        TransportCommand::Connect => r#"{"cmd":"connect"}"#.to_string(),
        TransportCommand::Disconnect => r#"{"cmd":"disconnect"}"#.to_string(),
        TransportCommand::OpenTerminal { machine_id, runtime_id, terminal_id, cols, rows } => format!(
            r#"{{"cmd":"open_terminal","machine_id":"{}","runtime_id":"{}","terminal_id":"{}","cols":{},"rows":{}}}"#,
            escape(machine_id.as_str()), escape(runtime_id), escape(terminal_id), cols, rows
        ),
        TransportCommand::WriteTerminal { machine_id, runtime_id, terminal_id, data } => format!(
            r#"{{"cmd":"write_terminal","machine_id":"{}","runtime_id":"{}","terminal_id":"{}","data":"{}"}}"#,
            escape(machine_id.as_str()), escape(runtime_id), escape(terminal_id), escape(&String::from_utf8_lossy(data))
        ),
        TransportCommand::ResizeTerminal { machine_id, runtime_id, terminal_id, cols, rows } => format!(
            r#"{{"cmd":"resize_terminal","machine_id":"{}","runtime_id":"{}","terminal_id":"{}","cols":{},"rows":{}}}"#,
            escape(machine_id.as_str()), escape(runtime_id), escape(terminal_id), cols, rows
        ),
        TransportCommand::CloseTerminal { machine_id, runtime_id, terminal_id } => format!(
            r#"{{"cmd":"close_terminal","machine_id":"{}","runtime_id":"{}","terminal_id":"{}"}}"#,
            escape(machine_id.as_str()), escape(runtime_id), escape(terminal_id)
        ),
        TransportCommand::CreateTask { machine_id, goal } => format!(
            r#"{{"cmd":"create_task","machine_id":"{}","goal":"{}"}}"#,
            escape(machine_id.as_str()), escape(goal)
        ),
        TransportCommand::SendTaskMessage { task_id, content } => format!(
            r#"{{"cmd":"send_task_message","task_id":"{}","content":"{}"}}"#,
            escape(task_id), escape(content)
        ),
        TransportCommand::StopTask { task_id } => format!(r#"{{"cmd":"stop_task","task_id":"{}"}}"#, escape(task_id)),
        TransportCommand::ApprovePermission { task_id, permission_id } => format!(r#"{{"cmd":"approve_permission","task_id":"{}","permission_id":"{}"}}"#, escape(task_id), escape(permission_id)),
        TransportCommand::DenyPermission { task_id, permission_id } => format!(r#"{{"cmd":"deny_permission","task_id":"{}","permission_id":"{}"}}"#, escape(task_id), escape(permission_id)),
        TransportCommand::ConfirmComplete { task_id } => format!(r#"{{"cmd":"confirm_complete","task_id":"{}"}}"#, escape(task_id)),
        TransportCommand::BrowserAction { runtime_id, action } => browser_command(runtime_id, action),
        TransportCommand::ComputerAction { runtime_id, action } => computer_command(runtime_id, action),
        TransportCommand::SubscribeBrowser { runtime_id } => format!(r#"{{"cmd":"subscribe_browser","runtime_id":"{}"}}"#, escape(runtime_id)),
        TransportCommand::UnsubscribeBrowser { runtime_id } => format!(r#"{{"cmd":"unsubscribe_browser","runtime_id":"{}"}}"#, escape(runtime_id)),
        TransportCommand::Shutdown => r#"{"cmd":"shutdown"}"#.to_string(),
    };
    Some(line)
}

fn browser_command(runtime_id: &str, action: &spark_transport::BrowserAction) -> String {
    let (kind, fields) = match action {
        spark_transport::BrowserAction::Open { url } => ("open", format!(r#""url":"{}""#, escape(url))),
        spark_transport::BrowserAction::Snapshot => ("snapshot", String::new()),
        spark_transport::BrowserAction::Click { reference } => ("click", format!(r#""ref":"{}""#, escape(reference))),
        spark_transport::BrowserAction::Fill { reference, text } => ("fill", format!(r#""ref":"{}","text":"{}""#, escape(reference), escape(text))),
        spark_transport::BrowserAction::Press { key } => ("press", format!(r#""key":"{}""#, escape(key))),
        spark_transport::BrowserAction::Screenshot { format, quality } => ("screenshot", format!(r#""format":"{}","quality":{}"#, escape(format), quality)),
        spark_transport::BrowserAction::Tabs => ("tabs", String::new()),
        spark_transport::BrowserAction::Close => ("close", String::new()),
    };
    if fields.is_empty() {
        format!(r#"{{"cmd":"browser_action","runtime_id":"{}","action":"{}"}}"#, escape(runtime_id), kind)
    } else {
        format!(r#"{{"cmd":"browser_action","runtime_id":"{}","action":"{}","{}"}}"#, escape(runtime_id), kind, fields)
    }
}

fn computer_command(runtime_id: &str, action: &spark_transport::ComputerAction) -> String {
    let (kind, fields) = match action {
        spark_transport::ComputerAction::Screenshot => ("screenshot", String::new()),
        spark_transport::ComputerAction::Click { x, y, button } => ("click", format!(r#""x":{},"y":{},"button":"{}""#, x, y, escape(button))),
        spark_transport::ComputerAction::Type { text } => ("type", format!(r#""text":"{}""#, escape(text))),
        spark_transport::ComputerAction::Move { x, y } => ("move", format!(r#""x":{},"y":{}"#, x, y)),
        spark_transport::ComputerAction::Key { key } => ("key", format!(r#""key":"{}""#, escape(key))),
        spark_transport::ComputerAction::Scroll { x, y, delta } => ("scroll", format!(r#""x":{},"y":{},"delta":{}"#, x, y, delta)),
    };
    if fields.is_empty() {
        format!(r#"{{"cmd":"computer_action","runtime_id":"{}","action":"{}"}}"#, escape(runtime_id), kind)
    } else {
        format!(r#"{{"cmd":"computer_action","runtime_id":"{}","action":"{}","{}"}}"#, escape(runtime_id), kind, fields)
    }
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

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut values = Vec::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' => values.push(byte - b'A'),
            b'a'..=b'z' => values.push(byte - b'a' + 26),
            b'0'..=b'9' => values.push(byte - b'0' + 52),
            b'+' => values.push(62),
            b'/' => values.push(63),
            b'=' => break,
            b'\n' | b'\r' | b' ' | b'\t' => {}
            _ => return None,
        }
    }
    if values.len() < 2 {
        return Some(Vec::new());
    }
    let mut output = Vec::with_capacity(values.len() / 4 * 3);
    for chunk in values.chunks(4) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let d = chunk.get(3).copied().unwrap_or(0) as u32;
        output.push(((a << 2) | (b >> 4)) as u8);
        if chunk.len() > 2 {
            output.push(((b << 4) | (c >> 2)) as u8);
        }
        if chunk.len() > 3 {
            output.push(((c << 6) | d) as u8);
        }
    }
    Some(output)
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
    #[test]
    fn frame_events_decode_runtime_payloads() {
        let line = r#"{"event":"browser_frame","runtime_id":"rt-1","frame_id":7,"width":2,"height":1,"format":"png","data":"aGVsbG8="}"#;
        match event_to_transport_event(line) {
            EventAction::Emit(TransportEvent::BrowserFrame { runtime_id, frame_id, data, .. }) => {
                assert_eq!(runtime_id, "rt-1");
                assert_eq!(frame_id, 7);
                assert_eq!(data, b"hello".to_vec());
            }
            _ => panic!("expected a decoded browser frame"),
        }
    }

}
