// 전체 세션 생명주기 관리 및 시그널 핸들링

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::watch;

use crate::buffer::BufferLayer;
use crate::error::{SessionError, SshError};
use crate::metrics::TransferMetrics;
use crate::progress::ProgressUI;
use crate::quic::{QuicClientCfg, QuicTunnel, fingerprint_from_hex};
use crate::remote_install::RemoteInstaller;
use crate::rsync::RsyncChild;
use crate::ssh::launch_remote_server;
use crate::stats::StatsReporter;
use crate::tcp_proxy::TcpProxy;
use crate::types::{CliArgs, StatsFormat};

/// 세션: SSH → QUIC → TCP_Proxy → rsync 전체 파이프라인을 관리한다.
pub struct Session {
    ssh_process: tokio::process::Child,
    tunnel: QuicTunnel,
    rsync: RsyncChild,
    started_at: std::time::Instant,
    metrics: Arc<TransferMetrics>,
    streams: u8,
    stats: bool,
    stats_format: StatsFormat,
    /// --web 활성 시 실행되는 모니터링 task들(서버 + 로그 tail). 전송 종료 시 abort된다.
    web_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// --web 파일 이벤트 로그 임시 경로. 종료 시 삭제한다.
    web_log_path: Option<std::path::PathBuf>,
    #[cfg(feature = "otel")]
    telemetry: Option<crate::telemetry::TelemetryExporter>,
    /// 세션 루트 span. 전송 단계 span들의 부모이며, 종료 시 transfer span을 기록한다.
    #[cfg(feature = "otel")]
    session_span: Option<crate::telemetry::SessionSpan>,
}

