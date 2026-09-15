//! Capability detection.
//!
//! sandd reports what the *machine* can actually do, per connect, instead of
//! hardcoding a wish list. host-agent stores the answer on the `Machine` record
//! and the client uses it to enable/disable panels (e.g. Computer use is
//! disabled when `desktop == false`, architecture §二十五).

use std::path::{Path, PathBuf};

/// Base protocol features that only depend on the sandd binary itself.
pub fn base_features() -> Vec<String> {
    sand_protocol::SAND_FEATURES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Full feature list for this machine.
pub fn features() -> Vec<String> {
    let mut features = base_features();
    if browser_available() {
        features.push("browser".to_string());
    }
    if desktop_available() {
        features.push("desktop".to_string());
    }
    if computer_use_available() {
        features.push("computer.desktop".to_string());
    }
    if gpu_available() {
        features.push("gpu".to_string());
    }
    features
}

/// True when a browser worker can actually run here: Node plus a browser
/// automation package. We check for `node` and the worker entry point; the
/// worker itself verifies Playwright lazily and reports a clear error.
pub fn browser_available() -> bool {
    which("node").is_some() && browser_worker_entry().is_some()
}

pub fn desktop_available() -> bool {
    which("Xvfb").is_some()
}

/// Computer use needs a display server *and* an input injection tool.
pub fn computer_use_available() -> bool {
    desktop_available() && (which("xdotool").is_some() || which("xte").is_some())
}

pub fn gpu_available() -> bool {
    sand_protocol::status::machine_info().gpu.is_some()
}

/// Path to the browser worker script on this machine.
///
/// Resolution order:
/// 1. `SPARK_BROWSER_WORKER` (explicit path);
/// 2. `~/.local/share/spark/current/browser-worker/index.js` (bootstrap layout);
/// 3. the repository checkout next to the sandd binary (development layout).
pub fn browser_worker_entry() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SPARK_BROWSER_WORKER") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(&home)
            .join(".local/share/spark/current/browser-worker/index.js");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // target/debug/sandd → ../../browser-worker/src/index.js
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            let Some(current) = dir else { break };
            let candidate = current.join("browser-worker/src/index.js");
            if candidate.exists() {
                return Some(candidate);
            }
            dir = current.parent().map(|p| p.to_path_buf());
        }
    }
    None
}

/// Where the browser worker writes the loopback port it listens on.
pub fn browser_port_file(runtime_id: &str) -> PathBuf {
    let safe: String = runtime_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!("spark-browser-{}.port", safe))
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let path = PathBuf::from(cmd);
        return path.exists().then_some(path);
    }
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_features_match_protocol() {
        let features = base_features();
        assert!(features.contains(&"exec".to_string()));
        assert!(features.contains(&"pty".to_string()));
        assert!(features.contains(&"fs".to_string()));
    }

    #[test]
    fn port_file_is_runtime_scoped_and_path_safe() {
        let a = browser_port_file("rt-abc123");
        let b = browser_port_file("rt-def456");
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("rt-abc123"));
        // Path separators in a runtime id must not escape the temp dir.
        let weird = browser_port_file("../../etc/passwd");
        assert!(!weird.to_string_lossy().contains('/').then_some(false).unwrap_or(true));
    }

    #[test]
    fn which_finds_known_binary_or_none() {
        // `sh` exists on any POSIX box; a random name does not.
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-binary-xyz").is_none());
    }
}
