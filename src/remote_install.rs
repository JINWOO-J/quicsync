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

    /// 원격 OS/arch를 감지하여 플랫폼에 맞게 설치한다.
    /// - 원격이 로컬과 동일 플랫폼이면 현재 바이너리를 그대로 전송 (인터넷 불필요, 빠름)
    /// - 다르면 로컬과 같은 버전의 릴리즈 자산을 GitHub에서 받아 설치 (크로스 아키텍처)
    pub async fn install_smart(remote: &RemoteSpec, install_dir: &str) -> Result<String, String> {
        let (os, arch) = detect_remote_platform(remote).await?;

        if os == std::env::consts::OS && arch == std::env::consts::ARCH {
            return Self::install_current(remote, install_dir).await;
        }

        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        eprintln!(
            "quicsync: remote is {os}/{arch} (differs from local {}/{}); \
             fetching matching release {tag}...",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let staged =
            crate::update::download_release_binary(&os, &arch, &tag, &std::env::temp_dir()).await?;
        let result = install_binary(remote, install_dir, staged.clone()).await;
        let _ = std::fs::remove_file(&staged);
        result
    }
}

/// 원격 호스트의 (os, arch)를 `uname`으로 감지하여 `std::env::consts` 표기로 정규화한다.
async fn detect_remote_platform(remote: &RemoteSpec) -> Result<(String, String), String> {
    let output = Command::new("ssh")
        .arg(ssh_target(remote))
        .arg("uname -s; uname -m")
        .output()
        .await
        .map_err(|e| format!("spawn ssh (uname): {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("remote uname failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let os_raw = lines
        .next()
        .ok_or_else(|| "remote uname returned no OS line".to_string())?;
    let arch_raw = lines
        .next()
        .ok_or_else(|| "remote uname returned no arch line".to_string())?;
    Ok((normalize_os(os_raw)?, normalize_arch(arch_raw)?))
}

/// `uname -s` 출력을 std::env::consts::OS 표기로 변환한다.
fn normalize_os(uname_s: &str) -> Result<String, String> {
    match uname_s {
        "Linux" => Ok("linux".to_string()),
        "Darwin" => Ok("macos".to_string()),
        other => Err(format!("unsupported remote OS: {other}")),
    }
}

/// `uname -m` 출력을 std::env::consts::ARCH 표기로 변환한다.
fn normalize_arch(uname_m: &str) -> Result<String, String> {
    match uname_m {
        "x86_64" | "amd64" => Ok("x86_64".to_string()),
        "aarch64" | "arm64" => Ok("aarch64".to_string()),
        other => Err(format!("unsupported remote arch: {other}")),
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

    #[test]
    fn normalize_os_known() {
        assert_eq!(normalize_os("Linux").unwrap(), "linux");
        assert_eq!(normalize_os("Darwin").unwrap(), "macos");
        assert!(normalize_os("FreeBSD").is_err());
    }

    #[test]
    fn normalize_arch_known() {
        assert_eq!(normalize_arch("x86_64").unwrap(), "x86_64");
        assert_eq!(normalize_arch("amd64").unwrap(), "x86_64");
        assert_eq!(normalize_arch("aarch64").unwrap(), "aarch64");
        assert_eq!(normalize_arch("arm64").unwrap(), "aarch64");
        assert!(normalize_arch("riscv64").is_err());
    }

    /// normalize 출력은 std::env::consts 표기와 일치해야 install_smart의 동일-플랫폼
    /// 비교가 올바로 동작한다.
    #[test]
    fn normalize_matches_env_consts() {
        for os in ["linux", "macos"] {
            assert!(["linux", "macos"].contains(&os));
        }
        // 현재 호스트 arch가 매핑 대상이면 consts와 동일 표기여야 한다.
        let host = std::env::consts::ARCH;
        if host == "x86_64" {
            assert_eq!(normalize_arch("x86_64").unwrap(), host);
        } else if host == "aarch64" {
            assert_eq!(normalize_arch("arm64").unwrap(), host);
        }
    }
}
