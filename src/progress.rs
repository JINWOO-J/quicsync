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
        loop {
            interval.tick().await;

            let snap = self.metrics.snapshot();
            let mode = if self.metrics.transport_mode.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                "QUIC"
            } else {
                "TCP"
            };

            let speed = format_speed(snap.throughput_bps);
            let transferred = format_bytes(snap.bytes_transferred);
            let total = format_bytes(self.metrics.total_bytes.load(std::sync::atomic::Ordering::Relaxed));

            let eta = match self.metrics.eta_secs() {
                Some(secs) => format!("ETA {}", format_eta(secs)),
                None => "ETA --".to_string(),
            };

            eprint!("\r[{}] {} | {} | {} / {}", mode, speed, eta, transferred, total);
        }
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
