// 전송 메트릭 수집 (lock-free AtomicU64 기반)

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// lock-free 전송 메트릭 (Arc로 공유)
pub struct TransferMetrics {
    pub bytes_transferred: AtomicU64,
    pub total_bytes: AtomicU64,
    pub rtt_sum_us: AtomicU64,
    pub rtt_count: AtomicU64,
    pub rtt_min_us: AtomicU64,
    pub rtt_max_us: AtomicU64,
    pub backpressure_count: AtomicU64,
    pub streams_active: AtomicU64,
    pub integrity_checks: AtomicU64,
    pub started_at: Instant,
}

/// RTT 통계 스냅샷
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RttStats {
    pub avg_us: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub count: u64,
}

/// 전체 메트릭 스냅샷 (serde 직렬화 가능)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsSnapshot {
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub elapsed_secs: f64,
    pub throughput_bps: f64,
    pub rtt: RttStats,
    pub backpressure_count: u64,
    pub streams_active: u64,
    pub integrity_checks: u64,
}

impl TransferMetrics {
    pub fn new() -> Self {
        Self {
            bytes_transferred: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            rtt_sum_us: AtomicU64::new(0),
            rtt_count: AtomicU64::new(0),
            rtt_min_us: AtomicU64::new(u64::MAX),
            rtt_max_us: AtomicU64::new(0),
            backpressure_count: AtomicU64::new(0),
            streams_active: AtomicU64::new(0),
            integrity_checks: AtomicU64::new(0),
            started_at: Instant::now(),
        }
    }

    /// 바이트 전송량 기록
    pub fn record_bytes(&self, bytes: u64) {
        self.bytes_transferred.fetch_add(bytes, Ordering::Relaxed);
    }

    /// RTT 샘플 기록 (마이크로초)
    pub fn record_rtt(&self, rtt_us: u64) {
        self.rtt_sum_us.fetch_add(rtt_us, Ordering::Relaxed);
        self.rtt_count.fetch_add(1, Ordering::Relaxed);
        self.rtt_min_us.fetch_min(rtt_us, Ordering::Relaxed);
        self.rtt_max_us.fetch_max(rtt_us, Ordering::Relaxed);
    }

    /// 현재 처리량 (bits per second)
    pub fn throughput_bps(&self) -> f64 {
        let bytes = self.bytes_transferred.load(Ordering::Relaxed);
        let elapsed = self.started_at.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            (bytes as f64 * 8.0) / elapsed
        } else {
            0.0
        }
    }

    /// 예상 남은 시간 (초)
    pub fn eta_secs(&self) -> f64 {
        let transferred = self.bytes_transferred.load(Ordering::Relaxed);
        let total = self.total_bytes.load(Ordering::Relaxed);
        if transferred == 0 || total == 0 {
            return 0.0;
        }
        let elapsed = self.started_at.elapsed().as_secs_f64();
        let remaining = total.saturating_sub(transferred);
        let rate = transferred as f64 / elapsed;
        if rate > 0.0 {
            remaining as f64 / rate
        } else {
            0.0
        }
    }

    /// RTT 통계 스냅샷
    pub fn rtt_snapshot(&self) -> RttStats {
        let count = self.rtt_count.load(Ordering::Relaxed);
        let sum = self.rtt_sum_us.load(Ordering::Relaxed);
        let min = self.rtt_min_us.load(Ordering::Relaxed);
        let max = self.rtt_max_us.load(Ordering::Relaxed);
        RttStats {
            avg_us: if count > 0 { sum / count } else { 0 },
            min_us: if count > 0 { min } else { 0 },
            max_us: max,
            count,
        }
    }

    /// 전체 메트릭 스냅샷
    pub fn snapshot(&self) -> MetricsSnapshot {
        let bytes_transferred = self.bytes_transferred.load(Ordering::Relaxed);
        let total_bytes = self.total_bytes.load(Ordering::Relaxed);
        let elapsed_secs = self.started_at.elapsed().as_secs_f64();
        MetricsSnapshot {
            bytes_transferred,
            total_bytes,
            elapsed_secs,
            throughput_bps: self.throughput_bps(),
            rtt: self.rtt_snapshot(),
            backpressure_count: self.backpressure_count.load(Ordering::Relaxed),
            streams_active: self.streams_active.load(Ordering::Relaxed),
            integrity_checks: self.integrity_checks.load(Ordering::Relaxed),
        }
    }
}

