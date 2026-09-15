use super::session::{AgentSession, Message, Role, ToolCall};
use super::state::{AgentStatus, Attention};
use crate::model::{Model, ModelResponse};
use crate::tools::{ToolRegistry, ToolResult};

#[derive(Debug, Clone)]
pub enum LoopEvent {
    MessageAdded { role: String, content: String },
    ToolCallStarted { name: String, args: String },
    ToolCallFinished { name: String, result: String },
    ModelCalled,
    Attention { attention: Attention },
    Completed { result: String },
    Failed { reason: String },
}

pub struct AgentLoop {
    pub max_iterations: usize,
}

impl AgentLoop {
    pub fn new() -> Self {
        Self { max_iterations: 50 }
    }

    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    // Core loop: user message -> LLM -> tool_calls -> exec -> LLM -> final
    pub fn run<F>(&self, session: &mut AgentSession, model: &dyn Model, tools: &ToolRegistry, mut event_cb: F) -> Result<String, String>
    where
        F: FnMut(LoopEvent),
    {
        session.status = AgentStatus::Running;
        let mut iterations = 0;

        loop {
            if iterations >= self.max_iterations {
                session.status = AgentStatus::Failed { reason: "max iterations reached".to_string() };
                event_cb(LoopEvent::Failed { reason: "max iterations reached".to_string() });
                return Err("max iterations reached".to_string());
            }
            iterations += 1;

            // Call model
            event_cb(LoopEvent::ModelCalled);
            let response = model.chat(&session.messages, tools).map_err(|e| {
                session.status = AgentStatus::Failed { reason: e.clone() };
                e
            })?;

            // If no tool calls, it's final answer
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

            // Execute tool calls (serial for now, can be concurrent)
            session.status = AgentStatus::WaitingTool;
            for tc in &response.tool_calls {
                event_cb(LoopEvent::ToolCallStarted { name: tc.name.clone(), args: tc.arguments.clone() });
                let result = tools.execute(&tc.name, &tc.arguments, &session.runtime_id);
                let result_str = match &result {
                    Ok(r) => r.content.clone(),
                    Err(e) => format!("error: {}", e),
                };

                // Check permission attention
                if result_str.contains("permission_required") {
                    event_cb(LoopEvent::Attention { attention: Attention::PermissionRequired { tool: tc.name.clone(), reason: result_str.clone() } });
                }

                // Add tool result message
                session.add_message(Message {
                    role: Role::Tool,
                    content: result_str.clone(),
                    tool_calls: vec![],
                    tool_call_id: Some(tc.id.clone()),
                    name: Some(tc.name.clone()),
                });

                event_cb(LoopEvent::ToolCallFinished { name: tc.name.clone(), result: result_str });
            }
            session.status = AgentStatus::Running;
        }
    }
}
