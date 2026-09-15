use super::{Tool, ToolDefinition, ToolResult, ToolExecutionContext};
use std::path::{Path, PathBuf};

pub struct FsReadTool;
pub struct FsWriteTool;
pub struct FsListTool;
pub struct FsSearchTool;
pub struct FsStatTool;
pub struct FsPatchTool;
pub struct FsMkdirTool;
pub struct FsRemoveTool;
pub struct FsRenameTool;
pub struct FsGlobTool;

// Helpers
fn get_runtime_workspace(runtime_id: &str) -> Option<String> {
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

fn canonical_workspace(ws: &str) -> Result<PathBuf, String> {
    let p = Path::new(ws);
    match p.canonicalize() {
        Ok(c) => Ok(c),
        Err(_) => Ok(p.to_path_buf()),
    }
}

fn is_beneath(workspace_canonical: &Path, path_canonical: &Path) -> bool {
    path_canonical.starts_with(workspace_canonical)
}

fn resolve_secure_existing(workspace: &str, user_path: &str) -> Result<PathBuf, String> {
    let ws_canonical = canonical_workspace(workspace)?;
    let joined = if user_path.starts_with('/') {
        PathBuf::from(user_path)
    } else {
        Path::new(workspace).join(user_path)
    };
    let canonical = joined.canonicalize().map_err(|e| format!("canonicalize failed for {}: {}", joined.display(), e))?;
    let ws_canonical_real = ws_canonical.canonicalize().unwrap_or(ws_canonical.clone());
    if !is_beneath(&ws_canonical_real, &canonical) {
        return Err(format!("workspace escape detected: {} (canonical {}) not beneath workspace {} (canonical {})", user_path, canonical.display(), workspace, ws_canonical_real.display()));
    }
    Ok(canonical)
}

fn resolve_secure_new(workspace: &str, user_path: &str) -> Result<PathBuf, String> {
    let ws_canonical = canonical_workspace(workspace)?;
    let ws_canonical_real = ws_canonical.canonicalize().unwrap_or(ws_canonical.clone());
    let joined = if user_path.starts_with('/') {
        PathBuf::from(user_path)
    } else {
        Path::new(workspace).join(user_path)
    };
    let parent = joined.parent().ok_or_else(|| format!("invalid path: {}", user_path))?;
    let mut current = parent.to_path_buf();
    let mut existing_ancestor: Option<PathBuf> = None;
    let mut suffix_components: Vec<String> = Vec::new();
    loop {
        if current.exists() {
            match current.canonicalize() {
                Ok(c) => { existing_ancestor = Some(c); break; }
                Err(_) => {
                    if let Some(p) = current.parent() {
                        suffix_components.push(current.file_name().unwrap_or_default().to_string_lossy().to_string());
                        current = p.to_path_buf();
                        continue;
                    } else { break; }
                }
            }
        } else {
            if let Some(p) = current.parent() {
                suffix_components.push(current.file_name().unwrap_or_default().to_string_lossy().to_string());
                current = p.to_path_buf();
                if current.as_os_str().is_empty() || current == Path::new("/") { break; }
                continue;
            } else { break; }
        }
    }
    if let Some(ancestor_canonical) = existing_ancestor {
        if !is_beneath(&ws_canonical_real, &ancestor_canonical) {
            return Err(format!("workspace escape via parent: {} ancestor {} not beneath workspace {}", user_path, ancestor_canonical.display(), ws_canonical_real.display()));
        }
        let mut full = ancestor_canonical;
        for comp in suffix_components.iter().rev() {
            if !comp.is_empty() { full = full.join(comp); }
        }
        if let Some(file_name) = joined.file_name() { full = full.join(file_name); }
        Ok(full)
    } else {
        if user_path.contains("..") {
            return Err(format!("path contains .. and no existing ancestor found, rejecting for security: {}", user_path));
        }
        let joined_str = joined.to_string_lossy();
        let ws_str = Path::new(workspace).to_string_lossy();
        if !joined_str.starts_with(ws_str.as_ref()) && user_path.starts_with('/') {
            return Err(format!("absolute path outside workspace: {} not beneath {}", user_path, workspace));
        }
        Ok(joined)
    }
}

fn resolve_secure_dir(workspace: &str, user_path: &str) -> Result<PathBuf, String> {
    resolve_secure_existing(workspace, user_path)
}

fn read_file_via_workspace_secure(runtime_id: &str, path: &str) -> Result<String, String> {
    let ws = get_runtime_workspace(runtime_id).ok_or("no workspace")?;
    let secure_path = resolve_secure_existing(&ws, path)?;
    std::fs::read_to_string(&secure_path).map_err(|e| format!("read failed {}: {}", secure_path.display(), e))
}

fn write_file_via_workspace_secure(runtime_id: &str, path: &str, content: &str) -> Result<(), String> {
    let ws = get_runtime_workspace(runtime_id).ok_or("no workspace")?;
    let secure_path = resolve_secure_new(&ws, path)?;
    if let Some(parent) = secure_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create_dir_all failed {}: {}", parent.display(), e))?;
    }
    std::fs::write(&secure_path, content).map_err(|e| format!("write failed {}: {}", secure_path.display(), e))
}

