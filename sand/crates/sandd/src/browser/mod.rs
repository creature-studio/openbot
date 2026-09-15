//! Remote browser control.
//!
//! The browser lives **inside the runtime** on the machine that owns it:
//! Xvfb (from [`crate::desktop`]) + the Node/Playwright browser worker, both
//! started on demand and belonging to the runtime's cgroup. The client never
//! talks to Chrome directly — it sends `browser.*` calls to sandd, which
//! proxies them to the worker over **loopback only** (no public port, no VNC
//! as the preview path: screenshots are frames, architecture §二十三/§二十四).
//!
//! Worker endpoints (see `sand/browser-worker/src/index.js`):
//! `/open`, `/snapshot`, `/click`, `/fill`, `/press`, `/scroll`, `/screenshot`,
//! `/tabs`, `/close`, `/health`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::capabilities;
use crate::desktop::DesktopManager;

/// Result of one worker call: JSON body plus optional raw bytes (screenshots).
pub struct WorkerResponse {
    pub json: String,
    pub binary: Vec<u8>,
}

pub struct BrowserManager {
    desktop: Arc<DesktopManager>,
    /// runtime_id → port the worker listens on.
    ports: Mutex<std::collections::HashMap<String, u16>>,
    /// runtime_id → worker child process.
    children: Mutex<std::collections::HashMap<String, std::process::Child>>,
}

