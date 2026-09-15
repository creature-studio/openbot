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
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};

use spark_model::{MachineId, MachineStatus};
use spark_transport::{BootstrapSummary, RuntimeInfo};
use spark_transport::MachineDraft;

use crate::machine::{MachineEvent, MachineManager};

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

/// Serve until `shutdown` or SIGTERM.
pub async fn serve(manager: Arc<MachineManager>, socket_path: Option<PathBuf>) -> Result<()> {
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
            }
        });
    }

    loop {
        let (stream, _addr) = listener.accept().await.context("accepting a client")?;
        let manager = manager.clone();
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
                if let Some(reply) = handle_command(&manager, &line).await {
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
async fn handle_command(manager: &Arc<MachineManager>, line: &str) -> Option<String> {
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
                "reconnect_machine" => manager
                    .reconnect_machine(&machine_id)
                    .await
                    .map(|survivors| Some(reconnected_json(&machine_id, &survivors))),
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
                    Some(format!(
                        "{{\"event\":\"machine_status\",\"machine_id\":\"{}\",\"status\":\"{}\",\"detail\":null}}",
                        machine_id.as_str(),
                        status.as_str()
                    ))
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
fn reconnected_json(machine_id: &MachineId, survivors: &[String]) -> String {
    let payload = serde_json::to_string(survivors).unwrap_or_else(|_| "[]".to_string());
    format!(
        "{{\"event\":\"reconnected\",\"machine_id\":\"{}\",\"runtimes\":{}}}",
        machine_id.as_str(),
        payload
    )
}

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