fn list_via_workspace_secure(runtime_id: &str, path: &str) -> Result<String, String> {
    let ws = get_runtime_workspace(runtime_id).ok_or("no workspace")?;
    let secure_path = resolve_secure_dir(&ws, path)?;
    let entries = std::fs::read_dir(&secure_path).map_err(|e| format!("read_dir failed {}: {}", secure_path.display(), e))?;
    let mut list = Vec::new();
    for e in entries {
        if let Ok(e) = e {
            let ft = e.file_type().map(|t| if t.is_dir() { "dir" } else { "file" }).unwrap_or("unknown");
            list.push(format!("{} ({})", e.file_name().to_string_lossy(), ft));
        }
    }
    Ok(list.join("\n"))
}

fn stat_via_workspace_secure(runtime_id: &str, path: &str) -> Result<String, String> {
    let ws = get_runtime_workspace(runtime_id).ok_or("no workspace")?;
    let secure_path = resolve_secure_existing(&ws, path)?;
    let meta = std::fs::metadata(&secure_path).map_err(|e| format!("metadata failed {}: {}", secure_path.display(), e))?;
    Ok(format!("path: {} (secure: {})\nsize: {}\nis_dir: {}\nis_file: {}\nmodified: {:?}", path, secure_path.display(), meta.len(), meta.is_dir(), meta.is_file(), meta.modified()))
}

impl Tool for FsReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.read".to_string(),
            description: "Read a file from filesystem or runtime workspace with symlink escape protection (RESOLVE_BENEATH). Returns content.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"File path to read, absolute or relative to runtime workspace, symlink escape blocked"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        if !runtime_id.is_empty() {
            // Try transport routing for remote filesystems
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Read { path: PathBuf::from(&path) };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(FsResponse::Ok { data }) => {
                            let content = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(format!("file: {} (via transport)\ncontent:\n{}", path, content));
                            r.tool_name = "file.read".to_string();
                            return Ok(r);
                        }
                        Ok(FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("read failed via transport: {}", message), Some("READ_FAILED".to_string()));
                            r.tool_name = "file.read".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            match read_file_via_workspace_secure(runtime_id, &path) {
                Ok(content) => {
                    let mut r = ToolResult::success(format!("file: {} (via secure workspace {})\ncontent:\n{}", path, runtime_id, content));
                    r.tool_name = "file.read".to_string();
                    return Ok(r);
                }
                Err(e) => {
                    if e.contains("workspace escape") {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: {} - symlink escape or path outside workspace detected", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.read".to_string();
                        return Ok(r);
                    }
                    let mut r = ToolResult::error(format!("read failed (secure): {}", e), Some("READ_FAILED".to_string()));
                    r.tool_name = "file.read".to_string();
                    return Ok(r);
                }
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let mut r = ToolResult::success(format!("file: {}\ncontent:\n{}", path, content));
                r.tool_name = "file.read".to_string();
                Ok(r)
            },
            Err(e) => {
                let mut r = ToolResult::error(format!("read failed {}: {}", path, e), Some("READ_FAILED".to_string()));
                r.tool_name = "file.read".to_string();
                Ok(r)
            },
        }
    }
}

impl Tool for FsWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.write".to_string(),
            description: "Write content to a file with workspace escape protection. Creates parent dirs if needed. No shell fallback for security.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        let content = extract_arg(args, "content").ok_or("missing content")?;
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Write { path: PathBuf::from(&path), data: content.as_bytes().to_vec() };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(FsResponse::Ok { data: _ }) => {
                            let mut r = ToolResult::success(format!("wrote {} via transport in {}", path, runtime_id));
                            r.tool_name = "file.write".to_string();
                            return Ok(r);
                        }
                        Ok(FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("write failed via transport: {}", message), Some("WRITE_FAILED".to_string()));
                            r.tool_name = "file.write".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            match write_file_via_workspace_secure(runtime_id, &path, &content) {
                Ok(()) => {
                    let mut r = ToolResult::success(format!("wrote {} via secure workspace {}", path, runtime_id));
                    r.tool_name = "file.write".to_string();
                    return Ok(r);
                }
                Err(e) => {
                    if e.contains("workspace escape") || e.contains("outside workspace") {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: {} - refusing write outside workspace", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.write".to_string();
                        return Ok(r);
                    }
                    let mut r = ToolResult::error(format!("write failed (secure): {}", e), Some("WRITE_FAILED".to_string()));
                    r.tool_name = "file.write".to_string();
                    return Ok(r);
                }
            }
        }
        if let Some(parent) = Path::new(&path).parent() { let _ = std::fs::create_dir_all(parent); }
        match std::fs::write(&path, content) {
            Ok(_) => {
                let mut r = ToolResult::success(format!("wrote {}", path));
                r.tool_name = "file.write".to_string();
                Ok(r)
            },
            Err(e) => {
                let mut r = ToolResult::error(format!("write failed {}: {}", path, e), Some("WRITE_FAILED".to_string()));
                r.tool_name = "file.write".to_string();
                Ok(r)
            },
        }
    }
}

