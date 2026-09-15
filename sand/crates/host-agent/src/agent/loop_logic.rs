use super::session::{AgentSession, Message, Role};
use super::state::{AgentStatus, Attention};
use crate::model::{Model, ModelResponse};
use crate::tools::{ToolRegistry, ToolResult, ToolStatus};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH, Instant};

#[derive(Debug, Clone)]
pub enum LoopEvent {
    MessageAdded { role: String, content: String },
    ToolCallStarted { name: String, args: String, call_id: String },
    ToolCallFinished { name: String, result: String, call_id: String, status: String, duration_ms: u64 },
    ModelCalled { iteration: usize },
    ModelStreaming { content: String },
    Attention { attention: Attention },
    Checkpoint { session_id: String, iteration: usize },
    Recovery { session_id: String, from_iteration: usize },
    Budget { tokens_used: usize, tool_calls: usize, elapsed_ms: u64 },
    Context { messages: usize, truncated: bool },
    Completed { result: String },
    Failed { reason: String },
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Budget {
    pub max_iterations: usize,
    pub max_tokens: usize,
    pub max_tool_calls: usize,
    pub max_elapsed_ms: u64,
    pub tokens_used: usize,
    pub tool_calls: usize,
    pub started_at: Instant,
}

impl Budget {
    pub fn new(max_iterations: usize) -> Self {
        Self {
            max_iterations,
            max_tokens: 100_000,
            max_tool_calls: 200,
            max_elapsed_ms: 30 * 60 * 1000, // 30 min
            tokens_used: 0,
            tool_calls: 0,
            started_at: Instant::now(),
        }
    }
    pub fn check(&self, iteration: usize) -> Result<(), String> {
        if iteration >= self.max_iterations {
            return Err(format!("budget exceeded: max iterations {} reached", self.max_iterations));
        }
        if self.tokens_used >= self.max_tokens {
            return Err(format!("budget exceeded: max tokens {} reached", self.max_tokens));
        }
        if self.tool_calls >= self.max_tool_calls {
            return Err(format!("budget exceeded: max tool calls {} reached", self.max_tool_calls));
        }
        if self.started_at.elapsed().as_millis() as u64 >= self.max_elapsed_ms {
            return Err(format!("budget exceeded: max elapsed {}ms reached", self.max_elapsed_ms));
        }
        Ok(())
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}

pub struct AgentLoop {
    pub max_iterations: usize,
    pub enable_checkpoint: bool,
    pub enable_recovery: bool,
    pub context_window: usize,
    pub budget: Budget,
}

impl AgentLoop {
    pub fn new() -> Self {
        Self {
            max_iterations: 50,
            enable_checkpoint: true,
            enable_recovery: true,
            context_window: 20, // keep last 20 messages, summarize older
            budget: Budget::new(50),
        }
    }

    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self.budget.max_iterations = max;
        self
    }

    pub fn with_checkpoint(mut self, enable: bool) -> Self {
        self.enable_checkpoint = enable;
        self
    }

    pub fn with_recovery(mut self, enable: bool) -> Self {
        self.enable_recovery = enable;
        self
    }

    // Core loop with streaming, cancellation, checkpoint, recovery, context, budget, attention, completion
    pub fn run<F>(&self, session: &mut AgentSession, model: &dyn Model, tools: &ToolRegistry, mut event_cb: F) -> Result<String, String>
    where
        F: FnMut(LoopEvent),
    {
        self.run_with_cancel(session, model, tools, Arc::new(AtomicBool::new(false)), &mut event_cb)
    }

