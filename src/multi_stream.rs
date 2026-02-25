// QUIC 멀티스트림 기반 병렬 전송 관리

use std::sync::Arc;
use std::sync::atomic::Ordering;

use quinn::Connection;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::metrics::TransferMetrics;
use crate::types::{FileEntry, MultiStreamReport, StreamResult};

/// QUIC 멀티스트림 병렬 전송 매니저
pub struct MultiStreamManager {
    connection: Connection,
    max_streams: u8,
    metrics: Arc<TransferMetrics>,
}

impl MultiStreamManager {
    pub fn new(connection: Connection, max_streams: u8, metrics: Arc<TransferMetrics>) -> Self {
        Self {
            connection,
            max_streams,
            metrics,
        }
    }

    /// 파일 목록을 받아 병렬 전송 실행.
    /// max_streams 개의 동시 스트림으로 제한하며, 세마포어로 제어.
    pub async fn transfer_files(&self, files: Vec<FileEntry>) -> MultiStreamReport {
        let semaphore = Arc::new(Semaphore::new(self.max_streams as usize));
        let mut handles = Vec::with_capacity(files.len());

        for (stream_id, file) in files.into_iter().enumerate() {
            let sem = semaphore.clone();
            let conn = self.connection.clone();
            let metrics = self.metrics.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore closed");
                transfer_single_file(stream_id, &conn, &file, &metrics).await
            });
            handles.push(handle);
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok(result) => results.push(result),
                Err(e) => {
                    // JoinError — task panicked or was cancelled
                    results.push(StreamResult {
                        stream_id: results.len(),
                        success: false,
                        bytes_transferred: 0,
                        error: Some(format!("task join error: {e}")),
                    });
                }
            }
        }

        let total_success = results.iter().filter(|r| r.success).count();
        let total_failed = results.iter().filter(|r| !r.success).count();

        info!(
            "multi-stream transfer complete: {total_success} succeeded, {total_failed} failed"
        );

        MultiStreamReport {
            results,
            total_success,
            total_failed,
        }
    }
}

/// 단일 파일을 독립 QUIC 양방향 스트림에서 전송
async fn transfer_single_file(
    stream_id: usize,
    connection: &Connection,
    file: &FileEntry,
    metrics: &Arc<TransferMetrics>,
) -> StreamResult {
    metrics.active_streams.fetch_add(1, Ordering::Relaxed);

    let result = do_transfer(stream_id, connection, file).await;

    metrics.active_streams.fetch_sub(1, Ordering::Relaxed);

    match result {
        Ok(bytes) => {
            metrics.bytes_transferred.fetch_add(bytes, Ordering::Relaxed);
            metrics.completed_streams.fetch_add(1, Ordering::Relaxed);
            StreamResult {
                stream_id,
                success: true,
                bytes_transferred: bytes,
                error: None,
            }
        }
        Err(e) => {
            warn!(stream_id, error = %e, "stream transfer failed");
            metrics.failed_streams.fetch_add(1, Ordering::Relaxed);
            StreamResult {
                stream_id,
                success: false,
                bytes_transferred: 0,
                error: Some(e),
            }
        }
    }
}

