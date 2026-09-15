mod runtime;
mod process;
mod exec;
mod pty;
mod state;
mod events;
mod cgroup;
mod rpc;

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

fn main() {
    // setup tracing via eprintln
    eprintln!("[sandd] starting at {}", now_ms());

    // ensure /run/sand exists, fallback to /tmp/sandd
    let sock_path = if Path::new("/run/sand").exists() || std::fs::create_dir_all("/run/sand").is_ok() {
        PathBuf::from("/run/sand/sandd.sock")
    } else {
        let _ = std::fs::create_dir_all("/tmp/sandd");
        PathBuf::from("/tmp/sandd/sandd.sock")
    };

    // clean old socket
    let _ = std::fs::remove_file(&sock_path);

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

    // start RPC server
    let rpc_server = RpcServer::new(sock_path.clone(), runtime_mgr.clone());

    eprintln!("[sandd] listening on {}", sock_path.display());

    // handle signals for graceful shutdown? simple loop
    if let Err(e) = rpc_server.run() {
        eprintln!("[sandd] rpc server error: {:?}", e);
    }

    eprintln!("[sandd] exiting");
}

// small helper for CLI testing without RPC: allow direct calls via env?
