// --web 대시보드 UI를 더미 메트릭으로 띄워 보는 데모.
//
// 실제 전송 없이 web 모니터링 화면만 확인하기 위한 example이다.
// 실행: cargo run --example web_dashboard
//   → http://127.0.0.1:PORT 가 출력되고 브라우저가 자동으로 열린다.
//   → Ctrl+C 로 종료.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use quicsync::metrics::TransferMetrics;
use quicsync::web;

#[tokio::main]
async fn main() {
    let metrics = Arc::new(TransferMetrics::new());
    // 총 50GB / 500 파일 전송을 가정 (대시보드에 진행률 바 + 파일 수가 보이도록).
    metrics.total_bytes.store(50_000_000_000, Ordering::Relaxed);
    metrics.total_files.store(500, Ordering::Relaxed);

    // 더미 진행: 50ms마다 10MB 증가 → 약 200MB/s, 250ms마다 파일 1개 완료.
    let m = metrics.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        let mut tick: u64 = 0;
        loop {
            ticker.tick().await;
            tick += 1;
            m.bytes_transferred.fetch_add(10_000_000, Ordering::Relaxed);
            if tick.is_multiple_of(5) {
                let n = m.completed_files.fetch_add(1, Ordering::Relaxed) + 1;
                if let Ok(mut cur) = m.current_file.lock() {
                    *cur = format!("data/chunk-{n:04}.bin");
                }
            }
        }
    });

    let (listener, addr) = web::bind().await.expect("bind 127.0.0.1:0");
    let url = format!("http://{addr}");
    println!("quicsync web dashboard (demo) → {url}");
    println!("브라우저가 자동으로 열립니다. 종료하려면 Ctrl+C.");

    // 브라우저 자동 오픈 (best-effort).
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();

    // serve는 무한 루프이므로 Ctrl+C까지 대시보드를 제공한다.
    web::serve(listener, metrics).await;
}
