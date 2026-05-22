// 실시간 모니터링 웹 서버 (--web)
//
// 127.0.0.1 전용 경량 HTTP/1.1 서버. 외부 웹 프레임워크 없이 tokio만으로 구현한다.
// GET 라우트 3종(/ , /api/metrics, 그 외 404)만 제공하는 read-only 모니터링 전용이다.
// 전송이 진행되는 동안 spawn되어 동작하며, 전송 종료 시 task abort 또는 프로세스 종료로 정리된다.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::metrics::TransferMetrics;
use crate::progress::{format_bytes, format_speed};

/// 대시보드 HTML은 컴파일 타임에 임베드한다 (런타임 파일 서빙 없음 → 트래버설 표면 제거).
const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

/// 127.0.0.1의 임의 포트(:0)에 바인딩하고 리스너와 실제 주소를 반환한다.
pub async fn bind() -> std::io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// 모니터링 서버 accept 루프. 연결마다 task를 띄워 단일 요청을 처리한다.
/// 이 함수는 무한 루프이므로 호출 측에서 `tokio::spawn`으로 띄우고,
/// 전송 종료 시 JoinHandle abort 또는 프로세스 종료로 정리한다.
pub async fn serve(listener: TcpListener, metrics: Arc<TransferMetrics>) {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let m = metrics.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, m).await {
                        tracing::debug!("web: connection error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::debug!("web: accept error: {e}");
            }
        }
    }
}

/// 단일 HTTP 연결을 처리한다. 요청 라인만 읽으면 라우팅에 충분하다.
async fn handle_conn(mut stream: TcpStream, metrics: Arc<TransferMetrics>) -> std::io::Result<()> {
    // GET 요청은 본문이 없으므로 첫 read의 요청 라인만 보면 된다.
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = parse_request_target(&req).unwrap_or("/");

    let response = route(path, &metrics);
    stream.write_all(&response).await?;
    stream.flush().await?;
    Ok(())
}

/// 요청 라인 "GET /path HTTP/1.1"에서 경로(target)를 추출한다.
fn parse_request_target(req: &str) -> Option<&str> {
    let line = req.lines().next()?;
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    let target = parts.next()?;
    Some(target)
}

/// 경로를 라우팅하여 완성된 HTTP 응답 바이트를 반환한다.
fn route(path: &str, metrics: &TransferMetrics) -> Vec<u8> {
    match path {
        "/" => http_response(
            "200 OK",
            "text/html; charset=utf-8",
            DASHBOARD_HTML.as_bytes(),
        ),
        "/api/metrics" => {
            let json = metrics_json(metrics);
            http_response("200 OK", "application/json", json.as_bytes())
        }
        _ => http_response("404 Not Found", "text/plain; charset=utf-8", b"not found"),
    }
}

/// 현재 메트릭을 모니터링용 JSON 문자열로 직렬화한다.
fn metrics_json(metrics: &TransferMetrics) -> String {
    let snap = metrics.snapshot();
    let mode = if metrics.transport_mode.load(Ordering::Relaxed) == 0 {
        "QUIC"
    } else {
        "TCP"
    };
    let total = metrics.total_bytes.load(Ordering::Relaxed);

    // 파일 진행
    let completed_files = metrics.completed_files.load(Ordering::Relaxed);
    let total_files = metrics.total_files.load(Ordering::Relaxed);
    let current_file = metrics
        .current_file
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    // 전체 파일 수를 알 때만 진행률(%)을 산출한다. 미상(0)이면 null.
    let file_progress_pct = if total_files > 0 {
        Some((completed_files as f64 / total_files as f64 * 100.0).min(100.0))
    } else {
        None
    };

    // serde_json으로 직렬화하여 수동 escape를 회피한다.
    let value = serde_json::json!({
        "bytes_transferred": snap.bytes_transferred,
        "total_bytes": total,
        "throughput_bps": snap.throughput_bps,
        "duration_secs": snap.duration_secs,
        "transport_mode": mode,
        "transferred_human": format_bytes(snap.bytes_transferred),
        "throughput_human": format_speed(snap.throughput_bps),
        "completed_files": completed_files,
        "total_files": total_files,
        "current_file": current_file,
        "file_progress_pct": file_progress_pct,
    });
    value.to_string()
}

