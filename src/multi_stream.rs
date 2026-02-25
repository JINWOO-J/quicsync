// 멀티스트림 전송 인프라

use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::error::MultiStreamError;
use crate::metrics::TransferMetrics;

/// 전송할 파일 항목
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

/// 개별 스트림 전송 결과
#[derive(Debug, Clone)]
pub struct StreamResult {
    pub stream_id: u32,
    pub success: bool,
    pub bytes_transferred: u64,
    pub error: Option<String>,
}

/// 멀티스트림 전송 집계 결과
#[derive(Debug, Clone)]
pub struct MultiStreamReport {
    pub results: Vec<StreamResult>,
    pub total_success: u32,
    pub total_failed: u32,
    pub total_bytes: u64,
}

impl MultiStreamReport {
    /// StreamResult 벡터에서 집계 보고서를 생성한다.
    pub fn from_results(results: Vec<StreamResult>) -> Self {
        let total_success = results.iter().filter(|r| r.success).count() as u32;
        let total_failed = results.iter().filter(|r| !r.success).count() as u32;
        let total_bytes = results.iter().map(|r| r.bytes_transferred).sum();
        Self {
            results,
            total_success,
            total_failed,
            total_bytes,
        }
    }
}

/// 멀티스트림 전송 관리자
///
/// 현재는 인프라만 제공한다. rsync 자체가 단일 스트림이므로
/// 실제 파일 전송은 추후 구현.
pub struct MultiStreamManager {
    pub max_streams: u16,
    pub semaphore: Arc<Semaphore>,
    pub metrics: Arc<TransferMetrics>,
}

impl MultiStreamManager {
    pub fn new(max_streams: u16, metrics: Arc<TransferMetrics>) -> Result<Self, MultiStreamError> {
        if max_streams < 1 || max_streams > 64 {
            return Err(MultiStreamError::InvalidStreamCount(format!(
                "stream count must be 1-64, got {max_streams}"
            )));
        }
        Ok(Self {
            max_streams,
            semaphore: Arc::new(Semaphore::new(max_streams as usize)),
            metrics,
        })
    }

    /// 파일 목록을 병렬 스트림으로 전송한다 (인프라 스텁).
    ///
    /// 실제 구현에서는 각 파일에 대해 QUIC 스트림을 열어 전송하지만,
    /// 현재는 인프라만 제공하고 각 파일의 성공 결과를 반환한다.
    pub async fn transfer_files(&self, files: &[FileEntry]) -> MultiStreamReport {
        let mut results = Vec::with_capacity(files.len());

        for (i, file) in files.iter().enumerate() {
            let _permit = self.semaphore.acquire().await.expect("semaphore poisoned");

            // 실제 전송 stub: 성공으로 보고
            self.metrics.record_bytes(file.size);
            results.push(StreamResult {
                stream_id: i as u32,
                success: true,
                bytes_transferred: file.size,
                error: None,
            });
        }

        MultiStreamReport::from_results(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn multi_stream_report_from_empty() {
        let report = MultiStreamReport::from_results(vec![]);
        assert_eq!(report.total_success, 0);
        assert_eq!(report.total_failed, 0);
        assert_eq!(report.total_bytes, 0);
    }

    #[test]
    fn multi_stream_report_mixed_results() {
        let results = vec![
            StreamResult { stream_id: 0, success: true, bytes_transferred: 100, error: None },
            StreamResult { stream_id: 1, success: false, bytes_transferred: 0, error: Some("timeout".into()) },
            StreamResult { stream_id: 2, success: true, bytes_transferred: 200, error: None },
        ];
        let report = MultiStreamReport::from_results(results);
        assert_eq!(report.total_success, 2);
        assert_eq!(report.total_failed, 1);
        assert_eq!(report.total_bytes, 300);
    }

    #[test]
    fn multi_stream_manager_invalid_stream_count() {
        let metrics = Arc::new(TransferMetrics::new());
        assert!(MultiStreamManager::new(0, metrics.clone()).is_err());
        assert!(MultiStreamManager::new(65, metrics).is_err());
    }

    #[test]
    fn multi_stream_manager_valid_stream_count() {
        let metrics = Arc::new(TransferMetrics::new());
        assert!(MultiStreamManager::new(1, metrics.clone()).is_ok());
        assert!(MultiStreamManager::new(64, metrics).is_ok());
    }

    #[tokio::test]
    async fn transfer_files_returns_correct_report() {
        let metrics = Arc::new(TransferMetrics::new());
        let mgr = MultiStreamManager::new(4, metrics.clone()).unwrap();
        let files = vec![
            FileEntry { path: "a.txt".into(), size: 100 },
            FileEntry { path: "b.txt".into(), size: 200 },
            FileEntry { path: "c.txt".into(), size: 300 },
        ];
        let report = mgr.transfer_files(&files).await;
        assert_eq!(report.total_success, 3);
        assert_eq!(report.total_failed, 0);
        assert_eq!(report.total_bytes, 600);
    }

    // Property 3: MultiStreamReport 집계 정확성
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_multi_stream_report_aggregation(
            successes in proptest::collection::vec(1u64..=1_000_000, 0..=20),
            failures in proptest::collection::vec(0u64..=0, 0..=10),
        ) {
            let mut results = Vec::new();
            let mut id = 0u32;

            for bytes in &successes {
                results.push(StreamResult {
                    stream_id: id,
                    success: true,
                    bytes_transferred: *bytes,
                    error: None,
                });
                id += 1;
            }
            for _ in &failures {
                results.push(StreamResult {
                    stream_id: id,
                    success: false,
                    bytes_transferred: 0,
                    error: Some("error".into()),
                });
                id += 1;
            }

            let report = MultiStreamReport::from_results(results);
            prop_assert_eq!(report.total_success, successes.len() as u32);
            prop_assert_eq!(report.total_failed, failures.len() as u32);
            prop_assert_eq!(report.total_bytes, successes.iter().sum::<u64>());
            prop_assert_eq!(
                report.results.len() as u32,
                report.total_success + report.total_failed
            );
        }
    }
}
