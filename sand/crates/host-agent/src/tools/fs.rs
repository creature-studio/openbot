use super::{Tool, ToolDefinition, ToolResult};
use std::path::Path;

pub struct FsReadTool;
pub struct FsWriteTool;
pub struct FsListTool;
pub struct FsSearchTool;
pub struct FsStatTool;
pub struct FsPatchTool;

// Helper to get runtime workspace via sandd, then operate via exec or local
fn get_runtime_workspace(runtime_id: &str) -> Option<String> {
    // Try to get workspace from sandd via GetRuntime
    let req = format!(r#"{{"method":"GetRuntime","id":"{}"}}"#, runtime_id);
    if let Ok(resp) = raw_rpc(&req) {
        if let Some(ws) = extract_field(&resp, "workspace") {
            return Some(ws);
        }
    }
    None
}

fn raw_rpc(req: &str) -> Result<String, String> {
    use std::os::unix::net::UnixStream;
    use std::io::{Write, BufRead, BufReader};
    let sock_path = if std::path::Path::new("/run/sand/sandd.sock").exists() { "/run/sand/sandd.sock" } else { "/tmp/sandd/sandd.sock" };
    let mut stream = UnixStream::connect(sock_path).map_err(|e| format!("connect failed: {}", e))?;
    writeln!(stream, "{}", req).map_err(|e| format!("write failed: {}", e))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| format!("read failed: {}", e))?;
    Ok(line.trim().to_string())
}

fn extract_field(s: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn exec_in_runtime(runtime_id: &str, command: &str) -> Result<String, String> {
    let client = sand_client::SandClient::new(None);
    let cmd_vec = vec!["bash".to_string(), "-lc".to_string(), command.to_string()];
    let resp = client.exec(runtime_id, cmd_vec).map_err(|e| e.to_string())?;
    // Decode stdout_b64
    let stdout_b64 = extract_field(&resp, "stdout_b64").unwrap_or_default();
    let stderr_b64 = extract_field(&resp, "stderr_b64").unwrap_or_default();
    let stdout = base64_decode(&stdout_b64);
    let stderr = base64_decode(&stderr_b64);
    let exit_code = extract_number_field(&resp, "exit_code").unwrap_or(0);
    if exit_code != 0 {
        Ok(format!("exit {} stdout: {} stderr: {}", exit_code, String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr)))
    } else {
        Ok(String::from_utf8_lossy(&stdout).to_string())
    }
}

fn extract_number_field(s: &str, field: &str) -> Option<i32> {
    let pat = format!("\"{}\":", field);
    let start = s.find(&pat)?;
    let rest = &s[start+pat.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() { table[c as usize]=i as u8; }
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i=0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i]==b'\n' || bytes[i]==b'\r' || bytes[i]==b' ') { i+=1; }
        if i+3 >= bytes.len() { break; }
        let mut vals=[0u8;4];
        let mut padding=0;
        let mut valid=true;
        for j in 0..4 {
            if i+j>=bytes.len() { valid=false; break; }
            let b=bytes[i+j];
            if b==b'=' { padding+=1; vals[j]=0; } else { let v=table[b as usize]; if v==255 { valid=false; break; } vals[j]=v; }
        }
        if !valid { i+=1; continue; }
        let n=((vals[0] as u32)<<18)|((vals[1] as u32)<<12)|((vals[2] as u32)<<6)|(vals[3] as u32);
        out.push(((n>>16)&0xFF) as u8);
        if padding<2 { out.push(((n>>8)&0xFF) as u8); }
        if padding<1 { out.push((n&0xFF) as u8); }
        i+=4;
        if padding>0 { break; }
    }
    out
}

impl Tool for FsReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.read".to_string(),
            description: "Read a file from filesystem or runtime workspace. Returns content.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"File path to read, absolute or relative to runtime workspace"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        // Try via sandd exec first if runtime exists
        if !runtime_id.is_empty() {
            if let Ok(content) = exec_in_runtime(runtime_id, &format!("cat '{}' 2>&1", path.replace('\'', "'\\''"))) {
                if !content.contains("No such file") && !content.contains("cannot open") {
                    return Ok(ToolResult { content: format!("file: {} (via runtime {})\ncontent:\n{}", path, runtime_id, content), is_error: false });
                }
            }
        }
        // Fallback local FS
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(ToolResult { content: format!("file: {}\ncontent:\n{}", path, content), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("read failed {}: {}", path, e), is_error: true }),
        }
    }
}