impl Tool for FsListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.list".to_string(),
            description: "List files in a directory with symlink escape protection.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"Directory path"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").unwrap_or_else(|| ".".to_string());
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::List { path: PathBuf::from(&path) };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(spark_transport::FsResponse::Ok { data }) => {
                            let list = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(format!("listing {} via transport:\n{}", path, list));
                            r.tool_name = "file.list".to_string();
                            return Ok(r);
                        }
                        Ok(spark_transport::FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("list failed via transport: {}", message), Some("LIST_FAILED".to_string()));
                            r.tool_name = "file.list".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            match list_via_workspace_secure(runtime_id, &path) {
                Ok(list) => {
                    let mut r = ToolResult::success(format!("listing {} via secure workspace {}:\n{}", path, runtime_id, list));
                    r.tool_name = "file.list".to_string();
                    return Ok(r);
                }
                Err(e) => {
                    if e.contains("workspace escape") {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: {}", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.list".to_string();
                        return Ok(r);
                    }
                    let mut r = ToolResult::error(format!("list failed (secure): {}", e), Some("LIST_FAILED".to_string()));
                    r.tool_name = "file.list".to_string();
                    return Ok(r);
                }
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
                let mut r = ToolResult::success(format!("listing {}:\n{}", path, list.join("\n")));
                r.tool_name = "file.list".to_string();
                Ok(r)
            }
            Err(e) => {
                let mut r = ToolResult::error(format!("list failed {}: {}", path, e), Some("LIST_FAILED".to_string()));
                r.tool_name = "file.list".to_string();
                Ok(r)
            },
        }
    }
}

impl Tool for FsSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.search".to_string(),
            description: "Search for files containing pattern via Rust, in runtime workspace with secure path check.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pattern = extract_arg(args, "pattern").ok_or("missing pattern")?;
        let path = extract_arg(args, "path").unwrap_or_else(|| ".".to_string());
        if !runtime_id.is_empty() {
            // Try transport routing for remote filesystems
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Search { path: PathBuf::from(&path), query: pattern.clone() };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(spark_transport::FsResponse::Ok { data }) => {
                            let content = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(format!("search '{}' in {} via transport:\n{}", pattern, path, content));
                            r.tool_name = "file.search".to_string();
                            return Ok(r);
                        }
                        Ok(spark_transport::FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("search failed via transport: {}", message), Some("SEARCH_FAILED".to_string()));
                            r.tool_name = "file.search".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            if let Some(ws) = get_runtime_workspace(runtime_id) {
                if let Err(e) = resolve_secure_dir(&ws, &path) {
                    if e.contains("workspace escape") {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: search path {} escapes workspace: {}", path, e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.search".to_string();
                        return Ok(r);
                    }
                }
                match search_via_rust(&ws, &path, &pattern) {
                    Ok(results) => {
                        let mut r = ToolResult::success(format!("search '{}' in {} via secure workspace {} (Rust, no shell):\n{}", pattern, path, runtime_id, results));
                        r.tool_name = "file.search".to_string();
                        return Ok(r);
                    }
                    Err(e) => {
                        let mut r = ToolResult::error(format!("search failed: {}", e), Some("SEARCH_FAILED".to_string()));
                        r.tool_name = "file.search".to_string();
                        return Ok(r);
                    }
                }
            }
        }
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("rg -n '{}' '{}' 2>/dev/null | head -n 100 || grep -Rn '{}' '{}' 2>/dev/null | head -n 100", pattern.replace('\'', "'\\''"), path.replace('\'', "'\\''"), pattern.replace('\'', "'\\''"), path.replace('\'', "'\\''"))])
            .output()
            .map_err(|e| format!("search failed: {}", e))?;
        let content = String::from_utf8_lossy(&output.stdout).to_string();
        let mut r = ToolResult::success(format!("search '{}' in {}:\n{}", pattern, path, content));
        r.tool_name = "file.search".to_string();
        Ok(r)
    }
}

