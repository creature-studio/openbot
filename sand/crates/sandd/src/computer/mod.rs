//! Computer use — the machine's own desktop, not the client's.
//!
//! ```text
//! screenshot / mouse / keyboard / scroll
//!        ↓
//!   remote sandd            (this module)
//!        ↓
//!   Xvfb DISPLAY of the runtime  →  injected with xdotool/xte
//! ```
//!
//! When the machine has no display server the runtime refuses the call and the
//! handshake does not advertise `desktop`/`computer`, which is what makes the
//! client hide the Computer panel instead of showing a broken button
//! (architecture §二十五).

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use crate::capabilities;
use crate::desktop::DesktopManager;

pub struct ComputerManager {
    desktop: Arc<DesktopManager>,
}

impl ComputerManager {
    pub fn new(desktop: Arc<DesktopManager>) -> Self {
        Self { desktop }
    }

    /// Is computer use possible on this machine at all?
    pub fn available(&self) -> bool {
        capabilities::computer_use_available()
    }

    /// Screenshot of the runtime's desktop.
    pub fn screenshot(&self, runtime_id: &str) -> Result<Vec<u8>, String> {
        self.desktop.screenshot(runtime_id)
    }

    pub fn dispatch(
        &self,
        runtime_id: &str,
        action: &str,
        params: &Params,
    ) -> Result<String, String> {
        // A screenshot works on any X server (no injection tool required).
        if action == "screenshot" {
            let bytes = self.screenshot(runtime_id)?;
            return Ok(format!("{{\"ok\":true,\"len\":{}}}", bytes.len()));
        }

        if !self.available() {
            return Err(
                "computer use is not available on this machine (needs Xvfb + xdotool)".to_string(),
            );
        }

        let display = self
            .desktop
            .ensure_display(runtime_id, 1280, 800)?;

        match action {
            "click" => self.tool(&display, &["mousemove", &params.x(), &params.y(), "click", &button(params)]),
            "move" => self.tool(&display, &["mousemove", &params.x(), &params.y()]),
            "type" => {
                let text = params.string("text");
                self.tool(&display, &["type", "--clearmodifiers", &text])
            }
            "key" => {
                let key = params.string("key");
                let normalized = normalize_key(&key);
                self.tool(&display, &["key", "--clearmodifiers", &normalized])
            }
            "scroll" => {
                // xdotool button 4 == wheel up, 5 == wheel down.
                let delta: i64 = params.number("delta").unwrap_or(0);
                let button = if delta >= 0 { "4" } else { "5" };
                let steps = (delta.unsigned_abs() / 100).max(1);
                for _ in 0..steps {
                    self.tool(&display, &["click", button])?;
                }
                Ok(format!(
                    "{{\"ok\":true,\"display\":\"{}\",\"action\":\"scroll\",\"steps\":{}}}",
                    crate::json::escape(&display),
                    steps
                ))
            }
            other => Err(format!("unknown computer action: {}", other)),
        }
    }

    /// Run an `xdotool` subcommand against the runtime display.
    fn tool(&self, display: &str, args: &[&str]) -> Result<String, String> {
        let xdotool = capabilities::which("xdotool")
            .ok_or_else(|| "xdotool not installed on this machine".to_string())?;
        let output = Command::new(xdotool)
            .env("DISPLAY", display)
            .args(args)
            .output()
            .map_err(|e| format!("xdotool failed: {}", e))?;
        if !output.status.success() {
            return Err(format!(
                "xdotool {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(format!(
            "{{\"ok\":true,\"display\":\"{}\",\"action\":\"{}\",\"stderr\":\"{}\"}}",
            crate::json::escape(display),
            crate::json::escape(args.first().copied().unwrap_or("")),
            crate::json::escape(String::from_utf8_lossy(&output.stderr).trim())
        ))
    }
}

/// Mouse button name → xdotool button number.
fn button(params: &Params) -> String {
    match params.string("button").to_lowercase().as_str() {
        "middle" => "2".to_string(),
        "right" => "3".to_string(),
        _ => "1".to_string(),
    }
}

/// Small subset of X keysym spellings the model tends to produce.
fn normalize_key(key: &str) -> String {
    match key.to_lowercase().replace(' ', "").as_str() {
        "enter" | "return" => "Return".to_string(),
        "esc" | "escape" => "Escape".to_string(),
        "tab" => "Tab".to_string(),
        "backspace" => "BackSpace".to_string(),
        "delete" | "del" => "Delete".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "ctrl+c" | "control+c" => "ctrl+c".to_string(),
        other => other.to_string(),
    }
}

/// Tiny typed view over a request's scalar fields.
pub struct Params {
    json: String,
}

impl Params {
    pub fn new(json: &str) -> Self {
        Self {
            json: json.to_string(),
        }
    }

    pub fn number(&self, field: &str) -> Option<i64> {
        crate::json::get_u64(&self.json, field).map(|v| v as i64)
    }

    pub fn string(&self, field: &str) -> String {
        crate::json::get_str(&self.json, field).unwrap_or_default()
    }

    pub fn x(&self) -> String {
        self.number("x").unwrap_or(0).to_string()
    }

    pub fn y(&self) -> String {
        self.number("y").unwrap_or(0).to_string()
    }
}

/// True when a display socket exists for `display` (":" + number).
pub fn display_socket_present(display: &str) -> bool {
    let number = display.trim_start_matches(':');
    Path::new(&format!("/tmp/.X11-unix/X{}", number)).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_keys() {
        assert_eq!(normalize_key("enter"), "Return");
        assert_eq!(normalize_key("ESC"), "Escape");
        assert_eq!(normalize_key("ArrowUp"), "arrowup");
    }

    #[test]
    fn reads_params() {
        let params = Params::new("{\"x\":100,\"y\":250,\"text\":\"hi\",\"button\":\"right\"}");
        assert_eq!(params.x(), "100");
        assert_eq!(params.y(), "250");
        assert_eq!(params.string("text"), "hi");
        assert_eq!(button(&params), "3");
    }

    #[test]
    fn refuses_without_display_support() {
        let desktop = Arc::new(DesktopManager::new());
        let computer = ComputerManager::new(desktop);
        if !computer.available() {
            let err = computer
                .dispatch("rt-x", "click", &Params::new("{\"x\":1,\"y\":1}"))
                .expect_err("must refuse");
            assert!(err.contains("not available"), "got {err}");
        }
    }
}
