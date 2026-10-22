// Progress UI - 전송 상태를 stderr에 실시간 표시

use std::sync::Arc;

use crate::metrics::TransferMetrics;

pub struct ProgressUI {
    metrics: Arc<TransferMetrics>,
    enabled: bool,
}

impl ProgressUI {
    pub fn new(metrics: Arc<TransferMetrics>, enabled: bool) -> Self {
        Self { metrics, enabled }
    }

    /// 500ms 주기로 stderr에 상태를 갱신하는 루프 (tokio task로 실행)
    pub async fn run(&self) {
        if !self.enabled {
            return;
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        let mut line = ProgressLine::default();
        loop {
            interval.tick().await;

            let snap = self.metrics.snapshot();
            let mode = if self
                .metrics
                .transport_mode
                .load(std::sync::atomic::Ordering::Relaxed)
                == 0
            {
                "QUIC"
            } else {
                "TCP"
            };

            line.draw(
                mode,
                snap.throughput_bps,
                snap.bytes_transferred,
                self.metrics.start_time.elapsed().as_secs_f64(),
            );
        }
    }
}

/// stderr 한 줄을 `\r`로 덮어쓰며 진행 상태를 그린다.
#[derive(Default)]
pub struct ProgressLine {
    prev_len: usize,
    frame: usize,
}

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

impl ProgressLine {
    /// QUIC 경로에서 rsync 진행 정보를 쓸 수 없을 때 터널 바이트로 그린다.
    pub fn draw(&mut self, mode: &str, speed_bps: f64, bytes: u64, elapsed_secs: f64) {
        self.draw_text(&format!(
            "{mode} {} · {} · {}",
            format_bytes(bytes),
            format_speed(speed_bps),
            format_eta(elapsed_secs)
        ));
    }

    pub fn draw_text(&mut self, text: &str) {
        let spinner = SPINNER[self.frame % SPINNER.len()];
        self.frame += 1;
        let mut line: String = format!("{spinner} {text}");
        // 터미널 폭을 넘으면 줄이 바뀌어 \r 덮어쓰기가 깨지므로 폭보다 한 칸 짧게 자른다.
        if let Some(width) = crate::ui::terminal_width() {
            line = line.chars().take(width.saturating_sub(1)).collect();
        }
        let len = line.chars().count();
        // 이전 줄이 더 길었으면 남은 글자를 공백으로 덮는다. 줄 지우기 escape(\x1b[2K)는 일부 터미널에서 줄이 보이지 않았다.
        let pad = self.prev_len.saturating_sub(len);
        self.prev_len = len;
        self.write(&format!("\r{line}{}", " ".repeat(pad)));
    }

    pub fn is_drawn(&self) -> bool {
        self.prev_len > 0
    }

    /// 진행 줄을 남겨 둔 채 줄을 바꾼다. 이후 출력이 진행 줄 뒤에 붙지 않게 한다.
    pub fn finish(&mut self) {
        if self.prev_len > 0 {
            self.write("\n");
            self.prev_len = 0;
        }
    }

    /// 진행 줄을 공백으로 덮어 지운다. 완료 요약이 그 자리에 나온다.
    pub fn clear(&mut self) {
        if self.prev_len > 0 {
            self.write(&format!("\r{}\r", " ".repeat(self.prev_len)));
            self.prev_len = 0;
        }
    }

    /// 줄 전체를 한 번의 write로 내보낸다.
    fn write(&self, text: &str) {
        let mut err = std::io::stderr().lock();
        let _ = std::io::Write::write_all(&mut err, text.as_bytes());
        let _ = std::io::Write::flush(&mut err);
    }
}

/// 바이트 수를 사람이 읽기 쉬운 단위로 변환 (SI 1000-based)
/// 예: 1536 → "1.5 KB", 1073741824 → "1.1 GB"
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1000 {
        format!("{} B", bytes)
    } else if bytes < 1_000_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else if bytes < 1_000_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    }
}

/// 전송 속도를 사람이 읽기 쉬운 단위로 변환 (SI 1000-based)
/// 예: 1536.0 → "1.5 KB/s", 1073741824.0 → "1.1 GB/s"
pub fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1000.0 {
        format!("{:.1} B/s", bytes_per_sec)
    } else if bytes_per_sec < 1_000_000.0 {
        format!("{:.1} KB/s", bytes_per_sec / 1_000.0)
    } else if bytes_per_sec < 1_000_000_000.0 {
        format!("{:.1} MB/s", bytes_per_sec / 1_000_000.0)
    } else {
        format!("{:.1} GB/s", bytes_per_sec / 1_000_000_000.0)
    }
}