fn search_via_rust(workspace: &str, user_path: &str, pattern: &str) -> Result<String, String> {
    let secure_path = resolve_secure_dir(workspace, user_path)?;
    let mut results = Vec::new();
    fn walk_dir(dir: &Path, pattern: &str, results: &mut Vec<String>, depth: usize) -> Result<(), String> {
        if depth > 10 { return Ok(()); }
        let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if let Ok(meta) = std::fs::symlink_metadata(&path) {
                    if meta.file_type().is_symlink() { continue; }
                }
                if path.is_dir() {
                    walk_dir(&path, pattern, results, depth+1)?;
                } else if path.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if content.contains(pattern) {
                            for (i, line) in content.lines().enumerate() {
                                if line.contains(pattern) {
                                    results.push(format!("{}:{}:{}", path.display(), i+1, line.chars().take(200).collect::<String>()));
                                    if results.len() >= 100 { return Ok(()); }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    walk_dir(&secure_path, pattern, &mut results, 0)?;
    Ok(results.join("\n"))
}

impl Tool for FsStatTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.stat".to_string(),
            description: "Stat a file with symlink escape protection.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Stat { path: PathBuf::from(&path) };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(spark_transport::FsResponse::Ok { data }) => {
                            let content = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(content);
                            r.tool_name = "file.stat".to_string();
                            return Ok(r);
                        }
                        Ok(spark_transport::FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("stat failed via transport: {}", message), Some("STAT_FAILED".to_string()));
                            r.tool_name = "file.stat".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            match stat_via_workspace_secure(runtime_id, &path) {
                Ok(content) => {
                    let mut r = ToolResult::success(content);
                    r.tool_name = "file.stat".to_string();
                    return Ok(r);
                }
                Err(e) => {
                    if e.contains("workspace escape") {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: {}", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.stat".to_string();
                        return Ok(r);
                    }
                    let mut r = ToolResult::error(format!("stat failed (secure): {}", e), Some("STAT_FAILED".to_string()));
                    r.tool_name = "file.stat".to_string();
                    return Ok(r);
                }
            }
        }
        match std::fs::metadata(&path) {
            Ok(meta) => {
                let content = format!("path: {}\nsize: {}\nis_dir: {}\nis_file: {}\nmodified: {:?}", path, meta.len(), meta.is_dir(), meta.is_file(), meta.modified());
                let mut r = ToolResult::success(content);
                r.tool_name = "file.stat".to_string();
                Ok(r)
            }
            Err(e) => {
                let mut r = ToolResult::error(format!("stat failed {}: {}", path, e), Some("STAT_FAILED".to_string()));
                r.tool_name = "file.stat".to_string();
                Ok(r)
            },
        }
    }
}

