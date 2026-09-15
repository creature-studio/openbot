//! host-agent — the process that owns machines, runtimes and the agent loop.
//!
//! ```text
//! host-agent run                      agent loop (uses the machine registry)
//! host-agent machine list             machines + status + latency
//! host-agent machine add <name> <host> [--user u] [--port p] [--alias a]
//! host-agent machine test <host>      `ssh host true` + host key report
//! host-agent machine connect <id>     connect + handshake (+ bootstrap)
//! host-agent machine reconnect <id>   backoff reconnect, lists live runtimes
//! host-agent machine disconnect <id>  closes the bridge only — runtimes stay
//! host-agent machine bootstrap <id>   force re-install + start sandd
//! host-agent machine exec <id> <cmd>  run a command inside a new runtime
//! host-agent machine trust <id>       user confirmed a host key fingerprint
//! host-agent session create|list      runtime helpers (legacy)
//! host-agent tools                    list tools
//! ```
//!
//! Everything machine-related goes through [`MachineManager`]: this binary is
//! the only place in Spark that opens an SSH connection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use host_agent::{
    default_tool_registry, AgentLoop, AgentStatus, HostAgentApi, MachineManager, Model, MockModel,
    SqlitePersistence,
};
use host_agent::agent::session::ToolCall;
use host_agent::model::ModelResponse;
use host_agent::tools::ToolExecutionContext;
use spark_model::MachineId;
use spark_transport::MachineDraft;

