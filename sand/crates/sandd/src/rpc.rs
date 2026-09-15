use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::os::unix::net::{UnixListener, UnixStream};
use std::io::{BufRead, BufReader, Write};
use std::collections::HashMap;

use crate::runtime::RuntimeManager;
use sand_protocol::{RuntimeId, RuntimeKind};

pub struct RpcServer {
    sock_path: PathBuf,
    runtime_mgr: Arc<RuntimeManager>,
}

impl RpcServer {
    pub fn new(sock_path: PathBuf, runtime_mgr: Arc<RuntimeManager>) -> Self {
        Self { sock_path, runtime_mgr }
    }

    pub fn run(&self) -> std::io::Result<()> {
        let listener = UnixListener::bind(&self.sock_path)?;
        // set permissions to 0660 (group sand if exists), not 777
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.sock_path, std::fs::Permissions::from_mode(0o660));
            // Try to chgrp to sand if group exists
            let _ = std::process::Command::new("chgrp")
                .args(["sand", &self.sock_path.to_string_lossy()])
                .output();
            // Fallback: chmod 660 already, future SO_PEERCRED will enforce
        }

        eprintln!("[rpc] listening on {} (mode 0660, SO_PEERCRED planned)", self.sock_path.display());

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let mgr = self.runtime_mgr.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = handle_client(stream, mgr) {
                            eprintln!("[rpc] client error: {:?}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[rpc] accept error: {:?}", e);
                }
            }
        }

        Ok(())
    }
}