impl Tool for FsPatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.patch".to_string(),
            description: "Apply a patch to a file with secure workspace check. Supports search/replace. Prefer this over shell for code changes.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"patch":{"type":"string","description":"Unified diff"},"search":{"type":"string"},"replace":{"type":"string"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        let patch = extract_arg(args, "patch");
        let search = extract_arg(args, "search");
        let replace = extract_arg(args, "replace");

        // Try transport routing for remote filesystems (patch via transport)
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    if let Some(patch_content) = patch {
                        let req = spark_transport::FsRequest::Patch { path: PathBuf::from(&path), patch: patch_content.as_bytes().to_vec() };
                        match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                            Ok(spark_transport::FsResponse::Ok { data }) => {
                                let content = String::from_utf8_lossy(&data);
                                let mut r = ToolResult::success(format!("patch applied to {} via transport:\n{}", path, content));
                                r.tool_name = "file.patch".to_string();
                                return Ok(r);
                            }
                            Ok(spark_transport::FsResponse::Error { message }) => {
                                let mut r = ToolResult::error(format!("patch failed via transport: {}", message), Some("PATCH_FAILED".to_string()));
                                r.tool_name = "file.patch".to_string();
                                return Ok(r);
                            }
                            Ok(_) => {}
                            Err(_) => {}
                        }
                    }
                    if let (Some(s), Some(r)) = (search.clone(), replace.clone()) {
                        let req = spark_transport::FsRequest::Patch { path: PathBuf::from(&path), patch: format!("--- a/{}\n+++ b/{}\n@@ -1 +1 @@\n-{}\n+{}", path, path, s, r).as_bytes().to_vec() };
                        match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                            Ok(spark_transport::FsResponse::Ok { data }) => {
                                let content = String::from_utf8_lossy(&data);
                                let mut r = ToolResult::success(format!("patched {} via transport (search/replace):\n{}", path, content));
                                r.tool_name = "file.patch".to_string();
                                return Ok(r);
                            }
                            Ok(spark_transport::FsResponse::Error { message }) => {
                                let mut r = ToolResult::error(format!("patch (search/replace) failed via transport: {}", message), Some("PATCH_FAILED".to_string()));
                                r.tool_name = "file.patch".to_string();
                                return Ok(r);
                            }
                            Ok(_) => {}
                            Err(_) => {}
                        }
                    }
                }
            }
        }

        if let (Some(s), Some(r)) = (search, replace) {
            if !runtime_id.is_empty() {
                if let Some(ws) = get_runtime_workspace(runtime_id) {
                    match resolve_secure_existing(&ws, &path) {
                        Ok(secure_path) => {
                            let content = std::fs::read_to_string(&secure_path).map_err(|e| format!("read failed: {}", e))?;
                            if content.contains(&s) {
                                let new_content = content.replace(&s, &r);
                                std::fs::write(&secure_path, new_content).map_err(|e| format!("write failed: {}", e))?;
                                let mut res = ToolResult::success(format!("patched {} via search/replace in secure workspace {} -> {}", path, runtime_id, secure_path.display()));
                                res.tool_name = "file.patch".to_string();
                                return Ok(res);
                            } else {
                                let mut res = ToolResult::error(format!("search string not found in {}", path), Some("PATCH_SEARCH_NOT_FOUND".to_string()));
                                res.tool_name = "file.patch".to_string();
                                return Ok(res);
                            }
                        }
                        Err(e) => {
                            if e.contains("workspace escape") {
                                let mut res = ToolResult::error(format!("SECURITY BLOCKED: {}", e), Some("WORKSPACE_ESCAPE".to_string()));
                                res.tool_name = "file.patch".to_string();
                                return Ok(res);
                            }
                            if let Ok(secure_path) = resolve_secure_new(&ws, &path) {
                                let mut res = ToolResult::error(format!("file not found for search/replace: {} (secure path {})", path, secure_path.display()), Some("FILE_NOT_FOUND".to_string()));
                                res.tool_name = "file.patch".to_string();
                                return Ok(res);
                            }
                        }
                    }
                }
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    if content.contains(&s) {
                        let new_content = content.replace(&s, &r);
                        if let Err(e) = std::fs::write(&path, new_content) {
                            let mut res = ToolResult::error(format!("write failed: {}", e), Some("WRITE_FAILED".to_string()));
                            res.tool_name = "file.patch".to_string();
                            return Ok(res);
                        }
                        let mut res = ToolResult::success(format!("patched {} via search/replace", path));
                        res.tool_name = "file.patch".to_string();
                        Ok(res)
                    } else {
                        let mut res = ToolResult::error(format!("search string not found in {}", path), Some("PATCH_SEARCH_NOT_FOUND".to_string()));
                        res.tool_name = "file.patch".to_string();
                        Ok(res)
                    }
                }
                Err(e) => {
                    let mut res = ToolResult::error(format!("read failed {}: {}", path, e), Some("READ_FAILED".to_string()));
                    res.tool_name = "file.patch".to_string();
                    Ok(res)
                },
            }
        } else if let Some(patch_content) = patch {
            if !runtime_id.is_empty() {
                // Try transport routing for remote filesystem patch
                if let Some(ctx) = ctx {
                    if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                        let req = spark_transport::FsRequest::Patch { path: PathBuf::from(&path), patch: patch_content.as_bytes().to_vec() };
                        match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                            Ok(spark_transport::FsResponse::Ok { data }) => {
                                let content = String::from_utf8_lossy(&data);
                                let mut r = ToolResult::success(format!("patch applied to {} via transport:\n{}", path, content));
                                r.tool_name = "file.patch".to_string();
                                return Ok(r);
                            }
                            Ok(spark_transport::FsResponse::Error { message }) => {
                                let mut r = ToolResult::error(format!("patch failed via transport: {}", message), Some("PATCH_FAILED".to_string()));
                                r.tool_name = "file.patch".to_string();
                                return Ok(r);
                            }
                            Ok(_) => {}
                            Err(_) => {}
                        }
                    }
                }
                if let Some(ws) = get_runtime_workspace(runtime_id) {
                    if let Err(e) = resolve_secure_existing(&ws, &path) {
                        if let Err(e2) = resolve_secure_new(&ws, &path) {
                            let mut res = ToolResult::error(format!("SECURITY BLOCKED: patch path {} escapes workspace: {} / {}", path, e, e2), Some("WORKSPACE_ESCAPE".to_string()));
                            res.tool_name = "file.patch".to_string();
                            return Ok(res);
                        }
                    }
                    if let Ok(secure_path) = resolve_secure_existing(&ws, &path).or_else(|_| resolve_secure_new(&ws, &path)) {
                        let tmp_patch = format!("/tmp/patch-{}-{}.diff", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
                        if let Err(e) = std::fs::write(&tmp_patch, &patch_content) {
                            let mut res = ToolResult::error(format!("failed to write temp patch: {}", e), Some("PATCH_FAILED".to_string()));
                            res.tool_name = "file.patch".to_string();
                            return Ok(res);
                        }
                        let output = std::process::Command::new("sh")
                            .args(["-c", &format!("cd {} && patch -p0 < {} 2>&1 || patch -p1 < {} 2>&1", secure_path.parent().unwrap_or(Path::new(".")).display(), tmp_patch, tmp_patch)])
                            .output();
                        let _ = std::fs::remove_file(&tmp_patch);
                        match output {
                            Ok(o) => {
                                let out = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
                                if out.to_lowercase().contains("failed") && !out.to_lowercase().contains("success") {
                                    let mut res = ToolResult::error(format!("patch failed {}: {}", path, out), Some("PATCH_FAILED".to_string()));
                                    res.tool_name = "file.patch".to_string();
                                    return Ok(res);
                                } else {
                                    let mut res = ToolResult::success(format!("patch applied to {} via secure workspace: {}", path, out));
                                    res.tool_name = "file.patch".to_string();
                                    return Ok(res);
                                }
                            }
                            Err(e) => {
                                let mut res = ToolResult::error(format!("patch command failed: {}", e), Some("PATCH_FAILED".to_string()));
                                res.tool_name = "file.patch".to_string();
                                return Ok(res);
                            },
                        }
                    }
                }
            }
            let tmp_patch = format!("/tmp/patch-{}-{}.diff", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
            if let Err(e) = std::fs::write(&tmp_patch, &patch_content) {
                let mut res = ToolResult::error(format!("failed to write temp patch: {}", e), Some("PATCH_FAILED".to_string()));
                res.tool_name = "file.patch".to_string();
                return Ok(res);
            }
            let output = std::process::Command::new("sh")
                .args(["-c", &format!("cd $(dirname {}) && patch -p0 < {} 2>&1 || patch -p1 < {} 2>&1 || git apply {} 2>&1", path, tmp_patch, tmp_patch, tmp_patch)])
                .output();
            let _ = std::fs::remove_file(&tmp_patch);
            match output {
                Ok(o) => {
                    let out = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
                    if out.to_lowercase().contains("failed") && !out.to_lowercase().contains("success") {
                        let mut res = ToolResult::error(format!("patch failed {}: {}", path, out), Some("PATCH_FAILED".to_string()));
                        res.tool_name = "file.patch".to_string();
                        Ok(res)
                    } else {
                        let mut res = ToolResult::success(format!("patch applied to {}: {}", path, out));
                        res.tool_name = "file.patch".to_string();
                        Ok(res)
                    }
                }
                Err(e) => {
                    let mut res = ToolResult::error(format!("patch command failed: {}", e), Some("PATCH_FAILED".to_string()));
                    res.tool_name = "file.patch".to_string();
                    Ok(res)
                },
            }
        } else {
            let mut res = ToolResult::error("need either search+replace or patch".to_string(), Some("INVALID_ARGS".to_string()));
            res.tool_name = "file.patch".to_string();
            Ok(res)
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
        for (i, c) in rest[1..].char_indices() {
            if escaped { escaped = false; continue; }
            if c == '\\' { escaped = true; continue; }
            if c == '"' { end = Some(i); break; }
        }
        let end = end?;
        let raw = &rest[1..1+end];
        Some(raw.replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\"))
    } else { None }
}

