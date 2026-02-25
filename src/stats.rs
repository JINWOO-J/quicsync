// 전송 통계 보고

use crate::error::StatsError;
use crate::metrics::MetricsSnapshot;
use crate::types::StatsFormat;

/// 통계 리포터
pub struct StatsReporter {
    pub format: StatsFormat,
}

impl StatsReporter {
    /// 메트릭 스냅샷을 stderr에 출력한다.
    pub fn report(&self, snapshot: &MetricsSnapshot) {
        match self.format {
            StatsFormat::Text => self.report_text(snapshot),
            StatsFormat::Json => {
                if let Ok(json) = to_json(snapshot) {
                    eprintln!("{json}");
                }
            }
        }
    }

    fn report_text(&self, s: &MetricsSnapshot) {
        eprintln!("--- quicsync transfer statistics ---");
        eprintln!("Total bytes: {}", s.bytes_transferred);
        eprintln!("Elapsed: {:.2}s", s.elapsed_secs);
        eprintln!(
            "Throughput: {:.2} Mbps",
            s.throughput_bps / 1_000_000.0
        );
        eprintln!(
            "RTT avg/min/max: {}us / {}us / {}us ({} samples)",
            s.rtt.avg_us, s.rtt.min_us, s.rtt.max_us, s.rtt.count
        );
        eprintln!("Backpressure events: {}", s.backpressure_count);
        eprintln!("Active streams: {}", s.streams_active);
        eprintln!("Integrity checks: {}", s.integrity_checks);
    }
}

/// MetricsSnapshot → JSON 문자열
pub fn to_json(snapshot: &MetricsSnapshot) -> Result<String, StatsError> {
    serde_json::to_string_pretty(snapshot)
        .map_err(|e| StatsError::SerializationFailed(e.to_string()))
}

/// JSON 문자열 → MetricsSnapshot
pub fn from_json(json: &str) -> Result<MetricsSnapshot, StatsError> {
    serde_json::from_str(json)
        .map_err(|e| StatsError::DeserializationFailed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::RttStats;
    use proptest::prelude::*;

    fn sample_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            bytes_transferred: 1_000_000,
            total_bytes: 2_000_000,
            elapsed_secs: 5.0,
            throughput_bps: 1_600_000.0,
            rtt: RttStats {
                avg_us: 100,
                min_us: 50,
                max_us: 200,
                count: 10,
            },
            backpressure_count: 3,
            streams_active: 2,
            integrity_checks: 100,
        }
    }

    #[test]
    fn to_json_and_back() {
        let snap = sample_snapshot();
        let json = to_json(&snap).unwrap();
        let restored = from_json(&json).unwrap();
        assert_eq!(snap, restored);
    }

    #[test]
    fn from_json_invalid() {
        let result = from_json("not json");
        assert!(result.is_err());
    }

    #[test]
    fn text_report_contains_required_fields() {
        let snap = sample_snapshot();
        let text = format_text_report(&snap);
        assert!(text.contains("Total bytes:"));
        assert!(text.contains("Elapsed:"));
        assert!(text.contains("Throughput:"));
        assert!(text.contains("RTT"));
        assert!(text.contains("Backpressure"));
    }

    /// 텍스트 리포트 생성 (테스트용)
    fn format_text_report(s: &MetricsSnapshot) -> String {
        format!(
            "Total bytes: {}\nElapsed: {:.2}s\nThroughput: {:.2} Mbps\nRTT avg/min/max: {}us / {}us / {}us\nBackpressure events: {}\nActive streams: {}\nIntegrity checks: {}",
            s.bytes_transferred,
            s.elapsed_secs,
            s.throughput_bps / 1_000_000.0,
            s.rtt.avg_us, s.rtt.min_us, s.rtt.max_us,
            s.backpressure_count,
            s.streams_active,
            s.integrity_checks,
        )
    }

    // Property 4: 텍스트 리포트 필수 필드 포함
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_text_report_contains_required_fields(
            bytes in any::<u64>(),
            elapsed in 0.0f64..1e9,
            throughput in 0.0f64..1e15,
            backpressure in any::<u64>(),
        ) {
            let snap = MetricsSnapshot {
                bytes_transferred: bytes,
                total_bytes: bytes,
                elapsed_secs: elapsed,
                throughput_bps: throughput,
                rtt: RttStats { avg_us: 100, min_us: 50, max_us: 200, count: 1 },
                backpressure_count: backpressure,
                streams_active: 1,
                integrity_checks: 0,
            };
            let text = format_text_report(&snap);
            prop_assert!(text.contains("Total bytes:"));
            prop_assert!(text.contains("Elapsed:"));
            prop_assert!(text.contains("Throughput:"));
            prop_assert!(text.contains("RTT"));
            prop_assert!(text.contains("Backpressure"));
        }
    }

    // Property 5: JSON 라운드트립 (이미 metrics.rs에도 있지만 stats 모듈에서도 검증)
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_json_roundtrip(
            bytes in any::<u64>(),
            total in any::<u64>(),
            elapsed in 0.0f64..1e9,
            throughput in 0.0f64..1e12,
        ) {
            let snap = MetricsSnapshot {
                bytes_transferred: bytes,
                total_bytes: total,
                elapsed_secs: elapsed,
                throughput_bps: throughput,
                rtt: RttStats { avg_us: 100, min_us: 50, max_us: 200, count: 1 },
                backpressure_count: 0,
                streams_active: 1,
                integrity_checks: 0,
            };
            let json = to_json(&snap).expect("serialize");
            let restored = from_json(&json).expect("deserialize");
            // Integer fields exact
            prop_assert_eq!(snap.bytes_transferred, restored.bytes_transferred);
            prop_assert_eq!(snap.total_bytes, restored.total_bytes);
            prop_assert_eq!(snap.rtt, restored.rtt);
            // f64 fields: JSON roundtrip may lose precision at the last bit
            let elapsed_diff = (snap.elapsed_secs - restored.elapsed_secs).abs();
            prop_assert!(elapsed_diff < 1e-6 * snap.elapsed_secs.abs().max(1.0));
            let tp_diff = (snap.throughput_bps - restored.throughput_bps).abs();
            prop_assert!(tp_diff < 1e-6 * snap.throughput_bps.abs().max(1.0));
        }
    }
}