fn handle_client(stream: UnixStream, mgr: Arc<RuntimeManager>) -> std::io::Result<()> {
    // Try to get peer credentials via SO_PEERCRED (Linux)
    #[cfg(unix)]
    {
        if let Ok(creds) = get_peer_cred(&stream) {
            eprintln!("[rpc] client peer pid={} uid={} gid={}", creds.pid, creds.uid, creds.gid);
        }
    }

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        // Check if SubscribeEvents streaming
        let method = extract_string_field(trimmed, "method").unwrap_or_default();
        if method == "SubscribeEvents" {
            // Stream events: first send current events, then stream new ones
            let id_filter = extract_string_field(trimmed, "id");
            // Send header ok
            writeln!(writer, "{{\"ok\":true,\"streaming\":true}}")?;
            writer.flush()?;
            // Subscribe
            let rx = mgr.event_bus().subscribe();
            // Also send existing events? We'll send list first as separate?
            loop {
                match rx.recv() {
                    Ok(ev) => {
                        if let Some(ref filter_id) = id_filter {
                            if ev.runtime_id.0 != *filter_id {
                                continue;
                            }
                        }
                        let line = format!("{{\"runtime_id\":\"{}\",\"kind\":\"{}\",\"at_ms\":{}}}", ev.runtime_id.0, ev.kind.as_str(), ev.at_ms);
                        if writeln!(writer, "{}", line).is_err() {
                            break;
                        }
                        if writer.flush().is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            break;
        }

        let response = handle_request(trimmed, &mgr);
        writeln!(writer, "{}", response)?;
        writer.flush()?;
        line.clear();
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct PeerCred {
    pid: i32,
    uid: u32,
    gid: u32,
}

#[cfg(unix)]
fn get_peer_cred(stream: &UnixStream) -> std::io::Result<PeerCred> {
    use std::os::unix::io::AsRawFd;
    use std::mem::MaybeUninit;
    // SO_PEERCRED = 17 on Linux
    const SO_PEERCRED: i32 = 17;
    const SOL_SOCKET: i32 = 1;
    #[repr(C)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }
    let fd = stream.as_raw_fd();
    let mut ucred = MaybeUninit::<Ucred>::uninit();
    let mut len = std::mem::size_of::<Ucred>() as u32;
    let ret = unsafe {
        getsockopt(fd, SOL_SOCKET, SO_PEERCRED, ucred.as_mut_ptr() as *mut _, &mut len as *mut _ as *mut _)
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ucred = unsafe { ucred.assume_init() };
    Ok(PeerCred { pid: ucred.pid, uid: ucred.uid, gid: ucred.gid })
}

#[cfg(unix)]
extern "C" {
    fn getsockopt(sockfd: i32, level: i32, optname: i32, optval: *mut std::os::raw::c_void, optlen: *mut u32) -> i32;
}

pub fn handle_request_public(req_str: &str, mgr: &RuntimeManager) -> String {
    handle_request(req_str, mgr)
}

fn handle_request(req_str: &str, mgr: &RuntimeManager) -> String {
    // Very simple JSON parsing without serde: expect {"method":"...","params":{...}}
    // We'll do naive extraction

    let method = extract_string_field(req_str, "method").unwrap_or_default();

    match method.as_str() {
        "CreateRuntime" => {
            let kind_str = extract_string_field(req_str, "kind").unwrap_or_else(|| "assistant".to_string());
            let workspace = extract_string_field(req_str, "workspace");
            let kind = RuntimeKind::from_str(&kind_str);
            let ws_path = workspace.map(|s| PathBuf::from(s));
            let machine_id = extract_string_field(req_str, "machine_id")
                .map(sand_protocol::MachineId::from_string);
            match mgr.create_runtime_on_machine(kind, ws_path, machine_id) {
                Ok(rt) => {
                    format!("{{\"ok\":true,\"id\":\"{}\",\"kind\":\"{}\",\"workspace\":\"{}\",\"machine_id\":\"{}\"}}", rt.id.0, rt.kind.as_str(), rt.workspace.display(), escape_json(rt.machine_id.as_str()))
                }
                Err(e) => {
                    format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e))
                }
            }
        }
        "GetRuntime" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let id = RuntimeId::from_string(id_str);
            if let Some(rt) = mgr.get_runtime(&id) {
                format!("{{\"ok\":true,\"id\":\"{}\",\"kind\":\"{}\",\"state\":\"{}\",\"workspace\":\"{}\",\"procs\":{},\"ptys\":{},\"machine_id\":\"{}\"}}", rt.id.0, rt.kind.as_str(), rt.state.as_str(), rt.workspace.display(), rt.process_count, rt.pty_count, escape_json(rt.machine_id.as_str()))
            } else {
                format!("{{\"ok\":false,\"error\":\"runtime not found\"}}")
            }
        }
        "ListRuntimes" => {
            let list = mgr.list_runtimes();
            let mut items = Vec::new();
            for rt in list {
                items.push(format!("{{\"id\":\"{}\",\"kind\":\"{}\",\"state\":\"{}\",\"workspace\":\"{}\",\"procs\":{},\"ptys\":{},\"machine_id\":\"{}\"}}", rt.id.0, rt.kind.as_str(), rt.state.as_str(), rt.workspace.display(), rt.process_count, rt.pty_count, escape_json(rt.machine_id.as_str())));
            }
            format!("{{\"ok\":true,\"runtimes\":[{}]}}", items.join(","))
        }
        "DestroyRuntime" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let id = RuntimeId::from_string(id_str);
            match mgr.destroy_runtime(&id) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "Exec" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let command_str = extract_string_field(req_str, "command").unwrap_or_default();
            // command is space separated? For simplicity, we expect command as string like "pwd" or "echo hello" or array? We'll parse as json array if present
            // Try to extract command array via naive method
            let command_vec = if req_str.contains("\"command\":[") {
                // extract array content
                if let Some(start) = req_str.find("\"command\":[") {
                    let rest = &req_str[start + "\"command\":[".len()..];
                    if let Some(end) = rest.find(']') {
                        let arr_str = &rest[..end];
                        // split by comma, trim quotes
                        arr_str.split(',').map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
                    } else {
                        vec![command_str.clone()]
                    }
                } else {
                    vec![command_str.clone()]
                }
            } else {
                // split by space but handle?
                vec![command_str.clone()]
            };

            // Actually better: if command_str contains spaces, split? But for echo hello we need ["echo","hello"]
            // Let's attempt to split command_str by whitespace if command_vec is single and contains space and not array
            let final_cmd = if command_vec.len() == 1 && command_vec[0].contains(' ') && !req_str.contains("\"command\":[") {
                // naive shell split? For simplicity, split by space
                // but for "bash -lc 'echo hello'" we need better
                // We'll try to parse as shell: if starts with "bash", keep rest as is?
                // For now, split by space
                command_vec[0].split_whitespace().map(|s| s.to_string()).collect()
            } else {
                command_vec
            };

            let cwd = extract_string_field(req_str, "cwd");
            let timeout = extract_number_field(req_str, "timeout_ms");

            let id = RuntimeId::from_string(id_str);
            let env = HashMap::new(); // ignore for now

            match mgr.exec(&id, final_cmd, cwd, env, timeout) {
                Ok(res) => {
                    let stdout_b64 = base64_encode(&res.stdout);
                    let stderr_b64 = base64_encode(&res.stderr);
                    format!("{{\"ok\":true,\"pid\":{},\"exit_code\":{},\"stdout_b64\":\"{}\",\"stderr_b64\":\"{}\",\"duration_ms\":{}}}", res.pid, res.exit_code.unwrap_or(-1), stdout_b64, stderr_b64, res.duration_ms)
                }
                Err(e) => {
                    format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e))
                }
            }
        }
        "SpawnBackground" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            // parse command array similar to Exec
            let command_vec = if req_str.contains("\"command\":[") {
                if let Some(start) = req_str.find("\"command\":[") {
                    let rest = &req_str[start + "\"command\":[".len()..];
                    if let Some(end) = rest.find(']') {
                        let arr_str = &rest[..end];
                        arr_str.split(',').map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            } else {
                let command_str = extract_string_field(req_str, "command").unwrap_or_default();
                if command_str.contains(' ') {
                    command_str.split_whitespace().map(|s| s.to_string()).collect()
                } else if !command_str.is_empty() {
                    vec![command_str]
                } else {
                    vec![]
                }
            };
            let cwd = extract_string_field(req_str, "cwd");
            let id = RuntimeId::from_string(id_str);
            let env = HashMap::new();
            match mgr.spawn_background(&id, command_vec, cwd, env) {
                Ok(pid) => format!("{{\"ok\":true,\"pid\":{}}}", pid),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "OpenPty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let cols = extract_number_field(req_str, "cols").unwrap_or(80) as u16;
            let rows = extract_number_field(req_str, "rows").unwrap_or(24) as u16;
            let shell = extract_string_field(req_str, "shell").unwrap_or_else(|| "/bin/bash".to_string());

            let cgroup_path = mgr.cgroup_manager().runtime_cgroup_path(&id_str);
            let cgroup_opt = if cgroup_path.exists() { Some(cgroup_path) } else { None };

            match mgr.pty_manager().open_pty(&id_str, &pty_id, cols, rows, &shell, cgroup_opt) {
                Ok(sess) => {
                    format!("{{\"ok\":true,\"runtime_id\":\"{}\",\"pty_id\":\"{}\",\"pid\":{}}}", sess.runtime_id, sess.pty_id, sess.pid)
                }
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "WritePty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let data_b64 = extract_string_field(req_str, "data_b64").unwrap_or_default();
            let data = base64_decode(&data_b64);
            match mgr.pty_manager().write_pty(&id_str, &pty_id, data) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ResizePty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let cols = extract_number_field(req_str, "cols").unwrap_or(80) as u16;
            let rows = extract_number_field(req_str, "rows").unwrap_or(24) as u16;
            match mgr.pty_manager().resize_pty(&id_str, &pty_id, cols, rows) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ClosePty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            match mgr.pty_manager().close_pty(&id_str, &pty_id) {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ReadPty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let clear = req_str.contains("\"clear\":true") || req_str.contains("\"clear\": 1");
            match mgr.pty_manager().read_pty(&id_str, &pty_id, clear) {
                Ok(data) => {
                    let b64 = base64_encode(&data);
                    format!("{{\"ok\":true,\"data_b64\":\"{}\",\"len\":{}}}", b64, data.len())
                }
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ListPtys" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let list = mgr.pty_manager().list_for_runtime(&id_str);
            let mut items = Vec::new();
            for sess in list {
                items.push(format!("{{\"pty_id\":\"{}\",\"pid\":{},\"cols\":{},\"rows\":{}}}", sess.pty_id, sess.pid, sess.cols, sess.rows));
            }
            format!("{{\"ok\":true,\"ptys\":[{}]}}", items.join(","))
        }
        "Status" => {
            let list = mgr.list_runtimes();
            let machine = sand_protocol::status::machine_info_json();
            format!("{{\"ok\":true,\"runtime_count\":{},\"version\":\"{}\",\"protocol_version\":{},{}}}", list.len(), sand_protocol::SANDB_VERSION, sand_protocol::SAND_PROTOCOL_VERSION, machine)
        }
        // Version negotiation. Runs before any runtime exists, which is why
        // every field here must be derivable locally by sandd alone.
        "Handshake" => {
            let client_protocol = extract_number_field(req_str, "protocol_version").unwrap_or(0) as u32;
            let compatible = client_protocol == sand_protocol::SAND_PROTOCOL_VERSION;
            // Features are probed per machine (node? Xvfb? xdotool?) so the
            // client can enable/hide panels honestly.
            let features: Vec<String> = crate::capabilities::features()
                .iter()
                .map(|f| format!("\"{}\"", f))
                .collect();
            let info = sand_protocol::status::machine_info();
            let gpu_json = match &info.gpu {
                Some(gpu) => format!("\"{}\"", escape_json(gpu)),
                None => "null".to_string(),
            };
            format!(
                "{{\"ok\":true,\"protocol_version\":{},\"sandd_version\":\"{}\",\"compatible\":{},\"features\":[{}],\"machine_id\":\"{}\",\"os\":\"{}\",\"arch\":\"{}\",\"hostname\":\"{}\",\"kernel\":\"{}\",\"uptime_seconds\":{},\"cpu_cores\":{},\"memory_total\":{},\"gpu\":{}}}",
                sand_protocol::SAND_PROTOCOL_VERSION,
                sand_protocol::SANDB_VERSION,
                compatible,
                features.join(","),
                escape_json(&info.machine_id),
                escape_json(&info.os),
                escape_json(&info.arch),
                escape_json(&info.hostname),
                escape_json(&info.kernel),
                info.uptime_seconds,
                info.cpu_cores,
                info.memory_total,
                gpu_json,
            )
        }
        "ListEvents" => {
            // `since` makes polling incremental: the bridge (and therefore a
            // reconnecting client) only ever receives events it has not seen.
            let since = extract_number_field(req_str, "since").unwrap_or(0);
            let id_str = extract_string_field(req_str, "id");
            let events = match id_str {
                Some(id_s) => mgr
                    .event_bus()
                    .list_since(Some(&RuntimeId::from_string(id_s)), since),
                None => mgr.event_bus().list_since(None, since),
            };
            let mut items = Vec::new();
            for (event_id, ev) in events.iter() {
                items.push(format!(
                    "{{\"event_id\":{},\"runtime_id\":\"{}\",\"kind\":\"{}\",\"at_ms\":{}}}",
                    event_id,
                    ev.runtime_id.0,
                    ev.kind.as_str(),
                    ev.at_ms
                ));
            }
            format!(
                "{{\"ok\":true,\"last_event_id\":{},\"events\":[{}]}}",
                mgr.event_bus().last_id(),
                items.join(",")
            )
        }
        "SubscribeEvents" => {
            format!("{{\"ok\":false,\"error\":\"use streaming handler\"}}")
        }
        // Cheap liveness probe used by MachineManager health checks over the
        // bridge: no runtime state touched, no subprocess spawned.
        "Ping" => {
            format!("{{\"ok\":true,\"at_ms\":{}}}", sand_protocol::now_ms())
        }
        "EnsureDisplay" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let width = extract_number_field(req_str, "width").unwrap_or(1280);
            let height = extract_number_field(req_str, "height").unwrap_or(720);
            match mgr.desktop_manager().ensure_display(&id_str, width as u32, height as u32) {
                Ok(d) => format!("{{\"ok\":true,\"display\":\"{}\"}}", d),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "GetDisplay" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            if let Some(d) = mgr.desktop_manager().get_display(&id_str) {
                format!("{{\"ok\":true,\"display\":\"{}\"}}", d)
            } else {
                format!("{{\"ok\":false,\"error\":\"no display\"}}")
            }
        }
        "SetDesiredState" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let should_exist = !req_str.contains("\"should_exist\":false");
            let should_running = !req_str.contains("\"should_running\":false");
            let min_procs = extract_number_field(req_str, "min_procs").unwrap_or(0);
            // For now, just log desired state, supervisor will handle
            eprintln!("[rpc] SetDesiredState id={} exist={} running={} min_procs={}", id_str, should_exist, should_running, min_procs);
            format!("{{\"ok\":true,\"id\":\"{}\",\"desired\":{{\"should_exist\":{},\"should_running\":{},\"min_procs\":{}}}}}", escape_json(&id_str), should_exist, should_running, min_procs)
        }
        "GetObservedState" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let id = RuntimeId::from_string(id_str.clone());
            if let Some(rt) = mgr.get_runtime(&id) {
                format!("{{\"ok\":true,\"id\":\"{}\",\"exists\":true,\"running\":{},\"procs\":{},\"ptys\":{},\"state\":\"{}\"}}", rt.id.0, rt.state.as_str() == "running", rt.process_count, rt.pty_count, rt.state.as_str())
            } else {
                format!("{{\"ok\":true,\"id\":\"{}\",\"exists\":false,\"running\":false,\"procs\":0,\"ptys\":0}}", escape_json(&id_str))
            }
        }
        "SignalPty" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let pty_id = extract_string_field(req_str, "pty_id").unwrap_or_else(|| "default".to_string());
            let signal = extract_number_field(req_str, "signal").unwrap_or(2) as i32; // default SIGINT
            // Also support string signals like "SIGINT", "SIGTERM", "CtrlC"
            let signal_str = extract_string_field(req_str, "signal_str").unwrap_or_default();
            let sig_num = match signal_str.to_lowercase().as_str() {
                "sigint" | "int" | "ctrlc" | "ctrl_c" | "c" => 2,
                "sigterm" | "term" => 15,
                "sigkill" | "kill" => 9,
                "sigquit" | "quit" => 3,
                "sigtstp" | "tstp" | "ctrlz" | "ctrl_z" => 20,
                "sigwinch" | "winch" => 28,
                _ => signal,
            };
            match mgr.pty_manager().signal_pty(&id_str, &pty_id, sig_num) {
                Ok(_) => format!("{{\"ok\":true,\"signal\":{}}}", sig_num),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "AcquireLease" => {
            let runtime_id = extract_string_field(req_str, "runtime_id").or_else(|| extract_string_field(req_str, "id")).unwrap_or_default();
            let owner = extract_string_field(req_str, "owner").unwrap_or_else(|| "task:default".to_string());
            let session_id = extract_string_field(req_str, "session_id").unwrap_or_else(|| format!("session-{}", sand_protocol::now_ms()));
            match mgr.acquire_lease(&runtime_id, &owner, &session_id) {
                Ok(lease) => format!("{{\"ok\":true,\"lease_id\":\"{}\",\"runtime_id\":\"{}\",\"owner\":\"{}\",\"session_id\":\"{}\",\"created_at\":{}}}", lease.lease_id, lease.runtime_id, lease.owner.as_str(), lease.session_id, lease.created_at_ms),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ReleaseLease" => {
            let lease_id = extract_string_field(req_str, "lease_id").unwrap_or_default();
            let session_id = extract_string_field(req_str, "session_id");
            let result = if let Some(sid) = session_id {
                mgr.release_lease_by_session(&sid)
            } else {
                mgr.release_lease(&lease_id)
            };
            match result {
                Ok(_) => format!("{{\"ok\":true}}"),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        "ListLeases" => {
            let runtime_id = extract_string_field(req_str, "runtime_id").or_else(|| extract_string_field(req_str, "id"));
            let leases = mgr.list_leases(runtime_id.as_deref());
            let mut items = Vec::new();
            for l in leases {
                items.push(format!("{{\"lease_id\":\"{}\",\"runtime_id\":\"{}\",\"owner\":\"{}\",\"session_id\":\"{}\",\"active\":{}}}", l.lease_id, l.runtime_id, l.owner.as_str(), l.session_id, l.active));
            }
            format!("{{\"ok\":true,\"leases\":[{}]}}", items.join(","))
        }
        "TaskComplete" => {
            let runtime_id = extract_string_field(req_str, "runtime_id").or_else(|| extract_string_field(req_str, "id")).unwrap_or_default();
            let owner = extract_string_field(req_str, "owner").unwrap_or_else(|| format!("task:{}", runtime_id));
            // Release all leases for this runtime? No, task complete+confirmed -> destroy runtime if no leases
            // For now, try to destroy if task complete
            match mgr.try_destroy_if_task_complete(&runtime_id, &owner) {
                Ok(destroyed) => format!("{{\"ok\":true,\"runtime_id\":\"{}\",\"destroyed\":{}}}", runtime_id, destroyed),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        // ----- Remote browser (proxied to the runtime's worker) -----
        "BrowserOpen" | "BrowserSnapshot" | "BrowserClick" | "BrowserFill" | "BrowserPress" | "BrowserScroll" | "BrowserTabs" | "BrowserClose" | "BrowserScreenshot" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let cgroup = mgr.cgroup_manager().runtime_cgroup_path(&id_str);
            let cgroup_opt = if cgroup.exists() { Some(cgroup) } else { None };
            let browser = mgr.browser_manager();

            if !browser.available() {
                return format!(
                    "{{\"ok\":false,\"unsupported\":true,\"error\":\"{}\"}}",
                    escape_json("browser worker not available on this machine")
                );
            }

            let action = match method.as_str() {
                "BrowserOpen" => "open",
                "BrowserSnapshot" => "snapshot",
                "BrowserClick" => "click",
                "BrowserFill" => "fill",
                "BrowserPress" => "press",
                "BrowserScroll" => "scroll",
                "BrowserTabs" => "tabs",
                "BrowserClose" => "close",
                _ => "screenshot",
            };

            // Forward the body sandd received; the worker understands the same
            // field names (url, ref, value, key, x, y).
            let body = if action == "open" {
                format!(
                    "{{\"url\":\"{}\"}}",
                    escape_json(&extract_string_field(req_str, "url").unwrap_or_default())
                )
            } else if action == "click" {
                format!(
                    "{{\"ref\":\"{}\"}}",
                    escape_json(
                        &extract_string_field(req_str, "ref")
                            .or_else(|| extract_string_field(req_str, "reference"))
                            .unwrap_or_default()
                    )
                )
            } else if action == "fill" {
                format!(
                    "{{\"ref\":\"{}\",\"value\":\"{}\"}}",
                    escape_json(
                        &extract_string_field(req_str, "ref")
                            .or_else(|| extract_string_field(req_str, "reference"))
                            .unwrap_or_default()
                    ),
                    escape_json(&extract_string_field(req_str, "text").unwrap_or_default())
                )
            } else if action == "press" {
                format!(
                    "{{\"key\":\"{}\"}}",
                    escape_json(&extract_string_field(req_str, "key").unwrap_or_default())
                )
            } else if action == "scroll" {
                format!(
                    "{{\"x\":{},\"y\":{}}}",
                    extract_number_field(req_str, "x").unwrap_or(0),
                    extract_number_field(req_str, "y").unwrap_or(0)
                )
            } else {
                String::new()
            };

            match browser.call(&id_str, cgroup_opt.as_deref(), action, Some(&body)) {
                Ok(response) => {
                    if action == "screenshot" {
                        // Screenshots are returned as raw bytes on the framed
                        // socket so the client gets frames, not base64.
                        if let Some(b64) = extract_string_field(&response.json, "screenshot_b64") {
                            let bytes = base64_decode(&b64);
                            let json = format!(
                                "{{\"ok\":true,\"len\":{},\"format\":\"png\",\"frame_id\":{}}}",
                                bytes.len(),
                                sand_protocol::now_ms()
                            );
                            return json;
                        }
                    }
                    response.json
                }
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        // ----- Remote computer use -----
        "ComputerScreenshot" | "ComputerClick" | "ComputerType" | "ComputerMove" | "ComputerKey" | "ComputerScroll" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let computer = mgr.computer_manager();
            let action = match method.as_str() {
                "ComputerClick" => "click",
                "ComputerType" => "type",
                "ComputerMove" => "move",
                "ComputerKey" => "key",
                "ComputerScroll" => "scroll",
                _ => "screenshot",
            };
            let params = crate::computer::Params::new(req_str);
            match computer.dispatch(&id_str, action, &params) {
                Ok(json) => json,
                Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(&e)),
            }
        }
        // ----- Runtime filesystem API (workspace confined) -----
        // The agent's file.* tools land here on every machine, local or
        // remote; there is no second filesystem implementation.
        "FsRead" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let path = extract_string_field(req_str, "path").unwrap_or_default();
            let root = match runtime_workspace(mgr, &id_str) {
                Some(ws) => ws,
                None => return format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json("runtime not found")),
            };
            match crate::fs::read(&root, &path) {
                Ok(data) => {
                    let b64 = base64_encode(&data);
                    format!("{{\"ok\":true,\"len\":{},\"data_b64\":\"{}\"}}", data.len(), b64)
                }
                Err(e) => format!("{{\"ok\":false,\"security\":{},\"error\":\"{}\"}}", e.security, escape_json(&e.message)),
            }
        }
        "FsWrite" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let path = extract_string_field(req_str, "path").unwrap_or_default();
            let data = match extract_string_field(req_str, "data_b64") {
                Some(b64) => base64_decode(&b64),
                None => Vec::new(),
            };
            let root = match runtime_workspace(mgr, &id_str) {
                Some(ws) => ws,
                None => return format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json("runtime not found")),
            };
            match crate::fs::write(&root, &path, &data) {
                Ok(len) => format!("{{\"ok\":true,\"bytes_written\":{}}}", len),
                Err(e) => format!("{{\"ok\":false,\"security\":{},\"error\":\"{}\"}}", e.security, escape_json(&e.message)),
            }
        }
        "FsList" | "FsStat" | "FsSearch" | "FsGlob" | "FsMkdir" | "FsRemove" | "FsRename" | "FsPatch" => {
            let id_str = extract_string_field(req_str, "id").unwrap_or_default();
            let path = extract_string_field(req_str, "path").unwrap_or_default();
            let root = match runtime_workspace(mgr, &id_str) {
                Some(ws) => ws,
                None => return format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json("runtime not found")),
            };
            let result = match method.as_str() {
                "FsList" => crate::fs::list(&root, &path).map(|data| format!("{{\"ok\":true,\"data\":{}}}", data)),
                "FsStat" => crate::fs::stat(&root, &path).map(|data| format!("{{\"ok\":true,\"data\":{}}}", data)),
                "FsSearch" => {
                    let query = extract_string_field(req_str, "query").unwrap_or_default();
                    let max = extract_number_field(req_str, "max_matches").unwrap_or(200) as usize;
                    crate::fs::search(&root, &path, &query, max).map(|data| format!("{{\"ok\":true,\"data\":{}}}", data))
                }
                "FsGlob" => {
                    let pattern = extract_string_field(req_str, "pattern").unwrap_or_else(|| "*".to_string());
                    let max = extract_number_field(req_str, "max_matches").unwrap_or(500) as usize;
                    crate::fs::glob(&root, &path, &pattern, max).map(|data| format!("{{\"ok\":true,\"data\":{}}}", data))
                }
                "FsMkdir" => {
                    let recursive = !req_str.contains("\"recursive\":false");
                    crate::fs::mkdir(&root, &path, recursive).map(|_| "{\"ok\":true}".to_string())
                }
                "FsRemove" => {
                    let recursive = req_str.contains("\"recursive\":true");
                    crate::fs::remove(&root, &path, recursive).map(|_| "{\"ok\":true}".to_string())
                }
                "FsRename" => {
                    let to = extract_string_field(req_str, "to").unwrap_or_default();
                    crate::fs::rename(&root, &path, &to).map(|_| "{\"ok\":true}".to_string())
                }
                "FsPatch" => {
                    // `patch` is text (unified diff); `patch_b64` is the escape
                    // hatch for byte-exact content.
                    let patch = extract_string_field(req_str, "patch").or_else(|| {
                        extract_string_field(req_str, "patch_b64")
                            .map(|b64| String::from_utf8_lossy(&base64_decode(&b64)).to_string())
                    });
                    let search = extract_string_field(req_str, "search");
                    let replace = extract_string_field(req_str, "replace");
                    let patch_ref = patch.as_deref();
                    let search_ref = search.as_deref();
                    let replace_ref = replace.as_deref();
                    crate::fs::apply_patch(&root, &path, patch_ref, search_ref, replace_ref)
                        .map(|updated| format!("{{\"ok\":true,\"bytes\":{}}}", updated.len()))
                }
                _ => unreachable!(),
            };
            match result {
                Ok(json) => json,
                Err(e) => format!("{{\"ok\":false,\"security\":{},\"error\":\"{}\"}}", e.security, escape_json(&e.message)),
            }
        }
        _ => {
            format!("{{\"ok\":false,\"error\":\"unknown method {}\"}}", escape_json(&method))
        }
    }
}

/// Workspace root of a runtime, used to confine filesystem operations.
/// Public field extractor for sibling modules (browser worker parsing).
pub fn extract_field_public(s: &str, field: &str) -> Option<String> {
    crate::json::get_str(s, field)
}

/// Public base64 decoder for sibling modules.
pub fn base64_decode_public(s: &str) -> Vec<u8> {
    base64_decode(s)
}

fn runtime_workspace(mgr: &RuntimeManager, runtime_id: &str) -> Option<PathBuf> {
    let id = RuntimeId::from_string(runtime_id.to_string());
    mgr.get_runtime(&id).map(|rt| rt.workspace)
}

fn extract_string_field(s: &str, field: &str) -> Option<String> {
    crate::json::get_str(s, field)
}

#[allow(dead_code)]
fn extract_string_field_legacy(s: &str, field: &str) -> Option<String> {
    // look for "field":"value" or "field": "value"
    let patterns = [format!("\"{}\":\"", field), format!("\"{}\" : \"", field), format!("\"{}\": \"", field)];
    for pat in &patterns {
        if let Some(start) = s.find(pat) {
            let rest = &s[start + pat.len()..];
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
    }
    // try single quotes?
    let patterns2 = [format!("\"{}\":'", field), format!("\"{}\": '", field)];
    for pat in &patterns2 {
        if let Some(start) = s.find(pat) {
            let rest = &s[start + pat.len()..];
            if let Some(end) = rest.find('\'') {
                return Some(rest[..end].to_string());
            }
        }
    }
    // try without quotes for id that may be like "id": "rt-..."
    // also try "field":value where value is not quoted but until comma
    let pat = format!("\"{}\":", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start + pat.len()..].trim_start();
        if rest.starts_with('"') {
            // already handled
        } else {
            // until comma or }
            let end = rest.find(|c| c == ',' || c == '}' || c == ' ' || c == '\n').unwrap_or(rest.len());
            let val = rest[..end].trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn extract_number_field(s: &str, field: &str) -> Option<u64> {
    crate::json::get_u64(s, field)
}

#[allow(dead_code)]
fn extract_number_field_legacy(s: &str, field: &str) -> Option<u64> {
    let pat = format!("\"{}\":", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start + pat.len()..].trim_start();
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if end > 0 {
            return rest[..end].parse().ok();
        }
    }
    None
}

fn escape_json(s: &str) -> String {
    crate::json::escape(s)
}

fn base64_encode(data: &[u8]) -> String {
    // simple base64 without external crate
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as u32;
        let b1 = if i+1 < data.len() { data[i+1] as u32 } else { 0 };
        let b2 = if i+2 < data.len() { data[i+2] as u32 } else { 0 };

        let n = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if i+1 < data.len() {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if i+2 < data.len() {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }

        i += 3;
    }
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // skip whitespace
        while i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b'\r' || bytes[i] == b' ' ) { i+=1; }
        if i+3 >= bytes.len() { break; }
        let mut vals = [0u8; 4];
        let mut padding = 0;
        let mut valid = true;
        for j in 0..4 {
            if i+j >= bytes.len() { valid = false; break; }
            let b = bytes[i+j];
            if b == b'=' {
                padding += 1;
                vals[j] = 0;
            } else {
                let v = table[b as usize];
                if v == 255 { valid = false; break; }
                vals[j] = v;
            }
        }
        if !valid { i+=1; continue; }
        let n = ((vals[0] as u32) << 18) | ((vals[1] as u32) << 12) | ((vals[2] as u32) << 6) | (vals[3] as u32);
        out.push(((n >> 16) & 0xFF) as u8);
        if padding < 2 { out.push(((n >> 8) & 0xFF) as u8); }
        if padding < 1 { out.push((n & 0xFF) as u8); }
        i += 4;
        if padding > 0 { break; }
    }
    out
}