impl Session {
    /// 모든 컴포넌트를 순서대로 초기화하고 연결한다.
    ///
    /// 1. SSH로 원격 quicsync server 실행 (포트+토큰 수신)
    /// 2. QUIC 터널 수립
    /// 3. 양방향 QUIC 스트림 열기 + 인증 토큰/rsync args 전송
    /// 4. 로컬 TCP 프록시 바인딩
    /// 5. rsync 자식 프로세스 실행
    /// 6. Buffer relay 태스크 spawn
    pub async fn start(args: CliArgs) -> Result<Self, SessionError> {
        let started_at = std::time::Instant::now();
        let metrics = Arc::new(TransferMetrics::new());

        // OpenTelemetry 초기화 (otel feature + endpoint 지정 시)
        #[cfg(feature = "otel")]
        let telemetry = if let Some(ref endpoint) = args.otel_endpoint {
            match crate::telemetry::TelemetryExporter::init(endpoint) {
                Ok(exporter) => {
                    tracing::info!("OpenTelemetry initialized: {}", endpoint);
                    Some(exporter)
                }
                Err(e) => {
                    tracing::warn!("OpenTelemetry init failed (continuing without): {e}");
                    None
                }
            }
        } else {
            None
        };

        #[cfg(feature = "otel")]
        let session_span = telemetry.as_ref().map(|t| t.start_session_span(&args));

        let remote_display = match &args.remote.user {
            Some(u) => format!("{}@{}:{}", u, args.remote.host, args.remote.path),
            None => format!("{}:{}", args.remote.host, args.remote.path),
        };
        let direction_label = match args.direction {
            crate::types::TransferDirection::Push => "push",
            crate::types::TransferDirection::Pull => "pull",
        };
        eprintln!(
            "quicsync: {} → {} ({})",
            remote_display,
            direction_label,
            if args.local_paths.len() == 1 {
                args.local_paths[0].display().to_string()
            } else {
                format!("{} paths", args.local_paths.len())
            }
        );

        // 재귀 옵션이 없으면 rsync가 디렉토리를 건너뛰어(skipping directory) 0개가 전송된다.
        // 막지는 않되(최상위 파일만 전송하려는 경우도 있으므로) 사용자에게 명확히 알린다.
        if !crate::rsync::has_recursive_flag(&args.rsync_options) {
            eprintln!(
                "quicsync: 경고 — 재귀 옵션(-a, -r 등)이 없어 디렉토리는 복사되지 않습니다(rsync가 건너뜀).\n          디렉토리를 동기화하려면 -a 를 추가하거나 QUICSYNC_DEFAULT_ARGS=-a 를 설정하세요."
            );
        }

        // 1. SSH로 원격 서버 실행
        tracing::info!("launching remote server via SSH...");
        let ssh_fut = launch_remote_server(&args.remote);
        #[cfg(feature = "otel")]
        let ssh_result = match &session_span {
            Some(s) => {
                use tracing::Instrument;
                ssh_fut.instrument(s.ssh_span().span()).await
            }
            None => ssh_fut.await,
        };
        #[cfg(not(feature = "otel"))]
        let ssh_result = ssh_fut.await;
        let handshake = match ssh_result {
            Ok(handshake) => handshake,
            Err(SshError::BinaryNotFound(e)) if args.install_remote => {
                eprintln!("quicsync: remote quicsync not found; installing matching binary...");
                let version = RemoteInstaller::install_smart(&args.remote, "$HOME/.local/bin")
                    .await
                    .map_err(|install_err| {
                        SessionError::InitFailed(format!(
                            "remote install failed after binary-not-found ({e}): {install_err}"
                        ))
                    })?;
                eprintln!("quicsync: installed remote {version}");
                launch_remote_server(&args.remote)
                    .await
                    .map_err(|retry_err| SessionError::InitFailed(format!("SSH: {retry_err}")))?
            }
            Err(e) => return Err(SessionError::InitFailed(format!("SSH: {e}"))),
        };

        let ssh_process = handshake.ssh_process;
        let host_port = format!("{}:{}", args.remote.host, handshake.remote_port);
        let remote_addr: SocketAddr = tokio::net::lookup_host(&host_port)
            .await
            .map_err(|e| SessionError::InitFailed(format!("DNS resolve {host_port}: {e}")))?
            .next()
            .ok_or_else(|| SessionError::InitFailed(format!("no address found for {host_port}")))?;

        // 2. QUIC 터널 수립 (핸드셰이크에 지문이 있으면 핀닝 검증 적용)
        tracing::info!("connecting QUIC tunnel to {}...", remote_addr);
        let fingerprint = match &handshake.fingerprint {
            Some(fp_hex) => Some(
                fingerprint_from_hex(fp_hex)
                    .map_err(|e| SessionError::InitFailed(format!("fingerprint: {e}")))?,
            ),
            None => None,
        };
        let quic_fut = QuicTunnel::connect(QuicClientCfg {
            remote_addr,
            auth_token: handshake.auth_token.clone(),
            server_name: "localhost".to_string(),
            window_bytes: args.quic_window,
            fingerprint,
        });
        #[cfg(feature = "otel")]
        let quic_result = match &session_span {
            Some(s) => {
                use tracing::Instrument;
                quic_fut.instrument(s.quic_span().span()).await
            }
            None => quic_fut.await,
        };
        #[cfg(not(feature = "otel"))]
        let quic_result = quic_fut.await;
        let tunnel = quic_result.map_err(|e| SessionError::InitFailed(format!("QUIC: {e}")))?;

        // 3. 양방향 스트림 열기
        let (mut send_stream, recv_stream) = tunnel
            .open_bi_stream()
            .await
            .map_err(|e| SessionError::InitFailed(format!("QUIC stream: {e}")))?;

        // 인증 토큰을 첫 번째 메시지로 전송
        // rsync 서버 인수는 --connect 모드가 rsh 호출에서 추출하여 TCP 프록시를 통해 전송한다.
        send_stream
            .write_all(format!("{}\n", handshake.auth_token).as_bytes())
            .await
            .map_err(|e| SessionError::InitFailed(format!("send token: {e}")))?;

        // 4. TCP 프록시 바인딩
        let proxy = TcpProxy::bind()
            .await
            .map_err(|e| SessionError::InitFailed(format!("TCP proxy: {e}")))?;
        let proxy_port = proxy.port();
        tracing::info!("TCP proxy listening on 127.0.0.1:{}", proxy_port);

        // 5. rsync 자식 프로세스 실행
        // --web이면 파일 이벤트 로그 임시 경로를 만들어 rsync가 기록하게 한다.
        let web_log_path = if args.web {
            let p = std::env::temp_dir().join(format!("quicsync-web-{}.log", std::process::id()));
            // tail이 곧바로 열 수 있도록 빈 파일을 생성한다(기존 내용 제거).
            let _ = std::fs::File::create(&p);
            Some(p)
        } else {
            None
        };
        let rsync = RsyncChild::spawn(
            &args.rsync_options,
            &args.local_paths,
            &args.remote,
            proxy_port,
            args.direction,
            web_log_path.as_deref(),
        )
        .map_err(|e| SessionError::InitFailed(format!("rsync: {e}")))?;

        // 6. Buffer relay 태스크 spawn
        let buffer = BufferLayer::from_env();
        let (fwd_tx, fwd_rx) = tokio::sync::mpsc::channel(1024);
        let (rev_tx, rev_rx) = tokio::sync::mpsc::channel(1024);

        // TCP_Proxy relay: rsync TCP ↔ 채널
        tokio::spawn(async move {
            if let Err(e) = proxy.relay(fwd_tx, rev_rx).await {
                tracing::error!("TCP proxy relay error: {e}");
            }
        });

        // Buffer forward relay: 채널 → QUIC SendStream
        let fwd_metrics = metrics.clone();
        tokio::spawn(async move {
            if let Err(e) = buffer.relay_forward(fwd_rx, send_stream, fwd_metrics).await {
                tracing::error!("forward relay error: {e}");
            }
        });

        // Buffer reverse relay: QUIC RecvStream → 채널
        let reverse_buffer = BufferLayer::from_env();
        let rev_metrics = metrics.clone();
        tokio::spawn(async move {
            if let Err(e) = reverse_buffer
                .relay_reverse(recv_stream, rev_tx, rev_metrics)
                .await
            {
                tracing::error!("reverse relay error: {e}");
            }
        });

        // TODO: integrate encode_chunk/decode_chunk into relay when no_integrity is false

        // Progress UI 스폰 (show_progress가 true일 때)
        if args.show_progress {
            let progress = ProgressUI::new(metrics.clone(), true);
            tokio::spawn(async move { progress.run().await });
        }

        // Web 모니터링 서버 + 보조 task 스폰 (--web 활성 시).
        // 127.0.0.1 ephemeral 포트에 바인딩하고 URL을 출력한 뒤 브라우저를 연다.
        // 바인딩/브라우저 오픈 실패는 비치명적이며 전송은 정상 진행한다.
        let mut web_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        if args.web {
            match crate::web::bind().await {
                Ok((listener, addr)) => {
                    let url = format!("http://{addr}");
                    eprintln!("quicsync: web monitor → {url}");
                    open_browser(&url);
                    let web_metrics = metrics.clone();
                    web_tasks.push(tokio::spawn(async move {
                        crate::web::serve(listener, web_metrics).await;
                    }));
                }
                Err(e) => {
                    eprintln!("quicsync: web monitor disabled (bind failed: {e})");
                }
            }

            // 파일 이벤트 로그 tail: 완료 파일 수와 현재 파일명을 갱신한다.
            if let Some(ref log_path) = web_log_path {
                let tail_metrics = metrics.clone();
                let lp = log_path.clone();
                web_tasks.push(tokio::spawn(tail_file_log(lp, tail_metrics)));
            }

            // Push는 로컬 소스를 미리 walk하여 전체 파일 수(진행률 분모)를 산정한다.
            // (rsync 필터는 미반영하므로 근사치. Pull은 원격이라 산정 불가 → 0=미상.)
            if args.direction == crate::types::TransferDirection::Push {
                let walk_metrics = metrics.clone();
                let paths = args.local_paths.clone();
                tokio::task::spawn_blocking(move || {
                    let n = count_files(&paths);
                    walk_metrics
                        .total_files
                        .store(n, std::sync::atomic::Ordering::Relaxed);
                });
            }
        }

        let streams = args.streams;
        let stats = args.stats;
        let stats_format = args.stats_format;
        let _no_integrity = args.no_integrity;

        tracing::info!("session started (streams={streams}), waiting for rsync to complete...");

        Ok(Self {
            ssh_process,
            tunnel,
            rsync,
            started_at,
            metrics,
            streams,
            stats,
            stats_format,
            web_tasks,
            web_log_path,
            #[cfg(feature = "otel")]
            telemetry,
            #[cfg(feature = "otel")]
            session_span,
        })
    }