impl Tool for FsWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.write".to_string(),
            description: "Write content to a file. Creates parent dirs if needed. Operates in runtime workspace if runtime exists.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        let content = extract_arg(args, "content").ok_or("missing content")?;
        
        if !runtime_id.is_empty() {
            // Use sandd exec to write via bash heredoc
            let escaped_content = content.replace('\'', "'\\''");
            let cmd = format!("mkdir -p $(dirname '{}') && cat > '{}' <<'__SAND_EOF__'\n{}\n__SAND_EOF__\n", path.replace('\'', "'\\''"), path.replace('\'', "'\\''"), content);
            if let Ok(out) = exec_in_runtime(runtime_id, &cmd) {
                return Ok(ToolResult { content: format!("wrote {} via runtime {}: {}", path, runtime_id, out), is_error: false });
            }
        }
        
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
            description: "List files in a directory, via runtime if available.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"Directory path"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").unwrap_or_else(|| ".".to_string());
        
        if !runtime_id.is_empty() {
            if let Ok(out) = exec_in_runtime(runtime_id, &format!("ls -la '{}' 2>&1", path.replace('\'', "'\\''"))) {
                return Ok(ToolResult { content: format!("listing {} via runtime {}:\n{}", path, runtime_id, out), is_error: false });
            }
        }
        
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
            description: "Search for files containing pattern via rg/grep, in runtime if available.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let pattern = extract_arg(args, "pattern").ok_or("missing pattern")?;
        let path = extract_arg(args, "path").unwrap_or_else(|| ".".to_string());
        
        let cmd = format!("rg -n '{}' '{}' 2>/dev/null | head -n 100 || grep -Rn '{}' '{}' 2>/dev/null | head -n 100", pattern, path, pattern, path);
        if !runtime_id.is_empty() {
            if let Ok(out) = exec_in_runtime(runtime_id, &cmd) {
                return Ok(ToolResult { content: format!("search '{}' in {} via runtime {}:\n{}", pattern, path, runtime_id, out), is_error: false });
            }
        }
        
        let output = std::process::Command::new("sh")
            .args(["-c", &cmd])
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
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        
        if !runtime_id.is_empty() {
            if let Ok(out) = exec_in_runtime(runtime_id, &format!("stat '{}' 2>&1 || ls -lh '{}'", path.replace('\'', "'\\''"), path.replace('\'', "'\\''"))) {
                return Ok(ToolResult { content: format!("stat {} via runtime {}:\n{}", path, runtime_id, out), is_error: false });
            }
        }
        
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
            description: "Apply a patch to a file. Supports unified diff, search/replace, or full rewrite. Prefer this over shell for code changes as it provides audit/diff.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"patch":{"type":"string","description":"Unified diff or search/replace instructions"},"search":{"type":"string","description":"Search string for search/replace mode"},"replace":{"type":"string","description":"Replace string"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        let patch = extract_arg(args, "patch");
        let search = extract_arg(args, "search");
        let replace = extract_arg(args, "replace");

        // Mode 1: search/replace
        if let (Some(s), Some(r)) = (search, replace) {
            if !runtime_id.is_empty() {
                let cmd = format!("cat '{}' 2>&1", path.replace('\'', "'\\''"));
                if let Ok(content) = exec_in_runtime(runtime_id, &cmd) {
                    if content.contains(&s) {
                        let new_content = content.replace(&s, &r);
                        let write_cmd = format!("cat > '{}' <<'__SAND_EOF__'\n{}\n__SAND_EOF__", path.replace('\'', "'\\''"), new_content);
                        let _ = exec_in_runtime(runtime_id, &write_cmd);
                        return Ok(ToolResult { content: format!("patched {} via search/replace in runtime {}", path, runtime_id), is_error: false });
                    }
                }
            }
            // Local
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    if content.contains(&s) {
                        let new_content = content.replace(&s, &r);
                        if let Err(e) = std::fs::write(&path, new_content) {
                            return Ok(ToolResult { content: format!("write failed: {}", e), is_error: true });
                        }
                        Ok(ToolResult { content: format!("patched {} via search/replace", path), is_error: false })
                    } else {
                        Ok(ToolResult { content: format!("search string not found in {}", path), is_error: true })
                    }
                }
                Err(e) => Ok(ToolResult { content: format!("read failed {}: {}", path, e), is_error: true }),
            }
        } else if let Some(patch_content) = patch {
            // Mode 2: unified diff via patch command
            let tmp_patch = format!("/tmp/patch-{}-{}.diff", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
            if let Err(e) = std::fs::write(&tmp_patch, &patch_content) {
                return Ok(ToolResult { content: format!("failed to write temp patch: {}", e), is_error: true });
            }
            
            let result = if !runtime_id.is_empty() {
                exec_in_runtime(runtime_id, &format!("cd $(dirname '{}') && patch -p0 < {} 2>&1 || patch -p1 < {} 2>&1 || git apply {} 2>&1", path.replace('\'', "'\\''"), tmp_patch, tmp_patch, tmp_patch))
            } else {
                let output = std::process::Command::new("sh")
                    .args(["-c", &format!("cd $(dirname {}) && patch -p0 < {} 2>&1 || patch -p1 < {} 2>&1 || git apply {} 2>&1", path, tmp_patch, tmp_patch, tmp_patch)])
                    .output();
                match output {
                    Ok(o) => Ok(String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)),
                    Err(e) => Err(e.to_string()),
                }
            };
            
            let _ = std::fs::remove_file(&tmp_patch);
            
            match result {
                Ok(out) => {
                    if out.contains("failed") || out.contains("error") {
                        Ok(ToolResult { content: format!("patch failed {}: {}", path, out), is_error: true })
                    } else {
                        Ok(ToolResult { content: format!("patch applied to {}: {}", path, out), is_error: false })
                    }
                }
                Err(e) => Ok(ToolResult { content: format!("patch command failed: {}", e), is_error: true }),
            }
        } else {
            Ok(ToolResult { content: "need either search+replace or patch".to_string(), is_error: true })
        }
    }
}

fn extract_arg(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)?;
    let rest = json[start+pat.len()..].trim_start();
    if rest.starts_with('"') {
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

