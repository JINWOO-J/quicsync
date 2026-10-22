// 사용자에게 보이는 stderr 출력(시작 줄, 경로, 경고, 완료 요약)의 형식을 한곳에서 정한다.

use std::io::IsTerminal;
use std::path::PathBuf;

use crate::types::{RemoteSpec, TransferDirection};

/// 색은 stderr가 터미널이고 NO_COLOR가 없을 때만 쓴다. 파이프·로그 파일에는 평문을 남긴다.
pub fn color_enabled() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn paint(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// 홈 디렉토리를 `~`로 줄여 경로를 짧게 보인다.
fn display_local(paths: &[PathBuf]) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let shorten = |p: &PathBuf| {
        let s = p.display().to_string();
        match s.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => s,
        }
    };
    match paths {
        [one] => shorten(one),
        _ => format!("{}개 경로", paths.len()),
    }
}

fn display_remote(remote: &RemoteSpec) -> String {
    match &remote.user {
        Some(u) => format!("{u}@{}:{}", remote.host, remote.path),
        None => format!("{}:{}", remote.host, remote.path),
    }
}

/// `quicsync 0.6.1 · push ~/src → user@host:/dst`처럼 데이터가 흐르는 방향대로 보인다.
pub fn header(direction: TransferDirection, local_paths: &[PathBuf], remote: &RemoteSpec) {
    let local = display_local(local_paths);
    let remote = display_remote(remote);
    let (label, from, to) = match direction {
        TransferDirection::Push => ("push", local, remote),
        TransferDirection::Pull => ("pull", remote, local),
    };
    eprintln!(
        "{} · {label} {from} → {to}",
        paint("1", &format!("quicsync {}", env!("CARGO_PKG_VERSION")))
    );
}

pub fn route(text: &str) {
    eprintln!("{} {text}", paint("2", "경로:"));
}

pub fn info(text: &str) {
    eprintln!("{} {text}", paint("2", "·"));
}

pub fn warn(text: &str) {
    eprintln!("{} {text}", paint("33", "⚠ 경고:"));
}

pub fn error(text: &str) {
    eprintln!("{} {text}", paint("31", "✗ 오류:"));
}

/// rsync `--stats`에서 읽은 전송 결과. 읽지 못한 항목은 None이다.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RsyncReport {
    pub files_transferred: Option<u64>,
    pub transferred_size: Option<u64>,
    pub bytes_sent: Option<u64>,
    pub bytes_received: Option<u64>,
}

/// 완료 줄: `✓ 완료 · 21.9s · 파일 45,503개 · 2.3 GB (전송 1.1 GB, 압축 2.2배) · 평균 49.0 MB/s · QUIC`
pub fn finish_line(
    code: i32,
    elapsed_secs: f64,
    transport: &str,
    direction: TransferDirection,
    report: Option<&RsyncReport>,
) -> String {
    let elapsed = format!("{elapsed_secs:.1}s");
    if code != 0 {
        return format!(
            "{} · rsync 종료 코드 {code} · {elapsed} · {transport}",
            paint("31", "✗ 실패")
        );
    }
    let ok = paint("32", "✓ 완료");
    let Some(r) = report else {
        return format!("{ok} · {elapsed} · {transport}");
    };
    let mut parts = vec![ok, elapsed];
    if r.files_transferred == Some(0) {
        parts.push("변경 없음".to_string());
    } else if let Some(n) = r.files_transferred {
        parts.push(format!("파일 {}개", group_digits(n)));
    }
    let wire = match direction {
        TransferDirection::Push => r.bytes_sent,
        TransferDirection::Pull => r.bytes_received,
    };
    if let (Some(size), Some(wire)) = (r.transferred_size, wire)
        && size > 0
    {
        let ratio = size as f64 / wire.max(1) as f64;
        // 압축이 거의 없으면 비율을 생략해 줄을 짧게 유지한다.
        if ratio >= 1.05 {
            parts.push(format!(
                "{} (전송 {}, 압축 {ratio:.1}배)",
                crate::progress::format_bytes(size),
                crate::progress::format_bytes(wire)
            ));
        } else {
            parts.push(crate::progress::format_bytes(size));
        }
        parts.push(format!(
            "평균 {}",
            crate::progress::format_speed(wire as f64 / elapsed_secs.max(0.001))
        ));
    }
    parts.push(transport.to_string());
    parts.join(" · ")
}

pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// stderr 터미널 폭. 진행 줄이 폭을 넘으면 줄이 바뀌어 `\r` 덮어쓰기가 깨지므로 이 폭에 맞춰 자른다.
pub fn terminal_width() -> Option<usize> {
    use std::os::fd::AsRawFd;

    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: stderr fd와 winsize 구조체 포인터를 넘기는 TIOCGWINSZ 조회만 한다.
    let rc = unsafe { libc::ioctl(std::io::stderr().as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
    (rc == 0 && ws.ws_col > 0).then_some(ws.ws_col as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> RsyncReport {
        RsyncReport {
            files_transferred: Some(45_503),
            transferred_size: Some(2_323_408_940),
            bytes_sent: Some(1_074_152_008),
            bytes_received: Some(883_337),
        }
    }

    #[test]
    fn group_digits_inserts_commas() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(45_503), "45,503");
        assert_eq!(group_digits(1_074_152_008), "1,074,152,008");
    }

    #[test]
    fn finish_line_summarizes_push() {
        let line = finish_line(0, 21.9, "QUIC", TransferDirection::Push, Some(&report()));
        assert!(line.contains("21.9s"), "{line}");
        assert!(line.contains("파일 45,503개"), "{line}");
        assert!(line.contains("2.3 GB (전송 1.1 GB, 압축 2.2배)"), "{line}");
        assert!(line.ends_with("QUIC"), "{line}");
    }

    #[test]
    fn finish_line_reports_no_change() {
        let r = RsyncReport {
            files_transferred: Some(0),
            transferred_size: Some(0),
            bytes_sent: Some(1_513_918),
            bytes_received: Some(4_239),
        };
        let line = finish_line(0, 0.8, "QUIC", TransferDirection::Push, Some(&r));
        assert!(line.contains("변경 없음"), "{line}");
        assert!(!line.contains("평균"), "{line}");
    }

    #[test]
    fn finish_line_reports_failure_code() {
        let line = finish_line(12, 0.6, "QUIC", TransferDirection::Pull, None);
        assert!(line.contains("rsync 종료 코드 12"), "{line}");
    }
}
