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
mod desktop;
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
    // setup tracing via eprintln
    eprintln!("[sandd] starting at {}", now_ms());

    // ensure /run/sand exists, fallback to /tmp/sandd
    let base_dir = if Path::new("/run/sand").exists() || std::fs::create_dir_all("/run/sand").is_ok() {
        PathBuf::from("/run/sand")
    } else {
        let _ = std::fs::create_dir_all("/tmp/sandd");
        PathBuf::from("/tmp/sandd")
    };
    let sock_path = base_dir.join("sandd.sock");
    let binary_sock_path = base_dir.join("sandd-binary.sock");

    // clean old sockets
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&binary_sock_path);

    // init managers
    let state_path = if Path::new("/run/sand").exists() {
        PathBuf::from("/run/sand/state.json")
    } else {
        PathBuf::from("/tmp/sandd/state.json")
    };

    let state_mgr = Arc::new(StateManager::new(state_path));
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

    eprintln!("[sandd] listening on {} and binary {}", sock_path.display(), binary_sock_path.display());

    // handle signals for graceful shutdown? simple loop
    if let Err(e) = rpc_server.run() {
        eprintln!("[sandd] rpc server error: {:?}", e);
    }

    eprintln!("[sandd] exiting");
}

// small helper for CLI testing without RPC: allow direct calls via env?