impl BrowserManager {
    pub fn new(desktop: Arc<DesktopManager>) -> Self {
        Self {
            desktop,
            ports: Mutex::new(std::collections::HashMap::new()),
            children: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// True when this machine can host a browser for the given runtime.
    pub fn available(&self) -> bool {
        capabilities::browser_available()
    }

    /// Ensure a worker is running for `runtime_id`, returning its loopback port.
    ///
    /// The worker is started with the runtime's `DISPLAY` so Chrome renders into
    /// the runtime's Xvfb, and it is added to the runtime cgroup so that
    /// `DestroyRuntime` cleans the browser up with everything else.
    pub fn ensure_worker(&self, runtime_id: &str, cgroup_path: Option<&std::path::Path>) -> Result<u16, String> {
        if let Some(port) = self.ports.lock().map_err(|e| e.to_string())?.get(runtime_id).copied() {
            if worker_alive(port) {
                return Ok(port);
            }
        }

        let entry = capabilities::browser_worker_entry()
            .ok_or_else(|| "browser worker not installed on this machine".to_string())?;
        let node = capabilities::which("node")
            .ok_or_else(|| "node not found on this machine".to_string())?;
        let display = self.desktop.ensure_display(runtime_id, 1280, 800)?;
        let port_file = capabilities::browser_port_file(runtime_id);
        let _ = std::fs::remove_file(&port_file);

        let mut command = Command::new(node);
        command
            .arg(entry)
            .env("DISPLAY", &display)
            .env("BROWSER_WORKER_PORT", "0")
            .env("BROWSER_WORKER_PORT_FILE", &port_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        let child = command
            .spawn()
            .map_err(|e| format!("failed to start browser worker: {}", e))?;

        // Put the worker (and everything it spawns) into the runtime cgroup, so
        // DestroyRuntime really does clean up Chrome.
        if let Some(cgroup) = cgroup_path {
            let procs = cgroup.join("cgroup.procs");
            let _ = std::fs::write(&procs, child.id().to_string());
        }

        self.children
            .lock()
            .map_err(|e| e.to_string())?
            .insert(runtime_id.to_string(), child);

        let port = wait_for_port(&port_file, Duration::from_secs(20))
            .ok_or_else(|| "browser worker did not report a port".to_string())?;
        self.ports
            .lock()
            .map_err(|e| e.to_string())?
            .insert(runtime_id.to_string(), port);
        Ok(port)
    }

    /// Proxy one action to the runtime's worker.
    pub fn call(
        &self,
        runtime_id: &str,
        cgroup_path: Option<&std::path::Path>,
        method: &str,
        body: Option<&str>,
    ) -> Result<WorkerResponse, String> {
        if !self.available() {
            return Err(
                "browser is not available on this machine (node + browser worker required)"
                    .to_string(),
            );
        }
        let port = self.ensure_worker(runtime_id, cgroup_path)?;

        let (http_method, path) = match method {
            "open" => ("POST", "/open"),
            "snapshot" => ("GET", "/snapshot"),
            "click" => ("POST", "/click"),
            "fill" => ("POST", "/fill"),
            "press" => ("POST", "/press"),
            "scroll" => ("POST", "/scroll"),
            "screenshot" => ("POST", "/screenshot"),
            "tabs" => ("GET", "/tabs"),
            "close" => ("POST", "/close"),
            "health" => ("GET", "/health"),
            other => return Err(format!("unknown browser action: {}", other)),
        };

        let response = http_call(port, http_method, path, body.unwrap_or("{}"))?;
        Ok(response)
    }

    /// Screenshot as raw bytes (JPEG/PNG) for the frame stream.
    pub fn screenshot_bytes(
        &self,
        runtime_id: &str,
        cgroup_path: Option<&std::path::Path>,
    ) -> Result<Vec<u8>, String> {
        let response = self.call(runtime_id, cgroup_path, "screenshot", None)?;
        if let Some(b64) = crate::rpc::extract_field_public(&response.json, "screenshot_b64") {
            return Ok(crate::rpc::base64_decode_public(&b64));
        }
        if !response.binary.is_empty() {
            return Ok(response.binary);
        }
        Err("worker returned no screenshot".to_string())
    }

    /// Stop the worker (and therefore Chrome) for a runtime.
    pub fn destroy(&self, runtime_id: &str) {
        if let Ok(mut children) = self.children.lock() {
            if let Some(mut child) = children.remove(runtime_id) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if let Ok(mut ports) = self.ports.lock() {
            ports.remove(runtime_id);
        }
        let _ = std::fs::remove_file(capabilities::browser_port_file(runtime_id));
    }
}

fn worker_alive(port: u16) -> bool {
    http_call(port, "GET", "/health", "{}").is_ok()
}

fn wait_for_port(port_file: &std::path::Path, timeout: Duration) -> Option<u16> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(port_file) {
            if let Ok(port) = content.trim().parse::<u16>() {
                if port > 0 {
                    return Some(port);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Minimal HTTP/1.1 client for the loopback worker.
///
/// A full HTTP stack is unnecessary here: the worker is always on 127.0.0.1,
/// speaks a fixed set of small JSON endpoints, and never streams.
fn http_call(port: u16, method: &str, path: &str, body: &str) -> Result<WorkerResponse, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("browser worker connect failed: {}", e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;

    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        method = method,
        path = path,
        port = port,
        len = body.len(),
        body = body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("browser worker write failed: {}", e))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("browser worker read failed: {}", e))?;

    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "browser worker returned a malformed response".to_string())?;
    let headers = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let payload = raw[header_end + 4..].to_vec();

    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    let json = String::from_utf8_lossy(&payload).to_string();
    if status >= 400 {
        return Err(format!("browser worker error (HTTP {}): {}", status, json));
    }
    Ok(WorkerResponse {
        json,
        binary: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_call_reports_connection_failure() {
        // Nothing listens on this port: the error must be explicit, never a
        // silent success.
        let err = http_call(1, "GET", "/health", "{}").expect_err("must fail");
        assert!(err.contains("connect failed"), "got {err}");
    }

    #[test]
    fn unavailable_without_worker_entry() {
        let desktop = Arc::new(DesktopManager::new());
        let manager = BrowserManager::new(desktop);
        // In this environment there is no browser worker configured, so the
        // module must say so rather than pretend.
        std::env::remove_var("SPARK_BROWSER_WORKER");
        if !manager.available() {
            let err = manager
                .call("rt-x", None, "snapshot", None)
                .expect_err("must refuse");
            assert!(err.contains("not available"), "got {err}");
        }
    }
}
