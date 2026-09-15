//! Runtime filesystem API.
//!
//! Every file operation an agent performs — local *or* remote — lands here,
//! rooted at the runtime workspace. There is exactly one implementation, so a
//! machine cannot drift in behaviour from the local box (architecture §二十二:
//! no SFTP dual stack).
//!
//! Security model (unchanged from the pre-existing tool layer, now enforced at
//! the kernel boundary instead of in the client):
//!
//! * relative paths resolve against the runtime workspace;
//! * absolute paths are allowed only when they canonicalise *beneath* the
//!   canonical workspace root — `..` traversal and symlink escapes are
//!   rejected with a `workspace escape` error;
//! * new files are checked through their nearest existing ancestor, so a write
//!   cannot create `../../etc/cron.d/x`.
//!
//! Everything here is plain filesystem I/O: no subprocesses, no `patch(1)`
//! dependency.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsError {
    pub message: String,
    /// True when the failure was a workspace-escape rejection.
    pub security: bool,
}

impl FsError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            security: false,
        }
    }

    fn security(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            security: true,
        }
    }
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

type FsResult<T> = Result<T, FsError>;

// ---------------------------------------------------------------------------
// Path confinement
// ---------------------------------------------------------------------------

/// Canonical form of the workspace root (falls back to a lexical cleanup when
/// the path does not exist yet).
pub fn canonical_root(root: &Path) -> PathBuf {
    match root.canonicalize() {
        Ok(p) => p,
        Err(_) => lexical_normalize(root),
    }
}

/// Resolve a user supplied path against `root`, refusing anything that escapes.
pub fn resolve(root: &Path, user_path: &str) -> FsResult<PathBuf> {
    let root_canonical = canonical_root(root);
    let joined = if user_path.starts_with('/') {
        PathBuf::from(user_path)
    } else {
        root_canonical.join(user_path)
    };

    if let Ok(canonical) = joined.canonicalize() {
        if !canonical.starts_with(&root_canonical) {
            return Err(FsError::security(format!(
                "workspace escape detected: {} resolves outside {}",
                user_path,
                root_canonical.display()
            )));
        }
        return Ok(canonical);
    }

    // Target does not exist yet: verify the closest existing ancestor.
    let normalized = lexical_normalize(&joined);
    let mut probe = normalized.clone();
    while !probe.exists() {
        match probe.parent() {
            Some(parent) => probe = parent.to_path_buf(),
            None => break,
        }
    }
    let probe_canonical = probe.canonicalize().unwrap_or(probe);
    if !probe_canonical.starts_with(&root_canonical) {
        return Err(FsError::security(format!(
            "workspace escape detected: {} would be created outside {}",
            user_path,
            root_canonical.display()
        )));
    }
    Ok(normalized)
}

/// Lexically remove `.` and `..` components (no filesystem access).
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

pub fn read(root: &Path, path: &str) -> FsResult<Vec<u8>> {
    let target = resolve(root, path)?;
    std::fs::read(&target).map_err(|e| FsError::new(format!("read {} failed: {}", target.display(), e)))
}

pub fn write(root: &Path, path: &str, data: &[u8]) -> FsResult<usize> {
    let target = resolve(root, path)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| FsError::new(format!("create parent {} failed: {}", parent.display(), e)))?;
    }
    std::fs::write(&target, data)
        .map_err(|e| FsError::new(format!("write {} failed: {}", target.display(), e)))?;
    Ok(data.len())
}

/// Directory listing as JSON: `{"entries":[{"name":..,"path":..,"dir":..,"size":..}]}`
pub fn list(root: &Path, path: &str) -> FsResult<String> {
    let target = resolve(root, path)?;
    let mut entries = Vec::new();
    let dir = std::fs::read_dir(&target)
        .map_err(|e| FsError::new(format!("list {} failed: {}", target.display(), e)))?;
    let mut names: Vec<(String, PathBuf)> = Vec::new();
    for entry in dir {
        let entry = entry.map_err(|e| FsError::new(format!("read dir entry failed: {}", e)))?;
        names.push((entry.file_name().to_string_lossy().to_string(), entry.path()));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, full) in names {
        let meta = std::fs::symlink_metadata(&full).ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        entries.push(format!(
            "{{\"name\":\"{}\",\"path\":\"{}\",\"dir\":{},\"size\":{}}}",
            crate::json::escape(&name),
            crate::json::escape(&full.display().to_string()),
            is_dir,
            size
        ));
    }

    Ok(format!(
        "{{\"path\":\"{}\",\"count\":{},\"entries\":[{}]}}",
        crate::json::escape(&target.display().to_string()),
        entries.len(),
        entries.join(",")
    ))
}