/// 초를 사람이 읽기 쉬운 시간으로 변환
/// 예: 133.0 → "2m 13s", 3661.0 → "1h 1m 1s"
pub fn format_eta(secs: f64) -> String {
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- format_bytes ---

    #[test]
    fn test_format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn test_format_bytes_boundary_b() {
        assert_eq!(format_bytes(999), "999 B");
    }

    #[test]
    fn test_format_bytes_boundary_kb() {
        assert_eq!(format_bytes(1000), "1.0 KB");
        assert_eq!(format_bytes(999_999), "1000.0 KB");
    }

    #[test]
    fn test_format_bytes_boundary_mb() {
        assert_eq!(format_bytes(1_000_000), "1.0 MB");
        assert_eq!(format_bytes(999_999_999), "1000.0 MB");
    }

    #[test]
    fn test_format_bytes_gb() {
        assert_eq!(format_bytes(1_000_000_000), "1.0 GB");
        assert_eq!(format_bytes(1_500_000_000), "1.5 GB");
    }

    #[test]
    fn test_format_bytes_example_1536() {
        assert_eq!(format_bytes(1536), "1.5 KB");
    }

    // --- format_speed ---

    #[test]
    fn test_format_speed_zero() {
        assert_eq!(format_speed(0.0), "0.0 B/s");
    }

    #[test]
    fn test_format_speed_boundary_bs() {
        assert_eq!(format_speed(999.0), "999.0 B/s");
    }

    #[test]
    fn test_format_speed_boundary_kbs() {
        assert_eq!(format_speed(1000.0), "1.0 KB/s");
    }

    #[test]
    fn test_format_speed_boundary_mbs() {
        assert_eq!(format_speed(1_000_000.0), "1.0 MB/s");
    }

    #[test]
    fn test_format_speed_gbs() {
        assert_eq!(format_speed(1_000_000_000.0), "1.0 GB/s");
    }

    #[test]
    fn test_format_speed_example_1536() {
        assert_eq!(format_speed(1536.0), "1.5 KB/s");
    }

    // --- format_eta ---

    #[test]
    fn test_format_eta_zero() {
        assert_eq!(format_eta(0.0), "0s");
    }

    #[test]
    fn test_format_eta_seconds_only() {
        assert_eq!(format_eta(45.0), "45s");
    }

    #[test]
    fn test_format_eta_minutes_and_seconds() {
        assert_eq!(format_eta(133.0), "2m 13s");
    }

    #[test]
    fn test_format_eta_hours() {
        assert_eq!(format_eta(3661.0), "1h 1m 1s");
    }

    #[test]
    fn test_format_eta_exact_hour() {
        assert_eq!(format_eta(3600.0), "1h 0m 0s");
    }

    #[test]
    fn test_format_eta_exact_minute() {
        assert_eq!(format_eta(60.0), "1m 0s");
    }

    // Feature: quicsync-phase2-enhancements, Property 1: 바이트/속도 포맷 함수 정확성
    // **Validates: Requirements 2.8, 2.9**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_format_bytes_unit_selection(bytes in any::<u64>()) {
            let result = format_bytes(bytes);
            if bytes < 1000 {
                prop_assert!(result.ends_with(" B"), "expected B suffix for {}, got {}", bytes, result);
            } else if bytes < 1_000_000 {
                prop_assert!(result.ends_with(" KB"), "expected KB suffix for {}, got {}", bytes, result);
            } else if bytes < 1_000_000_000 {
                prop_assert!(result.ends_with(" MB"), "expected MB suffix for {}, got {}", bytes, result);
            } else {
                prop_assert!(result.ends_with(" GB"), "expected GB suffix for {}, got {}", bytes, result);
            }
        }

        #[test]
        fn prop_format_speed_unit_selection(bps in 0.0f64..10_000_000_000.0f64) {
            let result = format_speed(bps);
            prop_assert!(result.ends_with("/s"), "expected /s suffix, got {}", result);
            if bps < 1000.0 {
                prop_assert!(result.ends_with(" B/s"), "expected B/s for {}, got {}", bps, result);
            } else if bps < 1_000_000.0 {
                prop_assert!(result.ends_with(" KB/s"), "expected KB/s for {}, got {}", bps, result);
            } else if bps < 1_000_000_000.0 {
                prop_assert!(result.ends_with(" MB/s"), "expected MB/s for {}, got {}", bps, result);
            } else {
                prop_assert!(result.ends_with(" GB/s"), "expected GB/s for {}, got {}", bps, result);
            }
        }
    }
}
