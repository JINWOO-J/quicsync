use std::process::Stdio;

use serde::Serialize;
use tokio::process::Command;

use crate::cli::parse_remote;
use crate::error::CliError;
use crate::quic::{QuicClientCfg, QuicTunnel, fingerprint_from_hex};
use crate::ssh::launch_remote_server;
use crate::types::RemoteSpec;

#[derive(Debug, Clone)]
pub struct DoctorArgs {
    pub remote: RemoteSpec,
    pub json: bool,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub target: String,
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

impl DoctorCheck {
    fn ok(name: &'static str, detail: String) -> Self {
        Self {
            name,
            ok: true,
            detail,
            hint: None,
        }
    }

    fn fail(name: &'static str, detail: String) -> Self {
        let hint = hint_for(name, &detail);
        Self {
            name,
            ok: false,
            detail,
            hint,
        }
    }
}

pub struct Doctor {
    args: DoctorArgs,
}

impl Doctor {
    pub fn new(args: DoctorArgs) -> Self {
        Self { args }
    }

    pub async fn run(self) -> DoctorReport {
        let mut checks = Vec::new();
        let target = ssh_target(&self.args.remote);

        checks.push(command_version("local rsync", "rsync", &["--version"]).await);
        checks.push(current_quicsync_version().await);
        checks.push(ssh_check(&target, "ssh connectivity", "true").await);
        checks.push(
            ssh_check(
                &target,
                "remote quicsync",
                "PATH=$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:$PATH quicsync --version",
            )
            .await,
        );
        checks.push(ssh_check(&target, "remote rsync", "rsync --version").await);
        checks.push(quic_check(&self.args.remote).await);

        let ok = checks.iter().all(|c| c.ok);
        DoctorReport { target, ok, checks }
    }
}

impl DoctorReport {
    pub fn print(&self) {
        eprintln!("quicsync doctor: {}", self.target);
        for check in &self.checks {
            let status = if check.ok { "ok" } else { "fail" };
            eprintln!("  [{status}] {} - {}", check.name, check.detail);
            if let Some(hint) = check.hint {
                eprintln!("    hint: {hint}");
            }
        }
        eprintln!(
            "quicsync doctor: {}",
            if self.ok {
                "all checks passed"
            } else {
                "checks failed"
            }
        );
    }

    pub fn print_json(&self) {
        match serde_json::to_string_pretty(self) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("quicsync doctor: failed to encode json: {e}"),
        }
    }
}

pub fn parse_doctor_args(args: &[String]) -> Result<DoctorArgs, CliError> {
    let mut json = false;
    let mut target: Option<&str> = None;

    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                return Err(CliError::InvalidArgs(
                    "usage: quicsync doctor [--json] [user@]host".to_string(),
                ));
            }
            other if other.starts_with('-') => {
                return Err(CliError::InvalidArgs(format!(
                    "unsupported doctor option '{other}'"
                )));
            }
            other => {
                if target.replace(other).is_some() {
                    return Err(CliError::InvalidArgs(
                        "doctor accepts exactly one target".to_string(),
                    ));
                }
            }
        }
    }

    let target = target.ok_or_else(|| {
        CliError::InvalidArgs("usage: quicsync doctor [--json] [user@]host".to_string())
    })?;
    let remote = parse_remote(&format!("{target}:"))?;

    Ok(DoctorArgs { remote, json })
}

fn ssh_target(remote: &RemoteSpec) -> String {
    match &remote.user {
        Some(user) => format!("{}@{}", user, remote.host),
        None => remote.host.clone(),
    }
}

async fn command_version(name: &'static str, cmd: &str, args: &[&str]) -> DoctorCheck {
    match Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(output) if output.status.success() => DoctorCheck::ok(
            name,
            first_line(&output.stdout).unwrap_or_else(|| "available".to_string()),
        ),
        Ok(output) => DoctorCheck::fail(
            name,
            first_line(&output.stderr)
                .unwrap_or_else(|| format!("command exited with status {}", output.status)),
        ),
        Err(e) => DoctorCheck::fail(name, e.to_string()),
    }
}

async fn current_quicsync_version() -> DoctorCheck {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            return DoctorCheck::fail("local quicsync", e.to_string());
        }
    };

    match Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(output) if output.status.success() => DoctorCheck::ok(
            "local quicsync",
            first_line(&output.stdout).unwrap_or_else(|| "available".to_string()),
        ),
        Ok(output) => DoctorCheck::fail(
            "local quicsync",
            first_line(&output.stderr)
                .unwrap_or_else(|| format!("command exited with status {}", output.status)),
        ),
        Err(e) => DoctorCheck::fail("local quicsync", e.to_string()),
    }
}

async fn ssh_check(target: &str, name: &'static str, remote_cmd: &str) -> DoctorCheck {
    match Command::new("ssh")
        .arg(target)
        .arg(remote_cmd)
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(output) if output.status.success() => DoctorCheck::ok(
            name,
            first_line(&output.stdout).unwrap_or_else(|| "ok".to_string()),
        ),
        Ok(output) => DoctorCheck::fail(
            name,
            first_line(&output.stderr)
                .unwrap_or_else(|| format!("command exited with status {}", output.status)),
        ),
        Err(e) => DoctorCheck::fail(name, e.to_string()),
    }
}