pub fn stat(root: &Path, path: &str) -> FsResult<String> {
    let target = resolve(root, path)?;
    let meta = std::fs::metadata(&target)
        .map_err(|e| FsError::new(format!("stat {} failed: {}", target.display(), e)))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(format!(
        "{{\"path\":\"{}\",\"size\":{},\"is_dir\":{},\"is_file\":{},\"mode\":{},\"modified_ms\":{}}}",
        crate::json::escape(&target.display().to_string()),
        meta.len(),
        meta.is_dir(),
        meta.is_file(),
        mode_of(&meta),
        modified
    ))
}

fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

pub fn mkdir(root: &Path, path: &str, recursive: bool) -> FsResult<()> {
    let target = resolve(root, path)?;
    let result = if recursive {
        std::fs::create_dir_all(&target)
    } else {
        std::fs::create_dir(&target)
    };
    result.map_err(|e| FsError::new(format!("mkdir {} failed: {}", target.display(), e)))
}

pub fn remove(root: &Path, path: &str, recursive: bool) -> FsResult<()> {
    let target = resolve(root, path)?;
    let meta = std::fs::symlink_metadata(&target)
        .map_err(|e| FsError::new(format!("remove {} failed: {}", target.display(), e)))?;
    if meta.is_dir() {
        if recursive {
            std::fs::remove_dir_all(&target)
        } else {
            std::fs::remove_dir(&target)
        }
    } else {
        std::fs::remove_file(&target)
    }
    .map_err(|e| FsError::new(format!("remove {} failed: {}", target.display(), e)))
}

pub fn rename(root: &Path, from: &str, to: &str) -> FsResult<()> {
    let source = resolve(root, from)?;
    let target = resolve(root, to)?;
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::rename(&source, &target).map_err(|e| {
        FsError::new(format!(
            "rename {} -> {} failed: {}",
            source.display(),
            target.display(),
            e
        ))
    })
}

/// Recursive grep. Returns `{"matches":[{"path":..,"line":..,"text":..}],"truncated":bool}`
pub fn search(root: &Path, path: &str, query: &str, max_matches: usize) -> FsResult<String> {
    let start = resolve(root, path)?;
    let mut matches: Vec<String> = Vec::new();
    let mut truncated = false;
    search_walk(&start, query, max_matches, &mut matches, &mut truncated)?;
    Ok(format!(
        "{{\"query\":\"{}\",\"count\":{},\"truncated\":{},\"matches\":[{}]}}",
        crate::json::escape(query),
        matches.len(),
        truncated,
        matches.join(",")
    ))
}

