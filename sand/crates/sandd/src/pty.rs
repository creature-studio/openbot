use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::sync::mpsc::{self, Sender};

#[derive(Debug, Clone)]
pub struct PtySession {
    pub runtime_id: String,
    pub pty_id: String,
    pub pid: i32,
    pub cols: u16,
    pub rows: u16,
    pub shell: String,
    pub master_fd: i32,
}

#[derive(Debug)]
struct PtyInner {
    child_pid: i32,
    writer_tx: Sender<Vec<u8>>,
}

#[derive(Debug)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<String, HashMap<String, PtySession>>>>,
    inners: Arc<Mutex<HashMap<String, PtyInner>>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            inners: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn count_for_runtime(&self, runtime_id: &str) -> usize {
        self.sessions.lock().unwrap().get(runtime_id).map(|m| m.len()).unwrap_or(0)
    }

    pub fn list_for_runtime(&self, runtime_id: &str) -> Vec<PtySession> {
        self.sessions.lock().unwrap().get(runtime_id).map(|m| m.values().cloned().collect()).unwrap_or_default()
    }

    pub fn open_pty(&self, runtime_id: &str, pty_id: &str, cols: u16, rows: u16, shell: &str, cgroup_path: Option<std::path::PathBuf>) -> Result<PtySession, String> {
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(map) = sessions.get(runtime_id) {
                if map.contains_key(pty_id) {
                    return Err(format!("pty {} already exists in runtime {}", pty_id, runtime_id));
                }
            }
        }
        self.open_pty_pipe(runtime_id, pty_id, cols, rows, shell, cgroup_path)
    }

    fn open_pty_pipe(&self, runtime_id: &str, pty_id: &str, cols: u16, rows: u16, shell: &str, cgroup_path: Option<std::path::PathBuf>) -> Result<PtySession, String> {
        let shell_path = if shell.is_empty() { "/bin/bash".to_string() } else { shell.to_string() };

        let mut cmd = Command::new(&shell_path);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(cg_path) = cgroup_path {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let cg_path_clone = cg_path.clone();
                unsafe {
                    cmd.pre_exec(move || {
                        let procs_file = cg_path_clone.join("cgroup.procs");
                        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&procs_file) {
                            let _ = writeln!(f, "{}", std::process::id());
                        }
                        Ok(())
                    });
                }
            }
        }

        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {}", e))?;
        let pid = child.id() as i32;

        let session = PtySession {
            runtime_id: runtime_id.to_string(),
            pty_id: pty_id.to_string(),
            pid,
            cols,
            rows,
            shell: shell_path.clone(),
            master_fd: -1,
        };

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        if let Some(mut stdin_handle) = stdin {
            thread::spawn(move || {
                for data in rx {
                    let _ = stdin_handle.write_all(&data);
                    let _ = stdin_handle.flush();
                }
            });
        }

        if let Some(mut out) = stdout {
            thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match out.read(&mut buf) {
                        Ok(0) => break,
                        Ok(_) => {},
                        Err(_) => break,
                    }
                }
            });
        }
        if let Some(mut err) = stderr {
            thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match err.read(&mut buf) {
                        Ok(0) => break,
                        Ok(_) => {},
                        Err(_) => break,
                    }
                }
            });
        }

        thread::spawn(move || {
            let _ = child.wait();
        });

        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.entry(runtime_id.to_string()).or_default().insert(pty_id.to_string(), session.clone());
        }
        {
            let mut inners = self.inners.lock().unwrap();
            let key = format!("{}:{}", runtime_id, pty_id);
            inners.insert(key, PtyInner {
                child_pid: pid,
                writer_tx: tx,
            });
        }

        Ok(session)
    }

    pub fn write_pty(&self, runtime_id: &str, pty_id: &str, data: Vec<u8>) -> Result<(), String> {
        let key = format!("{}:{}", runtime_id, pty_id);
        let inners = self.inners.lock().unwrap();
        if let Some(inner) = inners.get(&key) {
            inner.writer_tx.send(data).map_err(|e| format!("send failed: {}", e))?;
            Ok(())
        } else {
            Err(format!("pty {} not found in runtime {}", pty_id, runtime_id))
        }
    }

    pub fn resize_pty(&self, runtime_id: &str, pty_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(map) = sessions.get_mut(runtime_id) {
            if let Some(sess) = map.get_mut(pty_id) {
                sess.cols = cols;
                sess.rows = rows;
                Ok(())
            } else {
                Err(format!("pty {} not found", pty_id))
            }
        } else {
            Err(format!("runtime {} not found", runtime_id))
        }
    }

    pub fn signal_pty(&self, runtime_id: &str, pty_id: &str, signal: i32) -> Result<(), String> {
        let sessions = self.sessions.lock().unwrap();
        if let Some(map) = sessions.get(runtime_id) {
            if let Some(sess) = map.get(pty_id) {
                unsafe {
                    extern "C" {
                        fn kill(pid: i32, sig: i32) -> i32;
                    }
                    kill(sess.pid, signal);
                }
                Ok(())
            } else {
                Err(format!("pty {} not found", pty_id))
            }
        } else {
            Err(format!("runtime {} not found", runtime_id))
        }
    }

    pub fn close_pty(&self, runtime_id: &str, pty_id: &str) -> Result<(), String> {
        let key = format!("{}:{}", runtime_id, pty_id);
        let mut inners = self.inners.lock().unwrap();
        if let Some(inner) = inners.remove(&key) {
            drop(inner.writer_tx);
            unsafe {
                extern "C" {
                    fn kill(pid: i32, sig: i32) -> i32;
                }
                kill(inner.child_pid, 9);
            }
        }
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(map) = sessions.get_mut(runtime_id) {
            map.remove(pty_id);
        }
        Ok(())
    }

    pub fn destroy_for_runtime(&self, runtime_id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(map) = sessions.remove(runtime_id) {
            let mut inners = self.inners.lock().unwrap();
            for (pty_id, sess) in map {
                let key = format!("{}:{}", runtime_id, pty_id);
                if let Some(inner) = inners.remove(&key) {
                    drop(inner.writer_tx);
                    unsafe {
                        extern "C" {
                            fn kill(pid: i32, sig: i32) -> i32;
                        }
                        kill(inner.child_pid, 9);
                        kill(sess.pid, 9);
                    }
                }
            }
        }
    }
}
