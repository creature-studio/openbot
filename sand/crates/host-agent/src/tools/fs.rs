use super::{Tool, ToolDefinition, ToolResult};
use std::path::Path;

pub struct FsReadTool;
pub struct FsWriteTool;
pub struct FsListTool;
pub struct FsSearchTool;
pub struct FsStatTool;
pub struct FsPatchTool;

impl Tool for FsReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.read".to_string(),
            description: "Read a file from filesystem. Returns content.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"File path to read"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(ToolResult { content: format!("file: {}\ncontent:\n{}", path, content), is_error: false }),
            Err(e) => {
                // try binary and show hex? For now error
                // Try via sandd exec cat as fallback
                Ok(ToolResult { content: format!("read failed {}: {}", path, e), is_error: true })
            }
        }
    }
}

impl Tool for FsWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.write".to_string(),
            description: "Write content to a file. Creates parent dirs if needed.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        let content = extract_arg(args, "content").ok_or("missing content")?;
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, content) {
            Ok(_) => Ok(ToolResult { content: format!("wrote {}", path), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("write failed {}: {}", path, e), is_error: true }),
        }
    }
}

impl Tool for FsListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.list".to_string(),
            description: "List files in a directory.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"Directory path"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").unwrap_or_else(|| ".".to_string());
        match std::fs::read_dir(&path) {
            Ok(entries) => {
                let mut list = Vec::new();
                for e in entries {
                    if let Ok(e) = e {
                        let ft = e.file_type().map(|t| if t.is_dir() { "dir" } else { "file" }).unwrap_or("unknown");
                        list.push(format!("{} ({})", e.path().display(), ft));
                    }
                }
                Ok(ToolResult { content: format!("listing {}:\n{}", path, list.join("\n")), is_error: false })
            }
            Err(e) => Ok(ToolResult { content: format!("list failed {}: {}", path, e), is_error: true }),
        }
    }
}

impl Tool for FsSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.search".to_string(),
            description: "Search for files containing pattern via grep -r.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let pattern = extract_arg(args, "pattern").ok_or("missing pattern")?;
        let path = extract_arg(args, "path").unwrap_or_else(|| ".".to_string());
        // Use rg if available, else grep
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("rg -n '{}' '{}' 2>/dev/null | head -n 100 || grep -Rn '{}' '{}' 2>/dev/null | head -n 100", pattern, path, pattern, path)])
            .output()
            .map_err(|e| format!("search failed: {}", e))?;
        let content = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(ToolResult { content: format!("search '{}' in {}:\n{}", pattern, path, content), is_error: false })
    }
}

impl Tool for FsStatTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.stat".to_string(),
            description: "Stat a file.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        match std::fs::metadata(&path) {
            Ok(meta) => {
                let content = format!("path: {}\nsize: {}\nis_dir: {}\nis_file: {}\nmodified: {:?}", path, meta.len(), meta.is_dir(), meta.is_file(), meta.modified());
                Ok(ToolResult { content, is_error: false })
            }
            Err(e) => Ok(ToolResult { content: format!("stat failed {}: {}", path, e), is_error: true }),
        }
    }
}

impl Tool for FsPatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.patch".to_string(),
            description: "Apply a patch to a file via unified diff. Better than rewriting whole file.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"patch":{"type":"string","description":"Unified diff patch"}},"required":["path","patch"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        let patch = extract_arg(args, "patch").ok_or("missing patch")?;
        // For MVP, we apply patch via `patch` command or via simple search/replace if patch is not unified diff but instructions
        // Try to use `git apply` or `patch`
        let tmp_patch = format!("/tmp/patch-{}.diff", std::process::id());
        if let Err(e) = std::fs::write(&tmp_patch, &patch) {
            return Ok(ToolResult { content: format!("failed to write temp patch: {}", e), is_error: true });
        }
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("cd $(dirname {}) && patch -p0 < {} 2>&1 || git apply {} 2>&1 || (echo 'trying manual apply' && cat {})", path, tmp_patch, tmp_patch, tmp_patch)])
            .output()
            .map_err(|e| format!("patch command failed: {}", e))?;
        let out_str = String::from_utf8_lossy(&output.stdout).to_string() + &String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_file(&tmp_patch);
        if output.status.success() {
            Ok(ToolResult { content: format!("patch applied to {}: {}", path, out_str), is_error: false })
        } else {
            Ok(ToolResult { content: format!("patch failed {}: {}", path, out_str), is_error: true })
        }
    }
}

fn extract_arg(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)?;
    let rest = json[start+pat.len()..].trim_start();
    if rest.starts_with('"') {
        // handle escaped quotes inside?
        let mut end = None;
        let mut escaped = false;
        let chars = rest[1..].char_indices();
        for (i, c) in chars {
            if escaped { escaped = false; continue; }
            if c == '\\' { escaped = true; continue; }
            if c == '"' { end = Some(i); break; }
        }
        let end = end?;
        let raw = &rest[1..1+end];
        Some(raw.replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\"))
    } else {
        None
    }
}