async fn quic_check(remote: &RemoteSpec) -> DoctorCheck {
    let handshake = match launch_remote_server(remote).await {
        Ok(handshake) => handshake,
        Err(e) => {
            return DoctorCheck::fail("quic tunnel", format!("remote server launch failed: {e}"));
        }
    };

    let mut ssh_process = handshake.ssh_process;
    let host_port = format!("{}:{}", remote.host, handshake.remote_port);
    let remote_addr = match tokio::net::lookup_host(&host_port).await {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => addr,
            None => {
                let _ = ssh_process.kill().await;
                return DoctorCheck::fail(
                    "quic tunnel",
                    format!("no address found for {host_port}"),
                );
            }
        },
        Err(e) => {
            let _ = ssh_process.kill().await;
            return DoctorCheck::fail("quic tunnel", format!("DNS resolve {host_port}: {e}"));
        }
    };

    let fingerprint = match handshake
        .fingerprint
        .as_deref()
        .map(fingerprint_from_hex)
        .transpose()
    {
        Ok(fp) => fp,
        Err(e) => {
            let _ = ssh_process.kill().await;
            return DoctorCheck::fail("quic tunnel", format!("fingerprint: {e}"));
        }
    };

    let result = QuicTunnel::connect(QuicClientCfg {
        remote_addr,
        auth_token: handshake.auth_token,
        server_name: "localhost".to_string(),
        window_bytes: crate::quic::window_bytes_from_env(),
        fingerprint,
    })
    .await;
    let _ = ssh_process.kill().await;

    match result {
        Ok(tunnel) => {
            let _ = tunnel.close().await;
            DoctorCheck::ok("quic tunnel", format!("connected to {remote_addr}"))
        }
        Err(e) => DoctorCheck::fail("quic tunnel", e.to_string()),
    }
}

fn hint_for(name: &str, detail: &str) -> Option<&'static str> {
    let lower = detail.to_ascii_lowercase();

    match name {
        "local rsync" => Some("Install rsync locally and ensure it is available in PATH."),
        "local quicsync" => Some("Rebuild or reinstall the local quicsync binary, then retry."),
        "ssh connectivity" => {
            Some("Verify the host, username, SSH key, ssh config, and non-interactive login.")
        }
        "remote quicsync" => Some(
            "Run 'quicsync install-remote <host>' or add quicsync to remote PATH, for example ~/.cargo/bin or ~/.local/bin.",
        ),
        "remote rsync" => {
            Some("Install rsync on the remote host and ensure it is available in PATH.")
        }
        "quic tunnel" if lower.contains("dns") || lower.contains("no address") => {
            Some("Verify that the remote hostname resolves from the local machine.")
        }
        "quic tunnel" if lower.contains("fingerprint") || lower.contains("tls") => Some(
            "Retry after reinstalling matching quicsync binaries; certificate fingerprint negotiation failed.",
        ),
        "quic tunnel" if lower.contains("timed out") || lower.contains("timeout") => {
            Some("Check UDP firewall/NAT rules between hosts, or retry with --fallback=rsync.")
        }
        "quic tunnel" if lower.contains("connection") || lower.contains("udp") => {
            Some("Check that UDP is allowed between hosts, or retry with --fallback=rsync.")
        }
        "quic tunnel" if lower.contains("remote server launch") => {
            Some("Run 'quicsync doctor' after confirming remote quicsync can start via SSH.")
        }
        "quic tunnel" => Some(
            "Check UDP reachability and remote quicsync server startup; use --fallback=rsync if needed.",
        ),
        _ => None,
    }
}

fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_doctor_target() {
        let args = s(&["quicsync", "doctor", "deploy@example.com"]);
        let parsed = parse_doctor_args(&args).unwrap();
        assert_eq!(parsed.remote.user.as_deref(), Some("deploy"));
        assert_eq!(parsed.remote.host, "example.com");
        assert!(!parsed.json);
    }

    #[test]
    fn parse_doctor_json() {
        let args = s(&["quicsync", "doctor", "--json", "example.com"]);
        let parsed = parse_doctor_args(&args).unwrap();
        assert_eq!(parsed.remote.user, None);
        assert_eq!(parsed.remote.host, "example.com");
        assert!(parsed.json);
    }

    #[test]
    fn parse_doctor_requires_target() {
        let args = s(&["quicsync", "doctor"]);
        assert!(parse_doctor_args(&args).is_err());
    }

    #[test]
    fn first_line_skips_empty_lines() {
        assert_eq!(
            first_line(b"\n\nrsync version 3.2\n"),
            Some("rsync version 3.2".into())
        );
    }

    #[test]
    fn failed_check_includes_hint() {
        let check = DoctorCheck::fail("remote quicsync", "command not found".into());
        assert_eq!(
            check.hint,
            Some(
                "Run 'quicsync install-remote <host>' or add quicsync to remote PATH, for example ~/.cargo/bin or ~/.local/bin."
            )
        );
    }

    #[test]
    fn successful_check_has_no_hint() {
        let check = DoctorCheck::ok("remote quicsync", "quicsync 0.1.4".into());
        assert_eq!(check.hint, None);
    }

    #[test]
    fn quic_timeout_hint_mentions_fallback() {
        let check = DoctorCheck::fail("quic tunnel", "connection timed out".into());
        assert!(check.hint.unwrap().contains("--fallback=rsync"));
    }
}
