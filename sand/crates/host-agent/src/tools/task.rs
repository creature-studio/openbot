use super::{Tool, ToolDefinition, ToolResult};

pub struct TaskCompleteTool;
pub struct TaskCreateTool;
pub struct TaskListTool;

impl Tool for TaskCompleteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "task.complete".to_string(),
            description: "Complete current task with result. Freezes result and will release runtime after approval. Don't call until work is truly done.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"result":{"type":"string","description":"Task result summary"}},"required":["result"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let result = extract_arg(args, "result").unwrap_or_else(|| "completed".to_string());
        // In real system, this would freeze artifacts and mark task completed, waiting for user check
        Ok(ToolResult { content: format!("task.complete called with result: {}\nTask marked ReadyForCheck, runtime will be kept until user approval, then released.", result), is_error: false })
    }
}

impl Tool for TaskCreateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "task.create".to_string(),
            description: "Create a new task with goal.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"goal":{"type":"string"}},"required":["goal"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let goal = extract_arg(args, "goal").ok_or("missing goal")?;
        Ok(ToolResult { content: format!("task.create goal '{}' in runtime {} - would create Task with session", goal, runtime_id), is_error: false })
    }
}

impl Tool for TaskListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "task.list".to_string(),
            description: "List tasks.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        // Query SQLite for tasks
        let path = if std::path::Path::new("/run/sand/state.db").exists() { "/run/sand/state.db" } else { "/tmp/sandd/state.db" };
        if std::path::Path::new(path).exists() {
            // Use sqlite3 via python for quick query (avoid FFI here)
            let output = std::process::Command::new("python3")
                .args(["-c", &format!("import sqlite3; conn=sqlite3.connect('{}'); cur=conn.cursor(); cur.execute('SELECT id, goal, status FROM task ORDER BY created_at DESC LIMIT 20'); rows=cur.fetchall(); print('\\n'.join([f\"{{r[0]}}: {{r[1][:50]}} [{{r[2]}}]\" for r in rows]))", path)])
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    let content = String::from_utf8_lossy(&out.stdout).to_string();
                    return Ok(ToolResult { content: format!("tasks:\n{}", content), is_error: false });
                }
            }
        }
        Ok(ToolResult { content: "no tasks or persistence not available".to_string(), is_error: false })
    }
}

fn extract_arg(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)?;
    let rest = json[start+pat.len()..].trim_start();
    if rest.starts_with('"') {
        let mut end = None;
        let mut escaped = false;
        for (i, c) in rest[1..].char_indices() {
            if escaped { escaped=false; continue; }
            if c=='\\' { escaped=true; continue; }
            if c=='"' { end=Some(i); break; }
        }
        let end = end?;
        let raw = &rest[1..1+end];
        Some(raw.replace("\\n","\n").replace("\\\"","\"").replace("\\\\","\\"))
    } else { None }
}