fn get_persistence() -> Option<Arc<SqlitePersistence>> {
    let path = if std::path::Path::new("/run/sand").exists() {
        PathBuf::from("/run/sand/state.db")
    } else {
        PathBuf::from("/tmp/sandd/state.db")
    };
    match SqlitePersistence::new(path.clone()) {
        Ok(p) => Some(Arc::new(p)),
        Err(e) => {
            eprintln!("[host-agent] persistence init failed: {e}, continuing without");
            None
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        return;
    }

    match args[1].as_str() {
        "machine" => machine_cli(&args[2..]),
        "serve" => {
            // The GPUI client talks to this socket; it never opens SSH itself.
            let socket = args
                .iter()
                .position(|arg| arg == "--socket")
                .and_then(|index| args.get(index + 1))
                .map(PathBuf::from);
            let manager = machine_manager();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            if let Err(e) = runtime.block_on(host_agent::serve::serve(manager, socket)) {
                eprintln!("serve failed: {e}");
                std::process::exit(1);
            }
        }
        "run" => run_agent(&args[2..]),
        "session" => {
            if args.len() < 3 {
                eprintln!("usage: host-agent session <create|list>");
                return;
            }
            match args[2].as_str() {
                "create" => {
                    let api = HostAgentApi::new();
                    match api.create_session("gpt-4o-mini".to_string()) {
                        Ok(s) => println!("created session {} runtime {}", s.id, s.runtime_id),
                        Err(e) => eprintln!("error: {e}"),
                    }
                }
                "list" => {
                    let api = HostAgentApi::new();
                    match api.list_runtimes() {
                        Ok(list) => println!("{list}"),
                        Err(e) => eprintln!("error: {e}"),
                    }
                }
                _ => eprintln!("unknown session subcommand"),
            }
        }
        "tools" => {
            for tool in default_tool_registry().list() {
                println!("{}: {}", tool.name, tool.description);
            }
        }
        _ => print_help(),
    }
}

// ---------------------------------------------------------------------------
// Machine CLI — this is what the E2E script drives
// ---------------------------------------------------------------------------

fn machine_manager() -> Arc<MachineManager> {
    match get_persistence() {
        Some(persistence) => Arc::new(MachineManager::with_persistence(persistence)),
        None => Arc::new(MachineManager::new()),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(future)
}

fn machine_cli(args: &[String]) {
    let manager = machine_manager();
    if args.is_empty() {
        println!("usage: host-agent machine <list|add|test|connect|reconnect|disconnect|bootstrap|exec|trust|status>");
        return;
    }

    match args[0].as_str() {
        "list" => {
            for machine in manager.list_machines() {
                let latency = machine
                    .metadata
                    .latency_ms
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{:<24} {:<10} {:<20} latency={:<8} sandd={}",
                    machine.id.as_str(),
                    machine.status.as_str(),
                    machine.connection_target(),
                    latency,
                    machine.metadata.sandd_version.as_deref().unwrap_or("-")
                );
            }
        }
        "add" => {
            // host-agent machine add <name> <host> [--user u] [--port p] [--alias a]
            if args.len() < 3 {
                eprintln!("usage: host-agent machine add <name> <host> [--user u] [--port p] [--alias a]");
                return;
            }
            let mut draft = MachineDraft::new(args[1].clone(), args[2].clone());
            let mut index = 3;
            while index < args.len() {
                match args[index].as_str() {
                    "--user" => {
                        if let Some(value) = args.get(index + 1) {
                            draft = draft.with_user(value.clone());
                        }
                        index += 2;
                    }
                    "--port" => {
                        if let Some(value) = args.get(index + 1).and_then(|v| v.parse().ok()) {
                            draft = draft.with_port(value);
                        }
                        index += 2;
                    }
                    "--alias" => {
                        if let Some(value) = args.get(index + 1) {
                            draft = draft.with_alias(value.clone());
                        }
                        index += 2;
                    }
                    _ => index += 1,
                }
            }
            match block_on(manager.add_machine(&draft)) {
                Ok(machine) => println!(
                    "added machine {} ({}) status={}",
                    machine.id.as_str(),
                    machine.connection_target(),
                    machine.status.as_str()
                ),
                Err(e) => eprintln!("add failed: {e}"),
            }
        }
        "test" => {
            let Some(target) = args.get(1) else {
                eprintln!("usage: host-agent machine test <host> [--user u] [--port p]");
                return;
            };
            let draft = MachineDraft::new(target.clone(), target.clone());
            match block_on(manager.test_connection(&draft)) {
                Ok(()) => println!("ssh {target}: ok"),
                Err(e) => println!("ssh {target}: {e}"),
            }
        }
        "connect" => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: host-agent machine connect <machine_id>");
                return;
            };
            let machine_id = MachineId::from_string(id.clone());
            match block_on(manager.connect_machine(&machine_id)) {
                Ok(()) => println!("connected {id}"),
                Err(e) => {
                    eprintln!("connect failed: {e}");
                    if let Some(message) = manager.attention_message(&machine_id) {
                        eprintln!("attention: {message}");
                    }
                    std::process::exit(1);
                }
            }
        }
        "reconnect" => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: host-agent machine reconnect <machine_id>");
                return;
            };
            let machine_id = MachineId::from_string(id.clone());
            match block_on(manager.reconnect_machine(&machine_id)) {
                Ok(runtimes) => {
                    println!("reconnected {id}");
                    println!("live runtimes ({}):", runtimes.len());
                    for runtime in runtimes {
                        println!("  {runtime}");
                    }
                }
                Err(e) => {
                    eprintln!("reconnect failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "disconnect" => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: host-agent machine disconnect <machine_id>");
                return;
            };
            let machine_id = MachineId::from_string(id.clone());
            match block_on(manager.disconnect_machine(&machine_id)) {
                Ok(()) => println!("disconnected {id} (remote runtimes untouched)"),
                Err(e) => eprintln!("disconnect failed: {e}"),
            }
        }
        "bootstrap" => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: host-agent machine bootstrap <machine_id>");
                return;
            };
            let machine_id = MachineId::from_string(id.clone());
            match block_on(manager.bootstrap_machine(&machine_id)) {
                Ok(report) => println!("{}", report.summary()),
                Err(e) => {
                    eprintln!("bootstrap failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "exec" => {
            // host-agent machine exec <machine_id> <kind> <command...>
            if args.len() < 4 {
                eprintln!("usage: host-agent machine exec <machine_id> <kind> <command...>");
                return;
            }
            let machine_id = MachineId::from_string(args[1].clone());
            let kind = args[2].clone();
            let command: Vec<String> = args[3..].to_vec();

            let result = block_on(async {
                let info = manager.create_runtime(&machine_id, &kind, None).await?;
                let transport = manager
                    .transport(&machine_id)
                    .ok_or_else(|| anyhow::anyhow!("machine not found"))?;
                let exec = transport
                    .exec(spark_transport::ExecRequest {
                        runtime_id: info.id.clone(),
                        command,
                        cwd: None,
                        env: HashMap::new(),
                        timeout_ms: Some(60_000),
                        stdin_data: None,
                    })
                    .await?;
                Ok::<_, anyhow::Error>((info.id, exec))
            });

            match result {
                Ok((runtime_id, exec)) => {
                    print!("{}", exec.stdout_lossy());
                    let stderr = exec.stderr_lossy();
                    if !stderr.is_empty() {
                        eprint!("{stderr}");
                    }
                    println!(
                        "[host-agent] runtime {} on {} exit_code={:?} in {}ms",
                        runtime_id,
                        machine_id,
                        exec.exit_code,
                        exec.duration_ms
                    );
                }
                Err(e) => {
                    eprintln!("exec failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "trust" => {
            // `machine trust` cannot invent confirmation: it prints what the
            // user must accept, and points at the UI path.
            let Some(id) = args.get(1) else {
                eprintln!("usage: host-agent machine trust <machine_id>");
                return;
            };
            let machine_id = MachineId::from_string(id.clone());
            match manager.attention_message(&machine_id) {
                Some(message) => println!("{message}\n(在 GPUI 客户端确认后点 [信任并连接]，或在 ~/.ssh/known_hosts 中手动添加)"),
                None => println!("machine {id} needs no confirmation"),
            }
        }
        "status" => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: host-agent machine status <machine_id>");
                return;
            };
            let machine_id = MachineId::from_string(id.clone());
            match manager.get_machine(&machine_id) {
                Some(machine) => {
                    println!("{}", serde_json::to_string_pretty(&machine).unwrap_or_default());
                }
                None => eprintln!("machine not found: {id}"),
            }
        }
        other => eprintln!("unknown machine subcommand: {other}"),
    }
}

// ---------------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------------

fn run_agent(args: &[String]) {
    // host-agent run [--machine <machine_id>] [goal...]
    let mut machine_id = MachineId::local();
    let mut goal_parts: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--machine" | "-m" => {
                if let Some(value) = args.get(index + 1) {
                    machine_id = MachineId::from_string(value.clone());
                }
                index += 2;
            }
            other => {
                goal_parts.push(other.to_string());
                index += 1;
            }
        }
    }
    let goal = if goal_parts.is_empty() {
        "help me explore this project".to_string()
    } else {
        goal_parts.join(" ")
    };

    println!("[host-agent] goal: {goal}");
    println!("[host-agent] machine: {machine_id}");

    let manager = machine_manager();
    if !machine_id.is_local() {
        // A remote machine must be reachable before we create a runtime on it.
        // Failures here are reported, not fatal: the machine record keeps the
        // reason so the UI can show it.
        match block_on(manager.connect_machine(&machine_id)) {
            Ok(()) => println!("[host-agent] connected to {machine_id}"),
            Err(e) => {
                eprintln!("[host-agent] cannot connect {machine_id}: {e}");
                if let Some(message) = manager.attention_message(&machine_id) {
                    eprintln!("[host-agent] attention: {message}");
                }
                return;
            }
        }
    }

    // Which machine a runtime lives on. Tools read this; the session fills it in
    // when the runtime is created. Until then a runtime is local.
    let runtime_machines: Arc<Mutex<HashMap<String, MachineId>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let manager_for_transport = manager.clone();
    let manager_for_machine = manager.clone();
    let manager_for_list = manager.clone();
    let machines_for_lookup = runtime_machines.clone();

    let ctx = ToolExecutionContext::with_runtime_machine(
        Arc::new(move |machine_id| manager_for_transport.transport(machine_id)),
        Arc::new(move |machine_id| manager_for_machine.get_machine(machine_id)),
        Arc::new(move || manager_for_list.list_machines()),
        machine_id.clone(),
        Arc::new(move |runtime_id| {
            machines_for_lookup
                .lock()
                .ok()
                .and_then(|map| map.get(runtime_id).cloned())
        }),
    );

    // Health checks run alongside the agent so the UI's status dots stay honest.
    {
        let manager = manager.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                runtime.block_on(async move {
                    manager.connect_saved_machines().await;
                    manager.run_health_loop().await;
                });
            }
        });
    }

    // The API owns the session → runtime → machine chain.
    let api = HostAgentApi::with_machines(manager.clone());
    let mut session = match block_on(api.create_session_on(&machine_id, "task", "gpt-4o-mini".into())) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("failed to create session on {machine_id}: {e}");
            return;
        }
    };
    if let Ok(mut map) = runtime_machines.lock() {
        map.insert(session.runtime_id.clone(), session.machine_id().clone());
    }
    println!(
        "[host-agent] session {} runtime {} on machine {}",
        session.id,
        session.runtime_id,
        session.machine_id()
    );

    if let Some(persistence) = get_persistence() {
        let _ = persistence.save_session(
            &session.id,
            &session.runtime_id,
            &session.model,
            session.status.as_str(),
            session.goal.as_deref(),
            &session.cwd,
            session.created_at,
            session.updated_at,
            "[]",
        );
    }

    session.add_system_message(
        "You are a helpful assistant with tools: shell.exec, file.read, file.list, file.search, \
         file.patch, terminal.open/write/read, browser.open/snapshot/click/fill, computer.*. \
         Tools run inside the task's runtime on whichever machine the task was created for; you \
         never need to know whether that machine is local or remote."
            .to_string(),
    );
    session.add_user_message(goal.to_string());

    let model: Box<dyn Model> = match host_agent::OpenAICompatibleModel::from_env() {
        Ok(m) => {
            println!("[host-agent] using OpenAI model {}", m.model_name);
            Box::new(m)
        }
        Err(e) => {
            if std::env::var("OPENAI_API_KEY").is_ok() {
                eprintln!("[host-agent] OpenAI env error {e}, using mock");
            } else {
                println!("[host-agent] no OPENAI_API_KEY, using mock model");
            }
            Box::new(MockModel::new(vec![
                ModelResponse {
                    content: "I'll explore the project structure".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "call-1".to_string(),
                        name: "file.list".to_string(),
                        arguments: r#"{"path":"."}"#.to_string(),
                    }],
                },
                ModelResponse {
                    content: "Let me check the workspace layout".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "call-2".to_string(),
                        name: "shell.exec".to_string(),
                        arguments: r#"{"command":"ls -la"}"#.to_string(),
                    }],
                },
                ModelResponse {
                    content: "Explored the runtime workspace. Ready for the next step.".to_string(),
                    tool_calls: vec![],
                },
            ]))
        }
    };

    let tools = default_tool_registry();
    let agent_loop = AgentLoop::new().with_max_iterations(10);
    let result = agent_loop.run_with_context(
        &mut session,
        model.as_ref(),
        &tools,
        Some(&ctx),
        |event| match event {
            host_agent::agent::LoopEvent::ModelCalled { iteration } => {
                println!("[loop] calling model iteration {iteration}...")
            }
            host_agent::agent::LoopEvent::ToolCallStarted { name, args, call_id } => {
                println!("[loop] tool call {call_id} {name} {args}")
            }
            host_agent::agent::LoopEvent::ToolCallFinished {
                name,
                result,
                call_id,
                status,
                duration_ms,
            } => println!(
                "[loop] tool result {call_id} {name} status={status} duration={duration_ms}ms: {}",
                result.chars().take(500).collect::<String>()
            ),
            host_agent::agent::LoopEvent::Completed { result } => {
                println!("[loop] completed: {result}")
            }
            host_agent::agent::LoopEvent::Failed { reason } => println!("[loop] failed: {reason}"),
            host_agent::agent::LoopEvent::Attention { attention } => {
                println!("[loop] attention: {attention:?}")
            }
            host_agent::agent::LoopEvent::Checkpoint { session_id, iteration } => {
                println!("[loop] checkpoint session {session_id} iter {iteration}")
            }
            host_agent::agent::LoopEvent::Recovery { session_id, from_iteration } => {
                println!("[loop] recovery session {session_id} from {from_iteration}")
            }
            host_agent::agent::LoopEvent::Budget { tokens_used, tool_calls, elapsed_ms } => {
                println!(
                    "[loop] budget tokens={tokens_used} calls={tool_calls} elapsed={elapsed_ms}ms"
                )
            }
            host_agent::agent::LoopEvent::ModelStreaming { content } => print!("{content}"),
            _ => {}
        },
    );

    match result {
        Ok(final_answer) => println!("\n=== Final Answer ===\n{final_answer}\n"),
        Err(e) => eprintln!("loop failed: {e}"),
    }

    // Task end destroys the *runtime*, never the machine: sandd, other runtimes,
    // PTYs and browsers on that machine keep running.
    match block_on(api.destroy_session_runtime(&session)) {
        Ok(()) => println!(
            "[host-agent] runtime {} destroyed (machine {})",
            session.runtime_id,
            session.machine_id()
        ),
        Err(e) => eprintln!(
            "[host-agent] destroy runtime {} failed: {e}",
            session.runtime_id
        ),
    }
}

fn print_help() {
    println!("host-agent - Bot Brain Runtime");
    println!();
    println!("Usage:");
    println!("  host-agent run [--machine <id>] [goal]   Run the agent loop on a machine");
    println!("  host-agent machine <subcommand>    Manage machines (see below)");
    println!("  host-agent serve [--socket PATH]   Serve the GPUI client (Unix socket)");
    println!("  host-agent session create|list     Runtime helpers");
    println!("  host-agent tools                   List available tools");
    println!();
    println!("Machine subcommands:");
    println!("  list | add | test | connect | reconnect | disconnect | bootstrap | exec | trust | status");
    println!();
    println!("Env:");
    println!("  OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_MODEL for a real LLM");
    println!("  SPARK_SANDD_BINARY / SPARK_DIST_DIR to choose the sandd uploaded by bootstrap");
}