    /// 세션 실행: rsync 완료 또는 시그널 수신까지 대기한다.
    ///
    /// - rsync 정상 완료 → shutdown → 종료 코드 반환
    /// - SIGINT/SIGTERM → abort → 종료 코드 반환
    pub async fn run(self) -> Result<i32, SessionError> {
        let mut signal_rx = install_signal_handlers()?;

        let Session {
            mut ssh_process,
            tunnel,
            rsync,
            started_at,
            metrics,
            streams,
            stats,
            stats_format,
            web_tasks,
            web_log_path,
            #[cfg(feature = "otel")]
            telemetry,
            #[cfg(feature = "otel")]
            session_span,
        } = self;

        tokio::select! {
            result = rsync.wait() => {
                let code = match result {
                    Ok(code) => {
                        tracing::info!("rsync completed with exit code {code}");
                        code
                    }
                    Err(crate::error::RsyncError::ExitCode(code)) => {
                        tracing::warn!("rsync exited with code {code}");
                        code
                    }
                    Err(e) => {
                        tracing::error!("rsync error: {e}");
                        1
                    }
                };
                shutdown(tunnel, &mut ssh_process).await;
                cleanup_web(&web_tasks, &web_log_path);
                let elapsed = started_at.elapsed();

                // Stats 리포트 출력 (--stats 플래그 활성 시)
                if stats {
                    let reporter = StatsReporter::new(stats_format);
                    reporter.report(&metrics.snapshot());
                }

                // 멀티스트림 병렬 전송 경로 (향후 파일 목록 기반 병렬 전송 시 활성화)
                // 현재 rsync는 단일 TCP 프록시 스트림으로 동작한다.
                // 디렉토리 전송 시 파일 목록을 확보하면 아래 경로로 병렬 전송:
                //
                //   if streams > 1 && has_file_list {
                //       let manager = MultiStreamManager::new(
                //           tunnel.connection().clone(), streams, metrics.clone(),
                //       );
                //       let report = manager.transfer_files(file_list).await;
                //       log_multi_stream_report(&report);
                //   }
                let _ = streams; // used when multi-stream transfer is activated

                // OpenTelemetry: 전송 완료 span에 최종 메트릭을 기록한 뒤 종료
                #[cfg(feature = "otel")]
                {
                    if let Some(s) = &session_span {
                        let transfer = s.transfer_span(&metrics);
                        let _enter = transfer.enter();
                        tracing::info!("transfer complete");
                    }
                    drop(session_span);
                    if let Some(telem) = telemetry {
                        telem.shutdown();
                    }
                }

                if code == 0 {
                    eprintln!("quicsync: done in {:.2}s", elapsed.as_secs_f64());
                } else {
                    eprintln!("quicsync: rsync exited with code {code} in {:.2}s", elapsed.as_secs_f64());
                }
                Ok(code)
            }
            _ = signal_rx.changed() => {
                tracing::warn!("signal received, aborting session...");
                abort(tunnel, &mut ssh_process).await;
                cleanup_web(&web_tasks, &web_log_path);

                #[cfg(feature = "otel")]
                {
                    drop(session_span);
                    if let Some(telem) = telemetry {
                        telem.shutdown();
                    }
                }

                Ok(130) // SIGINT → 128+2=130
            }
        }
    }
}

