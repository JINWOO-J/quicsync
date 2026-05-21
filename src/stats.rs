// Stats Reporter - 전송 완료 후 성능 리포트 출력

use crate::metrics::MetricsSnapshot;
use crate::types::StatsFormat;

/// 성능 리포트 출력기
pub struct StatsReporter {
    format: StatsFormat,
}

impl StatsReporter {
    pub fn new(format: StatsFormat) -> Self {
        Self { format }
    }

    /// 성능 리포트를 stderr에 출력
    pub fn report(&self, snapshot: &MetricsSnapshot) {
        match self.format {
            StatsFormat::Text => self.report_text(snapshot),
            StatsFormat::Json => self.report_json(snapshot),
        }
    }

    fn report_text(&self, s: &MetricsSnapshot) {
        eprint!("{}", format_text_report(s));
    }

    fn report_json(&self, snapshot: &MetricsSnapshot) {
        let json = to_json(snapshot);
        eprintln!("{}", json);
    }
}

/// MetricsSnapshot을 텍스트 리포트 문자열로 포맷
pub fn format_text_report(s: &MetricsSnapshot) -> String {
    let mut report = format!(
        "--- Transfer Statistics ---\n\
         Total bytes transferred: {}\n\
         Average throughput: {:.2} bytes/sec\n\
         Duration: {:.3} seconds\n",
        s.bytes_transferred, s.throughput_bps, s.duration_secs,
    );
    if s.rtt.avg_us > 0.0 || s.rtt.min_us > 0 || s.rtt.max_us > 0 {
        report.push_str(&format!(
            "RTT avg: {:.1} us, min: {} us, max: {} us\n",
            s.rtt.avg_us, s.rtt.min_us, s.rtt.max_us,
        ));
    }
    if s.max_queue_depth > 0 {
        report.push_str(&format!("Max queue depth: {}\n", s.max_queue_depth));
    }
    if s.backpressure_count > 0 {
        report.push_str(&format!("Backpressure count: {}\n", s.backpressure_count));
    }
    report
}

/// MetricsSnapshot을 JSON 문자열로 직렬화
pub fn to_json(snapshot: &MetricsSnapshot) -> String {
    serde_json::to_string_pretty(snapshot).expect("MetricsSnapshot serialization should not fail")
}

