use host_agent::{AgentSession, AgentLoop, Model, MockModel, default_tool_registry, HostAgentApi, AgentStatus};
use host_agent::agent::session::{Message, Role, ToolCall};
use host_agent::model::ModelResponse;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        return;
    }

    match args[1].as_str() {
        "run" => {
            // Minimal bot run: user message -> loop
            let goal = if args.len() > 2 { args[2..].join(" ") } else { "help me explore this project".to_string() };
            println!("[host-agent] goal: {}", goal);

            let api = HostAgentApi::new();
            let mut session = match api.create_session("gpt-4o-mini".to_string()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("failed to create session: {}", e);
                    return;
                }
            };
            println!("[host-agent] created session {} runtime {}", session.id.0, session.runtime_id);

            session.add_system_message("You are a helpful assistant with tools: shell.exec, file.read, file.list, file.search, etc. Explore the project and answer.".to_string());
            session.add_user_message(goal.clone());

            // Try OpenAI model if env set, else mock
            let model: Box<dyn Model> = if std::env::var("OPENAI_API_KEY").is_ok() {
                match host_agent::OpenAICompatibleModel::from_env() {
                    Ok(m) => {
                        println!("[host-agent] using OpenAI model {}", m.model_name);
                        Box::new(m)
                    }
                    Err(e) => {
                        eprintln!("[host-agent] OpenAI env error {}, using mock", e);
                        Box::new(MockModel::new(vec![
                            ModelResponse {
                                content: "I'll explore the project".to_string(),
                                tool_calls: vec![ToolCall { id: "call-1".to_string(), name: "file.list".to_string(), arguments: r#"{"path":"."}"#.to_string() }],
                            },
                            ModelResponse {
                                content: "Let me check Cargo.toml".to_string(),
                                tool_calls: vec![ToolCall { id: "call-2".to_string(), name: "file.read".to_string(), arguments: r#"{"path":"Cargo.toml"}"#.to_string() }],
                            },
                            ModelResponse {
                                content: "Project is Rust workspace with sandd runtime kernel. Ready.".to_string(),
                                tool_calls: vec![],
                            },
                        ]))
                    }
                }
            } else {
                println!("[host-agent] no OPENAI_API_KEY, using mock model that lists files and reads Cargo.toml");
                Box::new(MockModel::new(vec![
                    ModelResponse {
                        content: "I'll explore the project structure".to_string(),
                        tool_calls: vec![ToolCall { id: "call-1".to_string(), name: "file.list".to_string(), arguments: r#"{"path":"/home/user/openbot/sand"}"#.to_string() }],
                    },
                    ModelResponse {
                        content: "Now check Cargo.toml".to_string(),
                        tool_calls: vec![ToolCall { id: "call-2".to_string(), name: "file.read".to_string(), arguments: r#"{"path":"/home/user/openbot/sand/Cargo.toml"}"#.to_string() }],
                    },
                    ModelResponse {
                        content: "Let me run cargo test list".to_string(),
                        tool_calls: vec![ToolCall { id: "call-3".to_string(), name: "shell.exec".to_string(), arguments: r#"{"command":"ls -la /home/user/openbot/sand/crates"}"#.to_string() }],
                    },
                    ModelResponse {
                        content: "This project is a Rust workspace implementing sandd Runtime Kernel with Runtime/Process/Exec/PTY/cgroup management. It has crates: sandd, sand-client, sand-cli, sand-protocol, host-agent. The sandd binary provides UDS RPC at /run/sand/sandd.sock. Ready for next phase.".to_string(),
                        tool_calls: vec![],
                    },
                ]))
            };

            let tools = default_tool_registry();
            let agent_loop = AgentLoop::new().with_max_iterations(10);

            let result = agent_loop.run(&mut session, model.as_ref(), &tools, |event| {
                match event {
                    host_agent::agent::LoopEvent::ModelCalled => println!("[loop] calling model..."),
                    host_agent::agent::LoopEvent::ToolCallStarted { name, args } => println!("[loop] tool call: {} {}", name, args),
                    host_agent::agent::LoopEvent::ToolCallFinished { name, result } => println!("[loop] tool result {}: {}", name, result.chars().take(500).collect::<String>()),
                    host_agent::agent::LoopEvent::Completed { result } => println!("[loop] completed: {}", result),
                    host_agent::agent::LoopEvent::Failed { reason } => println!("[loop] failed: {}", reason),
                    host_agent::agent::LoopEvent::Attention { attention } => println!("[loop] attention: {:?}", attention),
                    _ => {}
                }
            });

            match result {
                Ok(final_answer) => {
                    println!("\n=== Final Answer ===\n{}\n", final_answer);
                }
                Err(e) => {
                    eprintln!("loop failed: {}", e);
                }
            }

            // Cleanup
            let _ = sand_client::SandClient::new(None).destroy_runtime(&session.runtime_id);
            println!("[host-agent] runtime {} destroyed", session.runtime_id);
        }
        "session" => {
            if args.len() < 3 {
                eprintln!("usage: host-agent session <create|list>");
                return;
            }
            match args[2].as_str() {
                "create" => {
                    let api = HostAgentApi::new();
                    match api.create_session("gpt-4o-mini".to_string()) {
                        Ok(s) => println!("created session {} runtime {}", s.id.0, s.runtime_id),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "list" => {
                    let api = HostAgentApi::new();
                    match api.list_runtimes() {
                        Ok(list) => println!("{}", list),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                _ => eprintln!("unknown session subcommand"),
            }
        }
        "tools" => {
            let registry = default_tool_registry();
            for tool in registry.list() {
                println!("{}: {}", tool.name, tool.description);
            }
        }
        _ => print_help(),
    }
}

fn print_help() {
    println!("host-agent - Bot Brain Runtime");
    println!("");
    println!("Usage:");
    println!("  host-agent run [goal]          Run minimal bot loop with goal");
    println!("  host-agent session create      Create a new agent session + runtime");
    println!("  host-agent session list        List runtimes");
    println!("  host-agent tools               List available tools");
    println!("");
    println!("Env:");
    println!("  OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_MODEL for real LLM");
    println!("  Without API key, uses mock model that explores project");
}
