// rsync 자식 프로세스 실행 및 관리

use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::error::RsyncError;
use crate::types::{RemoteSpec, TransferDirection};

pub struct RsyncChild {
    pub(crate) process: Child,
    /// `--info=progress2`와 `--stats` 출력을 진행 줄·요약으로 바꾸는 task
    output_task: Option<tokio::task::JoinHandle<crate::ui::RsyncReport>>,
}

/// rsync `--log-file` 한 줄에서 추출한 전송 항목.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsyncLogItem {
    /// 일반 파일이면 true (디렉토리/심볼릭 등은 false)
    pub is_file: bool,
    /// 상대 경로 파일명
    pub name: String,
}

/// itemize 토큰인지 판별한다 (11자: update-type 1 + file-type 1 + 속성 9).
fn is_itemize_token(t: &str) -> bool {
    if t.len() != 11 {
        return false;
    }
    let b = t.as_bytes();
    matches!(b[0], b'<' | b'>' | b'c' | b'h' | b'.' | b'*')
        && matches!(b[1], b'f' | b'd' | b'L' | b'D' | b'S')
}

/// rsync `--log-file`(`--log-file-format='%i %n'`) 한 줄을 파싱한다.
///
/// 줄 형식: `YYYY/MM/DD HH:MM:SS [pid] <itemize 11자> <파일명>`
/// itemize(`%i`)는 11자 고정폭이며 속성 위치에 공백이 올 수 있으므로,
/// `[pid] ` 접두 뒤 정확히 11자를 잘라낸다(공백 split에 의존하지 않음).
/// 전송 항목이 아니면(시작/요약 메시지 등) None.
pub fn parse_log_file_line(line: &str) -> Option<RsyncLogItem> {
    // "...[pid] " 접두를 건너뛴다.
    let after_pid = line.split_once("] ")?.1;
    if after_pid.len() < 12 {
        return None;
    }
    let (itemize, rest) = after_pid.split_at(11);
    if !is_itemize_token(itemize) {
        return None;
    }
    let name = rest.strip_prefix(' ')?.trim_end();
    if name.is_empty() {
        return None;
    }
    Some(RsyncLogItem {
        is_file: itemize.as_bytes()[1] == b'f',
        name: name.to_string(),
    })
}

/// `QUICSYNC_DEFAULT_ARGS` 환경변수에서 기본 rsync 옵션을 읽는다.
/// 공백으로 분리하며, 미설정/공백이면 빈 벡터를 반환한다.
/// 코어는 rsync passthrough를 유지하되, 사용자가 매번 `-a` 등을 타이핑하지 않도록
/// 기본 옵션을 환경변수로만 선택 주입하는 용도다(설계 모델 C).
pub fn default_rsync_args_from_env() -> Vec<String> {
    std::env::var("QUICSYNC_DEFAULT_ARGS")
        .ok()
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

/// 디렉토리 전송 플래그가 옵션에 있는지 판별한다.
/// `-a`/`-r`/`-d`(단축 묶음 내부의 a/r/d 포함)와
/// `--archive`/`--recursive`/`--dirs`/`--files-from`을 인식한다.
/// 하나도 없으면 rsync가 디렉토리를 "skipping directory"로 건너뛴다.
pub fn has_recursive_flag(opts: &[String]) -> bool {
    opts.iter().any(|o| {
        if matches!(o.as_str(), "--archive" | "--recursive" | "--dirs")
            || o.starts_with("--files-from")
        {
            return true;
        }
        if o.starts_with("--") {
            return false;
        }
        // 단축 플래그 묶음(`-av`, `-rlt` 등): `=` 앞부분에서 a/r/d 탐색
        match o.strip_prefix('-') {
            Some(short) => short
                .split('=')
                .next()
                .unwrap_or("")
                .chars()
                .any(|c| c == 'a' || c == 'r' || c == 'd'),
            None => false,
        }
    })
}

/// rsync에 전달할 원격 경로 문자열 생성 (`[user@]host:path`)
fn format_remote_spec(remote: &RemoteSpec) -> String {
    match &remote.user {
        Some(user) => format!("{}@{}:{}", user, remote.host, remote.path),
        None => format!("{}:{}", remote.host, remote.path),
    }
}

/// rsync 명령어 인수를 구성한다 (테스트 가능한 순수 함수).
///
/// 반환값: rsync에 전달할 전체 인수 벡터
pub fn build_rsync_args(
    rsync_options: &[String],
    local_paths: &[PathBuf],
    remote: &RemoteSpec,
    proxy_port: u16,
    direction: TransferDirection,
) -> Vec<String> {
    let mut args = Vec::new();

    // --stats가 없으면 자동 추가하여 전송 요약을 항상 표시한다.
    let has_stats = rsync_options.iter().any(|opt| opt == "--stats");
    if !has_stats {
        args.push("--stats".to_string());
    }

    // 사용자 rsync 옵션을 그대로 전달 (Req 7.2)
    args.extend(rsync_options.iter().cloned());

    // --rsh 옵션으로 TCP_Proxy 포트를 통해 연결하도록 리다이렉션 (Req 7.1)
    // rsync는 --rsh 프로그램을 `PROGRAM host rsync --server ...` 형태로 호출한다.
    // quicsync --connect 모드가 localhost:proxy_port에 TCP 연결 후 stdin/stdout relay를 수행한다.
    // current_exe()로 현재 실행 중인 바이너리의 전체 경로를 사용하여
    // PATH에 설치된 다른 버전이 호출되는 문제를 방지한다.
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "quicsync".to_string());
    args.push(format!("--rsh={} --connect {}", exe, proxy_port));

    let remote_spec = format_remote_spec(remote);

    match direction {
        TransferDirection::Push => {
            // rsync [options] --rsh=... <local_paths...> <remote_spec>
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
            args.push(remote_spec);
        }
        TransferDirection::Pull => {
            // rsync [options] --rsh=... <remote_spec> <local_path>
            args.push(remote_spec);
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
        }
    }

    args
}

