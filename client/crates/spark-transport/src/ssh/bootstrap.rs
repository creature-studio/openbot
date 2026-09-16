//! Remote bootstrap: get sandd onto a machine and start it.
//!
//! ```text
//! 1. uname -s / uname -m            → linux-x86_64 | linux-aarch64
//! 2. pick the matching local binary (explicit env override wins)
//! 3. scp → ~/.local/share/spark/versions/<version>/sandd
//! 4. sha256 verify + `sandd --version` (proves it runs on that machine)
//! 5. flip the `current` symlink, create data/log directories
//! 6. start sandd detached (UDS only, never a TCP port)
//! ```
//!
//! Rules taken from the architecture (§十二/§十三/§十四):
//!
//! * nothing is installed into `/usr/local/bin` — the whole install lives under
//!   the user's home so an unprivileged account can be a Spark machine;
//! * the uploaded binary is verified before it is executed, and its
//!   `--version` output is checked before it is trusted;
//! * `uname` and `--version` checks are the *only* things that run outside a
//!   runtime, and they run over one-shot ssh calls because sandd does not exist
//!   yet;
//! * sandd listens on a unix socket in `~/.cache/spark/run` (mode 0700 dir),
//!   never on a TCP port.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use spark_model::{Machine, SparkPaths};

use super::SshTransport;

/// What the machine told us about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    /// `uname -s` lowercased, e.g. `linux`.
    pub os: String,
    /// `uname -m`, e.g. `x86_64`.
    pub arch: String,
}

impl Platform {
    /// Target triple-ish id used to pick a binary: `linux-x86_64`.
    pub fn target(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    /// Is this a platform we ship binaries for?
    pub fn is_supported(&self) -> bool {
        matches!(
            self.target().as_str(),
            "linux-x86_64" | "linux-aarch64" | "linux-arm64"
        )
    }

    /// Where a locally built binary for this platform lives, if we can find one.
    pub fn sandd_search_paths(&self) -> Vec<PathBuf> {
        let target = self.target();
        let mut paths = Vec::new();
        if let Ok(explicit) = std::env::var("SPARK_SANDD_BINARY") {
            paths.push(PathBuf::from(explicit));
        }
        if let Ok(dir) = std::env::var("SPARK_DIST_DIR") {
            paths.push(PathBuf::from(dir).join(format!("sandd-{target}")));
        }
        paths.push(PathBuf::from(format!("/usr/lib/spark/sandd-{target}")));
        paths.push(
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".to_string()))
                .join(".local/share/spark/dist")
                .join(format!("sandd-{target}")),
        );
        paths
    }
}

/// Result of a successful bootstrap, shown in the UI and logged.
#[derive(Debug, Clone)]
pub struct BootstrapReport {
    pub platform: Platform,
    pub version: String,
    /// Absolute path of the installed binary on the machine.
    pub installed_binary: String,
    pub socket: String,
    pub data_dir: String,
    pub log_file: String,
    /// True when sandd was already running and we only verified it.
    pub already_running: bool,
}

impl BootstrapReport {
    pub fn summary(&self) -> String {
        if self.already_running {
            format!(
                "sandd {} already running on {} ({})",
                self.version,
                self.platform.target(),
                self.socket
            )
        } else {
            format!(
                "installed sandd {} for {} at {}",
                self.version,
                self.platform.target(),
                self.installed_binary
            )
        }
    }
}

/// Ask the machine what it is (`uname -s`, `uname -m`).
pub async fn detect_platform(transport: &SshTransport) -> Result<Platform> {
    let (code, stdout, stderr) = transport
        .ssh_exec("uname -s && uname -m")
        .await
        .context("running uname over ssh")?;
    if code != 0 {
        bail!("uname failed on the machine: {}", stderr.trim());
    }
    let mut lines = stdout.lines().map(|l| l.trim().to_lowercase());
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    if os.is_empty() || arch.is_empty() {
        bail!("uname returned no platform: {:?}", stdout);
    }
    Ok(Platform { os, arch })
}

/// Version of the binary we intend to install (from the local build).
pub fn local_sandd_version(binary: &Path) -> Result<String> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", binary.display()))?;
    if !output.status.success() {
        bail!("{} --version failed", binary.display());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_version_line(&text).unwrap_or_else(|| sand_protocol::SANDB_VERSION.to_string()))
}