/// 상태/Content-Type/본문으로 HTTP/1.1 응답을 조립한다. 연결은 요청마다 닫는다.
fn http_response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let header = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Cache-Control: no-store\r\n\r\n",
        body.len()
    );
    let mut out = header.into_bytes();
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 응답 바이트에서 본문(헤더 종료 이후)을 추출한다.
    fn body_of(resp: &[u8]) -> &[u8] {
        let s = resp.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        &resp[s + 4..]
    }

    #[test]
    fn parse_target_typical_get() {
        let req = "GET /api/metrics HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(parse_request_target(req), Some("/api/metrics"));
    }

    #[test]
    fn parse_target_root() {
        let req = "GET / HTTP/1.1\r\n\r\n";
        assert_eq!(parse_request_target(req), Some("/"));
    }

    #[test]
    fn parse_target_empty_returns_none() {
        assert_eq!(parse_request_target(""), None);
    }

    #[test]
    fn route_root_returns_html() {
        let m = TransferMetrics::new();
        let resp = route("/", &m);
        let head = String::from_utf8_lossy(&resp);
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert!(head.contains("text/html"));
    }

    #[test]
    fn route_metrics_returns_json() {
        let m = TransferMetrics::new();
        m.bytes_transferred.store(4096, Ordering::Relaxed);
        let resp = route("/api/metrics", &m);
        let head = String::from_utf8_lossy(&resp);
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert!(head.contains("application/json"));

        let body = body_of(&resp);
        let parsed: serde_json::Value = serde_json::from_slice(body).expect("valid JSON");
        assert_eq!(parsed["bytes_transferred"], 4096);
        assert_eq!(parsed["transport_mode"], "QUIC");
        assert!(parsed["transferred_human"].is_string());
    }

    #[test]
    fn route_unknown_returns_404() {
        let m = TransferMetrics::new();
        let resp = route("/secret", &m);
        let head = String::from_utf8_lossy(&resp);
        assert!(head.starts_with("HTTP/1.1 404 Not Found"));
    }

    #[test]
    fn http_response_has_content_length() {
        let resp = http_response("200 OK", "text/plain", b"hello");
        let head = String::from_utf8_lossy(&resp);
        assert!(head.contains("Content-Length: 5"));
        assert_eq!(body_of(&resp), b"hello");
    }

    #[test]
    fn metrics_json_reflects_transport_mode_tcp() {
        let m = TransferMetrics::new();
        m.transport_mode.store(1, Ordering::Relaxed);
        let json = metrics_json(&m);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["transport_mode"], "TCP");
    }

    #[test]
    fn metrics_json_file_progress_with_total() {
        let m = TransferMetrics::new();
        m.total_files.store(200, Ordering::Relaxed);
        m.completed_files.store(50, Ordering::Relaxed);
        *m.current_file.lock().unwrap() = "dir/some file.bin".to_string();

        let parsed: serde_json::Value = serde_json::from_str(&metrics_json(&m)).unwrap();
        assert_eq!(parsed["completed_files"], 50);
        assert_eq!(parsed["total_files"], 200);
        assert_eq!(parsed["current_file"], "dir/some file.bin");
        assert_eq!(parsed["file_progress_pct"], 25.0);
    }

    #[test]
    fn metrics_json_file_progress_null_when_total_unknown() {
        let m = TransferMetrics::new();
        m.completed_files.store(7, Ordering::Relaxed);
        // total_files = 0 (예: pull) → 진행률은 null.
        let parsed: serde_json::Value = serde_json::from_str(&metrics_json(&m)).unwrap();
        assert_eq!(parsed["completed_files"], 7);
        assert_eq!(parsed["total_files"], 0);
        assert!(parsed["file_progress_pct"].is_null());
    }

    /// over-the-wire 검증: bind→serve 후 실제 TCP 연결로 GET /api/metrics 요청 시
    /// 200 + 현재 메트릭 JSON을 돌려준다.
    #[tokio::test]
    async fn serve_responds_to_metrics_over_tcp() {
        let (listener, addr) = bind().await.expect("bind 127.0.0.1:0");
        let metrics = Arc::new(TransferMetrics::new());
        metrics.bytes_transferred.store(2048, Ordering::Relaxed);

        let handle = tokio::spawn(serve(listener, metrics.clone()));

        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(b"GET /api/metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();

        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "status: {text}");
        assert!(text.contains("application/json"));

        let body = body_of(&resp);
        let parsed: serde_json::Value = serde_json::from_slice(body).expect("valid JSON");
        assert_eq!(parsed["bytes_transferred"], 2048);

        handle.abort();
    }

    /// 정의되지 않은 경로는 over-the-wire에서도 404를 반환한다.
    #[tokio::test]
    async fn serve_returns_404_for_unknown_path_over_tcp() {
        let (listener, addr) = bind().await.expect("bind 127.0.0.1:0");
        let metrics = Arc::new(TransferMetrics::new());
        let handle = tokio::spawn(serve(listener, metrics));

        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(b"GET /../etc/passwd HTTP/1.1\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();

        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(text.starts_with("HTTP/1.1 404 Not Found"), "status: {text}");

        handle.abort();
    }
}