/// 진행 표시를 켤 수 있으면 `--info=progress2`를 붙이고 true를 돌려준다.
fn plan_progress(args: &mut Vec<String>, rsync_options: &[String], show_progress: bool) -> bool {
    let enabled =
        show_progress && !has_progress_option(rsync_options) && local_rsync_supports_info();
    if enabled {
        args.insert(0, "--info=progress2".to_string());
    }
    enabled
}

/// 사용자가 rsync 진행 출력을 직접 고른 경우 그대로 둔다.
pub(crate) fn has_progress_option(opts: &[String]) -> bool {
    opts.iter().any(|o| {
        o == "--progress"
            || o.starts_with("--info")
            || (o.starts_with('-') && !o.starts_with("--") && o.contains('P'))
    })
}

/// `--info=progress2`는 rsync 3.1부터 있다. macOS 기본 openrsync는 2.6.9 호환이라 지원하지 않는다.
fn local_rsync_supports_info() -> bool {
    std::process::Command::new("rsync")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| parse_rsync_version(&String::from_utf8_lossy(&o.stdout)))
        .is_some_and(|v| v >= (3, 1))
}

/// `rsync --version` 출력에서 (major, minor)를 꺼낸다.
fn parse_rsync_version(text: &str) -> Option<(u32, u32)> {
    // openrsync는 `protocol version 29` 뒤에 `rsync version 2.6.9`를 출력하므로 점이 있는 첫 값을 쓴다.
    let ver = text
        .split("version")
        .skip(1)
        .filter_map(|after| after.split_whitespace().next())
        .find(|v| v.contains('.'))?;
    let mut parts = ver.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// `--info=progress2` 한 조각에서 읽은 전체 진행 상태.
/// 예: `  1,234,567  45%   12.34MB/s    0:00:05 (xfr#12, to-chk=30/100)`
#[derive(Debug, PartialEq, Eq)]
struct Progress2 {
    bytes: u64,
    percent: u8,
    files_done: Option<u64>,
    files_total: Option<u64>,
    /// `ir-chk`: rsync가 아직 파일 목록을 만드는 중이라 전체 파일 수가 늘어날 수 있다.
    scanning: bool,
}

fn parse_progress2(segment: &str) -> Option<Progress2> {
    let mut tokens = segment.split_whitespace();
    let bytes: u64 = tokens.next()?.replace(',', "").parse().ok()?;
    let percent: u8 = tokens.next()?.strip_suffix('%')?.parse().ok()?;
    if !tokens.next()?.ends_with("/s") {
        return None;
    }
    let mut progress = Progress2 {
        bytes,
        percent,
        files_done: None,
        files_total: None,
        scanning: false,
    };
    if let Some(idx) = segment.find("-chk=") {
        progress.scanning = segment[..idx].ends_with("ir");
        let rest = segment[idx + 5..].trim().trim_end_matches(')');
        if let Some((remaining, total)) = rest.split_once('/')
            && let (Ok(remaining), Ok(total)) = (remaining.parse::<u64>(), total.parse::<u64>())
        {
            progress.files_done = Some(total.saturating_sub(remaining));
            progress.files_total = Some(total);
        }
    }
    Some(progress)
}

/// `--stats` 블록의 한 줄을 읽어 report에 반영한다.
fn parse_stats_line(line: &str, report: &mut crate::ui::RsyncReport) {
    let value = |prefix: &str| -> Option<u64> {
        let rest = line.strip_prefix(prefix)?;
        rest.split_whitespace()
            .next()?
            .replace(',', "")
            .parse()
            .ok()
    };
    if let Some(v) = value("Number of regular files transferred:") {
        report.files_transferred = Some(v);
    } else if let Some(v) = value("Total transferred file size:") {
        report.transferred_size = Some(v);
    } else if let Some(v) = value("Total bytes sent:") {
        report.bytes_sent = Some(v);
    } else if let Some(v) = value("Total bytes received:") {
        report.bytes_received = Some(v);
    }
}

/// 사용자가 rsync의 자세한 출력을 원하면 통계 원문도 그대로 보인다.
fn wants_raw_stats(opts: &[String]) -> bool {
    opts.iter().any(|o| {
        o == "--verbose"
            || o.starts_with("--info=stats")
            || (o.starts_with('-') && !o.starts_with("--") && o.contains('v'))
    })
}

/// 진행 줄에 쓰는 최근 처리량. rsync가 표시하는 값은 파일 하나 기준이라 직접 계산한다.
struct RecentSpeed {
    samples: std::collections::VecDeque<(std::time::Instant, u64)>,
}

impl RecentSpeed {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

    fn push(&mut self, now: std::time::Instant, bytes: u64) -> f64 {
        self.samples.push_back((now, bytes));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > Self::WINDOW {
            self.samples.pop_front();
        }
        let (t0, b0) = self.samples[0];
        let secs = now.duration_since(t0).as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            bytes.saturating_sub(b0) as f64 / secs
        }
    }
}

