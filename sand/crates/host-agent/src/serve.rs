//! `host-agent serve` — the link the GPUI client talks to.
//!
//! The client never opens an SSH connection (architecture §一). It connects to
//! this Unix socket, sends commands and receives events:
//!
//! ```text
//! GPUI client ──newline JSON──▶ host-agent serve ──MachineManager──▶ LocalTransport / SshTransport
//! ```
//!
//! Protocol (one JSON object per line, both directions):
//!
//! client → host-agent
//! ```json
//! {"cmd":"list_machines"}
//! {"cmd":"add_machine","name":"devbox","host":"10.0.0.42","user":"ubuntu","port":22,"alias":null}
//! {"cmd":"test_machine","name":"devbox","host":"10.0.0.42"}
//! {"cmd":"connect_machine","machine_id":"mach-…"}
//! {"cmd":"disconnect_machine","machine_id":"mach-…"}
//! {"cmd":"reconnect_machine","machine_id":"mach-…"}
//! {"cmd":"bootstrap_machine","machine_id":"mach-…"}
//! {"cmd":"trust_host_key","machine_id":"mach-…"}
//! {"cmd":"refresh_status","machine_id":"mach-…"}
//! {"cmd":"list_runtimes","machine_id":"mach-…"}
//! {"cmd":"destroy_runtime","machine_id":"mach-…","runtime_id":"rt-…"}
//! {"cmd":"shutdown"}
//! ```
//!
//! host-agent → client (broadcast to every connection)
//! ```json
//! {"event":"machines","machines":[ … ]}
//! {"event":"machine_status","machine_id":"…","status":"connected","detail":null}
//! {"event":"machine_metadata","machine":{ … }}
//! {"event":"machine_attention","machine_id":"…","message":"…","fingerprint":"SHA256:…","changed":false}
//! {"event":"machine_tested","ok":false,"message":"…","fingerprint":"…","requires_user_action":true}
//! {"event":"bootstrap_finished","machine_id":"…","report":{ … }}
//! {"event":"bootstrap_progress","machine_id":"…","message":"…"}
//! {"event":"runtimes","machine_id":"…","runtimes":[ … ]}
//! {"event":"ok","cmd":"connect_machine"}
//! {"event":"error","cmd":"connect_machine","message":"…"}
//! ```
//!
//! The socket lives at `$SPARK_HOST_AGENT_SOCKET`, else
//! `~/.cache/spark/run/host-agent.sock`, and is created `0600` inside a `0700`
//! directory: only the user can drive their own machines.

use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};

use spark_model::{MachineId, MachineStatus};
use spark_transport::{BootstrapSummary, RuntimeInfo};
use spark_transport::MachineDraft;

use crate::agent::{AgentSession, AgentLoop, TaskManager};
use crate::machine::{MachineEvent, MachineManager};
use crate::model::{Model, MockModel, OpenAICompatibleModel};
use crate::tools::ToolExecutionContext;

/// Where the client socket lives.
pub fn default_socket_path() -> PathBuf {
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

/// State shared by all GPUI socket clients. Sessions/tasks are host-agent
/// state, not GPUI state, so closing the client does not lose remote work.
struct ServeState {
    sessions: Mutex<HashMap<String, AgentSession>>,
    tasks: Mutex<TaskManager>,
    /// One cancellation flag per running task. The flag is shared with the
    /// blocking AgentLoop and is never used to destroy the runtime.
    cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Messages submitted while a worker is between model/tool calls. They
    /// are appended to the durable session and picked up by the next worker
    /// turn, rather than racing a mutable session clone in another thread.
    pending_messages: Mutex<HashMap<String, Vec<String>>>,
}

impl ServeState {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            tasks: Mutex::new(TaskManager::new()),
            cancel_flags: Mutex::new(HashMap::new()),
            pending_messages: Mutex::new(HashMap::new()),
        }
    }
}

/// Serve until `shutdown` or SIGTERM.
pub async fn serve(manager: Arc<MachineManager>, socket_path: Option<PathBuf>) -> Result<()> {
    let state = Arc::new(ServeState::new());
    let socket_path = socket_path.unwrap_or_else(default_socket_path);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        restrict_dir(parent)?;
    }
    // A stale socket from a previous run would make bind() fail.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding {}", socket_path.display()))?;
    restrict_socket(&socket_path)?;
    tracing::info!("host-agent listening on {}", socket_path.display());

    // One channel per connection; every manager event is fanned out to all of
    // them so two windows stay in sync.
    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel::<String>(512);

    // Manager events → broadcast.
    {
        let broadcast_tx = broadcast_tx.clone();
        manager.subscribe(Arc::new(move |event: MachineEvent| {
            let line = machine_event_json(&event);
            let _ = broadcast_tx.send(line);
        }));
    }

    // Recover saved remote machines in the background. A host-agent restart must
    // not turn an existing remote runtime into a new task: reconnect the bridge,
    // then publish the machine's runtime inventory so the client can reattach.
    {
        let manager = manager.clone();
        let broadcast_tx = broadcast_tx.clone();
        tokio::spawn(async move {
            for machine in manager.list_machines() {
                if matches!(machine.kind, spark_model::MachineKind::Local) {
                    continue;
                }
                if manager.connect_machine(&machine.id).await.is_ok() {
                    if let Ok(runtimes) = manager.list_runtimes(&machine.id).await {
                        let _ = broadcast_tx.send(runtimes_json(&machine.id, &runtimes));
                    }
                }
            }
        });
    }

    // Health loop: status dots must stay honest without the client asking.
    {
        let manager = manager.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                ticker.tick().await;
                // `set_status` inside the manager emits StatusChanged, which the
                // subscriber above broadcasts — no duplicate event here.
                let _ = manager.health_check_once().await;
                manager.schedule_reconnects().await;
            }
        });
    }

    loop {
        let (stream, _addr) = listener.accept().await.context("accepting a client")?;
        let manager = manager.clone();
        let state = state.clone();
        let mut broadcast_rx = broadcast_rx.resubscribe();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            // Writer: broadcast events + this connection's own replies.
            let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();
            let writer_task = tokio::spawn(async move {
                loop {
                    let line = tokio::select! {
                        Ok(line) = broadcast_rx.recv() => line,
                        Some(line) = reply_rx.recv() => line,
                        else => break,
                    };
                    if writer.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    if writer.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
            });

            // Send the current state immediately: the client renders the sidebar
            // without asking.
            let _ = reply_tx.send(machines_json(&manager));

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if let Some(reply) = handle_command(&manager, &state, &broadcast_tx, &line).await {
                    if reply == "__shutdown__" {
                        let _ = reply_tx.send(
                            "{\"event\":\"ok\",\"cmd\":\"shutdown\"}".to_string(),
                        );
                        break;
                    }
                    let _ = reply_tx.send(reply);
                }
            }

            drop(reply_tx);
            let _ = writer_task.await;
        });
    }
}