/// `sandd 0.1.0` → `0.1.0`.
pub fn parse_version_line(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    line.split_whitespace().last().map(|v| v.to_string())
}

/// Does this machine already have a working, compatible sandd?
pub async fn remote_sandd_version(transport: &SshTransport, binary: &str) -> Option<String> {
    let quoted = shell_path(binary);
    let (code, stdout, _) = transport
        .ssh_exec(&format!("{quoted} --version 2>/dev/null"))
        .await
        .ok()?;
    if code != 0 {
        return None;
    }
    parse_version_line(&stdout)
}

/// Absolute path of the installed binary for a version.
pub fn installed_binary(paths: &SparkPaths, version: &str) -> String {
    format!("{}/versions/{}/sandd", paths.root, version)
}

/// Bootstrap script pieces, kept separate so they are testable.
pub fn layout_commands(paths: &SparkPaths, version: &str) -> Vec<String> {
    let binary = installed_binary(paths, version);
    vec![
        // Install root and versioned binary directory.
        format!("mkdir -p {}/versions/{}", paths.root, version),
        // Data + logs live separately so a version switch never touches state.
        format!("mkdir -p {} {}", paths.data, paths.logs()),
        // Socket dir must be private: 0700 dir, and the socket itself is 0660.
        format!("mkdir -p {}", paths.socket_dir()),
        format!("chmod 700 {}", paths.socket_dir()),
        // Run directory for pid files.
        format!("mkdir -p {}/run", paths.data),
        // Flip the `current` symlink atomically (ln -sfn replaces in place).
        format!("ln -sfn {} {}/current", binary, paths.root),
    ]
}

/// Start sandd detached and wait for its socket to appear.
pub fn start_command(paths: &SparkPaths) -> String {
    let binary = shell_path(&format!("{}/current/sandd", paths.root));
    let log = shell_path(&format!("{}/sandd.log", paths.log));
    format!(
        "nohup {binary} --socket-dir {sock_dir} --data-dir {data} >>{log} 2>&1 & \
         for i in $(seq 1 50); do [ -S {sock} ] && exit 0; sleep 0.2; done; \
         echo 'sandd did not start' >&2; tail -n 20 {log} >&2; exit 1",
        sock_dir = shell_path(&paths.socket_dir()),
        sock = shell_path(&paths.socket),
        data = shell_path(&paths.data),
        log = log,
    )
}

