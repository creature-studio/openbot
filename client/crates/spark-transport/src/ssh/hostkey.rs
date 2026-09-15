//! SSH host key verification.
//!
//! We never pass `StrictHostKeyChecking=no` and never auto-accept a key. The
//! flow is:
//!
//! 1. `ssh` runs with `StrictHostKeyChecking=yes` against the user's
//!    `known_hosts` (the file OpenSSH already trusts);
//! 2. when it fails, the key is inspected with `ssh-keyscan` / the error text is
//!    parsed and turned into a typed [`HostKeyIssue`];
//! 3. the caller (MachineManager) converts that into
//!    `Attention::PermissionRequired` with the fingerprint, and only an explicit
//!    user confirmation calls [`trust_host_key`] — which appends to
//!    `known_hosts`. A *changed* key is never written automatically.
//!
//! This mirrors architecture §十.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

/// What went wrong with the host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyIssue {
    /// Host is not in `known_hosts` yet — needs a first-use confirmation.
    Unknown {
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        /// The key line as it appears in `known_hosts` (used to persist it).
        key_line: String,
    },
    /// A key is present but differs from the recorded one. High risk.
    Changed {
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        key_line: String,
    },
}

impl HostKeyIssue {
    pub fn host(&self) -> &str {
        match self {
            HostKeyIssue::Unknown { host, .. } | HostKeyIssue::Changed { host, .. } => host,
        }
    }

    pub fn port(&self) -> u16 {
        match self {
            HostKeyIssue::Unknown { port, .. } | HostKeyIssue::Changed { port, .. } => *port,
        }
    }

    pub fn fingerprint(&self) -> &str {
        match self {
            HostKeyIssue::Unknown { fingerprint, .. } | HostKeyIssue::Changed { fingerprint, .. } => {
                fingerprint
            }
        }
    }

    pub fn key_type(&self) -> &str {
        match self {
            HostKeyIssue::Unknown { key_type, .. } | HostKeyIssue::Changed { key_type, .. } => key_type,
        }
    }

    pub fn key_line(&self) -> &str {
        match self {
            HostKeyIssue::Unknown { key_line, .. } | HostKeyIssue::Changed { key_line, .. } => key_line,
        }
    }

    /// A changed key must always be the higher risk level, and is never
    /// accepted automatically.
    pub fn is_high_risk(&self) -> bool {
        matches!(self, HostKeyIssue::Changed { .. })
    }

    /// Message shown in the Attention card.
    pub fn attention_message(&self) -> String {
        match self {
            HostKeyIssue::Unknown { host, key_type, fingerprint, .. } => format!(
                "首次连接到 {host}\n\n{key_type} fingerprint:\n{fingerprint}\n\n[取消]  [信任并连接]"
            ),
            HostKeyIssue::Changed { host, key_type, fingerprint, .. } => format!(
                "警告：{host} 的主机密钥已改变\n\n新的 {key_type} fingerprint:\n{fingerprint}\n\n这可能意味着中间人攻击。请先确认这台机器确实被重装过。\n[取消]  [我确认，更新密钥]"
            ),
        }
    }
}

/// Does the raw `ssh` stderr indicate a host key problem?
pub fn classify_ssh_stderr(stderr: &str) -> Option<Severity> {
    let lower = stderr.to_lowercase();
    if lower.contains("remote host identification has changed")
        || lower.contains("host key verification failed") && lower.contains("changed")
    {
        return Some(Severity::Changed);
    }
    if lower.contains("host key verification failed")
        || lower.contains("no matching host key")
        || lower.contains("unknown host key")
    {
        return Some(Severity::Unknown);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Unknown,
    Changed,
}

/// The `known_hosts` file OpenSSH will consult.
pub fn known_hosts_path() -> PathBuf {
    if let Ok(path) = std::env::var("SPARK_KNOWN_HOSTS") {
        return PathBuf::from(path);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".ssh/known_hosts");
    }
    PathBuf::from("/etc/ssh/ssh_known_hosts")
}

/// Fetch the key offered by a host (does not write anything).
pub fn scan_host_key(host: &str, port: u16) -> Result<HostKeyIssue> {
    let output = Command::new("ssh-keyscan")
        .args(["-p", &port.to_string(), "-T", "5", "-t", "ed25519,ecdsa,rsa", "--", host])
        .output()
        .context("running ssh-keyscan (is OpenSSH installed?)")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let key_line = stdout
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "ssh-keyscan returned no key for {host}:{} ({})",
                port,
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?
        .trim()
        .to_string();

    let mut parts = key_line.split_whitespace();
    let _host_part = parts.next();
    let key_type = parts.next().unwrap_or("unknown").to_string();
    let blob = parts.next().unwrap_or_default();

    Ok(HostKeyIssue::Unknown {
        host: host.to_string(),
        port,
        key_type,
        fingerprint: fingerprint_of(blob),
        key_line,
    })
}

