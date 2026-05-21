// Transfer metrics - lock-free 성능 메트릭 수집

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// 전송 중 수집되는 성능 메트릭 (lock-free)
pub struct TransferMetrics {
    pub bytes_transferred: AtomicU64,
    pub total_bytes: AtomicU64,
    pub start_time: Instant,

    // RTT 통계 (마이크로초 단위)
    pub rtt_sum_us: AtomicU64,
    pub rtt_count: AtomicU64,
    pub rtt_min_us: AtomicU64,
    pub rtt_max_us: AtomicU64,

    // 큐/backpressure
    pub max_queue_depth: AtomicU64,
    pub backpressure_count: AtomicU64,

    // 스트림
    pub active_streams: AtomicU64,
    pub completed_streams: AtomicU64,
    pub failed_streams: AtomicU64,

    // 무결성
    pub integrity_chunks_verified: AtomicU64,
    pub integrity_bytes_verified: AtomicU64,

    // 전송 모드: 0=QUIC, 1=TCP
    pub transport_mode: AtomicU64,
}

impl TransferMetrics {
    pub fn new() -> Self {
        Self {
            bytes_transferred: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            start_time: Instant::now(),
            rtt_sum_us: AtomicU64::new(0),
            rtt_count: AtomicU64::new(0),
            rtt_min_us: AtomicU64::new(u64::MAX),
            rtt_max_us: AtomicU64::new(0),
            max_queue_depth: AtomicU64::new(0),
            backpressure_count: AtomicU64::new(0),
            active_streams: AtomicU64::new(0),
            completed_streams: AtomicU64::new(0),
            failed_streams: AtomicU64::new(0),
            integrity_chunks_verified: AtomicU64::new(0),
            integrity_bytes_verified: AtomicU64::new(0),
            transport_mode: AtomicU64::new(0),
        }
    }

    /// 현재 전송 속도 (bytes/sec)
    pub fn throughput_bps(&self) -> f64 {
        let bytes = self.bytes_transferred.load(Ordering::Relaxed);
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            bytes as f64 / elapsed
        } else {
            0.0
        }
    }

    /// 예상 완료 시간 (초). 처리량이 0이면 None.
    pub fn eta_secs(&self) -> Option<f64> {
        let throughput = self.throughput_bps();
        if throughput <= 0.0 {
            return None;
        }
        let total = self.total_bytes.load(Ordering::Relaxed);
        let transferred = self.bytes_transferred.load(Ordering::Relaxed);
        let remaining = total.saturating_sub(transferred);
        Some(remaining as f64 / throughput)
    }

    /// RTT 통계 스냅샷
    pub fn rtt_snapshot(&self) -> RttStats {
        let count = self.rtt_count.load(Ordering::Relaxed);
        let sum = self.rtt_sum_us.load(Ordering::Relaxed);
        let min = self.rtt_min_us.load(Ordering::Relaxed);
        let max = self.rtt_max_us.load(Ordering::Relaxed);

        let avg_us = if count > 0 {
            sum as f64 / count as f64
        } else {
            0.0
        };
        let min_us = if min == u64::MAX { 0 } else { min };

        RttStats {
            avg_us,
            min_us,
            max_us: max,
        }
    }

    /// 최종 리포트용 불변 스냅샷
    pub fn snapshot(&self) -> MetricsSnapshot {
        let rtt = self.rtt_snapshot();
        MetricsSnapshot {
            bytes_transferred: self.bytes_transferred.load(Ordering::Relaxed),
            duration_secs: self.start_time.elapsed().as_secs_f64(),
            throughput_bps: self.throughput_bps(),
            rtt,
            max_queue_depth: self.max_queue_depth.load(Ordering::Relaxed),
            backpressure_count: self.backpressure_count.load(Ordering::Relaxed),
            streams_completed: self.completed_streams.load(Ordering::Relaxed),
            streams_failed: self.failed_streams.load(Ordering::Relaxed),
            integrity_chunks: self.integrity_chunks_verified.load(Ordering::Relaxed),
            integrity_bytes: self.integrity_bytes_verified.load(Ordering::Relaxed),
        }
    }
}

/// RTT 통계 스냅샷
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RttStats {
    pub avg_us: f64,
    pub min_us: u64,
    pub max_us: u64,
}