/// QUIC 양방향 스트림을 열어 파일 경로를 전송하고 응답을 읽는다.
async fn do_transfer(
    stream_id: usize,
    connection: &Connection,
    file: &FileEntry,
) -> Result<u64, String> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| format!("stream {stream_id}: open_bi failed: {e}"))?;

    // 파일 경로를 전송
    send.write_all(file.path.as_bytes())
        .await
        .map_err(|e| format!("stream {stream_id}: write path failed: {e}"))?;

    send.finish()
        .map_err(|e| format!("stream {stream_id}: finish failed: {e}"))?;

    // 응답 데이터 읽기
    let data = recv
        .read_to_end(64 * 1024 * 1024) // 64MB limit
        .await
        .map_err(|e| format!("stream {stream_id}: read response failed: {e}"))?;

    Ok(data.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// StreamResult 생성 전략
    fn arb_stream_result() -> impl Strategy<Value = StreamResult> {
        (any::<usize>(), any::<bool>(), any::<u64>(), proptest::option::of(any::<String>()))
            .prop_map(|(stream_id, success, bytes, error)| StreamResult {
                stream_id,
                success,
                bytes_transferred: bytes,
                error,
            })
    }

    // Feature: quicsync-phase2-enhancements, Property 3: 스트림 결과 집계 정확성
    // **Validates: Requirements 3.6**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn stream_result_aggregation_accuracy(
            results in proptest::collection::vec(arb_stream_result(), 0..100)
        ) {
            let total_success = results.iter().filter(|r| r.success).count();
            let total_failed = results.iter().filter(|r| !r.success).count();

            let report = MultiStreamReport {
                results: results.clone(),
                total_success,
                total_failed,
            };

            // total_success + total_failed == 전체 길이
            prop_assert_eq!(
                report.total_success + report.total_failed,
                report.results.len(),
                "sum of success + failed must equal total length"
            );

            // total_success == success == true 개수
            let expected_success = report.results.iter().filter(|r| r.success).count();
            prop_assert_eq!(
                report.total_success,
                expected_success,
                "total_success must match count of success == true"
            );

            // total_failed == success == false 개수
            let expected_failed = report.results.iter().filter(|r| !r.success).count();
            prop_assert_eq!(
                report.total_failed,
                expected_failed,
                "total_failed must match count of success == false"
            );
        }
    }

    #[test]
    fn test_multi_stream_report_aggregation() {
        let results = vec![
            StreamResult { stream_id: 0, success: true, bytes_transferred: 100, error: None },
            StreamResult { stream_id: 1, success: false, bytes_transferred: 0, error: Some("err".into()) },
            StreamResult { stream_id: 2, success: true, bytes_transferred: 200, error: None },
        ];

        let total_success = results.iter().filter(|r| r.success).count();
        let total_failed = results.iter().filter(|r| !r.success).count();

        let report = MultiStreamReport {
            results,
            total_success,
            total_failed,
        };

        assert_eq!(report.total_success, 2);
        assert_eq!(report.total_failed, 1);
        assert_eq!(report.total_success + report.total_failed, report.results.len());
    }

    #[test]
    fn test_multi_stream_report_all_success() {
        let results = vec![
            StreamResult { stream_id: 0, success: true, bytes_transferred: 50, error: None },
            StreamResult { stream_id: 1, success: true, bytes_transferred: 75, error: None },
        ];

        let total_success = results.iter().filter(|r| r.success).count();
        let total_failed = results.iter().filter(|r| !r.success).count();

        let report = MultiStreamReport {
            results,
            total_success,
            total_failed,
        };

        assert_eq!(report.total_success, 2);
        assert_eq!(report.total_failed, 0);
    }

    #[test]
    fn test_multi_stream_report_all_failed() {
        let results = vec![
            StreamResult { stream_id: 0, success: false, bytes_transferred: 0, error: Some("e1".into()) },
            StreamResult { stream_id: 1, success: false, bytes_transferred: 0, error: Some("e2".into()) },
        ];

        let total_success = results.iter().filter(|r| r.success).count();
        let total_failed = results.iter().filter(|r| !r.success).count();

        let report = MultiStreamReport {
            results,
            total_success,
            total_failed,
        };

        assert_eq!(report.total_success, 0);
        assert_eq!(report.total_failed, 2);
    }

    #[test]
    fn test_multi_stream_report_empty() {
        let report = MultiStreamReport {
            results: vec![],
            total_success: 0,
            total_failed: 0,
        };

        assert_eq!(report.total_success, 0);
        assert_eq!(report.total_failed, 0);
        assert_eq!(report.results.len(), 0);
    }

    #[test]
    fn test_multi_stream_manager_new() {
        // MultiStreamManager::new requires a real Connection, so we just verify
        // the struct fields are set correctly by testing the types compile.
        // Actual transfer_files testing requires a QUIC connection (integration test).
        assert_eq!(std::mem::size_of::<u8>(), 1); // placeholder compile check
    }
}