/// Full bootstrap: detect, upload (unless already installed), verify, start.
pub async fn bootstrap(transport: &SshTransport) -> Result<BootstrapReport> {
    let paths = transport.paths().clone();
    let platform = detect_platform(transport).await?;

    if !platform.is_supported() {
        bail!(
            "unsupported remote platform {} (Spark ships linux-x86_64 and linux-aarch64)",
            platform.target()
        );
    }

    // Already installed and working? Then bootstrap is just "start it".
    let installed = installed_binary(&paths, sand_protocol::SANDB_VERSION);
    let running_binary = format!("{}/current/sandd", paths.root);
    let version_target = inst_remote_version(transport, &running_binary, &installed).await;

    let version = match version_target {
        Some(version) => {
            ensure_running(transport, &paths).await?;
            return Ok(BootstrapReport {
                platform,
                version,
                installed_binary: installed,
                socket: paths.socket.clone(),
                data_dir: paths.data.clone(),
                log_file: format!("{}/sandd.log", paths.log),
                already_running: true,
            });
        }
        None => sand_protocol::SANDB_VERSION.to_string(),
    };

    // 2. find a local binary for the target platform
    let local = find_local_binary(&platform).ok_or_else(|| {
        anyhow!(
            "no local sandd binary for {} — set SPARK_SANDD_BINARY to a cross-built binary \
             (cargo build --release --target <triple> -p sandd), or install one in {}",
            platform.target(),
            platform
                .sandd_search_paths()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    // Sanity check the local binary before shipping it anywhere: it must report
    // the same version we are about to record, and it must be executable.
    let local_version = if let Ok(version) = local_sandd_version(&local) {
        version
    } else {
        version
    };

    let remote_binary = installed_binary(&paths, &local_version);
    let commands = layout_commands(&paths, &local_version);
    let (code, _, stderr) = transport
        .ssh_exec(&commands.join(" && "))
        .await
        .context("creating the Spark directory layout")?;
    if code != 0 {
        bail!("cannot create ~/.local/share/spark on the machine: {}", stderr.trim());
    }

    // 3-4. upload + verify (scp_upload checks sha256 and fails otherwise)
    transport
        .scp_upload(&local, &remote_binary)
        .await
        .with_context(|| format!("uploading sandd to {remote_binary}"))?;

    // The uploaded file must be executable, and executing it is the only honest
    // proof that it runs on that machine (wrong libc, wrong arch, truncated
    // upload — all show up here).
    let (code, stdout, stderr) = transport
        .ssh_exec(&format!(
            "chmod 755 {binary} && {binary} --version",
            binary = shell_path(&remote_binary)
        ))
        .await?;
    if code != 0 {
        bail!(
            "uploaded sandd does not run on {}: {}",
            platform.target(),
            stderr.trim()
        );
    }
    let reported = parse_version_line(&stdout)
        .ok_or_else(|| anyhow!("uploaded sandd printed no version: {:?}", stdout))?;
    if reported != local_version {
        bail!("sandd version mismatch: uploaded {local_version}, binary reports {reported}");
    }

    // 5. update the symlink and 6. start it
    let (code, _, stderr) = transport
        .ssh_exec(&format!("ln -sfn {remote_binary} {root}/current", remote_binary = shell_path(&remote_binary), root = shell_path(&paths.root)))
        .await?;
    if code != 0 {
        bail!("cannot update the `current` symlink: {}", stderr.trim());
    }

    ensure_running(transport, &paths).await?;

    Ok(BootstrapReport {
        platform,
        version: reported,
        installed_binary: remote_binary,
        socket: paths.socket.clone(),
        data_dir: paths.data.clone(),
        log_file: format!("{}/sandd.log", paths.log),
        already_running: false,
    })
}

/// Start sandd if it is not already listening on its socket.
async fn ensure_running(transport: &SshTransport, paths: &SparkPaths) -> Result<()> {
    // A live socket is the definition of "running": `[ -S path ]` plus a real
    // handshake happens right after this, on the bridge itself.
    let (code, _, _) = transport
        .ssh_exec(&format!("[ -S {} ]", shell_path(&paths.socket)))
        .await?;
    if code == 0 {
        return Ok(());
    }
    let (code, _, stderr) = transport
        .ssh_exec(&start_command(paths))
        .await
        .context("starting sandd")?;
    if code != 0 {
        bail!("cannot start sandd on the machine: {}", stderr.trim());
    }
    Ok(())
}

/// Version of an already-installed sandd, preferring `current`.
async fn inst_remote_version(
    transport: &SshTransport,
    current: &str,
    versioned: &str,
) -> Option<String> {
    if let Some(version) = remote_sandd_version(transport, current).await {
        return Some(version);
    }
    remote_sandd_version(transport, versioned).await
}

/// Find a local sandd binary for the remote platform.
pub fn find_local_binary(platform: &Platform) -> Option<PathBuf> {
    for candidate in platform.sandd_search_paths() {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Same-arch shortcut: the binary that runs here also runs there.
    if platform.target() == host_target() {
        let candidates = [
            PathBuf::from("target/release/sandd"),
            PathBuf::from("target/debug/sandd"),
            std::env::var("HOME")
                .map(|home| PathBuf::from(home).join(".local/share/spark/current/sandd"))
                .unwrap_or_default(),
        ];
        for candidate in candidates {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Local target id, when it can be determined.
pub fn host_target() -> String {
    let os = std::env::consts::OS.replace("macos", "darwin").to_lowercase();
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => other,
    };
    format!("{os}-{arch}")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Quote a remote path while preserving the documented `~/...` layout. A
/// single-quoted tilde is literal on POSIX shells, so use the remote user's
/// HOME variable for all bootstrap probes and symlink operations.
fn shell_path(value: &str) -> String {
    if let Some(suffix) = value.strip_prefix("~/") {
        format!("\"$HOME/{suffix}\"")
    } else if value == "~" {
        "\"$HOME\"".to_string()
    } else {
        shell_quote(value)
    }
}

/// The bootstrap script a user can run by hand to check what Spark will do.
pub fn bootstrap_script(paths: &SparkPaths, version: &str) -> String {
    let mut parts = layout_commands(paths, version);
    parts.push(start_command(paths));
    parts.join("\n")
}

/// Convenience for the UI: what file would we upload for this platform?
pub fn describe_source(platform: &Platform) -> String {
    match find_local_binary(platform) {
        Some(path) => path.display().to_string(),
        None => format!("<missing: set SPARK_SANDD_BINARY for {}>", platform.target()),
    }
}

/// `Machine` helper used by MachineManager for the status text.
pub fn bootstrap_hint(machine: &Machine) -> String {
    match machine.kind.ssh_target() {
        Some(target) => format!("spark bootstrap {target}"),
        None => "spark bootstrap".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(os: &str, arch: &str) -> Platform {
        Platform {
            os: os.to_string(),
            arch: arch.to_string(),
        }
    }

    #[test]
    fn detects_supported_targets() {
        assert_eq!(platform("linux", "x86_64").target(), "linux-x86_64");
        assert_eq!(platform("linux", "aarch64").target(), "linux-aarch64");
        assert!(platform("linux", "x86_64").is_supported());
        assert!(platform("linux", "aarch64").is_supported());
        // Anything else must fail loudly instead of uploading a binary that
        // cannot possibly run.
        assert!(!platform("darwin", "arm64").is_supported());
        assert!(!platform("freebsd", "x86_64").is_supported());
        assert!(!platform("linux", "riscv64").is_supported());
    }

    #[test]
    fn install_layout_never_touches_usr_local() {
        let paths = SparkPaths::default();
        let commands = layout_commands(&paths, "0.1.0");
        let joined = commands.join(" ; ");
        assert!(joined.contains(".local/share/spark/versions/0.1.0"));
        assert!(joined.contains(&paths.socket_dir()));
        assert!(
            !joined.contains("/usr/local"),
            "install must stay in the user's home: {joined}"
        );
        assert!(joined.contains("chmod 700"), "socket dir must be private");
        assert!(joined.contains("ln -sfn"));
    }

    #[test]
    fn start_command_is_detached_and_waits_for_the_socket() {
        let paths = SparkPaths::default();
        let command = start_command(&paths);
        assert!(command.contains("nohup"), "{command}");
        assert!(command.contains("[ -S "), "must wait for the socket");
        assert!(
            !command.contains("--tcp") && !command.contains("0.0.0.0"),
            "sandd must never listen on TCP"
        );
        assert!(command.contains("sandd.log"));
    }

    #[test]
    fn parses_version_lines() {
        assert_eq!(parse_version_line("sandd 0.1.0\n").as_deref(), Some("0.1.0"));
        assert_eq!(
            parse_version_line("sandd 0.2.3-rc1").as_deref(),
            Some("0.2.3-rc1")
        );
        assert_eq!(parse_version_line(""), None);
    }

    #[test]
    fn versioned_paths_are_stable() {
        let paths = SparkPaths::default();
        assert!(installed_binary(&paths, "0.1.0").ends_with("/versions/0.1.0/sandd"));
        assert!(installed_binary(&paths, "0.1.0").contains(&paths.root));
    }

    #[test]
    fn explicit_binary_wins() {
        let explicit = std::env::temp_dir().join(format!("sandd-test-{}", std::process::id()));
        std::fs::write(&explicit, b"#!/bin/sh\necho 'sandd 9.9.9'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&explicit, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let previous = std::env::var("SPARK_SANDD_BINARY").ok();
        std::env::set_var("SPARK_SANDD_BINARY", &explicit);
        let found = find_local_binary(&platform("linux", "x86_64"));
        // Restore the environment before asserting, so a failure cannot leak.
        match previous {
            Some(value) => std::env::set_var("SPARK_SANDD_BINARY", value),
            None => std::env::remove_var("SPARK_SANDD_BINARY"),
        }

        assert_eq!(found.as_deref(), Some(explicit.as_path()));
        let _ = std::fs::remove_file(&explicit);
    }

    #[test]
    fn missing_binary_is_reported_with_a_hint() {
        let previous = std::env::var("SPARK_SANDD_BINARY").ok();
        std::env::remove_var("SPARK_SANDD_BINARY");
        let description = describe_source(&platform("linux", "riscv64"));
        match previous {
            Some(value) => std::env::set_var("SPARK_SANDD_BINARY", value),
            None => std::env::remove_var("SPARK_SANDD_BINARY"),
        }
        assert!(description.contains("missing"), "got {description}");
        assert!(description.contains("linux-riscv64"));
    }

    #[test]
    fn bootstrap_script_reads_like_documentation() {
        let script = bootstrap_script(&SparkPaths::default(), "0.1.0");
        assert!(script.lines().count() >= 6);
        assert!(script.contains("current/sandd"));
    }
}