impl Tool for FsMkdirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.mkdir".to_string(),
            description: "Create directory (mkdir -p) with workspace escape protection. No shell fallback.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Mkdir { path: PathBuf::from(&path), recursive: true };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(spark_transport::FsResponse::Ok { data }) => {
                            let content = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(format!("mkdir {} via transport:\n{}", path, content));
                            r.tool_name = "file.mkdir".to_string();
                            return Ok(r);
                        }
                        Ok(spark_transport::FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("mkdir failed via transport: {}", message), Some("MKDIR_FAILED".to_string()));
                            r.tool_name = "file.mkdir".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            if let Some(ws) = get_runtime_workspace(runtime_id) {
                match resolve_secure_new(&ws, &path) {
                    Ok(secure_path) => {
                        match std::fs::create_dir_all(&secure_path) {
                            Ok(()) => {
                                let mut r = ToolResult::success(format!("mkdir {} via secure workspace {} -> {}", path, runtime_id, secure_path.display()));
                                r.tool_name = "file.mkdir".to_string();
                                return Ok(r);
                            },
                            Err(e) => {
                                let mut r = ToolResult::error(format!("mkdir failed {}: {}", secure_path.display(), e), Some("MKDIR_FAILED".to_string()));
                                r.tool_name = "file.mkdir".to_string();
                                return Ok(r);
                            },
                        }
                    }
                    Err(e) => {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: {}", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.mkdir".to_string();
                        return Ok(r);
                    }
                }
            }
        }
        match std::fs::create_dir_all(&path) {
            Ok(_) => {
                let mut r = ToolResult::success(format!("mkdir {}", path));
                r.tool_name = "file.mkdir".to_string();
                Ok(r)
            },
            Err(e) => {
                let mut r = ToolResult::error(format!("mkdir failed {}: {}", path, e), Some("MKDIR_FAILED".to_string()));
                r.tool_name = "file.mkdir".to_string();
                Ok(r)
            },
        }
    }
}

