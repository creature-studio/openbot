use crate::agent::session::{Message, ToolCall};
use crate::tools::ToolRegistry;

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

pub trait Model {
    fn chat(&self, messages: &[Message], tools: &ToolRegistry) -> Result<ModelResponse, String>;
    fn name(&self) -> &str;
}

// Mock model for testing without LLM
pub struct MockModel {
    pub responses: Vec<ModelResponse>,
    pub index: std::sync::Mutex<usize>,
}

impl MockModel {
    pub fn new(responses: Vec<ModelResponse>) -> Self {
        Self { responses, index: std::sync::Mutex::new(0) }
    }
    pub fn simple_final(content: &str) -> Self {
        Self {
            responses: vec![ModelResponse { content: content.to_string(), tool_calls: vec![] }],
            index: std::sync::Mutex::new(0),
        }
    }
}

impl Model for MockModel {
    fn chat(&self, _messages: &[Message], _tools: &ToolRegistry) -> Result<ModelResponse, String> {
        let mut idx = self.index.lock().unwrap();
        if *idx < self.responses.len() {
            let resp = self.responses[*idx].clone();
            *idx += 1;
            Ok(resp)
        } else {
            Ok(ModelResponse { content: "done".to_string(), tool_calls: vec![] })
        }
    }
    fn name(&self) -> &str { "mock" }
}

// OpenAI compatible model using curl for TLS (avoids needing reqwest)
pub struct OpenAICompatibleModel {
    pub api_key: String,
    pub base_url: String,
    pub model_name: String,
}

impl OpenAICompatibleModel {
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY not set")?;
        let base_url = std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model_name = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
        Ok(Self { api_key, base_url, model_name })
    }

    fn build_tools_json(&self, tools: &ToolRegistry) -> String {
        let mut tool_defs = Vec::new();
        for tool in tools.list() {
            let def = format!(r#"{{"type":"function","function":{{"name":"{}","description":{},"parameters":{}}}}}"#,
                tool.name,
                serde_json_string(&tool.description),
                tool.parameters_schema
            );
            tool_defs.push(def);
        }
        format!("[{}]", tool_defs.join(","))
    }

    fn build_messages_json(&self, messages: &[Message]) -> String {
        let mut msgs = Vec::new();
        for m in messages {
            let role = m.role.as_str();
            if m.role == crate::agent::session::Role::Tool {
                // tool message needs tool_call_id
                let tool_call_id = m.tool_call_id.as_deref().unwrap_or("");
                let content = serde_json_string(&m.content);
                msgs.push(format!(r#"{{"role":"{}","tool_call_id":"{}","content":{}}}"#, role, tool_call_id, content));
            } else if !m.tool_calls.is_empty() {
                // assistant with tool calls
                let mut tc_json = Vec::new();
                for tc in &m.tool_calls {
                    tc_json.push(format!(r#"{{"id":"{}","type":"function","function":{{"name":"{}","arguments":{}}}}}"#,
                        tc.id, tc.name, serde_json_string(&tc.arguments)
                    ));
                }
                let content = serde_json_string(&m.content);
                msgs.push(format!(r#"{{"role":"{}","content":{},"tool_calls":[{}]}}"#, role, content, tc_json.join(",")));
            } else {
                let content = serde_json_string(&m.content);
                msgs.push(format!(r#"{{"role":"{}","content":{}}}"#, role, content));
            }
        }
        format!("[{}]", msgs.join(","))
    }
}

impl Model for OpenAICompatibleModel {
    fn chat(&self, messages: &[Message], tools: &ToolRegistry) -> Result<ModelResponse, String> {
        let tools_json = self.build_tools_json(tools);
        let messages_json = self.build_messages_json(messages);
        let body = format!(r#"{{"model":"{}","messages":{},"tools":{},"tool_choice":"auto"}}"#,
            self.model_name, messages_json, tools_json
        );

        // Use curl via std::process::Command for TLS
        let output = std::process::Command::new("curl")
            .args([
                "-s",
                "-X", "POST",
                &format!("{}/chat/completions", self.base_url),
                "-H", &format!("Authorization: Bearer {}", self.api_key),
                "-H", "Content-Type: application/json",
                "-d", &body,
            ])
            .output()
            .map_err(|e| format!("curl failed: {}", e))?;

        if !output.status.success() {
            return Err(format!("curl exit {}: {}", output.status, String::from_utf8_lossy(&output.stderr)));
        }

        let resp_str = String::from_utf8_lossy(&output.stdout);
        parse_openai_response(&resp_str)
    }

    fn name(&self) -> &str { &self.model_name }
}

fn serde_json_string(s: &str) -> String {
    // Very naive JSON string escape
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
    format!("\"{}\"", escaped)
}

fn parse_openai_response(resp: &str) -> Result<ModelResponse, String> {
    // Naive parsing of OpenAI response, extract first choice message content and tool_calls
    // Look for "content": "..." and "tool_calls": [...]
    // This is fragile but works for MVP without serde_json crate

    // Find choices[0].message
    let content = extract_json_string_field(resp, "\"content\":")
        .unwrap_or_default();

    // Try to extract tool_calls array
    let mut tool_calls = Vec::new();
    if let Some(tc_start) = resp.find("\"tool_calls\"") {
        let tc_slice = &resp[tc_start..];
        // Find each tool call object
        // Look for "id": "call_...", "function": {"name": "...", "arguments": "..."}
        let mut pos = 0;
        while let Some(id_pos) = tc_slice[pos..].find("\"id\"") {
            let id_start = pos + id_pos;
            let id = extract_json_string_field(&tc_slice[id_start..], "\"id\":").unwrap_or_else(|| format!("call-{}", tool_calls.len()));
            if let Some(name_pos) = tc_slice[id_start..].find("\"name\"") {
                let name = extract_json_string_field(&tc_slice[id_start+name_pos..], "\"name\":").unwrap_or_default();
                if let Some(args_pos) = tc_slice[id_start+name_pos..].find("\"arguments\"") {
                    let args = extract_json_string_field(&tc_slice[id_start+name_pos+args_pos..], "\"arguments\":").unwrap_or_else(|| "{}".to_string());
                    // arguments is itself a JSON string that may be escaped
                    let unescaped = args.replace("\\\"", "\"").replace("\\\\", "\\");
                    tool_calls.push(ToolCall { id: id.clone(), name, arguments: unescaped });
                }
            }
            pos = id_start + 10;
            if pos >= tc_slice.len() { break; }
            // Break after a few to avoid infinite
            if tool_calls.len() > 10 { break; }
        }
    }

    Ok(ModelResponse { content, tool_calls })
}

fn extract_json_string_field(s: &str, field: &str) -> Option<String> {
    let start = s.find(field)?;
    let rest = &s[start+field.len()..].trim_start();
    if !rest.starts_with('"') {
        // might be null
        if rest.starts_with("null") { return Some("".to_string()); }
        return None;
    }
    let rest = &rest[1..];
    let mut escaped = false;
    let mut end = None;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            end = Some(i);
            break;
        }
    }
    let end = end?;
    let raw = &rest[..end];
    // Unescape minimal
    let unescaped = raw.replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\").replace("\\r", "\r").replace("\\t", "\t");
    Some(unescaped)
}