fn progress2_text(label: &str, p: &Progress2, speed: f64, elapsed: f64) -> String {
    let mut text = format!(
        "{label} {}% · {} · {} · {}",
        p.percent,
        crate::progress::format_bytes(p.bytes),
        crate::progress::format_speed(speed),
        crate::progress::format_eta(elapsed)
    );
    if let (Some(done), Some(total)) = (p.files_done, p.files_total) {
        let more = if p.scanning { "+" } else { "" };
        text.push_str(&format!(
            " · 파일 {}/{}{more}",
            crate::ui::group_digits(done),
            crate::ui::group_digits(total)
        ));
    }
    text
}

/// rsync stdout을 읽어 progress2는 진행 줄로 그리고, `--stats` 블록은 요약용으로 읽는다.
/// 통계 원문은 `raw_stats`일 때만 보이고, 그 밖의 출력은 그대로 stdout에 넘긴다.
async fn relay_rsync_output(
    mut stdout: tokio::process::ChildStdout,
    label: &'static str,
    raw_stats: bool,
) -> crate::ui::RsyncReport {
    use std::io::Write;
    use tokio::io::AsyncReadExt;

    let started = std::time::Instant::now();
    let mut line = crate::progress::ProgressLine::default();
    let mut speed = RecentSpeed {
        samples: std::collections::VecDeque::new(),
    };
    let mut report = crate::ui::RsyncReport::default();
    let mut in_stats = false;
    let mut blank_lines = 0usize;
    let mut last_draw: Option<std::time::Instant> = None;
    let mut pending = Vec::new();
    let mut buf = [0u8; 8192];
    let write_out = |bytes: &[u8]| {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    };
    loop {
        let n = match stdout.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        pending.extend_from_slice(&buf[..n]);
        while let Some(pos) = pending.iter().position(|&b| b == b'\r' || b == b'\n') {
            let sep = pending[pos];
            let segment: Vec<u8> = pending.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&segment[..segment.len() - 1]).to_string();
            let trimmed = text.trim();
            if let Some(p) = parse_progress2(&text) {
                let now = std::time::Instant::now();
                let current = speed.push(now, p.bytes);
                if last_draw.is_none_or(|t| now.duration_since(t).as_millis() >= 250) {
                    let elapsed = started.elapsed().as_secs_f64();
                    line.draw_text(&progress2_text(label, &p, current, elapsed));
                    last_draw = Some(now);
                }
                continue;
            }
            if trimmed.starts_with("Number of files:") {
                in_stats = true;
            }
            if in_stats {
                parse_stats_line(trimmed, &mut report);
                let ends = trimmed.starts_with("total size is");
                if raw_stats {
                    line.finish();
                    write_out(&segment);
                }
                if ends {
                    in_stats = false;
                }
                continue;
            }
            if trimmed.is_empty() {
                // 빈 줄은 다음 출력이 있을 때만 내보낸다. 통계를 숨길 때 앞뒤로 빈 줄만 남지 않게 한다.
                if sep == b'\n' {
                    blank_lines += 1;
                }
                continue;
            }
            line.finish();
            for _ in 0..std::mem::take(&mut blank_lines) {
                write_out(b"\n");
            }
            write_out(&segment);
        }
    }
    line.clear();
    if !pending.is_empty() {
        write_out(&pending);
    }
    report
}