/// 최종 리포트용 불변 스냅샷
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub bytes_transferred: u64,
    pub duration_secs: f64,
    pub throughput_bps: f64,
    pub rtt: RttStats,
    pub max_queue_depth: u64,
    pub backpressure_count: u64,
    pub streams_completed: u64,
    pub streams_failed: u64,
    pub integrity_chunks: u64,
    pub integrity_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_new_defaults() {
        let m = TransferMetrics::new();
        assert_eq!(m.bytes_transferred.load(Ordering::Relaxed), 0);
        assert_eq!(m.total_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(m.rtt_min_us.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(m.rtt_max_us.load(Ordering::Relaxed), 0);
        assert_eq!(m.transport_mode.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_throughput_bps_zero_bytes() {
        let m = TransferMetrics::new();
        // start_time이 방금 생성되었으므로 elapsed ≈ 0
        // bytes_transferred = 0이므로 throughput ≈ 0
        let t = m.throughput_bps();
        assert!(t >= 0.0);
    }

    #[test]
    fn test_throughput_bps_with_data() {
        let m = TransferMetrics::new();
        m.bytes_transferred.store(1_000_000, Ordering::Relaxed);
        // 약간의 시간이 경과했으므로 throughput > 0
        thread::sleep(Duration::from_millis(10));
        let t = m.throughput_bps();
        assert!(t > 0.0);
    }

    #[test]
    fn test_eta_secs_none_when_no_throughput() {
        let m = TransferMetrics::new();
        m.total_bytes.store(1000, Ordering::Relaxed);
        // bytes_transferred = 0, elapsed ≈ 0 → throughput ≈ 0 → None
        assert!(m.eta_secs().is_none());
    }

    #[test]
    fn test_eta_secs_some_when_transferring() {
        let m = TransferMetrics::new();
        m.total_bytes.store(2_000_000, Ordering::Relaxed);
        m.bytes_transferred.store(1_000_000, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(10));
        let eta = m.eta_secs();
        assert!(eta.is_some());
        assert!(eta.unwrap() > 0.0);
    }

    #[test]
    fn test_rtt_snapshot_no_samples() {
        let m = TransferMetrics::new();
        let rtt = m.rtt_snapshot();
        assert_eq!(rtt.avg_us, 0.0);
        assert_eq!(rtt.min_us, 0); // u64::MAX → 0 when no samples
        assert_eq!(rtt.max_us, 0);
    }

    #[test]
    fn test_rtt_snapshot_with_samples() {
        let m = TransferMetrics::new();
        m.rtt_sum_us.store(300, Ordering::Relaxed);
        m.rtt_count.store(3, Ordering::Relaxed);
        m.rtt_min_us.store(50, Ordering::Relaxed);
        m.rtt_max_us.store(200, Ordering::Relaxed);

        let rtt = m.rtt_snapshot();
        assert_eq!(rtt.avg_us, 100.0);
        assert_eq!(rtt.min_us, 50);
        assert_eq!(rtt.max_us, 200);
    }

    #[test]
    fn test_snapshot_captures_all_fields() {
        let m = TransferMetrics::new();
        m.bytes_transferred.store(5000, Ordering::Relaxed);
        m.max_queue_depth.store(64, Ordering::Relaxed);
        m.backpressure_count.store(2, Ordering::Relaxed);
        m.completed_streams.store(10, Ordering::Relaxed);
        m.failed_streams.store(1, Ordering::Relaxed);
        m.integrity_chunks_verified.store(100, Ordering::Relaxed);
        m.integrity_bytes_verified.store(5000, Ordering::Relaxed);

        let snap = m.snapshot();
        assert_eq!(snap.bytes_transferred, 5000);
        assert_eq!(snap.max_queue_depth, 64);
        assert_eq!(snap.backpressure_count, 2);
        assert_eq!(snap.streams_completed, 10);
        assert_eq!(snap.streams_failed, 1);
        assert_eq!(snap.integrity_chunks, 100);
        assert_eq!(snap.integrity_bytes, 5000);
        assert!(snap.duration_secs >= 0.0);
    }

    #[test]
    fn test_metrics_snapshot_serde_roundtrip() {
        let snap = MetricsSnapshot {
            bytes_transferred: 1024,
            duration_secs: 1.5,
            throughput_bps: 682.67,
            rtt: RttStats {
                avg_us: 100.0,
                min_us: 50,
                max_us: 200,
            },
            max_queue_depth: 32,
            backpressure_count: 1,
            streams_completed: 5,
            streams_failed: 0,
            integrity_chunks: 10,
            integrity_bytes: 1024,
        };

        let json = serde_json::to_string(&snap).unwrap();
        let deserialized: MetricsSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, deserialized);
    }

    #[test]
    fn test_rtt_stats_serde_roundtrip() {
        let rtt = RttStats {
            avg_us: 45.5,
            min_us: 10,
            max_us: 100,
        };

        let json = serde_json::to_string(&rtt).unwrap();
        let deserialized: RttStats = serde_json::from_str(&json).unwrap();
        assert_eq!(rtt, deserialized);
    }
}
