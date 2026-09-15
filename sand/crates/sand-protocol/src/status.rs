//! Machine introspection shared by sandd (`Status`, `Handshake`), the sandd
//! CLI and the `sand bridge` handshake.
//!
//! Everything here is derived from `/proc` and `uname(2)` — **no subprocesses**,
//! because `MachineManager` health checks and the bootstrap handshake both need
//! to be cheap enough to run on every connect.

use std::path::Path;

use crate::{host_machine_id, local_host_machine_id, MachineId};

/// Static view of the machine sandd is running on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineInfo {
    /// Stable id derived from `/etc/machine-id` (or hostname as fallback).
    pub machine_id: String,
    /// e.g. `Ubuntu 24.04`, `Debian GNU/Linux 12`, `Linux 6.1.0` as a fallback.
    pub os: String,
    /// e.g. `x86_64`, `aarch64`.
    pub arch: String,
    /// e.g. `devbox-01`.
    pub hostname: String,
    /// Kernel release, e.g. `6.5.0-15-generic`.
    pub kernel: String,
    pub cpu_cores: u64,
    pub memory_total: u64,
    pub uptime_seconds: u64,
    /// GPU description when one can be detected without spawning a process.
    pub gpu: Option<String>,
}

impl MachineInfo {
    /// Build a compact `"key":value,...` JSON fragment (no leading comma).
    /// Callers append it to their own JSON object.
    pub fn to_json_fragment(&self) -> String {
        let mut parts = vec![
            format!("\"machine_id\":\"{}\"", crate::frame::json::escape(&self.machine_id)),
            format!("\"os\":\"{}\"", crate::frame::json::escape(&self.os)),
            format!("\"arch\":\"{}\"", crate::frame::json::escape(&self.arch)),
            format!("\"hostname\":\"{}\"", crate::frame::json::escape(&self.hostname)),
            format!("\"kernel\":\"{}\"", crate::frame::json::escape(&self.kernel)),
            format!("\"cpu_cores\":{}", self.cpu_cores),
            format!("\"memory_total\":{}", self.memory_total),
            format!("\"uptime_seconds\":{}", self.uptime_seconds),
        ];
        match &self.gpu {
            Some(gpu) => parts.push(format!("\"gpu\":\"{}\"", crate::frame::json::escape(gpu))),
            None => parts.push("\"gpu\":null".to_string()),
        }
        parts.join(",")
    }
}

/// Collect machine info from `/proc` + `uname`. Always succeeds.
pub fn machine_info() -> MachineInfo {
    MachineInfo {
        machine_id: local_host_machine_id().0,
        os: os_pretty_name(),
        arch: std::env::consts::ARCH.to_string(),
        hostname: hostname(),
        kernel: kernel_release(),
        cpu_cores: cpu_cores(),
        memory_total: memory_total(),
        uptime_seconds: uptime_seconds(),
        gpu: gpu_description(),
    }
}

/// `machine_info()` rendered as a JSON fragment, for callers that only need
/// the string.
pub fn machine_info_json() -> String {
    format!(
        "\"machine_id\":\"{}\",\"platform\":{}",
        crate::frame::json::escape(&machine_info().machine_id),
        machine_platform_json()
    )
}

/// Platform subset of [`MachineInfo`] suitable for the sandd compat `Status`
/// RPC (kept small: `Status` is polled).
pub fn machine_platform_json() -> String {
    let info = machine_info();
    info.to_json_fragment()
}

/// The machine id of the host running this function.
pub fn current_machine_id() -> MachineId {
    local_host_machine_id()
}

fn os_pretty_name() -> String {
    if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
        for key in ["PRETTY_NAME=", "NAME="] {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix(key) {
                    let value = rest.trim().trim_matches('"').to_string();
                    if !value.is_empty() {
                        return value;
                    }
                }
            }
        }
    }
    let kernel = kernel_release();
    if kernel.is_empty() {
        "Linux".to_string()
    } else {
        format!("Linux {}", kernel)
    }
}

fn hostname() -> String {
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            let value = content.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string())
}

fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn cpu_cores() -> u64 {
    // /proc/cpuinfo lists one block per logical CPU.
    if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
        let count = content
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count();
        if count > 0 {
            return count as u64;
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get() as u64)
        .unwrap_or(0)
}

fn memory_total() -> u64 {
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                return kb * 1024;
            }
        }
    }
    0
}

fn uptime_seconds() -> u64 {
    if let Ok(lib) = std::fs::read_to_string("/proc/uptime") {
        if let Some(first) = lib.split_whitespace().next() {
            if let Ok(secs) = first.parse::<f64>() {
                return secs as u64;
            }
        }
    }
    // `linux` fallback: boot time from /proc/stat
    0
}

/// Detect a GPU cheaply: NVIDIA's procfs node, otherwise DRM vendor ids.
fn gpu_description() -> Option<String> {
    if let Ok(content) = std::fs::read_to_string("/proc/driver/nvidia/version") {
        if let Some(line) = content.lines().next() {
            let name = line
                .split("  ")
                .next()
                .unwrap_or(line)
                .trim()
                .to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }

    let drm = Path::new("/sys/class/drm");
    if let Ok(entries) = std::fs::read_dir(drm) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let device = entry.path().join("device/vendor");
            let vendor = std::fs::read_to_string(&device)
                .map(|s| s.trim().to_lowercase())
                .unwrap_or_default();
            let label = match vendor.as_str() {
                "0x10de" => "NVIDIA GPU",
                "0x1002" | "0x1022" => "AMD GPU",
                "0x8086" => "Intel GPU",
                _ => continue,
            };
            return Some(label.to_string());
        }
    }
    None
}

/// Seed used for a machine id when sandd runs somewhere without
/// `/etc/machine-id`; exposed for tests and for the bridge handshake.
pub fn machine_id_from_seed(seed: &str) -> MachineId {
    host_machine_id(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_info_is_populated() {
        let info = machine_info();
        assert!(!info.machine_id.is_empty());
        assert!(!info.arch.is_empty());
        assert!(!info.os.is_empty());
        // JSON fragment must be well formed enough for the naive parsers.
        let json = info.to_json_fragment();
        assert!(json.contains("\"machine_id\":\""));
        assert!(json.contains("\"cpu_cores\":"));
    }

    #[test]
    fn machine_id_is_stable_and_host_scoped() {
        let a = machine_id_from_seed("devbox");
        let b = machine_id_from_seed("devbox");
        let c = machine_id_from_seed("otherbox");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.as_str().starts_with("mach-"));
    }
}
