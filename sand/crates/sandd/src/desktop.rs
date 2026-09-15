use std::path::PathBuf;
use std::process::{Command, Child};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

pub struct DesktopManager {
    displays: Arc<Mutex<HashMap<String, DesktopSession>>>,
}

struct DesktopSession {
    display: String,
    xvfb: Option<Child>,
    width: u32,
    height: u32,
}

impl DesktopManager {
    pub fn new() -> Self {
        Self {
            displays: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn ensure_display(&self, runtime_id: &str, width: u32, height: u32) -> Result<String, String> {
        let mut displays = self.displays.lock().map_err(|e| format!("lock failed: {}", e))?;
        if let Some(sess) = displays.get(runtime_id) {
            return Ok(sess.display.clone());
        }
        // Allocate display number based on hash of runtime_id
        let hash = runtime_id.bytes().fold(0u32, |acc, b| acc.wrapping_add(b as u32));
        let display_num = 10 + (hash % 90); // 10-99
        let display = format!(":{}", display_num);
        let width = if width == 0 { 1280 } else { width };
        let height = if height == 0 { 720 } else { height };

        // Try to start Xvfb if available
        let xvfb_child = if which("Xvfb").is_some() {
            // Check if display already in use
            let sock_path = format!("/tmp/.X11-unix/X{}", display_num);
            if std::path::Path::new(&sock_path).exists() {
                eprintln!("[desktop] display {} already in use, reusing", display);
                None
            } else {
                eprintln!("[desktop] starting Xvfb {} {}x{} for runtime {}", display, width, height, runtime_id);
                let child = Command::new("Xvfb")
                    .args([&display, "-screen", "0", &format!("{}x{}x24", width, height), "-ac", "-nolisten", "tcp"])
                    .spawn();
                match child {
                    Ok(c) => {
                        // Wait a bit for Xvfb to start
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        Some(c)
                    }
                    Err(e) => {
                        eprintln!("[desktop] Xvfb start failed: {}", e);
                        None
                    }
                }
            }
        } else {
            eprintln!("[desktop] Xvfb not found, display {} may not work", display);
            None
        };

        let sess = DesktopSession {
            display: display.clone(),
            xvfb: xvfb_child,
            width,
            height,
        };
        displays.insert(runtime_id.to_string(), sess);
        Ok(display)
    }

    pub fn get_display(&self, runtime_id: &str) -> Option<String> {
        self.displays.lock().ok()?.get(runtime_id).map(|s| s.display.clone())
    }

    pub fn screenshot(&self, runtime_id: &str) -> Result<Vec<u8>, String> {
        let display = self.get_display(runtime_id).ok_or_else(|| format!("no display for runtime {}", runtime_id))?;
        // Try to capture via import or scrot or xwd
        let candidates = [
            format!("DISPLAY={} import -window root png:- 2>/dev/null", display),
            format!("DISPLAY={} scrot -z -o - 2>/dev/null", display),
            format!("DISPLAY={} xwd -root -silent 2>/dev/null | convert xwd:- png:- 2>/dev/null", display),
        ];
        for cmd in &candidates {
            if let Ok(out) = Command::new("sh").args(["-c", cmd]).output() {
                if !out.stdout.is_empty() && out.stdout.len() > 100 {
                    return Ok(out.stdout);
                }
            }
        }
        Err("screenshot failed, no tool or no display".to_string())
    }

    pub fn destroy(&self, runtime_id: &str) {
        if let Ok(mut displays) = self.displays.lock() {
            if let Some(mut sess) = displays.remove(runtime_id) {
                if let Some(mut child) = sess.xvfb.take() {
                    let _ = child.kill();
                }
            }
        }
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let p = PathBuf::from(dir).join(cmd);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}
