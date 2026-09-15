use std::path::{Path, PathBuf};
use std::fs;
use std::io::Write;

#[derive(Debug)]
pub struct CgroupManager {
    root: PathBuf,
    fallback_root: PathBuf,
    use_fallback: bool,
}

impl CgroupManager {
    pub fn new() -> Self {
        let root = PathBuf::from("/sys/fs/cgroup/sand");
        let fallback_root = PathBuf::from("/tmp/sand-cgroups");
        let mut use_fallback = false;

        // Ensure root exists
        if !root.exists() {
            let out = std::process::Command::new("sudo")
                .args(["mkdir", "-p", "/sys/fs/cgroup/sand"])
                .output();
            if out.map(|o| o.status.success()).unwrap_or(false) {
                // root now exists
            } else {
                use_fallback = true;
            }
        }

        // Ensure fallback exists
        let _ = fs::create_dir_all(&fallback_root);

        if !use_fallback {
            let _ = fs::create_dir_all(&root);
            // Try to enable controllers in subtree_control
            Self::ensure_subtree_controllers(&root);
        }

        // Probe if we can actually create subcgroups without sudo
        // Try to create a test dir and remove
        if !use_fallback {
            let test_path = root.join("__probe__");
            let probe = fs::create_dir_all(&test_path);
            if probe.is_ok() {
                let _ = fs::remove_dir(&test_path);
            } else {
                // try sudo mkdir probe
                let out = std::process::Command::new("sudo")
                    .args(["mkdir", "-p", test_path.to_str().unwrap()])
                    .output();
                if out.map(|o| o.status.success()).unwrap_or(false) {
                    let _ = std::process::Command::new("sudo")
                        .args(["rmdir", test_path.to_str().unwrap()])
                        .output();
                    // we need sudo for operations, but we can still use real root
                    // use_fallback remains false, but operations will use sudo
                } else {
                    use_fallback = true;
                }
            }
        }

        Self {
            root,
            fallback_root,
            use_fallback,
        }
    }

    fn ensure_subtree_controllers(root: &Path) {
        let subtree_file = root.join("cgroup.subtree_control");
        // Read current
        let current = fs::read_to_string(&subtree_file).unwrap_or_default();
        // If controllers missing, try to enable
        let needed = ["cpu", "memory", "pids", "io"];
        let mut to_enable = Vec::new();
        for ctrl in needed {
            if !current.contains(ctrl) {
                to_enable.push(format!("+{}", ctrl));
            }
        }
        if to_enable.is_empty() {
            return;
        }
        let enable_str = to_enable.join(" ");
        // Try direct write
        let direct = fs::OpenOptions::new().write(true).open(&subtree_file)
            .and_then(|mut f| writeln!(f, "{}", enable_str));
        if direct.is_ok() {
            return;
        }
        // Try sudo
        let cmd = format!("echo '{}' > {}", enable_str, subtree_file.display());
        let _ = std::process::Command::new("sudo")
            .args(["sh", "-c", &cmd])
            .output();
    }

    pub fn runtime_cgroup_path(&self, runtime_id: &str) -> PathBuf {
        if self.use_fallback {
            self.fallback_root.join(format!("runtime-{}", runtime_id))
        } else {
            self.root.join(format!("runtime-{}", runtime_id))
        }
    }

    pub fn create_cgroup(&self, runtime_id: &str) -> std::io::Result<PathBuf> {
        let path = self.runtime_cgroup_path(runtime_id);
        let created = fs::create_dir_all(&path);
        if created.is_ok() {
            // Ensure controllers enabled for this cgroup's children if needed
            // (not strictly necessary, but ensure parent has controllers)
            Self::ensure_subtree_controllers(&self.root);
            return Ok(path);
        }
        // Try sudo
        let out = std::process::Command::new("sudo")
            .args(["mkdir", "-p", path.to_str().unwrap()])
            .output();
        if out.map(|o| o.status.success()).unwrap_or(false) {
            // Ensure permissions for reading
            let _ = std::process::Command::new("sudo")
                .args(["chmod", "755", path.to_str().unwrap()])
                .output();
            Self::ensure_subtree_controllers(&self.root);
            Ok(path)
        } else {
            // Fallback
            let fb = self.fallback_root.join(format!("runtime-{}", runtime_id));
            fs::create_dir_all(&fb)?;
            Ok(fb)
        }
    }

