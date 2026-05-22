use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::error::CliError;

const DEFAULT_REPO: &str = "jinwoo-j/quicsync";
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateArgs {
    pub check: bool,
    pub force: bool,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallSource {
    Brew,
    CargoInstall,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Install {
    source: InstallSource,
    binary_path: PathBuf,
    dir: PathBuf,
    os: &'static str,
    arch: &'static str,
}

pub struct Updater;

impl Updater {
    pub async fn run(args: UpdateArgs) -> Result<i32, String> {
        let install = detect_install()?;
        let current = env!("CARGO_PKG_VERSION");
        let pinned = args.target.clone();
        let target = match pinned.as_deref() {
            Some(tag) => normalize_tag(tag),
            None => latest_tag().await?,
        };

        let cmp = compare_semver(current, &target);
        if args.check {
            if cmp >= 0 {
                println!(
                    "up-to-date: {} (latest {}, source {:?})",
                    current, target, install.source
                );
                return Ok(0);
            }
            println!(
                "update available: {} -> {}",
                trim_v(current),
                trim_v(&target)
            );
            return Ok(1);
        }

        println!(
            "current: {}\nlatest:  {}\nsource:  {:?} ({})",
            current,
            target,
            install.source,
            install.binary_path.display()
        );

        if cmp >= 0 && !args.force {
            println!("already up-to-date; pass --force to reinstall.");
            return Ok(0);
        }

        match install.source {
            InstallSource::Brew if pinned.is_none() => run_brew_upgrade().await,
            InstallSource::CargoInstall => {
                println!(
                    "detected cargo-install build; run:\n  cargo install --git https://github.com/{DEFAULT_REPO} quicsync --force"
                );
                Ok(0)
            }
            InstallSource::Manual | InstallSource::Brew => {
                let staged = download_verified(&install, &target).await?;
                atomic_replace_with_sudo(&staged, &install.binary_path).await?;
                println!("updated to {target}");
                Ok(0)
            }
        }
    }
}

pub fn parse_update_args(args: &[String]) -> Result<UpdateArgs, CliError> {
    let mut check = false;
    let mut force = false;
    let mut target = None;
    let mut iter = args.iter().skip(2);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--force" => force = true,
            "--to" => {
                target = Some(
                    iter.next()
                        .ok_or_else(|| CliError::InvalidArgs("--to requires a tag".to_string()))?
                        .to_string(),
                );
            }
            "--help" | "-h" => {
                return Err(CliError::InvalidArgs(
                    "usage: quicsync update [--check] [--force] [--to vX.Y.Z]".to_string(),
                ));
            }
            other if other.starts_with('-') => {
                return Err(CliError::InvalidArgs(format!(
                    "unsupported update option '{other}'"
                )));
            }
            other => {
                return Err(CliError::InvalidArgs(format!(
                    "unexpected update argument '{other}'"
                )));
            }
        }
    }

    Ok(UpdateArgs {
        check,
        force,
        target,
    })
}

fn detect_install() -> Result<Install, String> {
    let exe = std::env::current_exe().map_err(|e| format!("locate running binary: {e}"))?;
    let binary_path = std::fs::canonicalize(&exe).unwrap_or(exe);
    let dir = binary_path
        .parent()
        .ok_or_else(|| format!("cannot determine install dir for {}", binary_path.display()))?
        .to_path_buf();
    let source = classify_install(&binary_path);

    Ok(Install {
        source,
        binary_path,
        dir,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    })
}

fn classify_install(path: &Path) -> InstallSource {
    let p = path.to_string_lossy();
    if p.starts_with("/opt/homebrew/")
        || p.starts_with("/usr/local/Cellar/")
        || p.starts_with("/usr/local/Homebrew/")
        || p.starts_with("/home/linuxbrew/.linuxbrew/")
    {
        return InstallSource::Brew;
    }
    if is_cargo_install_path(path) {
        return InstallSource::CargoInstall;
    }
    InstallSource::Manual
}