impl Tool for FsRemoveTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.remove".to_string(),
            description: "Remove file or directory with workspace escape protection. No shell fallback for security.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let path = extract_arg(args, "path").ok_or("missing path")?;
        if path == "/" || path == "/*" || path == "/etc" || path == "/usr" || path == "/bin" {
            return Ok(ToolResult::permission_required("file.remove".to_string(), format!("refusing to remove dangerous path '{}'", path)));
        }
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Remove { path: PathBuf::from(&path), recursive: false };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(spark_transport::FsResponse::Ok { data }) => {
                            let content = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(format!("removed {} via transport:\n{}", path, content));
                            r.tool_name = "file.remove".to_string();
                            return Ok(r);
                        }
                        Ok(spark_transport::FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("remove failed via transport: {}", message), Some("REMOVE_FAILED".to_string()));
                            r.tool_name = "file.remove".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            if let Some(ws) = get_runtime_workspace(runtime_id) {
                match resolve_secure_existing(&ws, &path) {
                    Ok(secure_path) => {
                        let ws_canonical = canonical_workspace(&ws).and_then(|p| p.canonicalize().map_err(|e| e.to_string())).unwrap_or_else(|_| PathBuf::from(&ws));
                        if secure_path == ws_canonical {
                            let mut r = ToolResult::error(format!("SECURITY BLOCKED: refusing to remove workspace root itself: {}", secure_path.display()), Some("WORKSPACE_ESCAPE".to_string()));
                            r.tool_name = "file.remove".to_string();
                            return Ok(r);
                        }
                        let result = if secure_path.is_dir() { std::fs::remove_dir_all(&secure_path) } else { std::fs::remove_file(&secure_path) };
                        match result {
                            Ok(()) => {
                                let mut r = ToolResult::success(format!("removed {} via secure workspace {} -> {}", path, runtime_id, secure_path.display()));
                                r.tool_name = "file.remove".to_string();
                                return Ok(r);
                            },
                            Err(e) => {
                                let mut r = ToolResult::error(format!("remove failed {}: {}", secure_path.display(), e), Some("REMOVE_FAILED".to_string()));
                                r.tool_name = "file.remove".to_string();
                                return Ok(r);
                            },
                        }
                    }
                    Err(e) => {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: {}", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.remove".to_string();
                        return Ok(r);
                    }
                }
            }
        }
        let p = Path::new(&path);
        let result = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p).or_else(|_| std::fs::remove_dir_all(p)) };
        match result {
            Ok(_) => {
                let mut r = ToolResult::success(format!("removed {}", path));
                r.tool_name = "file.remove".to_string();
                Ok(r)
            },
            Err(e) => {
                let mut r = ToolResult::error(format!("remove failed {}: {}", path, e), Some("REMOVE_FAILED".to_string()));
                r.tool_name = "file.remove".to_string();
                Ok(r)
            },
        }
    }
}

impl Tool for FsRenameTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.rename".to_string(),
            description: "Rename/move file with workspace escape protection. No shell fallback.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"}},"required":["from","to"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let from = extract_arg(args, "from").or_else(|| extract_arg(args, "path")).or_else(|| extract_arg(args, "old")).ok_or("missing from")?;
        let to = extract_arg(args, "to").or_else(|| extract_arg(args, "new")).or_else(|| extract_arg(args, "dest")).ok_or("missing to")?;
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Rename { from: PathBuf::from(&from), to: PathBuf::from(&to) };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(spark_transport::FsResponse::Ok { data }) => {
                            let content = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(format!("renamed {} -> {} via transport:\n{}", from, to, content));
                            r.tool_name = "file.rename".to_string();
                            return Ok(r);
                        }
                        Ok(spark_transport::FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("rename failed via transport: {}", message), Some("RENAME_FAILED".to_string()));
                            r.tool_name = "file.rename".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            if let Some(ws) = get_runtime_workspace(runtime_id) {
                match (resolve_secure_existing(&ws, &from), resolve_secure_new(&ws, &to)) {
                    (Ok(secure_from), Ok(secure_to)) => {
                        if let Some(parent) = secure_to.parent() {
                            if let Err(e) = std::fs::create_dir_all(parent) {
                                let mut r = ToolResult::error(format!("create parent failed {}: {}", parent.display(), e), Some("MKDIR_FAILED".to_string()));
                                r.tool_name = "file.rename".to_string();
                                return Ok(r);
                            }
                        }
                        match std::fs::rename(&secure_from, &secure_to) {
                            Ok(()) => {
                                let mut r = ToolResult::success(format!("renamed {} -> {} via secure workspace {} ({} -> {})", from, to, runtime_id, secure_from.display(), secure_to.display()));
                                r.tool_name = "file.rename".to_string();
                                return Ok(r);
                            },
                            Err(e) => {
                                let mut r = ToolResult::error(format!("rename failed {} -> {}: {}", secure_from.display(), secure_to.display(), e), Some("RENAME_FAILED".to_string()));
                                r.tool_name = "file.rename".to_string();
                                return Ok(r);
                            },
                        }
                    }
                    (Err(e), _) => {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED from path: {}", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.rename".to_string();
                        return Ok(r);
                    }
                    (_, Err(e)) => {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED to path: {}", e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.rename".to_string();
                        return Ok(r);
                    }
                }
            }
        }
        if let Some(parent) = Path::new(&to).parent() { let _ = std::fs::create_dir_all(parent); }
        match std::fs::rename(&from, &to) {
            Ok(_) => {
                let mut r = ToolResult::success(format!("renamed {} -> {}", from, to));
                r.tool_name = "file.rename".to_string();
                Ok(r)
            },
            Err(e) => {
                let mut r = ToolResult::error(format!("rename failed {} -> {}: {}", from, to, e), Some("RENAME_FAILED".to_string()));
                r.tool_name = "file.rename".to_string();
                Ok(r)
            },
        }
    }
}

