// 전체 세션 생명주기 관리 및 시그널 핸들링

use std::net::SocketAddr;

use tokio::sync::watch;

use crate::buffer::BufferLayer;
use crate::error::SessionError;
use crate::quic::{QuicClientCfg, QuicTunnel};
use crate::rsync::RsyncChild;
use crate::ssh::launch_remote_server;
use crate::tcp_proxy::TcpProxy;
use crate::types::CliArgs;

/// 세션: SSH → QUIC → TCP_Proxy → rsync 전체 파이프라인을 관리한다.
pub struct Session {
    ssh_process: tokio::process::Child,
    tunnel: QuicTunnel,
    rsync: RsyncChild,
    started_at: std::time::Instant,
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
        let remote_display = match &args.remote.user {
            Some(u) => format!("{}@{}:{}", u, args.remote.host, args.remote.path),
            None => format!("{}:{}", args.remote.host, args.remote.path),
        };
        let direction_label = match args.direction {
            crate::types::TransferDirection::Push => "push",
            crate::types::TransferDirection::Pull => "pull",
        };
        eprintln!("quicsync: {} → {} ({})", remote_display, direction_label,
            if args.local_paths.len() == 1 {
                args.local_paths[0].display().to_string()
            } else {
                format!("{} paths", args.local_paths.len())
            }
        );

        // 1. SSH로 원격 서버 실행
        tracing::info!("launching remote server via SSH...");
        let handshake = launch_remote_server(&args.remote)
            .await
            .map_err(|e| SessionError::InitFailed(format!("SSH: {e}")))?;

        let ssh_process = handshake.ssh_process;
        let host_port = format!("{}:{}", args.remote.host, handshake.remote_port);
        let remote_addr: SocketAddr = tokio::net::lookup_host(&host_port)
            .await
            .map_err(|e| SessionError::InitFailed(format!("DNS resolve {host_port}: {e}")))?
            .next()
            .ok_or_else(|| SessionError::InitFailed(format!("no address found for {host_port}")))?;

        // 2. QUIC 터널 수립
        tracing::info!("connecting QUIC tunnel to {}...", remote_addr);
        let tunnel = QuicTunnel::connect(QuicClientCfg {
            remote_addr,
            auth_token: handshake.auth_token.clone(),
            server_name: "localhost".to_string(),
            window_bytes: args.quic_window,
        })
        .await
        .map_err(|e| SessionError::InitFailed(format!("QUIC: {e}")))?;

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
        let rsync = RsyncChild::spawn(
            &args.rsync_options,
            &args.local_paths,
            &args.remote,
            proxy_port,
            args.direction,
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
        tokio::spawn(async move {
            if let Err(e) = buffer.relay_forward(fwd_rx, send_stream).await {
                tracing::error!("forward relay error: {e}");
            }
        });

        // Buffer reverse relay: QUIC RecvStream → 채널
        let reverse_buffer = BufferLayer::from_env();
        tokio::spawn(async move {
            if let Err(e) = reverse_buffer.relay_reverse(recv_stream, rev_tx).await {
                tracing::error!("reverse relay error: {e}");
            }
        });

        tracing::info!("session started, waiting for rsync to complete...");

        Ok(Self {
            ssh_process,
            tunnel,
            rsync,
            started_at,
        })
    }

    /// 세션 실행: rsync 완료 또는 시그널 수신까지 대기한다.
    ///
    /// - rsync 정상 완료 → shutdown → 종료 코드 반환
    /// - SIGINT/SIGTERM → abort → 종료 코드 반환
    pub async fn run(self) -> Result<i32, SessionError> {
        let mut signal_rx = install_signal_handlers()?;

        let Session { mut ssh_process, tunnel, rsync, started_at } = self;

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
                let elapsed = started_at.elapsed();
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
/// SIGINT/SIGTERM 시그널 핸들러를 등록한다.
/// 시그널 수신 시 watch 채널을 통해 true를 전파한다.
pub fn install_signal_handlers() -> Result<watch::Receiver<bool>, SessionError> {
    let (tx, rx) = watch::channel(false);

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate())
                .expect("failed to register SIGTERM handler");

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