fn is_cargo_install_path(path: &Path) -> bool {
    if let Some(home) = std::env::var_os("CARGO_HOME")
        && path.starts_with(Path::new(&home).join("bin"))
    {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME")
        && path.starts_with(Path::new(&home).join(".cargo").join("bin"))
    {
        return true;
    }
    false
}

pub fn asset_name(os: &str, arch: &str) -> Option<String> {
    match (os, arch) {
        ("linux", "x86_64" | "aarch64") | ("macos", "x86_64" | "aarch64") => {
            Some(format!("quicsync_{os}_{arch}.tar.gz"))
        }
        _ => None,
    }
}

async fn latest_tag() -> Result<String, String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            &format!("https://api.github.com/repos/{DEFAULT_REPO}/releases/latest"),
        ])
        .output()
        .await
        .map_err(|e| format!("run curl: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("parse release JSON: {e}"))?;
    payload
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(normalize_tag)
        .ok_or_else(|| "release JSON did not contain tag_name".to_string())
}

async fn download_verified(install: &Install, tag: &str) -> Result<PathBuf, String> {
    let staging_dir = pick_staging_dir(&install.dir);
    download_release_binary(install.os, install.arch, tag, &staging_dir).await
}

/// 지정한 os/arch/tag의 릴리즈 자산을 다운로드·체크섬 검증·해제하여
/// staged 바이너리 경로를 반환한다. self-update와 원격 설치(install-remote)가 공유한다.
///
/// - `os`/`arch`: `std::env::consts` 표기 (linux|macos, x86_64|aarch64)
/// - `tag`: 릴리즈 태그 (예: `v0.3.0`)
/// - `staging_dir`: 작업 디렉토리를 만들 부모 경로 (원격 설치는 보통 임시 디렉토리)
pub async fn download_release_binary(
    os: &str,
    arch: &str,
    tag: &str,
    staging_dir: &Path,
) -> Result<PathBuf, String> {
    let asset = asset_name(os, arch).ok_or_else(|| format!("unsupported platform: {os}/{arch}"))?;
    let workdir = tempfile_dir(staging_dir)?;
    let archive = workdir.join(&asset);
    let checksums = workdir.join("checksums.txt");
    let base = format!("https://github.com/{DEFAULT_REPO}/releases/download/{tag}");

    println!("downloading {asset} ({tag}) to {}", workdir.display());
    curl_download(&format!("{base}/{asset}"), &archive).await?;
    let archive_size = std::fs::metadata(&archive)
        .map_err(|e| format!("stat archive: {e}"))?
        .len();
    if archive_size > MAX_ARCHIVE_BYTES {
        return Err(format!("archive too large: {archive_size} bytes"));
    }
    curl_download(&format!("{base}/checksums.txt"), &checksums).await?;

    let expected = expected_checksum(&checksums, &asset)?;
    verify_sha256(&archive, &expected)?;
    extract_binary(&archive, &workdir).await
}

async fn curl_download(url: &str, output: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", url, "-o"])
        .arg(output)
        .status()
        .await
        .map_err(|e| format!("run curl: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("download failed: {url} ({status})"))
    }
}

fn expected_checksum(path: &Path, asset: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read checksums.txt: {e}"))?;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(sum), Some(name)) = (fields.next(), fields.next())
            && name == asset
        {
            return Ok(sum.to_ascii_lowercase());
        }
    }
    Err(format!("checksums.txt has no entry for {asset}"))
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("open archive: {e}"))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = [0u8; 8192];
    loop {
        let n =
            std::io::Read::read(&mut file, &mut buf).map_err(|e| format!("hash archive: {e}"))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    let actual = hex::encode(ctx.finish().as_ref());
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch (expected {expected}, got {actual})"
        ))
    }
}

async fn extract_binary(archive: &Path, workdir: &Path) -> Result<PathBuf, String> {
    let extract_dir = workdir.join("extract");
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("create extract dir: {e}"))?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .await
        .map_err(|e| format!("run tar: {e}"))?;
    if !status.success() {
        return Err(format!("extract failed: {status}"));
    }
    let binary = find_extracted_binary(&extract_dir)
        .ok_or_else(|| "archive did not contain quicsync binary".to_string())?;
    let staged = workdir.join("quicsync.new");
    std::fs::copy(&binary, &staged).map_err(|e| format!("stage binary: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod staged binary: {e}"))?;
    }
    Ok(staged)
}

fn find_extracted_binary(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_extracted_binary(&path) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|name| name == "quicsync") {
            return Some(path);
        }
    }
    None
}

