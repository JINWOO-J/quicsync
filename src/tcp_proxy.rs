// 로컬 TCP 리스닝 및 양방향 바이트 스트림 중계

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::error::ProxyError;

/// 로컬 TCP 프록시 — rsync 트래픽을 수신하여 채널로 중계
pub struct TcpProxy {
    listener: TcpListener,
    port: u16,
}

/// TCP 읽기 버퍼 크기: 64KB
const TCP_READ_BUF_SIZE: usize = 64 * 1024;

impl TcpProxy {
    /// 127.0.0.1:0에 바인딩하여 OS가 임시 포트를 할당하도록 한다.
    pub async fn bind() -> Result<Self, ProxyError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| ProxyError::BindFailed(e.to_string()))?;

        let port = listener
            .local_addr()
            .map_err(|e| ProxyError::BindFailed(e.to_string()))?
            .port();

        Ok(Self { listener, port })
    }

    /// 바인딩된 포트 번호를 반환한다.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// rsync 연결을 1개 수락한 뒤 양방향 바이트 스트림을 중계한다.
    ///
    /// - `tcp_to_quic`: TCP에서 읽은 데이터를 Buffer_Layer 방향으로 전달
    /// - `quic_to_tcp`: QUIC 방향에서 수신한 데이터를 TCP로 전달
    ///
    /// 어느 한쪽 방향이 종료되면 나머지도 정리하고 반환한다.
    pub async fn relay(
        self,
        tcp_to_quic: mpsc::Sender<Bytes>,
        mut quic_to_tcp: mpsc::Receiver<Bytes>,
    ) -> Result<(), ProxyError> {
        let (stream, _addr) = self
            .listener
            .accept()
            .await
            .map_err(|e| ProxyError::RelayError(format!("accept failed: {e}")))?;

        // listener는 연결 1개만 수락하므로 drop하여 리소스 해제 (Req 3.5)
        drop(self.listener);

        let (mut read_half, mut write_half) = stream.into_split();

        // TCP → channel (rsync → Buffer_Layer 방향)
        let tx = tcp_to_quic;
        let tcp_read = tokio::spawn(async move {
            let mut buf = vec![0u8; TCP_READ_BUF_SIZE];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => {
                        tracing::debug!("tcp_proxy: TCP read EOF (rsync/connect closed write side)");
                        break Ok(());
                    }
                    Ok(n) => {
                        if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                            break Ok(()); // receiver dropped
                        }
                    }
                    Err(e) => break Err(ProxyError::RelayError(format!("tcp read: {e}"))),
                }
            }
        });

        // channel → TCP (Buffer_Layer → rsync 방향)
        let tcp_write = tokio::spawn(async move {
            while let Some(data) = quic_to_tcp.recv().await {
                if let Err(e) = write_half.write_all(&data).await {
                    return Err(ProxyError::RelayError(format!("tcp write: {e}")));
                }
            }
            tracing::debug!("tcp_proxy: tcp_write channel closed, shutting down write half");
            let _ = write_half.shutdown().await;
            Ok(())
        });

        // 어느 한쪽이 끝나면 나머지도 정리
        tokio::select! {
            result = tcp_read => {
                match result {
                    Ok(inner) => inner?,
                    Err(e) => return Err(ProxyError::RelayError(format!("tcp_read task: {e}"))),
                }
            }
            result = tcp_write => {
                match result {
                    Ok(inner) => inner?,
                    Err(e) => return Err(ProxyError::RelayError(format!("tcp_write task: {e}"))),
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_assigns_nonzero_port() {
        let proxy = TcpProxy::bind().await.unwrap();
        assert!(proxy.port() > 0);
    }

    #[tokio::test]
    async fn bind_port_is_accessible() {
        let proxy = TcpProxy::bind().await.unwrap();
        let port = proxy.port();

        // 같은 포트에 다시 바인딩하면 실패해야 한다 (이미 사용 중)
        let result = TcpListener::bind(format!("127.0.0.1:{port}")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn relay_forwards_tcp_to_channel() {
        let proxy = TcpProxy::bind().await.unwrap();
        let port = proxy.port();

        let (tx, mut rx) = mpsc::channel::<Bytes>(16);
        let (_quic_tx, quic_rx) = mpsc::channel::<Bytes>(16);

        // relay를 백그라운드에서 실행
        let relay_handle = tokio::spawn(async move {
            proxy.relay(tx, quic_rx).await
        });

        // rsync 역할: TCP 연결 후 데이터 전송
        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        client.write_all(b"hello from rsync").await.unwrap();
        drop(client); // 연결 종료

        // 채널에서 데이터 수신 확인
        let received = rx.recv().await.unwrap();
        assert_eq!(&received[..], b"hello from rsync");

        // relay 종료 대기
        let _ = relay_handle.await;
    }

    #[tokio::test]
    async fn relay_forwards_channel_to_tcp() {
        let proxy = TcpProxy::bind().await.unwrap();
        let port = proxy.port();

        let (tx, _rx) = mpsc::channel::<Bytes>(16);
        let (quic_tx, quic_rx) = mpsc::channel::<Bytes>(16);

        let relay_handle = tokio::spawn(async move {
            proxy.relay(tx, quic_rx).await
        });

        // rsync 역할: TCP 연결
        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        // QUIC 방향에서 데이터 전송
        quic_tx.send(Bytes::from_static(b"hello from quic")).await.unwrap();
        drop(quic_tx); // 채널 종료 → write 루프 종료

        // TCP에서 데이터 수신 확인
        let mut buf = vec![0u8; 64];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from quic");

        let _ = relay_handle.await;
    }

    #[tokio::test]
    async fn relay_bidirectional() {
        let proxy = TcpProxy::bind().await.unwrap();
        let port = proxy.port();

        let (tx, mut rx) = mpsc::channel::<Bytes>(16);
        let (quic_tx, quic_rx) = mpsc::channel::<Bytes>(16);

        let relay_handle = tokio::spawn(async move {
            proxy.relay(tx, quic_rx).await
        });

        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        // TCP → channel 방향
        client.write_all(b"request").await.unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(&received[..], b"request");

        // channel → TCP 방향
        quic_tx.send(Bytes::from_static(b"response")).await.unwrap();

        let mut buf = vec![0u8; 64];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"response");

        // 정리
        drop(client);
        drop(quic_tx);
        let _ = relay_handle.await;
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    // Feature: quicsync-tunnel-mvp, Property 4: 양방향 데이터 무결성
    // **Validates: Requirements 3.3, 3.4**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Forward: 임의 바이트를 TCP에 쓰면 채널에서 동일한 바이트가 수신된다.
        #[test]
        fn prop_forward_data_integrity(data in proptest::collection::vec(any::<u8>(), 1..=4096)) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let proxy = TcpProxy::bind().await.unwrap();
                let port = proxy.port();

                let (tx, mut rx) = mpsc::channel::<Bytes>(64);
                let (_quic_tx, quic_rx) = mpsc::channel::<Bytes>(64);

                let relay_handle = tokio::spawn(async move {
                    proxy.relay(tx, quic_rx).await
                });

                let data_clone = data.clone();
                let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                    .await
                    .unwrap();
                client.write_all(&data_clone).await.unwrap();
                drop(client);

                // 채널에서 모든 청크를 수집하여 원본과 비교
                let mut received = Vec::new();
                while let Some(chunk) = rx.recv().await {
                    received.extend_from_slice(&chunk);
                    if received.len() >= data.len() {
                        break;
                    }
                }

                prop_assert_eq!(received, data);

                let _ = relay_handle.await;
                Ok(())
            })?;
        }

        /// Reverse: 채널로 보낸 임의 바이트가 TCP에서 동일하게 수신된다.
        #[test]
        fn prop_reverse_data_integrity(data in proptest::collection::vec(any::<u8>(), 1..=4096)) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let proxy = TcpProxy::bind().await.unwrap();
                let port = proxy.port();

                let (tx, _rx) = mpsc::channel::<Bytes>(64);
                let (quic_tx, quic_rx) = mpsc::channel::<Bytes>(64);

                let relay_handle = tokio::spawn(async move {
                    proxy.relay(tx, quic_rx).await
                });

                let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                    .await
                    .unwrap();

                let data_clone = data.clone();
                quic_tx.send(Bytes::from(data_clone)).await.unwrap();
                drop(quic_tx);

                // TCP에서 모든 데이터를 수집
                let mut received = Vec::new();
                let mut buf = vec![0u8; 8192];
                loop {
                    match client.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => received.extend_from_slice(&buf[..n]),
                        Err(_) => break,
                    }
                }

                prop_assert_eq!(received, data);

                let _ = relay_handle.await;
                Ok(())
            })?;
        }

        /// Bidirectional: 양방향 동시 전송 시 각 방향의 데이터 무결성이 보존된다.
        #[test]
        fn prop_bidirectional_data_integrity(
            forward_data in proptest::collection::vec(any::<u8>(), 1..=2048),
            reverse_data in proptest::collection::vec(any::<u8>(), 1..=2048),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let proxy = TcpProxy::bind().await.unwrap();
                let port = proxy.port();

                let (tx, mut rx) = mpsc::channel::<Bytes>(64);
                let (quic_tx, quic_rx) = mpsc::channel::<Bytes>(64);

                let relay_handle = tokio::spawn(async move {
                    proxy.relay(tx, quic_rx).await
                });

                let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                    .await
                    .unwrap();

                // reverse 방향: channel → TCP
                let reverse_clone = reverse_data.clone();
                quic_tx.send(Bytes::from(reverse_clone)).await.unwrap();
                drop(quic_tx);

                // TCP에서 reverse 데이터 수신
                let mut reverse_received = Vec::new();
                let mut buf = vec![0u8; 8192];
                loop {
                    match client.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            reverse_received.extend_from_slice(&buf[..n]);
                            if reverse_received.len() >= reverse_data.len() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }

                // forward 방향: TCP → channel
                let forward_clone = forward_data.clone();
                client.write_all(&forward_clone).await.unwrap();
                drop(client);

                let mut forward_received = Vec::new();
                while let Some(chunk) = rx.recv().await {
                    forward_received.extend_from_slice(&chunk);
                    if forward_received.len() >= forward_data.len() {
                        break;
                    }
                }

                prop_assert_eq!(forward_received, forward_data);
                prop_assert_eq!(reverse_received, reverse_data);

                let _ = relay_handle.await;
                Ok(())
            })?;
        }
    }
}