    pub fn run_with_cancel<F>(&self, session: &mut AgentSession, model: &dyn Model, tools: &ToolRegistry, cancel_flag: Arc<AtomicBool>, event_cb: &mut F) -> Result<String, String>
    where
        F: FnMut(LoopEvent),
    {
        // Recovery: if session has checkpoint, resume from there
        let mut start_iteration = 0;
        if self.enable_recovery {
            if let Some(checkpoint_iter) = session.metadata.get("checkpoint_iteration").and_then(|s| s.parse::<usize>().ok()) {
                start_iteration = checkpoint_iter;
                event_cb(LoopEvent::Recovery { session_id: session.id.clone(), from_iteration: start_iteration });
                eprintln!("[agent-loop] recovery: resuming from iteration {}", start_iteration);
            }
        }

        session.status = AgentStatus::Running;
        let mut iterations = start_iteration;
        let mut budget = self.budget.clone();

        loop {
            // Cancellation check
            if cancel_flag.load(Ordering::Relaxed) {
                session.status = AgentStatus::Failed { reason: "cancelled".to_string() };
                event_cb(LoopEvent::Cancelled);
                return Err("cancelled".to_string());
            }

            // Budget check
            if let Err(e) = budget.check(iterations) {
                session.status = AgentStatus::Failed { reason: e.clone() };
                event_cb(LoopEvent::Failed { reason: e.clone() });
                return Err(e);
            }

            if iterations >= self.max_iterations {
                session.status = AgentStatus::Failed { reason: "max iterations reached".to_string() };
                event_cb(LoopEvent::Failed { reason: "max iterations reached".to_string() });
                return Err("max iterations reached".to_string());
            }
            iterations += 1;

            // Context management: truncate old messages if too many
            let context_truncated = if session.messages.len() > self.context_window {
                // Keep system + last N messages, summarize older? For MVP, just keep last N
                let keep = self.context_window;
                let total = session.messages.len();
                // For simplicity, we don't actually truncate session.messages here, but we emit event
                // In real implementation, we would summarize older messages via model
                event_cb(LoopEvent::Context { messages: total, truncated: true });
                true
            } else {
                event_cb(LoopEvent::Context { messages: session.messages.len(), truncated: false });
                false
            };

            // Model call with streaming simulation
            event_cb(LoopEvent::ModelCalled { iteration: iterations });
            event_cb(LoopEvent::Budget { tokens_used: budget.tokens_used, tool_calls: budget.tool_calls, elapsed_ms: budget.elapsed_ms() });

            let response = model.chat(&session.messages, tools).map_err(|e| {
                session.status = AgentStatus::Failed { reason: e.clone() };
                e
            })?;

            // Streaming: emit content chunks
            if !response.content.is_empty() {
                // Simulate streaming by chunking content
                for chunk in response.content.chars().collect::<Vec<_>>().chunks(20) {
                    if cancel_flag.load(Ordering::Relaxed) {
                        session.status = AgentStatus::Failed { reason: "cancelled".to_string() };
                        event_cb(LoopEvent::Cancelled);
                        return Err("cancelled".to_string());
                    }
                    let chunk_str: String = chunk.iter().collect();
                    event_cb(LoopEvent::ModelStreaming { content: chunk_str });
                }
            }

            budget.tokens_used += response.content.len() / 4; // rough estimate

            // If no tool calls, it's final answer -> completion
            if response.tool_calls.is_empty() {
                let final_content = response.content.clone();
                session.add_message(Message {
                    role: Role::Assistant,
                    content: final_content.clone(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    name: None,
                });
                session.status = AgentStatus::Completed;
                // Checkpoint final
                if self.enable_checkpoint {
                    session.metadata.insert("checkpoint_iteration".to_string(), iterations.to_string());
                    event_cb(LoopEvent::Checkpoint { session_id: session.id.clone(), iteration: iterations });
                }
                event_cb(LoopEvent::Completed { result: final_content.clone() });
                event_cb(LoopEvent::Attention { attention: Attention::Completed });
                return Ok(final_content);
            }

            // Add assistant message with tool calls
            session.add_message(Message {
                role: Role::Assistant,
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
                tool_call_id: None,
                name: None,
            });

            event_cb(LoopEvent::MessageAdded { role: "assistant".to_string(), content: response.content.clone() });

            // Execute tool calls
            session.status = AgentStatus::WaitingTool;
            let mut should_freeze = false;
            let mut freeze_reason = String::new();

            for tc in &response.tool_calls {
                // Cancellation check before each tool
                if cancel_flag.load(Ordering::Relaxed) {
                    session.status = AgentStatus::Failed { reason: "cancelled".to_string() };
                    event_cb(LoopEvent::Cancelled);
                    return Err("cancelled".to_string());
                }

                budget.tool_calls += 1;
                let call_id = tc.id.clone();
                event_cb(LoopEvent::ToolCallStarted { name: tc.name.clone(), args: tc.arguments.clone(), call_id: call_id.clone() });

                let start = SystemTime::now();
                let result = tools.execute(&tc.name, &tc.arguments, &session.runtime_id);
                let duration_ms = start.elapsed().unwrap_or_default().as_millis() as u64;

                let (result_str, status_str, is_permission, is_complete) = match &result {
                    Ok(r) => {
                        let s = r.content.clone();
                        let status = r.status.as_str().to_string();
                        let perm = r.status == ToolStatus::PermissionRequired || s.contains("permission_required") || s.contains("PERMISSION_REQUIRED");
                        let complete = tc.name == "task.complete" || s.contains("ReadyForCheck") || s.contains("task completed") || r.tool_name == "task.complete";
                        (s, status, perm, complete)
                    }
                    Err(e) => (format!("error: {}", e), "error".to_string(), false, false),
                };

                // Attention handling
                if is_permission {
                    let attention = Attention::PermissionRequired { tool: tc.name.clone(), reason: result_str.clone() };
                    event_cb(LoopEvent::Attention { attention: attention.clone() });
                    session.status = AgentStatus::WaitingInput;
                    should_freeze = true;
                    freeze_reason = format!("permission_required for {}", tc.name);
                }

                if is_complete {
                    should_freeze = true;
                    freeze_reason = "task.complete ReadyForCheck".to_string();
                    event_cb(LoopEvent::Attention { attention: Attention::ReadyForCheck });
                }

                // Add tool result message
                session.add_message(Message {
                    role: Role::Tool,
                    content: result_str.clone(),
                    tool_calls: vec![],
                    tool_call_id: Some(call_id.clone()),
                    name: Some(tc.name.clone()),
                });

                event_cb(LoopEvent::MessageAdded { role: "tool".to_string(), content: result_str.clone() });
                event_cb(LoopEvent::ToolCallFinished { name: tc.name.clone(), result: result_str.clone(), call_id: call_id.clone(), status: status_str, duration_ms });

                // Checkpoint after each tool
                if self.enable_checkpoint {
                    session.metadata.insert("checkpoint_iteration".to_string(), iterations.to_string());
                    session.metadata.insert("last_tool".to_string(), tc.name.clone());
                    // In real impl, persist session to disk/sqlite
                    event_cb(LoopEvent::Checkpoint { session_id: session.id.clone(), iteration: iterations });
                }
            }

            if should_freeze {
                if session.status == AgentStatus::WaitingInput {
                    event_cb(LoopEvent::Attention { attention: Attention::WaitingInput });
                    return Ok(format!("Permission required, awaiting approval: {}", freeze_reason));
                } else {
                    session.status = AgentStatus::ReadyForCheck;
                    event_cb(LoopEvent::Completed { result: format!("Task marked ReadyForCheck, awaiting human approval: {}", freeze_reason) });
                    return Ok("Task completed, awaiting approval (ReadyForCheck)".to_string());
                }
            }

            session.status = AgentStatus::Running;
        }
    }

    // Streaming version that returns iterator of events
    pub fn run_streaming<F>(&self, session: &mut AgentSession, model: &dyn Model, tools: &ToolRegistry, cancel_flag: Arc<AtomicBool>, mut event_cb: F) -> Result<String, String>
    where
        F: FnMut(LoopEvent),
    {
        self.run_with_cancel(session, model, tools, cancel_flag, &mut event_cb)
    }
}