/// Dispatch one command line. Returns the reply line, when there is one.
async fn handle_command(
    manager: &Arc<MachineManager>,
    state: &Arc<ServeState>,
    broadcast_tx: &tokio::sync::broadcast::Sender<String>,
    line: &str,
) -> Option<String> {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => {
            return Some(format!(
                "{{\"event\":\"error\",\"message\":\"bad json: {}\"}}",
                escape(&e.to_string())
            ))
        }
    };
    let cmd = value
        .get("cmd")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    match cmd.as_str() {
        "connect" => Some(machines_json(manager)),
        // The GPUI-to-host-agent UDS is owned by spark-client; disconnecting
        // that logical link must not tear down SSH machines or remote runtimes.
        "disconnect" => Some(ok_json(&cmd)),
        "list_machines" => Some(machines_json(manager)),
        "add_machine" => {
            let Some(draft) = draft_from_json(&value) else {
                return Some(error_json(&cmd, "machine needs a name and a host"));
            };
            match manager.add_machine(&draft).await {
                Ok(machine) => {
                    // The client learns the new list from the Added event, but a
                    // direct reply makes the form's state deterministic.
                    let _ = machine;
                    Some(machines_json(manager))
                }
                Err(e) => Some(error_json(&cmd, &e.to_string())),
            }
        }
        "test_machine" => {
            let Some(draft) = draft_from_json(&value) else {
                return Some(error_json(&cmd, "machine needs a name and a host"));
            };
            match manager.test_connection(&draft).await {
                Ok(()) => Some(
                    "{\"event\":\"machine_tested\",\"ok\":true,\"message\":\"ssh ok\",\"fingerprint\":null,\"requires_user_action\":false}"
                        .to_string(),
                ),
                Err(error) => Some(format!(
                    "{{\"event\":\"machine_tested\",\"ok\":false,\"message\":\"{}\",\"fingerprint\":{},\"requires_user_action\":{}}}",
                    escape(&error.message()),
                    match error.host_key_issue() {
                        Some(issue) => format!("\"{}\"", escape(issue.fingerprint())),
                        None => "null".to_string(),
                    },
                    error.status() == MachineStatus::RequiresUserAction,
                )),
            }
        }
        "create_runtime" => {
            let machine_id = match value.get("machine_id").and_then(|v| v.as_str()) {
                Some(id) => MachineId::from_string(id.to_string()),
                None => return Some(error_json(&cmd, "machine_id is required")),
            };
            let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("assistant");
            let workspace = value.get("workspace").and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
            match manager.create_runtime(&machine_id, kind, workspace).await {
                Ok(_) => match manager.list_runtimes(&machine_id).await {
                    Ok(runtimes) => Some(runtimes_json(&machine_id, &runtimes)),
                    Err(e) => Some(error_json(&cmd, &e.to_string())),
                },
                Err(e) => Some(error_json(&cmd, &e.to_string())),
            }
        }
        "create_task" => {
            let machine_id = match value.get("machine_id").and_then(|v| v.as_str()) {
                Some(id) => MachineId::from_string(id.to_string()),
                None => return Some(error_json(&cmd, "machine_id is required")),
            };
            let goal = value.get("goal").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            match manager.create_runtime(&machine_id, "assistant", None).await {
                Ok(runtime) => {
                    let mut session = AgentSession::on_machine(machine_id.clone(), runtime.id.clone(), "default".into());
                    session.add_system_message(
                        "You are a helpful assistant. Use the registered runtime tools for shell, files, terminal, browser, and computer actions. Never assume the runtime is local; all tools are routed by its fixed machine ownership.".into(),
                    );
                    session.add_user_message(goal.clone());
                    let session_id = session.id.clone();
                    let created = state.tasks.lock().ok().and_then(|mut tasks| {
                        let task_id = tasks.create(goal.clone(), session.id.clone(), runtime.id.clone());
                        tasks.get(&task_id).cloned()
                    });
                    if let Some(task) = created {
                        manager.persist_session(&session);
                        manager.persist_task(&task, &machine_id);
                        let cancel_flag = Arc::new(AtomicBool::new(false));
                        if let Ok(mut flags) = state.cancel_flags.lock() {
                            flags.insert(task.id.clone(), cancel_flag.clone());
                        }
                        if let Ok(mut sessions) = state.sessions.lock() {
                            sessions.insert(session_id.clone(), session.clone());
                        }
                        spawn_agent_task(
                            manager.clone(),
                            state.clone(),
                            broadcast_tx.clone(),
                            task.id.clone(),
                            session_id.clone(),
                            session,
                            machine_id.clone(),
                            cancel_flag,
                        );
                        Some(format!("{{\"event\":\"task_created\",\"task_id\":\"{}\",\"session_id\":\"{}\",\"machine_id\":\"{}\",\"runtime_id\":\"{}\",\"goal\":\"{}\"}}", escape(&task.id), escape(&session_id), escape(machine_id.as_str()), escape(&runtime.id), escape(&goal)))
                    } else { Some(error_json(&cmd, "task state unavailable")) }
                }
                Err(e) => Some(error_json(&cmd, &e.to_string())),
            }
        }
        "send_task_message" => {
            let task_id = value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
            let content = value.get("content").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let task = state.tasks.lock().ok().and_then(|tasks| tasks.get(task_id).cloned());
            let Some(task) = task else { return Some(error_json(&cmd, "task not found")); };

            // AgentLoop owns a session snapshot while it is running. Queue a
            // follow-up instead of mutating that snapshot from the socket task;
            // the worker drains this queue at its checkpoint and starts the next
            // turn after persisting the current one.
            let worker_running = state
                .cancel_flags
                .lock()
                .ok()
                .map(|flags| flags.contains_key(task_id))
                .unwrap_or(false);
            let message_id = format!("user-{}-{}", task_id, unix_seconds());
            if worker_running {
                if let Ok(mut pending) = state.pending_messages.lock() {
                    pending.entry(task_id.to_string()).or_default().push(content.clone());
                }
                let _ = broadcast_tx.send(user_message_json(task_id, &message_id, &content));
                return Some(format!(
                    "{{\"event\":\"task_message_accepted\",\"task_id\":\"{}\",\"queued\":true}}",
                    escape(task_id)
                ));
            }

            let Some(mut session) = state
                .sessions
                .lock()
                .ok()
                .and_then(|sessions| sessions.get(&task.session_id).cloned())
            else {
                return Some(error_json(&cmd, "session not found"));
            };
            session.add_user_message(content.clone());
            let _ = broadcast_tx.send(user_message_json(task_id, &message_id, &content));
            if let Ok(mut sessions) = state.sessions.lock() {
                sessions.insert(session.id.clone(), session.clone());
            }
            if let Ok(mut tasks) = state.tasks.lock() {
                if let Some(task) = tasks.get_mut(task_id) {
                    task.status = crate::agent::state::TaskStatus::Running;
                    task.updated_at = unix_seconds();
                    manager.persist_task(task, &session.machine_id);
                }
            }
            manager.persist_session(&session);
            let cancel_flag = Arc::new(AtomicBool::new(false));
            if let Ok(mut flags) = state.cancel_flags.lock() {
                flags.insert(task_id.to_string(), cancel_flag.clone());
            }
            let machine_id = session.machine_id.clone();
            spawn_agent_task(
                manager.clone(),
                state.clone(),
                broadcast_tx.clone(),
                task_id.to_string(),
                session.id.clone(),
                session,
                machine_id,
                cancel_flag,
            );
            Some(format!(
                "{{\"event\":\"task_message_accepted\",\"task_id\":\"{}\",\"session_id\":\"{}\",\"queued\":false}}",
                escape(task_id),
                escape(&task.session_id.0)
            ))
        }
        "stop_task" => {
            let task_id = value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
            if let Ok(flags) = state.cancel_flags.lock() {
                if let Some(flag) = flags.get(task_id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            if let Ok(mut pending) = state.pending_messages.lock() {
                pending.remove(task_id);
            }
            let result = state.tasks.lock().map_err(|_| "task state unavailable".to_string()).and_then(|mut tasks| {
                tasks.fail(task_id, "stopped by user".into())?;
                tasks.get(task_id).cloned().ok_or_else(|| "task not found".to_string())
            });
            match result {
                Ok(task) => {
                    let session = state.sessions.lock().ok().and_then(|sessions| sessions.get(&task.session_id).cloned());
                    if let Some(session) = session { manager.persist_task(&task, &session.machine_id); }
                    Some(format!("{{\"event\":\"task_stopped\",\"task_id\":\"{}\"}}", escape(task_id)))
                }
                Err(e) => Some(error_json(&cmd, &e)),
            }
        }
        "subscribe_browser" | "unsubscribe_browser" => Some(ok_json(&cmd)),
        "approve_permission" | "deny_permission" => {
            let task_id = value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
            let permission_id = value.get("permission_id").and_then(|v| v.as_str()).unwrap_or_default();
            let task = state.tasks.lock().ok().and_then(|tasks| tasks.get(task_id).cloned());
            let Some(task) = task else { return Some(error_json(&cmd, "task not found")); };
            if task.status != crate::agent::state::TaskStatus::WaitingApproval {
                return Some(error_json(&cmd, "task is not waiting for permission"));
            }
            let session = state
                .sessions
                .lock()
                .ok()
                .and_then(|sessions| sessions.get(&task.session_id).cloned());
            let Some(mut session) = session else { return Some(error_json(&cmd, "session not found")); };
            let allowed = cmd == "approve_permission";
            let decision_message = if allowed {
                format!("Permission {permission_id} approved by the user.")
            } else {
                format!("Permission {permission_id} denied by the user.")
            };
            let decision_message_id = format!("permission-decision-{}", unix_seconds());
            session.add_user_message(decision_message.clone());
            let _ = broadcast_tx.send(user_message_json(task_id, &decision_message_id, &decision_message));
            if let Ok(mut sessions) = state.sessions.lock() {
                sessions.insert(session.id.clone(), session.clone());
            }
            if let Ok(mut tasks) = state.tasks.lock() {
                if let Some(task) = tasks.get_mut(task_id) {
                    task.status = crate::agent::state::TaskStatus::Running;
                    task.updated_at = unix_seconds();
                    manager.persist_task(task, &session.machine_id);
                }
            }
            manager.persist_session(&session);
            let cancel_flag = Arc::new(AtomicBool::new(false));
            if let Ok(mut flags) = state.cancel_flags.lock() {
                flags.insert(task_id.to_string(), cancel_flag.clone());
            }
            let machine_id = session.machine_id.clone();
            spawn_agent_task(
                manager.clone(),
                state.clone(),
                broadcast_tx.clone(),
                task_id.to_string(),
                session.id.clone(),
                session,
                machine_id,
                cancel_flag,
            );
            Some(format!(
                "{{\"event\":\"permission_resolved\",\"task_id\":\"{}\",\"permission_id\":\"{}\",\"allowed\":{}}}",
                escape(task_id),
                escape(permission_id),
                allowed
            ))
        }
        "confirm_complete" => {
            let task_id = value.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
            let task = state.tasks.lock().ok().and_then(|tasks| tasks.get(task_id).cloned());
            let Some(task) = task else { return Some(error_json(&cmd, "task not found")); };
            let session = state.sessions.lock().ok().and_then(|sessions| sessions.get(&task.session_id).cloned());
            let Some(session) = session else { return Some(error_json(&cmd, "session not found")); };
            match manager.destroy_runtime(&session.machine_id, &task.runtime_id).await {
                Ok(()) => {
                    if let Ok(mut tasks) = state.tasks.lock() {
                        let _ = tasks.complete(task_id, "confirmed by user".into());
                        if let Some(updated) = tasks.get(task_id).cloned() {
                            manager.persist_task(&updated, &session.machine_id);
                        }
                    }
                    Some(format!("{{\"event\":\"check_confirmed\",\"task_id\":\"{}\"}}", escape(task_id)))
                }
                Err(e) => Some(error_json(&cmd, &e.to_string())),
            }
        }
        "open_terminal" => {
            let runtime_id = value.get("runtime_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let terminal_id = value.get("terminal_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let request = spark_transport::PtyOpenRequest { runtime_id: runtime_id.clone(), pty_id: terminal_id.clone(), cols: value.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16, rows: value.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16, shell: "/bin/bash".into() };
            match manager.open_pty(&runtime_id, request).await { Ok(()) => Some(format!("{{\"event\":\"terminal_opened\",\"runtime_id\":\"{}\",\"terminal_id\":\"{}\"}}", escape(&runtime_id), escape(&terminal_id))), Err(e) => Some(error_json(&cmd, &e.to_string())) }
        }
        "write_terminal" => {
            let runtime_id = value.get("runtime_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let terminal_id = value.get("terminal_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let data = value.get("data").and_then(|v| v.as_str()).unwrap_or_default().as_bytes().to_vec();
            match manager.write_pty(&runtime_id, spark_transport::PtyWriteRequest { runtime_id: runtime_id.clone(), pty_id: terminal_id.clone(), data }).await { Ok(()) => Some(ok_json(&cmd)), Err(e) => Some(error_json(&cmd, &e.to_string())) }
        }
        "resize_terminal" => {
            let runtime_id = value.get("runtime_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let terminal_id = value.get("terminal_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let request = spark_transport::PtyResizeRequest { runtime_id: runtime_id.clone(), pty_id: terminal_id, cols: value.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16, rows: value.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16 };
            match manager.resize_pty(&runtime_id, request).await { Ok(()) => Some(ok_json(&cmd)), Err(e) => Some(error_json(&cmd, &e.to_string())) }
        }
        "close_terminal" => {
            let runtime_id = value.get("runtime_id").and_then(|v| v.as_str()).unwrap_or_default();
            let terminal_id = value.get("terminal_id").and_then(|v| v.as_str()).unwrap_or_default();
            match manager.close_pty(runtime_id, terminal_id).await { Ok(()) => Some(ok_json(&cmd)), Err(e) => Some(error_json(&cmd, &e.to_string())) }
        }
        "browser_action" => {
            let runtime_id = value.get("runtime_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let Some(action) = browser_action_from_json(&value) else { return Some(error_json(&cmd, "invalid browser action")); };
            match manager.browser_request(&runtime_id, spark_transport::BrowserRequest { runtime_id: runtime_id.clone(), action }).await {
                Ok(response) => Some(browser_response_json(&runtime_id, &response)), Err(e) => Some(error_json(&cmd, &e.to_string())),
            }
        }
        "computer_action" => {
            let runtime_id = value.get("runtime_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let Some(action) = computer_action_from_json(&value) else { return Some(error_json(&cmd, "invalid computer action")); };
            match manager.computer_request(&runtime_id, spark_transport::ComputerRequest { runtime_id: runtime_id.clone(), action }).await {
                Ok(response) => Some(computer_response_json(&runtime_id, &response)), Err(e) => Some(error_json(&cmd, &e.to_string())),
            }
        }
        "remove_machine" | "connect_machine" | "disconnect_machine" | "reconnect_machine"
        | "bootstrap_machine" | "trust_host_key" | "refresh_status" | "list_runtimes"
        | "destroy_runtime" => {
            let machine_id = match value.get("machine_id").and_then(|v| v.as_str()) {
                Some(id) => MachineId::from_string(id.to_string()),
                None => return Some(error_json(&cmd, "machine_id is required")),
            };
            let result = match cmd.as_str() {
                "remove_machine" => manager.remove_machine(&machine_id).await.map(|_| None),
                "connect_machine" => manager.connect_machine(&machine_id).await.map(|_| None),
                "disconnect_machine" => {
                    manager.disconnect_machine(&machine_id).await.map(|_| None)
                }
                "reconnect_machine" => match manager.reconnect_machine(&machine_id).await {
                    Ok(_) => manager.list_runtimes(&machine_id).await.map(|runtimes| Some(runtimes_json(&machine_id, &runtimes))),
                    Err(error) => Err(error),
                },
                "bootstrap_machine" => manager.bootstrap_machine(&machine_id).await.map(|report| {
                    Some(format!(
                        "{{\"event\":\"bootstrap_finished\",\"machine_id\":\"{}\",\"report\":{}}}",
                        machine_id.as_str(),
                        serde_json::to_string(&BootstrapSummary::from_report(&report))
                            .unwrap_or_else(|_| "null".to_string())
                    ))
                }),
                "trust_host_key" => {
                    // Only reachable because the user pressed [信任并连接]; the
                    // manager writes the key it remembered from the Attention
                    // event and then reconnects.
                    manager.trust_host_key(&machine_id).await.map(|_| None)
                }
                "refresh_status" => {
                    let status = manager
                        .get_machine(&machine_id)
                        .map(|machine| machine.status)
                        .unwrap_or(MachineStatus::Disconnected);
                    Ok(Some(format!(
                        "{{\"event\":\"machine_status\",\"machine_id\":\"{}\",\"status\":\"{}\",\"detail\":null}}",
                        machine_id.as_str(),
                        status.as_str()
                    )))
                }
                "list_runtimes" => manager
                    .list_runtimes(&machine_id)
                    .await
                    .map(|runtimes| Some(runtimes_json(&machine_id, &runtimes))),
                "destroy_runtime" => {
                    let runtime_id = value
                        .get("runtime_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    manager
                        .destroy_runtime(&machine_id, runtime_id)
                        .await
                        .map(|_| None)
                }
                _ => unreachable!(),
            };
            match result {
                Ok(extra) => match extra {
                    Some(line) => Some(line),
                    None => Some(ok_json(&cmd)),
                },
                Err(e) => Some(error_json(&cmd, &e.to_string())),
            }
        }
        "shutdown" => Some("__shutdown__".to_string()),
        other => Some(error_json(other, "unknown command")),
    }
}

/// Run one task outside the Tokio worker threads and stream normalized AgentLoop
/// events to every connected client. The runtime is deliberately not owned by
/// this worker: cancellation only flips the loop flag, while runtime destroy
/// remains an explicit ConfirmComplete operation.
fn spawn_agent_task(
    manager: Arc<MachineManager>,
    state: Arc<ServeState>,
    broadcast_tx: tokio::sync::broadcast::Sender<String>,
    task_id: String,
    session_id: String,
    mut session: AgentSession,
    machine_id: MachineId,
    cancel_flag: Arc<AtomicBool>,
) {
    let _ = tokio::task::spawn_blocking(move || {
        let manager_for_transport = manager.clone();
        let manager_for_machine = manager.clone();
        let manager_for_list = manager.clone();
        let manager_for_runtime = manager.clone();
        let ctx = ToolExecutionContext::with_runtime_machine(
            Arc::new(move |machine_id| manager_for_transport.transport(machine_id)),
            Arc::new(move |machine_id| manager_for_machine.get_machine(machine_id)),
            Arc::new(move || manager_for_list.list_machines()),
            machine_id.clone(),
            Arc::new(move |runtime_id| manager_for_runtime.runtime_machine_id(runtime_id)),
        );
        let tools = crate::default_tool_registry();
        let model: Box<dyn Model> = match OpenAICompatibleModel::from_env() {
            Ok(model) => Box::new(model),
            Err(_) => Box::new(MockModel::simple_final(
                "No model API was configured. The runtime and task are ready; configure OPENAI_API_KEY and send the task again.",
            )),
        };

        let _ = broadcast_tx.send(task_status_json(&task_id, "running"));
        let agent_loop = AgentLoop::new();
        let result = agent_loop.run_with_cancel(
            &mut session,
            model.as_ref(),
            &tools,
            Some(&ctx),
            cancel_flag,
            |event| {
                if let Some(line) = loop_event_json(&task_id, &event) {
                    let _ = broadcast_tx.send(line);
                }
            },
        );

        let was_cancelled = matches!(result.as_ref(), Err(error) if error == "cancelled");
        let status = if was_cancelled {
            "cancelled"
        } else if result.is_err() {
            "failed"
        } else {
            match session.status.as_str() {
                "ready_for_check" => "ready_for_check",
                "waiting_input" => "waiting_approval",
                "completed" => "completed",
                "failed" => "failed",
                _ => "running",
            }
        };

        let follow_up = if !was_cancelled {
            state
                .pending_messages
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&task_id))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if !follow_up.is_empty() {
            for content in follow_up {
                session.add_user_message(content);
            }
            if let Ok(mut sessions) = state.sessions.lock() {
                sessions.insert(session.id.clone(), session.clone());
            }
            if let Ok(mut tasks) = state.tasks.lock() {
                if let Some(task) = tasks.get_mut(&task_id) {
                    task.status = crate::agent::state::TaskStatus::Running;
                    task.result = None;
                    task.updated_at = unix_seconds();
                    manager.persist_task(task, &machine_id);
                }
            }
            manager.persist_session(&session);
            if let Ok(mut flags) = state.cancel_flags.lock() {
                flags.remove(&task_id);
            }
            let next_cancel = Arc::new(AtomicBool::new(false));
            if let Ok(mut flags) = state.cancel_flags.lock() {
                flags.insert(task_id.clone(), next_cancel.clone());
            }
            spawn_agent_task(
                manager.clone(),
                state.clone(),
                broadcast_tx.clone(),
                task_id.clone(),
                session.id.clone(),
                session,
                machine_id.clone(),
                next_cancel,
            );
            let _ = broadcast_tx.send(task_status_json(&task_id, "running"));
            return;
        }

        if let Ok(mut sessions) = state.sessions.lock() {
            sessions.insert(session_id, session.clone());
        }
        if let Ok(mut tasks) = state.tasks.lock() {
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = match status {
                    "completed" => crate::agent::state::TaskStatus::Completed,
                    "ready_for_check" => crate::agent::state::TaskStatus::WaitingApproval,
                    "waiting_approval" => crate::agent::state::TaskStatus::WaitingApproval,
                    "cancelled" => crate::agent::state::TaskStatus::Cancelled,
                    "failed" => crate::agent::state::TaskStatus::Failed,
                    _ => crate::agent::state::TaskStatus::Running,
                };
                task.result = result.clone().ok();
                task.updated_at = unix_seconds();
                manager.persist_task(task, &machine_id);
            }
        }
        manager.persist_session(&session);
        if let Ok(mut flags) = state.cancel_flags.lock() {
            flags.remove(&task_id);
        }
        let _ = broadcast_tx.send(task_status_json(&task_id, status));
        if let Err(error) = result {
            let _ = broadcast_tx.send(error_json("agent_loop", &error));
        }
    });
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn task_status_json(task_id: &str, status: &str) -> String {
    format!(
        "{{\"event\":\"task_status\",\"task_id\":\"{}\",\"status\":\"{}\"}}",
        escape(task_id),
        escape(status)
    )
}

fn user_message_json(task_id: &str, message_id: &str, content: &str) -> String {
    format!(
        "{{\"event\":\"user_message\",\"task_id\":\"{}\",\"message_id\":\"{}\",\"content\":\"{}\"}}",
        escape(task_id),
        escape(message_id),
        escape(content)
    )
}

fn loop_event_json(task_id: &str, event: &crate::agent::LoopEvent) -> Option<String> {
    use crate::agent::{Attention, LoopEvent};
    let task = escape(task_id);
    match event {
        LoopEvent::MessageAdded { role, content } if role == "assistant" => Some(format!(
            "{{\"event\":\"assistant_message\",\"task_id\":\"{}\",\"message_id\":\"assistant-{}\",\"content\":\"{}\",\"streaming\":false}}",
            task, task, escape(content)
        )),
        LoopEvent::MessageAdded { role, content } => Some(format!(
            "{{\"event\":\"tool_output\",\"task_id\":\"{}\",\"call_id\":\"message-{}\",\"delta\":\"{}\"}}",
            task, task, escape(content)
        )),
        LoopEvent::ModelStreaming { content } => Some(format!(
            "{{\"event\":\"assistant_streaming\",\"task_id\":\"{}\",\"message_id\":\"assistant-{}\",\"delta\":\"{}\"}}",
            task, task, escape(content)
        )),
        LoopEvent::ToolCallStarted { name, args, call_id } => Some(format!(
            "{{\"event\":\"tool_started\",\"task_id\":\"{}\",\"call_id\":\"{}\",\"tool_name\":\"{}\",\"args_json\":\"{}\"}}",
            task, escape(call_id), escape(name), escape(args)
        )),
        LoopEvent::ToolCallFinished { name, result, call_id, status, duration_ms } => Some(format!(
            "{{\"event\":\"tool_finished\",\"task_id\":\"{}\",\"call_id\":\"{}\",\"tool_name\":\"{}\",\"status\":\"{}\",\"result\":\"{}\",\"duration_ms\":{}}}",
            task, escape(call_id), escape(name), escape(status), escape(result), duration_ms
        )),
        LoopEvent::Attention { attention: Attention::PermissionRequired { tool, reason } } => Some(format!(
            "{{\"event\":\"permission_required\",\"task_id\":\"{}\",\"permission_id\":\"permission-{}\",\"tool_name\":\"{}\",\"reason\":\"{}\"}}",
            task, task, escape(tool), escape(reason)
        )),
        LoopEvent::Attention { attention: Attention::ReadyForCheck } => Some(format!(
            "{{\"event\":\"ready_for_check\",\"task_id\":\"{}\",\"summary\":\"Agent is ready for review\",\"files_changed\":0,\"tests_passed\":false,\"browser_verified\":false}}",
            task
        )),
        LoopEvent::Attention { attention: Attention::WaitingInput } => Some(task_status_json(task_id, "waiting_approval")),
        LoopEvent::Attention { attention: Attention::Completed } => Some(task_status_json(task_id, "completed")),
        LoopEvent::Completed { result } => Some(format!(
            "{{\"event\":\"assistant_message\",\"task_id\":\"{}\",\"message_id\":\"final-{}\",\"content\":\"{}\",\"streaming\":false}}",
            task, task, escape(result)
        )),
        LoopEvent::Failed { reason } => Some(format!(
            "{{\"event\":\"error\",\"task_id\":\"{}\",\"message\":\"{}\",\"recoverable\":true}}",
            task, escape(reason)
        )),
        LoopEvent::Cancelled => Some(task_status_json(task_id, "cancelled")),
        _ => None,
    }
}

fn browser_action_from_json(value: &serde_json::Value) -> Option<spark_transport::BrowserAction> {
    let kind = value.get("action")?.as_str()?;
    match kind {
        "open" => Some(spark_transport::BrowserAction::Open { url: value.get("url")?.as_str()?.to_string() }),
        "snapshot" => Some(spark_transport::BrowserAction::Snapshot),
        "click" => Some(spark_transport::BrowserAction::Click { reference: value.get("ref")?.as_str()?.to_string() }),
        "fill" => Some(spark_transport::BrowserAction::Fill { reference: value.get("ref")?.as_str()?.to_string(), text: value.get("text")?.as_str()?.to_string() }),
        "press" => Some(spark_transport::BrowserAction::Press { key: value.get("key")?.as_str()?.to_string() }),
        "screenshot" => Some(spark_transport::BrowserAction::Screenshot { format: value.get("format").and_then(|v| v.as_str()).unwrap_or("png").into(), quality: value.get("quality").and_then(|v| v.as_u64()).unwrap_or(85) as u8 }),
        "tabs" => Some(spark_transport::BrowserAction::Tabs),
        "close" => Some(spark_transport::BrowserAction::Close),
        _ => None,
    }
}

fn computer_action_from_json(value: &serde_json::Value) -> Option<spark_transport::ComputerAction> {
    let kind = value.get("action")?.as_str()?;
    let number = |key: &str| value.get(key).and_then(|v| v.as_i64()).map(|v| v as i32);
    match kind {
        "screenshot" => Some(spark_transport::ComputerAction::Screenshot),
        "click" => Some(spark_transport::ComputerAction::Click { x: number("x")?, y: number("y")?, button: value.get("button").and_then(|v| v.as_str()).unwrap_or("left").into() }),
        "type" => Some(spark_transport::ComputerAction::Type { text: value.get("text")?.as_str()?.into() }),
        "move" => Some(spark_transport::ComputerAction::Move { x: number("x")?, y: number("y")? }),
        "key" => Some(spark_transport::ComputerAction::Key { key: value.get("key")?.as_str()?.into() }),
        "scroll" => Some(spark_transport::ComputerAction::Scroll { x: number("x")?, y: number("y")?, delta: number("delta")? }),
        _ => None,
    }
}

fn browser_response_json(runtime_id: &str, response: &spark_transport::BrowserResponse) -> String {
    if let Some(error) = &response.error {
        return format!("{{\"event\":\"error\",\"message\":\"{}\"}}", escape(error));
    }
    if let Some(frame) = &response.frame {
        return format!(
            "{{\"event\":\"browser_frame\",\"runtime_id\":\"{}\",\"frame_id\":{},\"width\":{},\"height\":{},\"format\":\"{}\",\"data\":\"{}\"}}",
            escape(runtime_id),
            frame.frame_id,
            frame.width,
            frame.height,
            escape(&frame.format),
            base64_encode(&frame.data),
        );
    }
    let snapshot = response
        .snapshot
        .as_deref()
        .map(|v| format!("\"snapshot\":\"{}\"", escape(v)))
        .unwrap_or_else(|| "\"snapshot\":null".into());
    format!("{{\"event\":\"browser_result\",{} }}", snapshot)
}

fn computer_response_json(runtime_id: &str, response: &spark_transport::ComputerResponse) -> String {
    if let Some(error) = &response.error {
        return format!("{{\"event\":\"error\",\"message\":\"{}\"}}", escape(error));
    }
    if let Some(frame) = &response.frame {
        return format!(
            "{{\"event\":\"computer_frame\",\"runtime_id\":\"{}\",\"frame_id\":{},\"width\":{},\"height\":{},\"format\":\"{}\",\"data\":\"{}\"}}",
            escape(runtime_id),
            frame.frame_id,
            frame.width,
            frame.height,
            escape(&frame.format),
            base64_encode(&frame.data),
        );
    }
    "{\"event\":\"computer_result\"}".to_string()
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        output.push(TABLE[(a >> 2) as usize] as char);
        output.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(c & 63) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

// ---------------------------------------------------------------------------
// JSON helpers (hand-rolled on purpose: no serde dependency on the wire shape
// of the *client* protocol keeps the format readable in `host-agent serve` logs)
// ---------------------------------------------------------------------------

fn draft_from_json(value: &serde_json::Value) -> Option<MachineDraft> {
    let name = value.get("name")?.as_str()?.to_string();
    let alias = value
        .get("alias")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let host = value
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let host = if host.is_empty() {
        alias.clone().unwrap_or_default()
    } else {
        host
    };
    let mut draft = MachineDraft::new(name, host);
    if let Some(user) = value.get("user").and_then(|v| v.as_str()) {
        if !user.is_empty() {
            draft = draft.with_user(user.to_string());
        }
    }
    if let Some(port) = value.get("port").and_then(|v| v.as_u64()) {
        draft = draft.with_port(port as u16);
    }
    if let Some(alias) = alias {
        if !alias.is_empty() {
            draft = draft.with_alias(alias);
        }
    }
    if draft.is_valid() {
        Some(draft)
    } else {
        None
    }
}

fn machines_json(manager: &Arc<MachineManager>) -> String {
    let machines = manager.list_machines();
    let payload = serde_json::to_string(&machines).unwrap_or_else(|_| "[]".to_string());
    format!("{{\"event\":\"machines\",\"machines\":{payload}}}")
}

fn runtimes_json(machine_id: &MachineId, runtimes: &[RuntimeInfo]) -> String {
    let payload = serde_json::to_string(runtimes).unwrap_or_else(|_| "[]".to_string());
    format!(
        "{{\"event\":\"runtimes\",\"machine_id\":\"{}\",\"runtimes\":{}}}",
        machine_id.as_str(),
        payload
    )
}

/// After a reconnect the client needs two things: "your work is still there"
/// and the ids it must re-attach to.
fn ok_json(cmd: &str) -> String {
    format!("{{\"event\":\"ok\",\"cmd\":\"{}\"}}", escape(cmd))
}

fn error_json(cmd: &str, message: &str) -> String {
    format!(
        "{{\"event\":\"error\",\"cmd\":\"{}\",\"message\":\"{}\"}}",
        escape(cmd),
        escape(message)
    )
}

/// Map a manager event onto the client protocol.
fn machine_event_json(event: &MachineEvent) -> String {
    match event {
        MachineEvent::Added(_) | MachineEvent::Removed(_) => {
            // The full list is the simplest correct answer to "something
            // changed" for a sidebar with a handful of machines.
            "{\"event\":\"refresh_machines\"}".to_string()
        }
        MachineEvent::StatusChanged {
            machine_id,
            status,
            detail,
        } => format!(
            "{{\"event\":\"machine_status\",\"machine_id\":\"{}\",\"status\":\"{}\",\"detail\":{}}}",
            machine_id.as_str(),
            status.as_str(),
            match detail {
                Some(detail) => format!("\"{}\"", escape(detail)),
                None => "null".to_string(),
            }
        ),
        MachineEvent::AttentionRequired { machine_id, issue } => format!(
            "{{\"event\":\"machine_attention\",\"machine_id\":\"{}\",\"message\":\"{}\",\"fingerprint\":\"{}\",\"changed\":{}}}",
            machine_id.as_str(),
            escape(&issue.attention_message()),
            escape(issue.fingerprint()),
            issue.is_high_risk(),
        ),
        MachineEvent::MetadataUpdated(machine) => format!(
            "{{\"event\":\"machine_metadata\",\"machine\":{}}}",
            serde_json::to_string(machine).unwrap_or_else(|_| "null".to_string())
        ),
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn restrict_dir(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 700 {}", path.display()))
}

fn restrict_socket(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))
}

/// Convenience: connect a client (used by tests and by `host-agent serve --ping`).
pub async fn connect_client(socket_path: &std::path::Path) -> Result<UnixStream> {
    UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to {}", socket_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_parsing_requires_a_target() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"cmd":"add_machine","name":"devbox"}"#).unwrap();
        assert!(draft_from_json(&value).is_none());

        let value: serde_json::Value = serde_json::from_str(
            r#"{"cmd":"add_machine","name":"devbox","host":"10.0.0.42","user":"ubuntu","port":2222}"#,
        )
        .unwrap();
        let draft = draft_from_json(&value).expect("draft");
        assert_eq!(draft.name, "devbox");
        assert_eq!(draft.port, 2222);
        assert_eq!(draft.user.as_deref(), Some("ubuntu"));
    }

    #[test]
    fn alias_only_draft_is_valid() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"name":"devbox","alias":"devbox"}"#).unwrap();
        let draft = draft_from_json(&value).expect("draft");
        assert_eq!(draft.host, "devbox");
        assert_eq!(draft.ssh_config_host.as_deref(), Some("devbox"));
    }

    #[test]
    fn status_events_are_valid_json_lines() {
        let event = MachineEvent::StatusChanged {
            machine_id: MachineId::from_string("mach-1".into()),
            status: MachineStatus::Connected,
            detail: Some("hello \"world\"".into()),
        };
        let line = machine_event_json(&event);
        assert!(!line.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(parsed["event"], "machine_status");
        assert_eq!(parsed["status"], "connected");
    }

    #[test]
    fn attention_events_carry_the_fingerprint() {
        let issue = spark_transport::HostKeyIssue::Unknown {
            host: "10.0.0.42".into(),
            port: 22,
            key_type: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            key_line: "10.0.0.42 ssh-ed25519 AAAA".into(),
        };
        let event = MachineEvent::AttentionRequired {
            machine_id: MachineId::from_string("mach-1".into()),
            issue: Box::new(issue),
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&machine_event_json(&event)).expect("valid json");
        assert_eq!(parsed["event"], "machine_attention");
        assert_eq!(parsed["fingerprint"], "SHA256:abc");
        assert_eq!(parsed["changed"], false);
    }
}