/// Build a `Changed` issue from a scan.
pub fn changed_issue(host: &str, port: u16, scan: HostKeyIssue) -> HostKeyIssue {
    match scan {
        HostKeyIssue::Unknown { key_type, fingerprint, key_line, .. } => HostKeyIssue::Changed {
            host: host.to_string(),
            port,
            key_type,
            fingerprint,
            key_line,
        },
        other => other,
    }
}

/// `SHA256:...` fingerprint of a base64 key blob, matching `ssh-keygen -lf`.
pub fn fingerprint_of(key_blob_base64: &str) -> String {
    let Some(raw) = base64_decode(key_blob_base64) else {
        return "SHA256:unavailable".to_string();
    };
    let digest = crate::hash::sha256(&raw);
    format!("SHA256:{}", crate::hash::base64_no_pad(&digest))
}

/// Append a confirmed key to the user's `known_hosts` (mode 0600), which is the
/// same thing `ssh` would do after the user types `yes`.
pub fn trust_host_key(path: &std::path::Path, key_line: &str) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("creating ~/.ssh")?;
    }
    let already_present = std::fs::read_to_string(path)
        .map(|content| content.lines().any(|l| l.trim() == key_line.trim()))
        .unwrap_or(false);

    if !already_present {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        writeln!(file, "{}", key_line.trim()).context("appending host key")?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Forget a recorded key (only used after a user confirmed a changed key:
/// the old line must be removed before the new one is trusted).
pub fn remove_host_key(path: &std::path::Path, host: &str, port: u16) -> Result<usize> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let target = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let mut removed = 0;
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| {
            let matches = line
                .split_whitespace()
                .next()
                .map(|field| {
                    field == target
                        || field
                            .split(',')
                            .any(|candidate| candidate == target)
                })
                .unwrap_or(false);
            if matches {
                removed += 1;
            }
            !matches
        })
        .collect();
    if removed > 0 {
        let mut body = kept.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        std::fs::write(path, body).context("rewriting known_hosts")?;
    }
    Ok(removed)
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let table: Vec<u8> = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".to_vec();
    let mut lookup = [255u8; 256];
    for (i, &c) in table.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut values = [0u8; 4];
        let mut padding = 0;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                padding += 1;
                values[i] = 0;
            } else {
                let v = lookup[b as usize];
                if v == 255 {
                    return None;
                }
                values[i] = v;
            }
        }
        let n = ((values[0] as u32) << 18)
            | ((values[1] as u32) << 12)
            | ((values[2] as u32) << 6)
            | (values[3] as u32);
        out.push(((n >> 16) & 0xFF) as u8);
        if padding < 2 && chunk.len() > 2 {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if padding < 1 && chunk.len() > 3 {
            out.push((n & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_ssh_errors() {
        assert_eq!(
            classify_ssh_stderr("Host key verification failed."),
            Some(Severity::Unknown)
        );
        assert_eq!(
            classify_ssh_stderr(
                "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
                 @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
                 @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@"
            ),
            Some(Severity::Changed)
        );
        assert_eq!(classify_ssh_stderr("Permission denied (publickey)."), None);
    }

    #[test]
    fn changed_keys_are_high_risk_and_never_auto_accepted() {
        let issue = changed_issue(
            "devbox",
            22,
            HostKeyIssue::Unknown {
                host: "devbox".into(),
                port: 22,
                key_type: "ED25519".into(),
                fingerprint: "SHA256:abc".into(),
                key_line: "devbox ssh-ed25519 AAAA".into(),
            },
        );
        assert!(issue.is_high_risk());
        assert!(issue.attention_message().contains("已改变"));
        let unknown = HostKeyIssue::Unknown {
            host: "devbox".into(),
            port: 22,
            key_type: "ED25519".into(),
            fingerprint: "SHA256:abc".into(),
            key_line: "devbox ssh-ed25519 AAAA".into(),
        };
        assert!(!unknown.is_high_risk());
        assert!(unknown.attention_message().contains("首次连接"));
    }

    #[test]
    fn fingerprints_the_ssh_keygen_way() {
        // SHA256 of the 3 bytes "abc" — the same string `ssh-keygen -lf` prints
        // for that blob (base64 of "abc" is "YWJj").
        assert_eq!(fingerprint_of("YWJj"), "SHA256:ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
        assert_eq!(fingerprint_of("!!!not-base64!!!"), "SHA256:unavailable");
    }

    #[test]
    fn trusts_and_forgets_keys() {
        let dir = std::env::temp_dir().join(format!("spark-known-hosts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("known_hosts");

        trust_host_key(&path, "devbox ssh-ed25519 AAAAKEY").expect("trust");
        trust_host_key(&path, "devbox ssh-ed25519 AAAAKEY").expect("idempotent");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);

        assert_eq!(remove_host_key(&path, "devbox", 22).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "");

        trust_host_key(&path, "[devbox]:2222 ssh-ed25519 BBBB").unwrap();
        assert_eq!(remove_host_key(&path, "devbox", 2222).unwrap(), 1);
    }
}