/// JSON 문자열을 MetricsSnapshot으로 역직렬화
pub fn from_json(json: &str) -> Result<MetricsSnapshot, serde_json::Error> {
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::RttStats;

    fn sample_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            bytes_transferred: 3_670_016_000,
            duration_secs: 81.2,
            throughput_bps: 45_185_000.0,
            rtt: RttStats {
                avg_us: 45200.0,
                min_us: 12000,
                max_us: 98000,
            },
            max_queue_depth: 128,
            backpressure_count: 3,
            streams_completed: 42,
            streams_failed: 0,
            integrity_chunks: 56000,
            integrity_bytes: 3_670_016_000,
        }
    }

    #[test]
    fn test_to_json_produces_valid_json() {
        let snap = sample_snapshot();
        let json = to_json(&snap);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["bytes_transferred"], 3_670_016_000u64);
        assert_eq!(parsed["max_queue_depth"], 128);
    }

    #[test]
    fn test_from_json_roundtrip() {
        let snap = sample_snapshot();
        let json = to_json(&snap);
        let restored = from_json(&json).unwrap();
        assert_eq!(snap, restored);
    }

    #[test]
    fn test_from_json_invalid() {
        let result = from_json("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_stats_reporter_new() {
        let reporter = StatsReporter::new(StatsFormat::Text);
        assert_eq!(reporter.format, StatsFormat::Text);

        let reporter = StatsReporter::new(StatsFormat::Json);
        assert_eq!(reporter.format, StatsFormat::Json);
    }

    #[test]
    fn test_report_text_does_not_panic() {
        let reporter = StatsReporter::new(StatsFormat::Text);
        let snap = sample_snapshot();
        // report() writes to stderr; just verify it doesn't panic
        reporter.report(&snap);
    }

    #[test]
    fn test_report_json_does_not_panic() {
        let reporter = StatsReporter::new(StatsFormat::Json);
        let snap = sample_snapshot();
        reporter.report(&snap);
    }

    #[test]
    fn test_zero_values_snapshot() {
        let snap = MetricsSnapshot {
            bytes_transferred: 0,
            duration_secs: 0.0,
            throughput_bps: 0.0,
            rtt: RttStats {
                avg_us: 0.0,
                min_us: 0,
                max_us: 0,
            },
            max_queue_depth: 0,
            backpressure_count: 0,
            streams_completed: 0,
            streams_failed: 0,
            integrity_chunks: 0,
            integrity_bytes: 0,
        };
        let json = to_json(&snap);
        let restored = from_json(&json).unwrap();
        assert_eq!(snap, restored);
    }

    #[test]
    fn test_text_report_omits_placeholder_fields_without_samples() {
        let snap = MetricsSnapshot {
            bytes_transferred: 0,
            duration_secs: 0.0,
            throughput_bps: 0.0,
            rtt: RttStats {
                avg_us: 0.0,
                min_us: 0,
                max_us: 0,
            },
            max_queue_depth: 0,
            backpressure_count: 0,
            streams_completed: 0,
            streams_failed: 0,
            integrity_chunks: 0,
            integrity_bytes: 0,
        };
        let report = format_text_report(&snap);
        assert!(!report.contains("RTT avg"));
        assert!(!report.contains("Max queue depth"));
        assert!(!report.contains("Backpressure count"));
    }

    use proptest::prelude::*;

    fn arb_metrics_snapshot() -> impl Strategy<Value = MetricsSnapshot> {
        (
            any::<u64>(),                                          // bytes_transferred
            (0u64..1_000_000_000).prop_map(|v| v as f64 / 1000.0), // duration_secs: 0..999999.999
            (0u64..1_000_000_000).prop_map(|v| v as f64 / 1000.0), // throughput_bps: 0..999999.999
            (0u64..1_000_000_000).prop_map(|v| v as f64 / 1000.0), // rtt avg_us: 0..999999.999
            any::<u64>(),                                          // rtt min_us
            any::<u64>(),                                          // rtt max_us
            any::<u64>(),                                          // max_queue_depth
            any::<u64>(),                                          // backpressure_count
            any::<u64>(),                                          // streams_completed
            any::<u64>(),                                          // streams_failed
            any::<u64>(),                                          // integrity_chunks
            any::<u64>(),                                          // integrity_bytes
        )
            .prop_map(
                |(bt, ds, tp, ra, ri, rx, mq, bp, sc, sf, ic, ib)| MetricsSnapshot {
                    bytes_transferred: bt,
                    duration_secs: ds,
                    throughput_bps: tp,
                    rtt: RttStats {
                        avg_us: ra,
                        min_us: ri,
                        max_us: rx,
                    },
                    max_queue_depth: mq,
                    backpressure_count: bp,
                    streams_completed: sc,
                    streams_failed: sf,
                    integrity_chunks: ic,
                    integrity_bytes: ib,
                },
            )
    }

    // Feature: quicsync-phase2-enhancements, Property 4: 텍스트 리포트 필수 필드 포함
    // **Validates: Requirements 4.2, 4.3, 4.4**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_text_report_contains_required_fields(snap in arb_metrics_snapshot()) {
            let report = format_text_report(&snap);

            // 4.2: 총 전송 바이트, 평균 처리량, 소요 시간
            prop_assert!(report.contains(&snap.bytes_transferred.to_string()),
                "missing bytes_transferred: {}", snap.bytes_transferred);
            prop_assert!(report.contains(&format!("{:.2}", snap.throughput_bps)),
                "missing throughput_bps: {:.2}", snap.throughput_bps);
            prop_assert!(report.contains(&format!("{:.3}", snap.duration_secs)),
                "missing duration_secs: {:.3}", snap.duration_secs);

            // 4.3: RTT avg/min/max
            if snap.rtt.avg_us > 0.0 || snap.rtt.min_us > 0 || snap.rtt.max_us > 0 {
                prop_assert!(report.contains(&format!("{:.1}", snap.rtt.avg_us)),
                    "missing rtt avg_us: {:.1}", snap.rtt.avg_us);
                prop_assert!(report.contains(&snap.rtt.min_us.to_string()),
                    "missing rtt min_us: {}", snap.rtt.min_us);
                prop_assert!(report.contains(&snap.rtt.max_us.to_string()),
                    "missing rtt max_us: {}", snap.rtt.max_us);
            }

            // 4.4: 큐 깊이, backpressure
            if snap.max_queue_depth > 0 {
                prop_assert!(report.contains(&snap.max_queue_depth.to_string()),
                    "missing max_queue_depth: {}", snap.max_queue_depth);
            }
            if snap.backpressure_count > 0 {
                prop_assert!(report.contains(&snap.backpressure_count.to_string()),
                    "missing backpressure_count: {}", snap.backpressure_count);
            }
        }

        // Feature: quicsync-phase2-enhancements, Property 5: MetricsSnapshot JSON 라운드트립
        // **Validates: Requirements 4.7**
        #[test]
        fn prop_metrics_snapshot_json_roundtrip(snap in arb_metrics_snapshot()) {
            let json = to_json(&snap);
            let restored = from_json(&json).expect("from_json should succeed for valid JSON");
            prop_assert_eq!(snap, restored);
        }
    }
}