/// fallback용 순정 rsync-over-SSH 인수를 구성한다.
pub fn build_direct_rsync_args(
    rsync_options: &[String],
    local_paths: &[PathBuf],
    remote: &RemoteSpec,
    direction: TransferDirection,
) -> Vec<String> {
    let mut args = Vec::new();

    let has_stats = rsync_options.iter().any(|opt| opt == "--stats");
    if !has_stats {
        args.push("--stats".to_string());
    }

    args.extend(rsync_options.iter().cloned());
    let remote_spec = format_remote_spec(remote);

    match direction {
        TransferDirection::Push => {
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
            args.push(remote_spec);
        }
        TransferDirection::Pull => {
            args.push(remote_spec);
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
        }
    }

    args
}

impl RsyncChild {
    /// rsync 자식 프로세스를 실행한다.
    ///
    /// proxy_port를 사용하여 원격 목적지를 로컬 TCP_Proxy로 리다이렉션한다.
    /// stderr를 캡처하여 비정상 종료 시 사용자에게 표시할 수 있도록 한다.
    pub fn spawn(
        rsync_options: &[String],
        local_paths: &[PathBuf],
        remote: &RemoteSpec,
        proxy_port: u16,
        direction: TransferDirection,
        log_path: Option<&std::path::Path>,
        show_progress: bool,
    ) -> Result<Self, RsyncError> {
        let mut args = build_rsync_args(rsync_options, local_paths, remote, proxy_port, direction);
        let summarize = plan_progress(&mut args, rsync_options, show_progress);

        // --web 모니터링용 파일 이벤트 로그. stdout(사용자 출력)은 건드리지 않고
        // 별도 로그 파일에 파일 단위 항목을 기록하게 한다.
        if let Some(p) = log_path {
            args.push(format!("--log-file={}", p.display()));
            args.push("--log-file-format=%i %n".to_string());
        }

        tracing::debug!("rsync spawn: rsync {}", args.join(" "));
        Self::spawn_with_output(&args, summarize, "QUIC", rsync_options)
    }