/// 정상 종료: QUIC 터널 → SSH 프로세스 순서로 정리한다. (Req 8.1)
async fn shutdown(tunnel: QuicTunnel, ssh_process: &mut tokio::process::Child) {
    tracing::info!("shutting down session...");

    if let Err(e) = tunnel.close().await {
        tracing::warn!("QUIC close error: {e}");
    }
    let _ = ssh_process.kill().await;

    tracing::info!("session shutdown complete");
}

/// 비정상 종료: QUIC close → SSH kill 순서로 정리한다. (Req 8.2, 8.3)
/// rsync는 select!에서 아직 실행 중이므로 drop 시 자동 정리된다.
async fn abort(tunnel: QuicTunnel, ssh_process: &mut tokio::process::Child) {
    tracing::warn!("aborting session...");

    if let Err(e) = tunnel.close().await {
        tracing::warn!("QUIC close error during abort: {e}");
    }
    let _ = ssh_process.kill().await;

    tracing::info!("session abort complete");
}

/// --web task들을 abort하고 임시 로그 파일을 삭제한다.
fn cleanup_web(tasks: &[tokio::task::JoinHandle<()>], log_path: &Option<PathBuf>) {
    for t in tasks {
        t.abort();
    }
    if let Some(p) = log_path {
        let _ = std::fs::remove_file(p);
    }
}