impl Tool for FsGlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file.glob".to_string(),
            description: "Glob files matching pattern with secure workspace check, no symlink follow.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string","description":"base path"}},"required":["pattern"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, runtime_id: &str) -> Result<ToolResult, String> {
        self.execute_with_context(args, runtime_id, None)
    }
    fn execute_with_context(&self, args: &str, runtime_id: &str, ctx: Option<&ToolExecutionContext>) -> Result<ToolResult, String> {
        let pattern = extract_arg(args, "pattern").ok_or("missing pattern")?;
        let base = extract_arg(args, "path").unwrap_or_else(|| ".".to_string());
        if !runtime_id.is_empty() {
            if let Some(ctx) = ctx {
                if let Some(transport) = ctx.transport_for_runtime(runtime_id) {
                    let req = spark_transport::FsRequest::Glob { pattern: pattern.clone() };
                    match super::context::block_on_transport(transport.fs_request(runtime_id, req)) {
                        Ok(spark_transport::FsResponse::Ok { data }) => {
                            let content = String::from_utf8_lossy(&data);
                            let mut r = ToolResult::success(format!("glob '{}' via transport:\n{}", pattern, content));
                            r.tool_name = "file.glob".to_string();
                            return Ok(r);
                        }
                        Ok(spark_transport::FsResponse::Error { message }) => {
                            let mut r = ToolResult::error(format!("glob failed via transport: {}", message), Some("GLOB_FAILED".to_string()));
                            r.tool_name = "file.glob".to_string();
                            return Ok(r);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            }
            if let Some(ws) = get_runtime_workspace(runtime_id) {
                match resolve_secure_dir(&ws, &base) {
                    Ok(secure_base) => {
                        match glob_via_rust(&secure_base, &pattern) {
                            Ok(results) => {
                                let mut r = ToolResult::success(format!("glob '{}' in {} via secure workspace {} (Rust, no symlink follow):\n{}", pattern, base, runtime_id, results));
                                r.tool_name = "file.glob".to_string();
                                return Ok(r);
                            }
                            Err(e) => {
                                let mut r = ToolResult::error(format!("glob failed: {}", e), Some("GLOB_FAILED".to_string()));
                                r.tool_name = "file.glob".to_string();
                                return Ok(r);
                            },
                        }
                    }
                    Err(e) => {
                        let mut r = ToolResult::error(format!("SECURITY BLOCKED: glob base path {} escapes workspace: {}", base, e), Some("WORKSPACE_ESCAPE".to_string()));
                        r.tool_name = "file.glob".to_string();
                        return Ok(r);
                    }
                }
            }
        }
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("find '{}' -path '{}' 2>/dev/null | head -n 200", base.replace('\'', "'\\''"), pattern.replace('\'', "'\\''"))])
            .output()
            .map_err(|e| format!("glob failed: {}", e))?;
        let content = String::from_utf8_lossy(&output.stdout).to_string();
        let mut r = ToolResult::success(format!("glob '{}' in {}:\n{}", pattern, base, content));
        r.tool_name = "file.glob".to_string();
        Ok(r)
    }
}

fn glob_via_rust(base: &Path, pattern: &str) -> Result<String, String> {
    let mut results = Vec::new();
    let is_recursive = pattern.contains("**");
    let file_pattern = if is_recursive { pattern.replace("**/", "").replace("**", "") } else { pattern.to_string() };
    fn matches_pattern(name: &str, pat: &str) -> bool {
        if pat == "*" { return true; }
        if pat.contains('*') {
            let parts: Vec<&str> = pat.split('*').collect();
            if parts.is_empty() { return true; }
            let mut remaining = name;
            for (i, part) in parts.iter().enumerate() {
                if part.is_empty() { continue; }
                if i == 0 {
                    if !remaining.starts_with(part) { return false; }
                    remaining = &remaining[part.len()..];
                } else if i == parts.len() - 1 {
                    if !remaining.ends_with(part) { return false; }
                } else {
                    if let Some(idx) = remaining.find(part) {
                        remaining = &remaining[idx + part.len()..];
                    } else { return false; }
                }
            }
            true
        } else { name == pat || name.contains(pat) }
    }
    fn walk(base: &Path, file_pattern: &str, is_recursive: bool, results: &mut Vec<String>, depth: usize) -> Result<(), String> {
        if depth > 10 { return Ok(()); }
        let entries = std::fs::read_dir(base).map_err(|e| e.to_string())?;
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if let Ok(meta) = std::fs::symlink_metadata(&path) {
                    if meta.file_type().is_symlink() { continue; }
                }
                let file_name = entry.file_name().to_string_lossy().to_string();
                if matches_pattern(&file_name, file_pattern) {
                    results.push(path.display().to_string());
                    if results.len() >= 200 { return Ok(()); }
                }
                if is_recursive && path.is_dir() {
                    walk(&path, file_pattern, is_recursive, results, depth+1)?;
                }
            }
        }
        Ok(())
    }
    walk(base, &file_pattern, is_recursive, &mut results, 0)?;
    Ok(results.join("\n"))
}