    pub fn destroy_cgroup(&self, runtime_id: &str) -> std::io::Result<()> {
        let path = self.runtime_cgroup_path(runtime_id);
        // Kill all first
        let _ = self.kill_cgroup(runtime_id);
        std::thread::sleep(std::time::Duration::from_millis(200));

        if path.exists() {
            // cgroup must be removed via rmdir, not rm -rf (interface files cannot be unlinked)
            let _ = fs::remove_dir(&path);
            if path.exists() {
                let out = std::process::Command::new("sudo")
                    .args(["rmdir", path.to_str().unwrap()])
                    .output();
                if !out.map(|o| o.status.success()).unwrap_or(false) {
                    // Retry kill then rmdir
                    let _ = self.kill_cgroup(runtime_id);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    let _ = std::process::Command::new("sudo")
                        .args(["rmdir", path.to_str().unwrap()])
                        .output();
                    // Final fallback: try rm -rf then rmdir (some systems)
                    if path.exists() {
                        let cmd_str = format!("rmdir {} 2>/dev/null; rmdir {} 2>/dev/null; true", path.to_str().unwrap(), path.to_str().unwrap());
                        let _ = std::process::Command::new("sudo")
                            .args(["sh", "-c", &cmd_str])
                            .output();
                    }
                }
            }
        }

        // Also clean fallback
        let fb = self.fallback_root.join(format!("runtime-{}", runtime_id));
        if fb.exists() {
            let _ = fs::remove_dir_all(&fb);
            if fb.exists() {
                let _ = std::process::Command::new("sudo")
                    .args(["rm", "-rf", fb.to_str().unwrap()])
                    .output();
            }
        }

        Ok(())
    }