/// rsync `--log-file`을 주기적으로 tail하여 완료 파일 수와 현재 파일명을 갱신한다.
/// 파일 경계(파일당 1줄)에서만 동작하므로 데이터 전송 성능에 영향을 주지 않는다.
async fn tail_file_log(path: PathBuf, metrics: Arc<TransferMetrics>) {
    use std::io::SeekFrom;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut offset: u64 = 0;
    let mut pending = String::new();

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let mut file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(_) => continue,
        };
        if file.seek(SeekFrom::Start(offset)).await.is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).await.is_err() || buf.is_empty() {
            continue;
        }
        offset += buf.len() as u64;
        pending.push_str(&String::from_utf8_lossy(&buf));

        while let Some(nl) = pending.find('\n') {
            let line: String = pending.drain(..=nl).collect();
            if let Some(item) = crate::rsync::parse_log_file_line(line.trim_end())
                && item.is_file
            {
                metrics
                    .completed_files
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut cur) = metrics.current_file.lock() {
                    *cur = item.name;
                }
            }
        }
    }
}

/// 경로 목록의 일반 파일 수를 재귀적으로 센다(진행률 분모 산정용 근사치).
fn count_files(paths: &[PathBuf]) -> u64 {
    paths.iter().map(|p| count_path(p)).sum()
}

/// 심볼릭 링크는 따라가지 않아 순환을 방지하고, 일반 파일만 1로 센다.
fn count_path(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if meta.is_file() {
        1
    } else if meta.is_dir() {
        std::fs::read_dir(p)
            .map(|entries| entries.flatten().map(|e| count_path(&e.path())).sum())
            .unwrap_or(0)
    } else {
        0 // 심볼릭 링크/특수 파일은 제외
    }
}

/// 기본 브라우저로 URL을 연다. quicsync는 Linux/macOS만 지원하므로
/// macOS는 `open`, Linux는 `xdg-open`을 사용한다. 실패는 비치명적이다(URL은 이미 출력됨).
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";

    let _ = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// SIGINT/SIGTERM 시그널 핸들러를 등록한다.
/// 시그널 수신 시 watch 채널을 통해 true를 전파한다.
pub fn install_signal_handlers() -> Result<watch::Receiver<bool>, SessionError> {
    let (tx, rx) = watch::channel(false);

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");

            tokio::select! {
                _ = ctrl_c => {}
                _ = sigterm.recv() => {}
            }
        }

        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }

        let _ = tx.send(true);
    });

    Ok(rx)
}