async fn atomic_replace_with_sudo(staged: &Path, target: &Path) -> Result<(), String> {
    if writable(
        target
            .parent()
            .ok_or_else(|| format!("cannot determine parent of {}", target.display()))?,
    ) {
        return atomic_replace(staged, target);
    }
    let status = Command::new("sudo")
        .arg("install")
        .arg("-m")
        .arg("0755")
        .arg(staged)
        .arg(target)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .map_err(|e| format!("run sudo install: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("sudo install failed: {status}"))
    }
}

fn atomic_replace(staged: &Path, target: &Path) -> Result<(), String> {
    let bak = target.with_extension("bak");
    let _ = std::fs::remove_file(&bak);
    if target.exists() {
        std::fs::copy(target, &bak).map_err(|e| format!("backup current binary: {e}"))?;
    }
    std::fs::rename(staged, target).map_err(|e| format!("install new binary: {e}"))?;
    Ok(())
}

fn pick_staging_dir(install_dir: &Path) -> PathBuf {
    if writable(install_dir) {
        install_dir.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".quicsync-update-probe-{}", std::process::id()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map(|file| {
            drop(file);
            let _ = std::fs::remove_file(probe);
            true
        })
        .unwrap_or(false)
}

fn tempfile_dir(parent: &Path) -> Result<PathBuf, String> {
    let path = parent.join(format!("quicsync-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).map_err(|e| format!("create temp dir: {e}"))?;
    Ok(path)
}

async fn run_brew_upgrade() -> Result<i32, String> {
    println!("running: brew upgrade quicsync");
    let status = Command::new("brew")
        .args(["upgrade", "quicsync"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .map_err(|e| format!("run brew: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

fn normalize_tag(tag: &str) -> String {
    let trimmed = tag.trim();
    if trimmed.starts_with('v') {
        trimmed.to_string()
    } else {
        format!("v{trimmed}")
    }
}

fn trim_v(v: &str) -> &str {
    v.strip_prefix('v').unwrap_or(v)
}

fn compare_semver(a: &str, b: &str) -> i32 {
    let (av, adirty) = parse_version(a);
    let (bv, bdirty) = parse_version(b);
    for i in 0..3 {
        if av[i] < bv[i] {
            return -1;
        }
        if av[i] > bv[i] {
            return 1;
        }
    }
    match (adirty, bdirty) {
        (true, false) => -1,
        (false, true) => 1,
        _ => 0,
    }
}

fn parse_version(v: &str) -> ([i32; 3], bool) {
    let v = trim_v(v.trim());
    let v = v.split(['-', '+']).next().unwrap_or(v);
    let mut out = [0; 3];
    let mut dirty = false;
    for (idx, part) in v.split('.').take(3).enumerate() {
        match part.parse::<i32>() {
            Ok(n) => out[idx] = n,
            Err(_) => dirty = true,
        }
    }
    if v.split('.').count() < 3 {
        dirty = true;
    }
    (out, dirty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_update_flags() {
        let args = s(&["quicsync", "update", "--check", "--force", "--to", "0.2.0"]);
        let parsed = parse_update_args(&args).unwrap();
        assert!(parsed.check);
        assert!(parsed.force);
        assert_eq!(parsed.target.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn parse_update_rejects_extra_arg() {
        let args = s(&["quicsync", "update", "now"]);
        assert!(parse_update_args(&args).is_err());
    }

    #[test]
    fn asset_name_matches_release_template() {
        assert_eq!(
            asset_name("linux", "x86_64").as_deref(),
            Some("quicsync_linux_x86_64.tar.gz")
        );
        assert_eq!(
            asset_name("macos", "aarch64").as_deref(),
            Some("quicsync_macos_aarch64.tar.gz")
        );
    }

    #[test]
    fn semver_compare_handles_v_prefix() {
        assert_eq!(compare_semver("0.1.4", "v0.1.5"), -1);
        assert_eq!(compare_semver("v0.2.0", "0.1.9"), 1);
        assert_eq!(compare_semver("v0.1.4", "0.1.4"), 0);
    }

    #[test]
    fn normalize_tag_adds_v() {
        assert_eq!(normalize_tag("0.2.0"), "v0.2.0");
        assert_eq!(normalize_tag("v0.2.0"), "v0.2.0");
    }
}
