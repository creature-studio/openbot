use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Sender};
use std::os::unix::io::FromRawFd;
use std::fs::File;
use std::io::{Read, Write};
use std::thread;

// PTY session exposed to outside
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

// Internal state per PTY
struct PtyInner {
    child_pid: i32,
    master_fd: i32, // original master fd for ioctl/close
    writer_tx: Sender<Vec<u8>>,
    output: Arc<Mutex<Vec<u8>>>, // ring buffer
    exit_code: Arc<Mutex<Option<i32>>>,
}

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
        // Try real PTY first, fallback to pipe if fails
        match self.open_pty_real(runtime_id, pty_id, cols, rows, shell, cgroup_path.clone()) {
            Ok(s) => Ok(s),
            Err(e) => {
                eprintln!("[pty] real PTY failed ({}), falling back to pipe: {}", pty_id, e);
                self.open_pty_pipe(runtime_id, pty_id, cols, rows, shell, cgroup_path)
            }
        }
    }

    // Real PTY via openpty + fork
    fn open_pty_real(&self, runtime_id: &str, pty_id: &str, cols: u16, rows: u16, shell: &str, cgroup_path: Option<std::path::PathBuf>) -> Result<PtySession, String> {
        let shell_path = if shell.is_empty() { "/bin/bash".to_string() } else { shell.to_string() };

        // SAFETY: FFI
        unsafe {
            let mut master_fd: i32 = -1;
            let mut slave_fd: i32 = -1;
            let mut ws = Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };

            let ret = openpty(&mut master_fd, &mut slave_fd, std::ptr::null_mut(), std::ptr::null(), &mut ws);
            if ret != 0 {
                return Err(format!("openpty failed: {}", std::io::Error::last_os_error()));
            }

            let pid = fork();
            if pid < 0 {
                close(master_fd);
                close(slave_fd);
                return Err(format!("fork failed: {}", std::io::Error::last_os_error()));
            }

            if pid == 0 {
                // Child
                // Setsid, make slave controlling terminal
                setsid();
                // ioctl slave TIOCSCTTY
                let _ = ioctl(slave_fd, TIOCSCTTY as u64, 0);

                // dup2 slave to stdin/out/err
                dup2(slave_fd, 0);
                dup2(slave_fd, 1);
                dup2(slave_fd, 2);

                // close master and slave (slave now dup'd)
                if master_fd > 2 { close(master_fd); }
                if slave_fd > 2 { close(slave_fd); }

                // Join cgroup if provided
                if let Some(cg_path) = cgroup_path {
                    let procs_file = cg_path.join("cgroup.procs");
                    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&procs_file) {
                        let _ = writeln!(f, "{}", std::process::id());
                    }
                }

                // Set env TERM
                std::env::set_var("TERM", "xterm-256color");

                // Exec shell
                // Use execvp: need c strings
                let shell_c = std::ffi::CString::new(shell_path.clone()).unwrap();
                let arg0_c = std::ffi::CString::new(shell_path.clone()).unwrap();
                let arg1_c = std::ffi::CString::new("-l").unwrap();
                let args: [*const i8; 3] = [arg0_c.as_ptr(), arg1_c.as_ptr(), std::ptr::null()];
                execvp(shell_c.as_ptr(), args.as_ptr());
                // If exec fails, exit
                _exit(1);
            }

            // Parent
            close(slave_fd);

            // Add pid to cgroup (parent side also, in case child didn't)
            if let Some(cg_path) = cgroup_path {
                let procs_file = cg_path.join("cgroup.procs");
                if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&procs_file) {
                    let _ = writeln!(f, "{}", pid);
                } else {
                    // try sudo
                    let cmd = format!("echo {} > {}", pid, procs_file.display());
                    let _ = std::process::Command::new("sudo").args(["sh", "-c", &cmd]).output();
                }
            }

            // Prepare output buffer and exit code
            let output = Arc::new(Mutex::new(Vec::<u8>::new()));
            let exit_code = Arc::new(Mutex::new(None::<i32>));
            let output_clone = output.clone();
            let exit_clone = exit_code.clone();

            // Dup master fd for reader and writer to avoid double close
            let master_for_reader = dup(master_fd);
            let master_for_writer = dup(master_fd);
            if master_for_reader < 0 || master_for_writer < 0 {
                close(master_fd);
                if master_for_reader >=0 { close(master_for_reader); }
                if master_for_writer >=0 { close(master_for_writer); }
                kill(pid, 9);
                return Err(format!("dup failed: {}", std::io::Error::last_os_error()));
            }

            // Reader thread: reads from PTY master and appends to output buffer
            thread::spawn(move || {
                let mut file = File::from_raw_fd(master_for_reader);
                let mut buf = [0u8; 4096];
                loop {
                    match file.read(&mut buf) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            let mut out = output_clone.lock().unwrap();
                            // ring buffer: keep last 1MB
                            out.extend_from_slice(&buf[..n]);
                            if out.len() > 1024*1024 {
                                let drain = out.len() - 1024*1024;
                                out.drain(0..drain);
                            }
                        }
                        Err(_) => break,
                    }
                }
            });

            // Writer thread: receives data and writes to PTY master
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            thread::spawn(move || {
                let mut file = File::from_raw_fd(master_for_writer);
                for data in rx {
                    let _ = file.write_all(&data);
                    let _ = file.flush();
                }
            });

            // Waiter thread: waitpid and set exit code
            let exit_wait = exit_clone.clone();
            thread::spawn(move || {
                let mut status: i32 = 0;
                unsafe { waitpid(pid, &mut status as *mut i32, 0); }
                let code = if libc_wifexited(status) {
                    libc_wexitstatus(status)
                } else if libc_wifsignaled(status) {
                    - (libc_wtermsig(status) as i32)
                } else {
                    -1
                };
                *exit_wait.lock().unwrap() = Some(code);
            });

            let session = PtySession {
                runtime_id: runtime_id.to_string(),
                pty_id: pty_id.to_string(),
                pid,
                cols,
                rows,
                shell: shell_path,
                master_fd,
            };

            {
                let mut sessions = self.sessions.lock().unwrap();
                sessions.entry(runtime_id.to_string()).or_default().insert(pty_id.to_string(), session.clone());
            }
            {
                let mut inners = self.inners.lock().unwrap();
                let key = format!("{}:{}", runtime_id, pty_id);
                inners.insert(key, PtyInner {
                    child_pid: pid,
                    master_fd,
                    writer_tx: tx,
                    output,
                    exit_code,
                });
            }

            Ok(session)
        }
    }

    // Pipe fallback (previous implementation) for environments where openpty fails
    fn open_pty_pipe(&self, runtime_id: &str, pty_id: &str, cols: u16, rows: u16, shell: &str, cgroup_path: Option<std::path::PathBuf>) -> Result<PtySession, String> {
        use std::process::{Command, Stdio};
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

        let output = Arc::new(Mutex::new(Vec::<u8>::new()));
        let exit_code = Arc::new(Mutex::new(None::<i32>));
        let output_clone = output.clone();

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
            let out_clone = output_clone.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match out.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut o = out_clone.lock().unwrap();
                            o.extend_from_slice(&buf[..n]);
                            if o.len() > 1024*1024 { let d = o.len()-1024*1024; o.drain(0..d); }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        if let Some(mut err) = stderr {
            let out_clone = output_clone.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match err.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut o = out_clone.lock().unwrap();
                            o.extend_from_slice(&buf[..n]);
                            if o.len() > 1024*1024 { let d = o.len()-1024*1024; o.drain(0..d); }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        let exit_clone = exit_code.clone();
        thread::spawn(move || {
            let status = child.wait();
            let code = match status {
                Ok(s) => s.code().unwrap_or(-1),
                Err(_) => -1,
            };
            *exit_clone.lock().unwrap() = Some(code);
        });

        let session = PtySession {
            runtime_id: runtime_id.to_string(),
            pty_id: pty_id.to_string(),
            pid,
            cols,
            rows,
            shell: shell_path,
            master_fd: -1,
        };
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.entry(runtime_id.to_string()).or_default().insert(pty_id.to_string(), session.clone());
        }
        {
            let mut inners = self.inners.lock().unwrap();
            let key = format!("{}:{}", runtime_id, pty_id);
            inners.insert(key, PtyInner {
                child_pid: pid,
                master_fd: -1,
                writer_tx: tx,
                output,
                exit_code,
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

    pub fn read_pty(&self, runtime_id: &str, pty_id: &str, clear: bool) -> Result<Vec<u8>, String> {
        let key = format!("{}:{}", runtime_id, pty_id);
        let inners = self.inners.lock().unwrap();
        if let Some(inner) = inners.get(&key) {
            let mut out = inner.output.lock().unwrap();
            let data = out.clone();
            if clear {
                out.clear();
            }
            Ok(data)
        } else {
            Err(format!("pty {} not found", pty_id))
        }
    }

    pub fn resize_pty(&self, runtime_id: &str, pty_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let key = format!("{}:{}", runtime_id, pty_id);
        let mut sessions = self.sessions.lock().unwrap();
        let master_fd = {
            if let Some(map) = sessions.get_mut(runtime_id) {
                if let Some(sess) = map.get_mut(pty_id) {
                    sess.cols = cols;
                    sess.rows = rows;
                    sess.master_fd
                } else {
                    return Err(format!("pty {} not found", pty_id));
                }
            } else {
                return Err(format!("runtime {} not found", runtime_id));
            }
        };
        if master_fd >= 0 {
            unsafe {
                let mut ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
                let ret = ioctl(master_fd, TIOCSWINSZ as u64, &mut ws as *mut Winsize as *mut std::os::raw::c_void);
                if ret != 0 {
                    return Err(format!("ioctl TIOCSWINSZ failed: {}", std::io::Error::last_os_error()));
                }
            }
        }
        // Also need to send SIGWINCH to child? Usually kernel does, but we can also kill -WINCH
        let inners = self.inners.lock().unwrap();
        if let Some(inner) = inners.get(&key) {
            unsafe { kill(inner.child_pid, 28); } // SIGWINCH 28
        }
        Ok(())
    }

    pub fn signal_pty(&self, runtime_id: &str, pty_id: &str, signal: i32) -> Result<(), String> {
        let key = format!("{}:{}", runtime_id, pty_id);
        let inners = self.inners.lock().unwrap();
        if let Some(inner) = inners.get(&key) {
            // Try to get foreground process group via tcgetpgrp on master fd, then kill(-pgid)
            // This ensures Ctrl+C goes to foreground group (bash -> npm -> node) not just shell pid
            unsafe {
                if inner.master_fd >= 0 {
                    let pgid = tcgetpgrp(inner.master_fd);
                    if pgid > 1 {
                        // kill(-pgid, signal) sends to process group
                        let ret = kill(-pgid, signal);
                        if ret == 0 {
                            return Ok(());
                        }
                        eprintln!("[pty] kill(-pgid {}, sig {}) failed: {}, fallback to child pid", pgid, signal, std::io::Error::last_os_error());
                    }
                }
                // Fallback: kill child pid directly
                kill(inner.child_pid, signal);
            }
            Ok(())
        } else {
            let sessions = self.sessions.lock().unwrap();
            if let Some(map) = sessions.get(runtime_id) {
                if let Some(sess) = map.get(pty_id) {
                    // Try pgid via master fd if available
                    unsafe {
                        if sess.master_fd >= 0 {
                            let pgid = tcgetpgrp(sess.master_fd);
                            if pgid > 1 {
                                if kill(-pgid, signal) == 0 {
                                    return Ok(());
                                }
                            }
                        }
                        kill(sess.pid, signal);
                    }
                    return Ok(());
                }
            }
            Err(format!("pty {} not found", pty_id))
        }
    }

    pub fn close_pty(&self, runtime_id: &str, pty_id: &str) -> Result<(), String> {
        let key = format!("{}:{}", runtime_id, pty_id);
        let mut inners = self.inners.lock().unwrap();
        if let Some(inner) = inners.remove(&key) {
            drop(inner.writer_tx);
            unsafe {
                if inner.master_fd >= 0 {
                    close(inner.master_fd);
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
                        if inner.master_fd >= 0 { close(inner.master_fd); }
                        if inner.child_pid > 0 { kill(inner.child_pid, 9); }
                        if sess.master_fd >= 0 && sess.master_fd != inner.master_fd { close(sess.master_fd); }
                        if sess.pid > 0 && sess.pid != inner.child_pid { kill(sess.pid, 9); }
                    }
                } else {
                    unsafe {
                        if sess.master_fd >= 0 { close(sess.master_fd); }
                        if sess.pid > 0 { kill(sess.pid, 9); }
                    }
                }
            }
        }
    }

    pub fn get_exit_code(&self, runtime_id: &str, pty_id: &str) -> Option<i32> {
        let key = format!("{}:{}", runtime_id, pty_id);
        let inners = self.inners.lock().unwrap();
        inners.get(&key).and_then(|inner| *inner.exit_code.lock().unwrap())
    }
}

// FFI definitions
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

const TIOCSCTTY: i32 = 0x540E;
const TIOCSWINSZ: i32 = 0x5414;

#[link(name = "util")]
extern "C" {
    fn openpty(amaster: *mut i32, aslave: *mut i32, name: *mut i8, termp: *const std::os::raw::c_void, winp: *mut Winsize) -> i32;
}

extern "C" {
    fn fork() -> i32;
    fn setsid() -> i32;
    fn close(fd: i32) -> i32;
    fn dup(fd: i32) -> i32;
    fn dup2(oldfd: i32, newfd: i32) -> i32;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn execvp(file: *const i8, argv: *const *const i8) -> i32;
    fn _exit(status: i32) -> !;
    fn kill(pid: i32, sig: i32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn tcgetpgrp(fd: i32) -> i32;
}

fn libc_wifexited(status: i32) -> bool {
    (status & 0x7f) == 0
}
fn libc_wexitstatus(status: i32) -> i32 {
    (status >> 8) & 0xff
}
fn libc_wifsignaled(status: i32) -> bool {
    let sig = status & 0x7f;
    sig != 0 && sig != 0x7f
}
fn libc_wtermsig(status: i32) -> i32 {
    status & 0x7f
}
