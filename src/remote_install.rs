use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::Command;

use crate::cli::parse_remote;
use crate::error::CliError;
use crate::types::RemoteSpec;

const DEFAULT_INSTALL_DIR: &str = "$HOME/.local/bin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInstallArgs {
    pub remote: RemoteSpec,
    pub install_dir: String,
}

pub struct RemoteInstaller;

impl RemoteInstaller {
    pub async fn install_current(remote: &RemoteSpec, install_dir: &str) -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| format!("locate current binary: {e}"))?;
        install_binary(remote, install_dir, exe).await
    }
}

pub fn parse_install_remote_args(args: &[String]) -> Result<RemoteInstallArgs, CliError> {
    let mut install_dir = DEFAULT_INSTALL_DIR.to_string();
    let mut target: Option<&str> = None;
    let mut iter = args.iter().skip(2);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dir" => {
                install_dir = iter
                    .next()
                    .ok_or_else(|| CliError::InvalidArgs("--dir requires a value".to_string()))?
                    .to_string();
            }
            "--help" | "-h" => {
                return Err(CliError::InvalidArgs(
                    "usage: quicsync install-remote [--dir DIR] [user@]host".to_string(),
                ));
            }
            other if other.starts_with('-') => {
                return Err(CliError::InvalidArgs(format!(
                    "unsupported install-remote option '{other}'"
                )));
            }
            other => {
                if target.replace(other).is_some() {
                    return Err(CliError::InvalidArgs(
                        "install-remote accepts exactly one target".to_string(),
                    ));
                }
            }
        }
    }

    let target = target.ok_or_else(|| {
        CliError::InvalidArgs("usage: quicsync install-remote [--dir DIR] [user@]host".to_string())
    })?;
    let remote = parse_remote(&format!("{target}:"))?;

    Ok(RemoteInstallArgs {
        remote,
        install_dir,
    })
}

async fn install_binary(
    remote: &RemoteSpec,
    install_dir: &str,
    local_binary: PathBuf,
) -> Result<String, String> {
    let input = std::fs::File::open(&local_binary)
        .map_err(|e| format!("open {}: {e}", local_binary.display()))?;
    let script = install_script(install_dir);
    let output = Command::new("ssh")
        .arg(ssh_target(remote))
        .arg(script)
        .stdin(Stdio::from(input))
        .output()
        .await
        .map_err(|e| format!("spawn ssh: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(first_non_empty_line(&stdout)
            .unwrap_or("remote quicsync installed")
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(if stderr.trim().is_empty() {
            format!("ssh exited with status {}", output.status)
        } else {
            stderr.trim().to_string()
        })
    }
}

fn install_script(install_dir: &str) -> String {
    let dir_assignment = if install_dir == DEFAULT_INSTALL_DIR {
        format!("dir={DEFAULT_INSTALL_DIR}")
    } else {
        format!("dir={}", shell_quote(install_dir))
    };
    format!(
        "set -eu; {dir_assignment}; mkdir -p \"$dir\"; tmp=\"$dir/.quicsync.tmp.$$\"; \
         cat > \"$tmp\"; chmod 0755 \"$tmp\"; \"$tmp\" --version >/dev/null; \
         mv \"$tmp\" \"$dir/quicsync\"; \"$dir/quicsync\" --version"
    )
}

fn ssh_target(remote: &RemoteSpec) -> String {
    match &remote.user {
        Some(user) => format!("{}@{}", user, remote.host),
        None => remote.host.clone(),
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn first_non_empty_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_install_remote_target() {
        let args = s(&["quicsync", "install-remote", "deploy@example.com"]);
        let parsed = parse_install_remote_args(&args).unwrap();
        assert_eq!(parsed.remote.user.as_deref(), Some("deploy"));
        assert_eq!(parsed.remote.host, "example.com");
        assert_eq!(parsed.install_dir, DEFAULT_INSTALL_DIR);
    }

    #[test]
    fn parse_install_remote_dir() {
        let args = s(&[
            "quicsync",
            "install-remote",
            "--dir",
            "/opt/quicsync/bin",
            "example.com",
        ]);
        let parsed = parse_install_remote_args(&args).unwrap();
        assert_eq!(parsed.install_dir, "/opt/quicsync/bin");
        assert_eq!(parsed.remote.host, "example.com");
    }

    #[test]
    fn install_script_uses_home_expansion_for_default() {
        let script = install_script(DEFAULT_INSTALL_DIR);
        assert!(script.contains("dir=$HOME/.local/bin"));
    }

    #[test]
    fn shell_quote_handles_single_quote() {
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
    }
}