    pub fn kill_cgroup(&self, runtime_id: &str) -> std::io::Result<()> {
        let path = self.runtime_cgroup_path(runtime_id);
        // Try cgroup.kill
        let kill_file = path.join("cgroup.kill");
        if kill_file.exists() {
            // Try direct write
            let direct = fs::OpenOptions::new().write(true).open(&kill_file)
                .and_then(|mut f| writeln!(f, "1"));
            if direct.is_err() {
                let cmd = format!("echo 1 > {}", kill_file.display());
                let _ = std::process::Command::new("sudo")
                    .args(["sh", "-c", &cmd])
                    .output();
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        // Fallback: kill all pids in cgroup.procs
        let procs_file = path.join("cgroup.procs");
        if procs_file.exists() {
            if let Ok(content) = fs::read_to_string(&procs_file) {
                for line in content.lines() {
                    if let Ok(pid) = line.trim().parse::<i32>() {
                        unsafe { libc_kill(pid, 9); }
                    }
                }
            } else {
                // Try sudo cat
                let out = std::process::Command::new("sudo")
                    .args(["cat", procs_file.to_str().unwrap()])
                    .output();
                if let Ok(o) = out {
                    if o.status.success() {
                        let content = String::from_utf8_lossy(&o.stdout);
                        for line in content.lines() {
                            if let Ok(pid) = line.trim().parse::<i32>() {
                                unsafe { libc_kill(pid, 9); }
                            }
                        }
                    }
                }
            }
        }

        // Also try pids in fallback
        let fb_procs = self.fallback_root.join(format!("runtime-{}", runtime_id)).join("pids");
        if fb_procs.exists() {
            if let Ok(content) = fs::read_to_string(&fb_procs) {
                for line in content.lines() {
                    if let Ok(pid) = line.trim().parse::<i32>() {
                        unsafe { libc_kill(pid, 9); }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn add_pid(&self, runtime_id: &str, pid: i32) -> std::io::Result<()> {
        let path = self.runtime_cgroup_path(runtime_id);
        let procs_file = path.join("cgroup.procs");
        // Try direct write
        let direct = fs::OpenOptions::new().write(true).open(&procs_file)
            .and_then(|mut f| writeln!(f, "{}", pid));
        if direct.is_err() {
            let cmd = format!("echo {} > {}", pid, procs_file.display());
            let _ = std::process::Command::new("sudo")
                .args(["sh", "-c", &cmd])
                .output();
        }

        // Also track in fallback pids file
        let fb = self.fallback_root.join(format!("runtime-{}", runtime_id));
        let _ = fs::create_dir_all(&fb);
        let pids_file = fb.join("pids");
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&pids_file) {
            let _ = writeln!(f, "{}", pid);
        }

        Ok(())
    }

    pub fn list_pids(&self, runtime_id: &str) -> Vec<i32> {
        let path = self.runtime_cgroup_path(runtime_id);
        let procs_file = path.join("cgroup.procs");
        let mut pids = Vec::new();
        if let Ok(content) = fs::read_to_string(&procs_file) {
            for line in content.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    pids.push(pid);
                }
            }
        } else {
            // Try sudo cat
            let out = std::process::Command::new("sudo")
                .args(["cat", procs_file.to_str().unwrap()])
                .output();
            if let Ok(o) = out {
                if o.status.success() {
                    let content = String::from_utf8_lossy(&o.stdout);
                    for line in content.lines() {
                        if let Ok(pid) = line.trim().parse::<i32>() {
                            pids.push(pid);
                        }
                    }
                }
            }
        }
        // Also fallback pids file
        let fb_pids = self.fallback_root.join(format!("runtime-{}", runtime_id)).join("pids");
        if let Ok(content) = fs::read_to_string(&fb_pids) {
            for line in content.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    if !pids.contains(&pid) {
                        pids.push(pid);
                    }
                }
            }
        }
        pids.into_iter().filter(|pid| is_pid_alive(*pid)).collect()
    }

    pub fn is_empty(&self, runtime_id: &str) -> bool {
        self.list_pids(runtime_id).is_empty()
    }

    pub fn read_stats(&self, runtime_id: &str) -> CgroupStats {
        let path = self.runtime_cgroup_path(runtime_id);
        let mut stats = CgroupStats::default();

        if let Ok(content) = fs::read_to_string(path.join("cpu.stat")) {
            for line in content.lines() {
                if line.starts_with("usage_usec") {
                    if let Some(v) = line.split_whitespace().nth(1) {
                        stats.cpu_usage_usec = v.parse().unwrap_or(0);
                    }
                }
            }
        } else {
            // sudo cat
            if let Ok(o) = std::process::Command::new("sudo").args(["cat", path.join("cpu.stat").to_str().unwrap()]).output() {
                if o.status.success() {
                    let content = String::from_utf8_lossy(&o.stdout);
                    for line in content.lines() {
                        if line.starts_with("usage_usec") {
                            if let Some(v) = line.split_whitespace().nth(1) {
                                stats.cpu_usage_usec = v.parse().unwrap_or(0);
                            }
                        }
                    }
                }
            }
        }

        if let Ok(content) = fs::read_to_string(path.join("memory.current")) {
            stats.memory_current = content.trim().parse().unwrap_or(0);
        } else if let Ok(o) = std::process::Command::new("sudo").args(["cat", path.join("memory.current").to_str().unwrap()]).output() {
            if o.status.success() {
                stats.memory_current = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
            }
        }

        if let Ok(content) = fs::read_to_string(path.join("memory.peak")) {
            stats.memory_peak = content.trim().parse().unwrap_or(0);
        } else if let Ok(o) = std::process::Command::new("sudo").args(["cat", path.join("memory.peak").to_str().unwrap()]).output() {
            if o.status.success() {
                stats.memory_peak = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
            }
        }

        if let Ok(content) = fs::read_to_string(path.join("pids.current")) {
            stats.pids_current = content.trim().parse().unwrap_or(0);
        } else if let Ok(o) = std::process::Command::new("sudo").args(["cat", path.join("pids.current").to_str().unwrap()]).output() {
            if o.status.success() {
                stats.pids_current = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
            } else {
                stats.pids_current = self.list_pids(runtime_id).len() as u64;
            }
        } else {
            stats.pids_current = self.list_pids(runtime_id).len() as u64;
        }

        if let Ok(content) = fs::read_to_string(path.join("memory.events")) {
            for line in content.lines() {
                if line.starts_with("oom_kill") {
                    if let Some(v) = line.split_whitespace().nth(1) {
                        stats.oom_kills = v.parse().unwrap_or(0);
                    }
                }
            }
        }

        stats
    }
}

#[derive(Debug, Default)]
pub struct CgroupStats {
    pub cpu_usage_usec: u64,
    pub memory_current: u64,
    pub memory_peak: u64,
    pub pids_current: u64,
    pub oom_kills: u64,
}

fn is_pid_alive(pid: i32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

unsafe fn libc_kill(pid: i32, sig: i32) {
    let _ = kill(pid, sig);
}

pub fn kill_pid(pid: i32, sig: i32) {
    unsafe {
        libc_kill(pid, sig);
    }
}
