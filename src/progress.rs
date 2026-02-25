// 전송 진행률 UI (stderr 출력)

use std::sync::Arc;

use crate::metrics::TransferMetrics;

/// 진행률 UI
pub struct ProgressUI {
    pub metrics: Arc<TransferMetrics>,
    pub enabled: bool,
}

impl ProgressUI {
    /// 500ms 간격으로 stderr에 진행 상황을 갱신한다.
    /// `\r`로 같은 줄을 덮어쓰며, 태스크 종료 시 자동으로 멈춘다.
    pub async fn run(&self) {
        if !self.enabled {
            return;
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;

            let transferred = self.metrics.bytes_transferred.load(std::sync::atomic::Ordering::Relaxed);
            let total = self.metrics.total_bytes.load(std::sync::atomic::Ordering::Relaxed);
            let speed = self.metrics.throughput_bps() / 8.0; // bytes/s
            let eta = self.metrics.eta_secs();

            if total > 0 {
                eprint!(
                    "\r[QUIC] {} | ETA {} | {} / {}    ",
                    format_speed(speed),
                    format_eta(eta),
                    format_bytes(transferred),
                    format_bytes(total),
                );
            } else {
                eprint!(
                    "\r[QUIC] {} | {}    ",
                    format_speed(speed),
                    format_bytes(transferred),
                );
            }
        }
    }
}

/// 바이트 수를 사람이 읽기 좋은 형태로 변환한다.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1_000 {
        format!("{bytes}B")
    } else if bytes < 1_000_000 {
        format!("{:.1}KB", bytes as f64 / 1_000.0)
    } else if bytes < 1_000_000_000 {
        format!("{:.1}MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{:.2}GB", bytes as f64 / 1_000_000_000.0)
    }
}

/// 속도를 사람이 읽기 좋은 형태로 변환한다 (bytes/s 입력).
pub fn format_speed(bytes_per_sec: f64) -> String {
    let bps = bytes_per_sec;
    if bps < 1_000.0 {
        format!("{:.0}B/s", bps)
    } else if bps < 1_000_000.0 {
        format!("{:.1}KB/s", bps / 1_000.0)
    } else if bps < 1_000_000_000.0 {
        format!("{:.1}MB/s", bps / 1_000_000.0)
    } else {
        format!("{:.2}GB/s", bps / 1_000_000_000.0)
    }
}

/// ETA를 "Xh Ym Zs" 형태로 변환한다.
pub fn format_eta(secs: f64) -> String {
    if secs <= 0.0 || !secs.is_finite() {
        return "--:--".to_string();
    }
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- format_bytes 단위 테스트 ---

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0B");
    }

    #[test]
    fn format_bytes_small() {
        assert_eq!(format_bytes(999), "999B");
    }

    #[test]
    fn format_bytes_kb() {
        assert_eq!(format_bytes(1_000), "1.0KB");
        assert_eq!(format_bytes(1_500), "1.5KB");
    }

    #[test]
    fn format_bytes_mb() {
        assert_eq!(format_bytes(1_000_000), "1.0MB");
        assert_eq!(format_bytes(500_000_000), "500.0MB");
    }

    #[test]
    fn format_bytes_gb() {
        assert_eq!(format_bytes(1_000_000_000), "1.00GB");
        assert_eq!(format_bytes(2_500_000_000), "2.50GB");
    }

    // --- format_speed 단위 테스트 ---

    #[test]
    fn format_speed_small() {
        assert_eq!(format_speed(500.0), "500B/s");
    }

    #[test]
    fn format_speed_kb() {
        assert_eq!(format_speed(1_500.0), "1.5KB/s");
    }

    #[test]
    fn format_speed_mb() {
        assert_eq!(format_speed(10_000_000.0), "10.0MB/s");
    }

    #[test]
    fn format_speed_gb() {
        assert_eq!(format_speed(1_500_000_000.0), "1.50GB/s");
    }

    // --- format_eta 단위 테스트 ---

    #[test]
    fn format_eta_zero() {
        assert_eq!(format_eta(0.0), "--:--");
    }

    #[test]
    fn format_eta_seconds() {
        assert_eq!(format_eta(45.0), "45s");
    }

    #[test]
    fn format_eta_minutes() {
        assert_eq!(format_eta(125.0), "2m 5s");
    }

    #[test]
    fn format_eta_hours() {
        assert_eq!(format_eta(3661.0), "1h 1m 1s");
    }

    #[test]
    fn format_eta_nan() {
        assert_eq!(format_eta(f64::NAN), "--:--");
    }

    #[test]
    fn format_eta_infinity() {
        assert_eq!(format_eta(f64::INFINITY), "--:--");
    }

    // Property 1: format_bytes 단위 선택
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_format_bytes_unit_selection(bytes in any::<u64>()) {
            let s = format_bytes(bytes);
            if bytes < 1_000 {
                prop_assert!(s.ends_with("B") && !s.contains("KB"), "expected B for {bytes}: {s}");
            } else if bytes < 1_000_000 {
                prop_assert!(s.ends_with("KB"), "expected KB for {bytes}: {s}");
            } else if bytes < 1_000_000_000 {
                prop_assert!(s.ends_with("MB"), "expected MB for {bytes}: {s}");
            } else {
                prop_assert!(s.ends_with("GB"), "expected GB for {bytes}: {s}");
            }
        }

        #[test]
        fn prop_format_speed_unit_selection(bps in 0.0f64..1e15) {
            let s = format_speed(bps);
            if bps < 1_000.0 {
                prop_assert!(s.ends_with("B/s") && !s.contains("KB"), "expected B/s for {bps}: {s}");
            } else if bps < 1_000_000.0 {
                prop_assert!(s.ends_with("KB/s"), "expected KB/s for {bps}: {s}");
            } else if bps < 1_000_000_000.0 {
                prop_assert!(s.ends_with("MB/s"), "expected MB/s for {bps}: {s}");
            } else {
                prop_assert!(s.ends_with("GB/s"), "expected GB/s for {bps}: {s}");
            }
        }
    }
}
