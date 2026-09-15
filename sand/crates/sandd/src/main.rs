mod runtime;
mod process;
mod exec;
mod pty;
mod state;
mod state_sqlite;
mod events;
mod cgroup;
mod rpc;
mod rpc_binary;
mod browser;
mod capabilities;
mod computer;
mod desktop;
mod fs;
mod json;
mod supervisor;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use sand_protocol::{RuntimeId, RuntimeKind, RuntimeState, Runtime, now_ms};

use runtime::RuntimeManager;
use state::StateManager;
use events::EventBus;
use cgroup::CgroupManager;
use rpc::RpcServer;
use rpc_binary::BinaryRpcServer;
use supervisor::Supervisor;

fn main() {
    // `sandd bridge --socket <path>`: run the SSH stdio ⇄ UDS tunnel instead of
    // the daemon. Used by host-agent's remote bootstrap when a machine has no
    // `sand` CLI installed yet (single self-contained binary upload).
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() > 1 && argv[1] == "bridge" {
        let options = match sand_bridge::options_from_args(argv[2..].to_vec()) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("error: {}", e);
                eprintln!("usage: sandd bridge [--socket <sandd.sock>] [-v]");
                std::process::exit(2);
            }
        };
        if let Err(e) = sand_bridge::run(options) {
            eprintln!("[sandd bridge] error: {}", e);
            std::process::exit(1);
        }
        return;
    }
    if argv.len() > 1 && (argv[1] == "--version" || argv[1] == "-V") {
        println!("sandd {}", sand_protocol::SANDB_VERSION);
        return;
    }

    // setup tracing via eprintln
    eprintln!("[sandd] starting {} at {}", sand_protocol::SANDB_VERSION, now_ms());

    // Socket directory. `--socket-dir` (or $SAND_SOCKET_DIR) is how the remote
    // bootstrap points sandd at ~/.cache/spark/run without needing root; the
    // /run/sand → /tmp/sandd fallback keeps existing single-machine setups
    // working unchanged.
    let flags = parse_flags(&argv);
    let base_dir = match flags.socket_dir {
        Some(dir) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("[sandd] cannot create socket dir {}: {}", dir.display(), e);
                std::process::exit(1);
            }
            restrict_permissions(&dir);
            dir
        }
        None => {
            if Path::new("/run/sand").exists() || std::fs::create_dir_all("/run/sand").is_ok() {
                PathBuf::from("/run/sand")
            } else {
                let _ = std::fs::create_dir_all("/tmp/sandd");
                PathBuf::from("/tmp/sandd")
            }
        }
    };
    let sock_path = base_dir.join("sandd.sock");
    let binary_sock_path = base_dir.join("sandd-binary.sock");

    // clean old sockets
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&binary_sock_path);

    // init managers
    let state_path = match flags.data_dir {
        Some(dir) => {
            let _ = std::fs::create_dir_all(&dir);
            dir.join("state.json")
        }
        None => base_dir.join("state.json"),
    };

    let state_mgr = Arc::new(StateManager::new(state_path));
    // Bring pre-existing databases up to the current schema (runtime.machine_id).
    if let Err(e) = state_mgr.ensure_schema() {
        eprintln!("[sandd] schema migration failed: {:?}", e);
    }
    let cgroup_mgr = Arc::new(CgroupManager::new());
    let event_bus = Arc::new(EventBus::new());
    let runtime_mgr = Arc::new(RuntimeManager::new(state_mgr.clone(), cgroup_mgr.clone(), event_bus.clone()));

    // try to recover from previous state
    if let Err(e) = runtime_mgr.recover() {
        eprintln!("[sandd] recover failed: {:?}", e);
    }

    // start supervisor
    let supervisor = std::sync::Arc::new(Supervisor::new(runtime_mgr.clone()));
    supervisor.clone().run_loop();
    eprintln!("[sandd] supervisor started (DesiredState/ObservedState reconcile every 5s)");

    // start binary RPC server in background thread
    let binary_mgr = runtime_mgr.clone();
    let binary_path = binary_sock_path.clone();
    std::thread::spawn(move || {
        let binary_server = BinaryRpcServer::new(binary_path, binary_mgr);
        if let Err(e) = binary_server.run() {
            eprintln!("[sandd] binary rpc error: {:?}", e);
        }
    });

    // start RPC server (blocking)
    let rpc_server = RpcServer::new(sock_path.clone(), runtime_mgr.clone());

    let info = sand_protocol::status::machine_info();
    eprintln!(
        "[sandd] machine {} ({}, {}, {} cores, uptime {}s) protocol v{}",
        info.machine_id,
        info.os,
        info.hostname,
        info.cpu_cores,
        info.uptime_seconds,
        sand_protocol::SAND_PROTOCOL_VERSION
    );
    eprintln!("[sandd] listening on {} and binary {}", sock_path.display(), binary_sock_path.display());

    // handle signals for graceful shutdown? simple loop
    if let Err(e) = rpc_server.run() {
        eprintln!("[sandd] rpc server error: {:?}", e);
    }

    eprintln!("[sandd] exiting");
}

// small helper for CLI testing without RPC: allow direct calls via env?

/// Minimal flag parser for the daemon binary.
struct Flags {
    socket_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
}

fn parse_flags(argv: &[String]) -> Flags {
    let mut flags = Flags {
        socket_dir: std::env::var("SAND_SOCKET_DIR").ok().map(PathBuf::from),
        data_dir: std::env::var("SAND_DATA_DIR").ok().map(PathBuf::from),
    };
    let mut iter = argv.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--socket-dir" | "--dir" => {
                if let Some(value) = iter.next() {
                    flags.socket_dir = Some(PathBuf::from(value));
                }
            }
            "--data-dir" => {
                if let Some(value) = iter.next() {
                    flags.data_dir = Some(PathBuf::from(value));
                }
            }
            other => {
                if let Some(value) = other.strip_prefix("--socket-dir=") {
                    flags.socket_dir = Some(PathBuf::from(value));
                } else if let Some(value) = other.strip_prefix("--data-dir=") {
                    flags.data_dir = Some(PathBuf::from(value));
                }
            }
        }
    }
    flags
}

/// Sockets must not be world readable: sandd RPC can start processes.
fn restrict_permissions(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}
