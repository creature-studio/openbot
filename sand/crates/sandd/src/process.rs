use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Instant, Duration};
use std::io::{Read, Write};

use sand_protocol::RuntimeId;
use crate::cgroup::CgroupManager;

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub pid: i32,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct ProcessManager {
    cgroup_mgr: Arc<CgroupManager>,
}

impl ProcessManager {
    pub fn new(cgroup_mgr: Arc<CgroupManager>) -> Self {
        Self { cgroup_mgr }
    }

    pub fn exec_robust(
        &self,
        runtime_id: &RuntimeId,
        workspace: &Path,
        command: Vec<String>,
        cwd: Option<String>,
        env: HashMap<String, String>,
        timeout_ms: Option<u64>,
    ) -> Result<ExecResult, String> {
        if command.is_empty() {
            return Err("empty command".to_string());
        }

        let mut cmd = Command::new(&command[0]);
        if command.len() > 1 {
            cmd.args(&command[1..]);
        }

        if let Some(cwd_str) = cwd {
            cmd.current_dir(cwd_str);
        } else {
            cmd.current_dir(workspace);
        }

        for (k, v) in env {
            cmd.env(k, v);
        }

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let cgroup_path = self.cgroup_mgr.runtime_cgroup_path(&runtime_id.0);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let cgroup_path_clone = cgroup_path.clone();
            unsafe {
                cmd.pre_exec(move || {
                    let pid = std::process::id() as i32;
                    let procs_file = cgroup_path_clone.join("cgroup.procs");
                    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&procs_file) {
                        let _ = writeln!(f, "{}", pid);
                    }
                    Ok(())
                });
            }
        }

        let start = Instant::now();
        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {}", e))?;
        let pid = child.id() as i32;
        let _ = self.cgroup_mgr.add_pid(&runtime_id.0, pid);

        // spawn threads to read stdout/stderr
        let stdout_handle = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = out.read_to_end(&mut buf);
                buf
            })
        });

        let stderr_handle = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = err.read_to_end(&mut buf);
                buf
            })
        });

        // wait with timeout
        let timeout = timeout_ms.map(Duration::from_millis);
        let status = if let Some(to) = timeout {
            let mut result = None;
            let wait_start = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => {
                        result = Some(s);
                        break;
                    }
                    Ok(None) => {
                        if wait_start.elapsed() > to {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => return Err(format!("wait error: {}", e)),
                }
            }
            result
        } else {
            Some(child.wait().map_err(|e| format!("wait failed: {}", e))?)
        };

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(h) = stdout_handle {
            stdout = h.join().unwrap_or_default();
        }
        if let Some(h) = stderr_handle {
            stderr = h.join().unwrap_or_default();
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        let (exit_code, signal) = if let Some(s) = status {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                (s.code(), s.signal())
            }
            #[cfg(not(unix))]
            {
                (s.code(), None)
            }
        } else {
            (None, None)
        };

        Ok(ExecResult {
            pid,
            exit_code,
            signal,
            stdout,
            stderr,
            duration_ms,
        })
    }

    pub fn spawn_background(
        &self,
        runtime_id: &RuntimeId,
        workspace: &Path,
        command: Vec<String>,
        cwd: Option<String>,
        env: HashMap<String, String>,
    ) -> Result<i32, String> {
        if command.is_empty() {
            return Err("empty command".to_string());
        }

        let mut cmd = Command::new(&command[0]);
        if command.len() > 1 {
            cmd.args(&command[1..]);
        }

        if let Some(cwd_str) = cwd {
            cmd.current_dir(cwd_str);
        } else {
            cmd.current_dir(workspace);
        }

        for (k, v) in env {
            cmd.env(k, v);
        }

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        let cgroup_path = self.cgroup_mgr.runtime_cgroup_path(&runtime_id.0);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let cgroup_path_clone = cgroup_path.clone();
            unsafe {
                cmd.pre_exec(move || {
                    let pid = std::process::id() as i32;
                    let procs_file = cgroup_path_clone.join("cgroup.procs");
                    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&procs_file) {
                        let _ = writeln!(f, "{}", pid);
                    }
                    Ok(())
                });
            }
        }

        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {}", e))?;
        let pid = child.id() as i32;
        let _ = self.cgroup_mgr.add_pid(&runtime_id.0, pid);

        std::thread::spawn(move || {
            let _ = child.wait();
        });

        Ok(pid)
    }

    pub fn kill_all_for_runtime(&self, runtime_id: &str) {
        let pids = self.cgroup_mgr.list_pids(runtime_id);
        for pid in pids {
            crate::cgroup::kill_pid(pid, 9);
        }
    }
}