    fn spawn_with_output(
        args: &[String],
        summarize: bool,
        label: &'static str,
        rsync_options: &[String],
    ) -> Result<Self, RsyncError> {
        let mut process = Command::new("rsync")
            .args(args)
            .stdin(Stdio::null())
            .stdout(if summarize {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RsyncError::SpawnFailed(e.to_string()))?;

        let raw_stats = wants_raw_stats(rsync_options);
        let output_task = if summarize {
            process
                .stdout
                .take()
                .map(|stdout| tokio::spawn(relay_rsync_output(stdout, label, raw_stats)))
        } else {
            None
        };

        Ok(Self {
            process,
            output_task,
        })
    }

    /// 진행 줄과 완료 요약을 rsync 출력에서 만드는지 여부. 아니면 rsync 출력이 그대로 보인다.
    pub fn summarizes(&self) -> bool {
        self.output_task.is_some()
    }

    /// 순정 rsync-over-SSH fallback 프로세스를 실행한다.
    pub fn spawn_direct(
        rsync_options: &[String],
        local_paths: &[PathBuf],
        remote: &RemoteSpec,
        direction: TransferDirection,
        show_progress: bool,
    ) -> Result<Self, RsyncError> {
        let mut args = build_direct_rsync_args(rsync_options, local_paths, remote, direction);
        let summarize = plan_progress(&mut args, rsync_options, show_progress);
        tracing::debug!("rsync fallback spawn: rsync {}", args.join(" "));
        Self::spawn_with_output(&args, summarize, "RSYNC", rsync_options)
    }

    /// rsync 종료를 비동기로 대기하고 종료 코드를 반환한다.
    ///
    /// - 종료 코드 0: Ok(0) 반환
    /// - 종료 코드 != 0: stderr를 출력하고 RsyncError::ExitCode 반환 (Req 7.4)
    /// - 시그널로 종료: stderr를 출력하고 RsyncError::Signal 반환
    pub async fn wait(self) -> Result<i32, RsyncError> {
        self.wait_report().await.0
    }

    /// 종료 코드와 함께, 출력을 요약한 경우 rsync 통계에서 읽은 결과를 돌려준다.
    pub async fn wait_report(self) -> (Result<i32, RsyncError>, Option<crate::ui::RsyncReport>) {
        let output = match self.process.wait_with_output().await {
            Ok(o) => o,
            Err(e) => return (Err(RsyncError::SpawnFailed(e.to_string())), None),
        };
        let report = match self.output_task {
            Some(task) => task.await.ok(),
            None => None,
        };
        (Self::exit_result(&output), report)
    }

    fn exit_result(output: &std::process::Output) -> Result<i32, RsyncError> {
        match output.status.code() {
            Some(0) => Ok(0),
            Some(code) => {
                // 비정상 종료: stderr를 사용자에게 표시 (Req 7.4)
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    eprintln!("{}", stderr);
                }
                Err(RsyncError::ExitCode(code))
            }
            None => {
                // 시그널에 의한 종료
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    eprintln!("{}", stderr);
                }
                Err(RsyncError::Signal)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parse_rsync_version_reads_major_minor() {
        assert_eq!(
            parse_rsync_version("rsync  version 3.5.1  protocol version 33\n"),
            Some((3, 5))
        );
        assert_eq!(
            parse_rsync_version("openrsync: protocol version 29\nrsync version 2.6.9 compatible\n"),
            Some((2, 6))
        );
    }

    #[test]
    fn parse_progress2_reads_bytes_percent_and_files() {
        let p = parse_progress2("    373,031,424  16%   88.30MB/s    0:00:04 (xfr#120, to-chk=30/100)")
            .unwrap();
        assert_eq!(p.bytes, 373_031_424);
        assert_eq!(p.percent, 16);
        assert_eq!((p.files_done, p.files_total), (Some(70), Some(100)));
        assert!(!p.scanning);
        let scanning = parse_progress2("  1,024   0%    1.00kB/s    0:00:00 (xfr#1, ir-chk=1000/1200)")
            .unwrap();
        assert!(scanning.scanning);
        assert_eq!(parse_progress2("Number of files: 48,912"), None);
        assert_eq!(parse_progress2("sent 1,513,918 bytes  received 4,239 bytes"), None);
    }

    #[test]
    fn parse_stats_line_reads_summary_fields() {
        let mut r = crate::ui::RsyncReport::default();
        for l in [
            "Number of regular files transferred: 45,503",
            "Total transferred file size: 2,323,408,940 bytes",
            "Total bytes sent: 1,074,152,008",
            "Total bytes received: 883,337",
            "Literal data: 2,323,408,940 bytes",
        ] {
            parse_stats_line(l, &mut r);
        }
        assert_eq!(r.files_transferred, Some(45_503));
        assert_eq!(r.transferred_size, Some(2_323_408_940));
        assert_eq!(r.bytes_sent, Some(1_074_152_008));
        assert_eq!(r.bytes_received, Some(883_337));
    }

    #[test]
    fn wants_raw_stats_detects_verbose() {
        assert!(wants_raw_stats(&["-avz".to_string()]));
        assert!(wants_raw_stats(&["--info=stats2".to_string()]));
        assert!(!wants_raw_stats(&["-az".to_string()]));
    }

    #[test]
    fn has_progress_option_detects_user_progress() {
        assert!(has_progress_option(&["-avP".to_string()]));
        assert!(has_progress_option(&["--info=progress2".to_string()]));
        assert!(!has_progress_option(&[
            "-az".to_string(),
            "--partial".to_string()
        ]));
    }

    // --- has_recursive_flag 단위 테스트 ---

    #[test]
    fn has_recursive_flag_present() {
        for o in [
            "-a", "-avz", "-r", "-rlt", "-d", "--archive", "--recursive", "--dirs",
            "--files-from=list.txt",
        ] {
            assert!(has_recursive_flag(&[o.to_string()]), "expected true for {o}");
        }
        // 다른 옵션 사이에 섞여 있어도 인식한다
        assert!(has_recursive_flag(&["-v".into(), "--delete".into(), "-a".into()]));
    }

    #[test]
    fn has_recursive_flag_absent() {
        assert!(!has_recursive_flag(&[]));
        for o in [
            "-v", "-vvv", "-z", "-P", "--delete", "--stats", "--progress", "--exclude=*.tmp",
        ] {
            assert!(!has_recursive_flag(&[o.to_string()]), "expected false for {o}");
        }
    }

    // --- parse_log_file_line 단위 테스트 ---

    #[test]
    fn parse_log_line_transferred_file() {
        let line = "2026/05/22 12:34:56 [12345] >f+++++++++ dir/file.txt";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(item.is_file);
        assert_eq!(item.name, "dir/file.txt");
    }

    #[test]
    fn parse_log_line_directory_is_not_file() {
        let line = "2026/05/22 12:34:56 [12345] cd+++++++++ subdir/";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(!item.is_file);
        assert_eq!(item.name, "subdir/");
    }

    #[test]
    fn parse_log_line_preserves_spaces_in_name() {
        let line = "2026/05/22 12:34:56 [99] >f+++++++++ my file with spaces.bin";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(item.is_file);
        assert_eq!(item.name, "my file with spaces.bin");
    }

    #[test]
    fn parse_log_line_unchanged_file_still_counts() {
        // 변경 없는 파일(.f.........)도 처리 대상이므로 파일로 센다. (itemize는 11자 고정폭)
        let line = "2026/05/22 12:34:56 [1] .f......... unchanged.txt";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(item.is_file);
        assert_eq!(item.name, "unchanged.txt");
    }

    #[test]
    fn parse_log_line_non_transfer_message_is_none() {
        assert!(parse_log_file_line("2026/05/22 12:34:56 [1] building file list").is_none());
        assert!(parse_log_file_line("2026/05/22 12:34:56 [1] sent 1000 bytes").is_none());
        assert!(parse_log_file_line("").is_none());
    }

    // Feature: quicsync-tunnel-mvp, Property 9: rsync 명령어 구성 정확성
    // **Validates: Requirements 7.1, 7.2**

    /// 영문 소문자+숫자로 구성된 비어있지 않은 문자열 생성기
    fn alphanumeric_str(min: usize, max: usize) -> impl Strategy<Value = String> {
        prop::string::string_regex(&format!("[a-z0-9]{{{},{}}}", min, max)).expect("valid regex")
    }

    /// rsync 옵션 생성기: `-` 로 시작하는 문자열
    fn rsync_option() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("-a".to_string()),
            Just("-v".to_string()),
            Just("-z".to_string()),
            Just("-r".to_string()),
            Just("--delete".to_string()),
            Just("--progress".to_string()),
            Just("--exclude=*.tmp".to_string()),
            Just("--compress".to_string()),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Property 9: 임의의 유효한 입력에 대해 build_rsync_args가 구성하는 인수가
        /// (1) 모든 사용자 옵션을 순서대로 포함하고
        /// (2) --rsh에 올바른 포트가 포함되며
        /// (3) remote spec이 올바르게 포맷되어야 한다.
        #[test]
        fn rsync_args_construction_correctness(
            user in prop::option::of(alphanumeric_str(1, 8)),
            host in alphanumeric_str(1, 12),
            remote_path in alphanumeric_str(1, 10).prop_map(|s| format!("/{}", s)),
            local_path in alphanumeric_str(1, 10).prop_map(|s| format!("/{}", s)),
            port in 1u16..=65535u16,
            direction in prop_oneof![
                Just(TransferDirection::Push),
                Just(TransferDirection::Pull),
            ],
            options in prop::collection::vec(rsync_option(), 0..6),
        ) {
            let remote = RemoteSpec {
                user: user.clone(),
                host: host.clone(),
                path: remote_path.clone(),
            };

            let args = build_rsync_args(
                &options,
                &[PathBuf::from(&local_path)],
                &remote,
                port,
                direction,
            );

            // --stats 자동 추가 여부 판별
            let has_stats = options.iter().any(|opt| opt == "--stats");
            let auto_prefix: usize = !has_stats as usize;
            let n = options.len();

            // (1) 사용자 옵션이 순서대로 args에 위치 (auto_prefix 오프셋 적용)
            for (i, opt) in options.iter().enumerate() {
                prop_assert_eq!(&args[auto_prefix + i], opt, "option at index {} mismatch", i);
            }

            // (2) --rsh 옵션이 사용자 옵션 바로 뒤에 위치하며 올바른 포트 포함
            let rsh = &args[auto_prefix + n];
            let expected_suffix = format!(" --connect {}", port);
            prop_assert!(rsh.starts_with("--rsh="), "rsh should start with --rsh=, got: {}", rsh);
            prop_assert!(rsh.ends_with(&expected_suffix), "rsh should end with '{}', got: {}", expected_suffix, rsh);

            // (3) remote spec 포맷 검증
            let expected_remote = match &user {
                Some(u) => format!("{}@{}:{}", u, host, remote_path),
                None => format!("{}:{}", host, remote_path),
            };

            // (4) 방향에 따른 인수 순서 검증
            match direction {
                TransferDirection::Push => {
                    prop_assert_eq!(&args[auto_prefix + n + 1], &local_path);
                    prop_assert_eq!(&args[auto_prefix + n + 2], &expected_remote);
                }
                TransferDirection::Pull => {
                    prop_assert_eq!(&args[auto_prefix + n + 1], &expected_remote);
                    prop_assert_eq!(&args[auto_prefix + n + 2], &local_path);
                }
            }

            // 총 인수 개수: (auto_prefix) + options + --rsh + local + remote
            prop_assert_eq!(args.len(), auto_prefix + n + 3);
        }
    }

    #[test]
    fn build_args_push_with_user() {
        let remote = RemoteSpec {
            user: Some("deploy".into()),
            host: "server.example.com".into(),
            path: "/data/backup".into(),
        };
        let args = build_rsync_args(
            &["-avz".into(), "--delete".into()],
            &[PathBuf::from("/local/src")],
            &remote,
            12345,
            TransferDirection::Push,
        );

        // -avz에 'a' 포함 → -a 자동 추가 안 됨, --stats 자동 추가됨
        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "-avz");
        assert_eq!(args[2], "--delete");
        assert!(args[3].starts_with("--rsh=") && args[3].ends_with(" --connect 12345"));
        assert_eq!(args[4], "/local/src");
        assert_eq!(args[5], "deploy@server.example.com:/data/backup");
    }