impl Default for TransferMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn new_metrics_are_zero() {
        let m = TransferMetrics::new();
        assert_eq!(m.bytes_transferred.load(Ordering::Relaxed), 0);
        assert_eq!(m.rtt_count.load(Ordering::Relaxed), 0);
        assert_eq!(m.rtt_min_us.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn record_bytes_accumulates() {
        let m = TransferMetrics::new();
        m.record_bytes(100);
        m.record_bytes(200);
        assert_eq!(m.bytes_transferred.load(Ordering::Relaxed), 300);
    }

    #[test]
    fn record_rtt_updates_min_max() {
        let m = TransferMetrics::new();
        m.record_rtt(100);
        m.record_rtt(50);
        m.record_rtt(200);
        let rtt = m.rtt_snapshot();
        assert_eq!(rtt.min_us, 50);
        assert_eq!(rtt.max_us, 200);
        assert_eq!(rtt.avg_us, 116); // (100+50+200)/3 = 116
        assert_eq!(rtt.count, 3);
    }

    #[test]
    fn rtt_snapshot_no_samples() {
        let m = TransferMetrics::new();
        let rtt = m.rtt_snapshot();
        assert_eq!(rtt.avg_us, 0);
        assert_eq!(rtt.min_us, 0);
        assert_eq!(rtt.max_us, 0);
        assert_eq!(rtt.count, 0);
    }

    #[test]
    fn throughput_bps_returns_non_negative() {
        let m = TransferMetrics::new();
        m.record_bytes(1000);
        assert!(m.throughput_bps() >= 0.0);
    }

    #[test]
    fn eta_secs_zero_when_no_total() {
        let m = TransferMetrics::new();
        m.record_bytes(100);
        assert_eq!(m.eta_secs(), 0.0);
    }

    #[test]
    fn snapshot_captures_all_fields() {
        let m = TransferMetrics::new();
        m.record_bytes(1024);
        m.total_bytes.store(2048, Ordering::Relaxed);
        m.record_rtt(500);
        m.backpressure_count.fetch_add(1, Ordering::Relaxed);
        m.streams_active.store(2, Ordering::Relaxed);
        m.integrity_checks.fetch_add(5, Ordering::Relaxed);

        let snap = m.snapshot();
        assert_eq!(snap.bytes_transferred, 1024);
        assert_eq!(snap.total_bytes, 2048);
        assert_eq!(snap.backpressure_count, 1);
        assert_eq!(snap.streams_active, 2);
        assert_eq!(snap.integrity_checks, 5);
        assert_eq!(snap.rtt.count, 1);
    }

    // Property 5: MetricsSnapshot JSON 라운드트립
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_metrics_snapshot_json_roundtrip(
            bytes_transferred in any::<u64>(),
            total_bytes in any::<u64>(),
            elapsed_secs in 0.0f64..1e9,
            throughput_bps in 0.0f64..1e12,
            avg_us in any::<u64>(),
            min_us in any::<u64>(),
            max_us in any::<u64>(),
            rtt_count in any::<u64>(),
            backpressure_count in any::<u64>(),
            streams_active in any::<u64>(),
            integrity_checks in any::<u64>(),
        ) {
            let snap = MetricsSnapshot {
                bytes_transferred,
                total_bytes,
                elapsed_secs,
                throughput_bps,
                rtt: RttStats {
                    avg_us,
                    min_us,
                    max_us,
                    count: rtt_count,
                },
                backpressure_count,
                streams_active,
                integrity_checks,
            };

            let json = serde_json::to_string(&snap).expect("serialize");
            let restored: MetricsSnapshot = serde_json::from_str(&json).expect("deserialize");
            // Integer fields must be exact
            prop_assert_eq!(snap.bytes_transferred, restored.bytes_transferred);
            prop_assert_eq!(snap.total_bytes, restored.total_bytes);
            prop_assert_eq!(snap.rtt, restored.rtt);
            prop_assert_eq!(snap.backpressure_count, restored.backpressure_count);
            prop_assert_eq!(snap.streams_active, restored.streams_active);
            prop_assert_eq!(snap.integrity_checks, restored.integrity_checks);
            // f64 fields: JSON roundtrip may lose the least significant bit
            let elapsed_diff = (snap.elapsed_secs - restored.elapsed_secs).abs();
            prop_assert!(elapsed_diff < 1e-6 * snap.elapsed_secs.abs().max(1.0),
                "elapsed_secs diff too large: {elapsed_diff}");
            let tp_diff = (snap.throughput_bps - restored.throughput_bps).abs();
            prop_assert!(tp_diff < 1e-6 * snap.throughput_bps.abs().max(1.0),
                "throughput_bps diff too large: {tp_diff}");
        }
    }
}