fn search_walk(
    start: &Path,
    query: &str,
    max_matches: usize,
    out: &mut Vec<String>,
    truncated: &mut bool,
) -> FsResult<()> {
    let meta = std::fs::symlink_metadata(start)
        .map_err(|e| FsError::new(format!("search {} failed: {}", start.display(), e)))?;

    if meta.is_file() {
        // Skip binary-ish files and anything huge.
        if meta.len() > 4 * 1024 * 1024 {
            return Ok(());
        }
        let content = match std::fs::read(start) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        if content.iter().take(4096).any(|b| *b == 0) {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&content);
        for (idx, line) in text.lines().enumerate() {
            if line.contains(query) {
                out.push(format!(
                    "{{\"path\":\"{}\",\"line\":{},\"text\":\"{}\"}}",
                    crate::json::escape(&start.display().to_string()),
                    idx + 1,
                    crate::json::escape(line.trim())
                ));
                if out.len() >= max_matches {
                    *truncated = true;
                    return Ok(());
                }
            }
        }
        return Ok(());
    }

    if meta.is_dir() {
        let mut children: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(start)
            .map_err(|e| FsError::new(format!("search dir {} failed: {}", start.display(), e)))?
        {
            let entry = entry.map_err(|e| FsError::new(format!("read dir entry failed: {}", e)))?;
            children.push(entry.path());
        }
        children.sort();
        for child in children {
            let name = child.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if matches!(name.as_str(), ".git" | "node_modules" | "target" | ".venv" | "__pycache__") {
                continue;
            }
            search_walk(&child, query, max_matches, out, truncated)?;
            if *truncated {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Glob underneath `path`. Supports `*`, `?`, `**` and character classes are
/// intentionally not supported (keep the matcher predictable).
pub fn glob(root: &Path, path: &str, pattern: &str, max_matches: usize) -> FsResult<String> {
    let start = resolve(root, path)?;
    let mut hits: Vec<String> = Vec::new();
    glob_walk(&start, pattern, max_matches, &mut hits)?;
    hits.sort();
    let truncated = hits.len() >= max_matches;
    Ok(format!(
        "{{\"pattern\":\"{}\",\"count\":{},\"truncated\":{},\"paths\":[{}]}}",
        crate::json::escape(pattern),
        hits.len(),
        truncated,
        hits
            .iter()
            .map(|p| format!("\"{}\"", crate::json::escape(p)))
            .collect::<Vec<_>>()
            .join(",")
    ))
}

fn glob_walk(start: &Path, pattern: &str, max: usize, out: &mut Vec<String>) -> FsResult<()> {
    if out.len() >= max {
        return Ok(());
    }
    let meta = match std::fs::symlink_metadata(start) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if meta.is_file() {
        if wildcard_match(pattern, &start.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()) {
            out.push(start.display().to_string());
        }
        return Ok(());
    }
    let mut children: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(start) {
        for entry in dir.flatten() {
            children.push(entry.path());
        }
    }
    children.sort();
    for child in children {
        glob_walk(&child, pattern, max, out)?;
        if out.len() >= max {
            return Ok(());
        }
    }
    Ok(())
}

/// `*` (any run, not crossing `/` in the pattern itself), `?` (one char),
/// `**` handled by the caller walking recursively.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == "**" {
        return true;
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    wildcard_inner(&p, &n)
}

fn wildcard_inner(p: &[char], n: &[char]) -> bool {
    if p.is_empty() {
        return n.is_empty();
    }
    match p[0] {
        '*' => {
            // collapse consecutive stars
            let mut rest = &p[1..];
            while !rest.is_empty() && rest[0] == '*' {
                rest = &rest[1..];
            }
            if rest.is_empty() {
                return true;
            }
            for skip in 0..=n.len() {
                if wildcard_inner(rest, &n[skip..]) {
                    return true;
                }
            }
            false
        }
        '?' => !n.is_empty() && wildcard_inner(&p[1..], &n[1..]),
        c => !n.is_empty() && n[0] == c && wildcard_inner(&p[1..], &n[1..]),
    }
}

// ---------------------------------------------------------------------------
// Patch
// ---------------------------------------------------------------------------

/// Apply either a unified diff or a literal search/replace, whichever is
/// provided. Returns the new file content.
pub fn apply_patch(
    root: &Path,
    path: &str,
    patch: Option<&str>,
    search: Option<&str>,
    replace: Option<&str>,
) -> FsResult<String> {
    let target = resolve(root, path)?;
    let original = String::from_utf8_lossy(
        &std::fs::read(&target)
            .map_err(|e| FsError::new(format!("patch target {} unreadable: {}", target.display(), e)))?,
    )
    .to_string();

    let updated = if let (Some(s), Some(r)) = (search, replace) {
        if !original.contains(s) {
            return Err(FsError::new(format!(
                "search string not found in {}",
                target.display()
            )));
        }
        original.replace(s, r)
    } else if let Some(diff) = patch {
        apply_unified_diff(&original, diff)?
    } else {
        return Err(FsError::new(
            "patch requires either `patch` or both `search` and `replace`",
        ));
    };

    std::fs::write(&target, updated.as_bytes()).map_err(|e| {
        FsError::new(format!("write patched {} failed: {}", target.display(), e))
    })?;
    Ok(updated)
}

/// Minimal but strict unified-diff applier.
///
/// Supported: `---`/`+++` headers (ignored), `@@ -old,count +new,count @@`
/// hunks, context/`-`/`+` lines, and a trailing `\ No newline at end of file`.
/// Context lines must match exactly — no fuzz — because a silent mis-apply is
/// worse than a failed patch.
pub fn apply_unified_diff(original: &str, diff: &str) -> FsResult<String> {
    let hunks = parse_hunks(diff)?;
    if hunks.is_empty() {
        return Err(FsError::new(
            "no hunks found in patch (expected `@@ -a,b +c,d @@` blocks)",
        ));
    }

    let original_lines: Vec<&str> = split_lines(original);
    let mut out: Vec<String> = Vec::new();
    let mut cursor: usize = 0;

    for hunk in hunks {
        let start = hunk.old_start;
        if start > original_lines.len() + 1 {
            return Err(FsError::new(format!(
                "hunk starts at line {} but file has only {} lines",
                start,
                original_lines.len()
            )));
        }

        // Copy untouched lines before the hunk.
        let hunk_index = start.saturating_sub(1);
        if hunk_index < cursor {
            return Err(FsError::new("overlapping hunks in patch"));
        }
        while cursor < hunk_index {
            out.push(original_lines[cursor].to_string());
            cursor += 1;
        }

        for line in &hunk.lines {
            match line.kind {
                HunkLineKind::Context => {
                    let expected = original_lines.get(cursor).copied().unwrap_or("");
                    if expected != line.text {
                        return Err(FsError::new(format!(
                            "context mismatch at line {}: expected {:?}, found {:?}",
                            cursor + 1,
                            line.text,
                            expected
                        )));
                    }
                    out.push(expected.to_string());
                    cursor += 1;
                }
                HunkLineKind::Remove => {
                    let expected = original_lines.get(cursor).copied().unwrap_or("");
                    if expected != line.text {
                        return Err(FsError::new(format!(
                            "removal mismatch at line {}: expected {:?}, found {:?}",
                            cursor + 1,
                            line.text,
                            expected
                        )));
                    }
                    cursor += 1;
                }
                HunkLineKind::Add => out.push(line.text.clone()),
            }
        }
    }

    while cursor < original_lines.len() {
        out.push(original_lines[cursor].to_string());
        cursor += 1;
    }

    let mut result = out.join("\n");
    if original.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    // `a\nb\n` splits into ["a","b",""] — drop the trailing empty piece.
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HunkLineKind {
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone)]
struct HunkLine {
    kind: HunkLineKind,
    text: String,
}

#[derive(Debug, Clone)]
struct Hunk {
    /// 1-based first line in the original file.
    old_start: usize,
    lines: Vec<HunkLine>,
}

fn parse_hunks(diff: &str) -> FsResult<Vec<Hunk>> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current: Option<Hunk> = None;

    for raw in diff.lines() {
        let line = raw.trim_end_matches('\r');
        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            let old_start = parse_hunk_header(line)?;
            current = Some(Hunk {
                old_start,
                lines: Vec::new(),
            });
            continue;
        }

        if line.starts_with("---") || line.starts_with("+++") || line.starts_with("diff ") || line.starts_with("index ") {
            continue;
        }
        if line.starts_with("\\ No newline") {
            continue;
        }

        let Some(hunk) = current.as_mut() else {
            // Lines before the first hunk header are part of the file header
            // block or stray text; ignore them.
            continue;
        };

        if let Some(rest) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine {
                kind: HunkLineKind::Add,
                text: rest.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine {
                kind: HunkLineKind::Remove,
                text: rest.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix(' ') {
            hunk.lines.push(HunkLine {
                kind: HunkLineKind::Context,
                text: rest.to_string(),
            });
        } else if line.is_empty() {
            hunk.lines.push(HunkLine {
                kind: HunkLineKind::Context,
                text: String::new(),
            });
        }
    }

    if let Some(hunk) = current.take() {
        hunks.push(hunk);
    }
    Ok(hunks)
}

fn parse_hunk_header(line: &str) -> FsResult<usize> {
    // `@@ -12,3 +14,5 @@` or `@@ -12 +14 @@`
    let after_minus = line
        .find('-')
        .ok_or_else(|| FsError::new(format!("malformed hunk header: {}", line)))?;
    let rest = &line[after_minus + 1..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse::<usize>()
        .map_err(|_| FsError::new(format!("malformed hunk header: {}", line)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sand-fs-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        root
    }

    #[test]
    fn write_read_roundtrip_inside_workspace() {
        let root = temp_root("roundtrip");
        write(&root, "src/main.rs", b"fn main() {}\n").expect("write");
        let data = read(&root, "src/main.rs").expect("read");
        assert_eq!(String::from_utf8_lossy(&data), "fn main() {}\n");
        let listing = list(&root, ".").expect("list");
        assert!(listing.contains("src"));
    }

    #[test]
    fn rejects_workspace_escape() {
        let root = temp_root("escape");
        let err = write(&root, "../../etc/passwd", b"x").expect_err("must reject");
        assert!(err.security, "expected security error, got {err}");
        let err = read(&root, "../../../etc/hostname").expect_err("must reject");
        assert!(err.security);
        let err = resolve(&root, "/etc/hostname").expect_err("must reject absolute");
        assert!(err.security);
    }

    #[test]
    fn allows_nested_new_directories() {
        let root = temp_root("nested");
        let target = resolve(&root, "a/b/c/file.txt").expect("resolve new file");
        assert!(target.starts_with(root.canonicalize().unwrap()));
    }

    #[test]
    fn applies_unified_diff() {
        let original = "line1\nline2\nline3\n";
        let diff = "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n line1\n-line2\n+changed\n line3\n";
        let updated = apply_unified_diff(original, diff).expect("apply");
        assert_eq!(updated, "line1\nchanged\nline3\n");
    }

    #[test]
    fn applies_addition_hunk() {
        let original = "a\nb\n";
        let diff = "@@ -2,1 +2,2 @@\n b\n+new\n";
        let updated = apply_unified_diff(original, diff).expect("apply");
        assert_eq!(updated, "a\nb\nnew\n");
    }

    #[test]
    fn rejects_mismatched_context() {
        let original = "a\nb\n";
        let diff = "@@ -1,2 +1,2 @@\n a\n-not-b\n+x\n";
        let err = apply_unified_diff(original, diff).expect_err("must fail");
        assert!(err.message.contains("mismatch"), "got {err}");
    }

    #[test]
    fn search_replace_requires_match() {
        let root = temp_root("search");
        write(&root, "f.txt", b"hello world\n").expect("write");
        let out = apply_patch(&root, "f.txt", None, Some("hello"), Some("goodbye")).expect("patch");
        assert_eq!(out, "goodbye world\n");
        let err = apply_patch(&root, "f.txt", None, Some("absent"), Some("x")).expect_err("miss");
        assert!(err.message.contains("not found"));
    }

    #[test]
    fn glob_and_wildcard() {
        assert!(wildcard_match("*.rs", "main.rs"));
        assert!(!wildcard_match("*.rs", "main.py"));
        assert!(wildcard_match("m?in.rs", "main.rs"));
        let root = temp_root("glob");
        write(&root, "a/x.rs", b"").expect("write");
        write(&root, "b/y.txt", b"").expect("write");
        let out = glob(&root, ".", "*.rs", 100).expect("glob");
        assert!(out.contains("x.rs"), "got {out}");
        assert!(!out.contains("y.txt"));
    }

    #[test]
    fn searches_text_files_only() {
        let root = temp_root("search-walk");
        write(&root, "src/lib.rs", b"fn target() {}\n").expect("write");
        write(&root, "bin.dat", &[0u8, 1, 2, 3]).expect("write");
        let out = search(&root, ".", "target", 50).expect("search");
        assert!(out.contains("lib.rs"), "got {out}");
        let out = search(&root, ".", "nothing-here", 50).expect("search");
        assert!(out.contains("\"count\":0"));
    }
}