    #[test]
    fn build_args_pull_without_user() {
        let remote = RemoteSpec {
            user: None,
            host: "10.0.0.1".into(),
            path: "/remote/files".into(),
        };
        let args = build_rsync_args(
            &["-r".into()],
            &[PathBuf::from("/local/dest")],
            &remote,
            54321,
            TransferDirection::Pull,
        );

        // -r에 'r' 포함 → -a 자동 추가 안 됨, --stats 자동 추가됨
        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "-r");
        assert!(args[2].starts_with("--rsh=") && args[2].ends_with(" --connect 54321"));
        // Pull: remote first, then local
        assert_eq!(args[3], "10.0.0.1:/remote/files");
        assert_eq!(args[4], "/local/dest");
    }

    #[test]
    fn build_args_no_options() {
        let remote = RemoteSpec {
            user: None,
            host: "host".into(),
            path: "/path".into(),
        };
        let args = build_rsync_args(
            &[],
            &[PathBuf::from("/local")],
            &remote,
            8080,
            TransferDirection::Push,
        );

        // 옵션 없으면 --stats만 자동 추가된다
        assert_eq!(args[0], "--stats");
        assert!(args[1].starts_with("--rsh=") && args[1].ends_with(" --connect 8080"));
        assert_eq!(args[2], "/local");
        assert_eq!(args[3], "host:/path");
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn build_direct_args_push_has_no_rsh() {
        let remote = RemoteSpec {
            user: Some("deploy".into()),
            host: "server".into(),
            path: "/dst".into(),
        };
        let args = build_direct_rsync_args(
            &["-a".into()],
            &[PathBuf::from("/src")],
            &remote,
            TransferDirection::Push,
        );

        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "-a");
        assert_eq!(args[2], "/src");
        assert_eq!(args[3], "deploy@server:/dst");
        assert!(!args.iter().any(|a| a.starts_with("--rsh=")));
    }

    #[test]
    fn build_direct_args_pull_has_no_rsh() {
        let remote = RemoteSpec {
            user: None,
            host: "server".into(),
            path: "/src".into(),
        };
        let args = build_direct_rsync_args(
            &[],
            &[PathBuf::from("/dst")],
            &remote,
            TransferDirection::Pull,
        );

        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "server:/src");
        assert_eq!(args[2], "/dst");
        assert!(!args.iter().any(|a| a.starts_with("--rsh=")));
    }

    #[test]
    fn build_args_preserves_all_options() {
        let remote = RemoteSpec {
            user: Some("root".into()),
            host: "box".into(),
            path: "/".into(),
        };
        let options: Vec<String> = vec![
            "-a",
            "-v",
            "-z",
            "--delete",
            "--exclude=*.tmp",
            "--progress",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let args = build_rsync_args(
            &options,
            &[PathBuf::from("/src")],
            &remote,
            9999,
            TransferDirection::Push,
        );

        // -a 포함 → -a 자동 추가 안 됨, --stats 자동 추가됨 (인덱스 +1)
        // 모든 사용자 옵션이 순서대로 보존되어야 한다 (Req 7.2)
        assert_eq!(args[0], "--stats");
        for (i, opt) in options.iter().enumerate() {
            assert_eq!(&args[i + 1], opt);
        }
        // --rsh 옵션이 사용자 옵션 뒤에 위치
        assert!(
            args[options.len() + 1].starts_with("--rsh=")
                && args[options.len() + 1].ends_with(" --connect 9999")
        );
    }

    #[test]
    fn format_remote_spec_with_user() {
        let remote = RemoteSpec {
            user: Some("admin".into()),
            host: "srv".into(),
            path: "/data".into(),
        };
        assert_eq!(format_remote_spec(&remote), "admin@srv:/data");
    }

    #[test]
    fn format_remote_spec_without_user() {
        let remote = RemoteSpec {
            user: None,
            host: "srv".into(),
            path: "/data".into(),
        };
        assert_eq!(format_remote_spec(&remote), "srv:/data");
    }

    // Feature: quicsync-tunnel-mvp, Property 10: 종료 코드 전파
    // **Validates: Requirements 7.3, 7.4**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Property 10: 임의의 u8 종료 코드(0–255)에 대해,
        /// 자식 프로세스가 해당 코드로 종료하면 wait()가 올바르게 전파해야 한다.
        /// - 코드 0: Ok(0)
        /// - 코드 1–255: Err(RsyncError::ExitCode(code))
        #[test]
        fn exit_code_propagation(code in 0u8..=255u8) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let process = Command::new("sh")
                    .arg("-c")
                    .arg(format!("exit {}", code))
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("failed to spawn sh");

                let child = RsyncChild {
                    process,
                    output_task: None,
                };
                let result = child.wait().await;
                let code_i32 = code as i32;

                match code {
                    0 => {
                        prop_assert!(result.is_ok(), "exit 0 should return Ok, got {:?}", result);
                        prop_assert_eq!(result.unwrap(), 0);
                    }
                    _ => {
                        prop_assert!(result.is_err(), "exit {} should return Err, got {:?}", code, result);
                        match result.unwrap_err() {
                            RsyncError::ExitCode(c) => {
                                prop_assert_eq!(c, code_i32, "exit code mismatch: expected {}, got {}", code_i32, c);
                            }
                            other => {
                                prop_assert!(false, "expected ExitCode({}), got {:?}", code_i32, other);
                            }
                        }
                    }
                }

                Ok::<_, proptest::test_runner::TestCaseError>(())
            })?;
        }
    }
}
