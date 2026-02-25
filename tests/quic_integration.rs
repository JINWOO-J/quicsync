// QUIC 클라이언트 ↔ 서버 통합 테스트
//
// 실제 quinn endpoint를 생성하여 localhost에서 연결하고,
// 양방향 스트림으로 데이터를 송수신하여 무결성을 검증한다.
//
// multi_thread 런타임을 사용하여 서버/클라이언트 태스크가
// 별도 스레드에서 독립적으로 스케줄링되도록 한다.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::ServerName;

/// 테스트용 서버 endpoint 생성
fn make_server_endpoint(bind_addr: SocketAddr) -> quinn::Endpoint {
    let (cert, key) = quicsync::quic::generate_self_signed_cert().unwrap();

    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();

    quicsync::quic::build_server_endpoint(bind_addr, tls_config, 8 * 1024 * 1024).unwrap()
}

/// 테스트용 클라이언트 endpoint 생성 (서버 인증서 무시)
fn make_client_endpoint() -> quinn::Endpoint {
    let mut endpoint = quicsync::quic::build_client_endpoint().unwrap();

    let rustls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerify))
        .with_no_client_auth();

    let quic_config = QuicClientConfig::try_from(rustls_config).unwrap();
    let client_config = quinn::ClientConfig::new(Arc::new(quic_config));
    endpoint.set_default_client_config(client_config);

    endpoint
}

#[derive(Debug)]
struct SkipVerify;

impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// QUIC 연결 수립 및 양방향 스트림 에코
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quic_connect_and_open_bi() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_ep = make_server_endpoint(addr);
    let server_addr = server_ep.local_addr().unwrap();
    let client_ep = make_client_endpoint();

    let payload = b"hello quicsync integration test!";

    let (server_received, client_received) = tokio::join!(
        // 서버
        async {
            let incoming = server_ep.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();

            let mut buf = vec![0u8; 1024];
            let n = recv.read(&mut buf).await.unwrap().unwrap_or(0);
            let received = buf[..n].to_vec();

            send.write_all(&received).await.unwrap();
            send.finish().unwrap();

            // 클라이언트가 close할 때까지 대기
            conn.closed().await;
            received
        },
        // 클라이언트
        async {
            let conn = client_ep
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            send.write_all(payload).await.unwrap();
            send.finish().unwrap();

            let mut response = vec![0u8; 1024];
            let n = recv.read(&mut response).await.unwrap().unwrap_or(0);
            let received = response[..n].to_vec();

            conn.close(0u32.into(), b"done");
            received
        }
    );

    assert_eq!(server_received, payload);
    assert_eq!(client_received, payload);
}

/// 대용량 데이터 전송 무결성 테스트 (1MB)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quic_large_data_transfer() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_ep = make_server_endpoint(addr);
    let server_addr = server_ep.local_addr().unwrap();
    let client_ep = make_client_endpoint();

    let data_size = 1024 * 1024; // 1MB
    let original_data: Vec<u8> = (0..data_size).map(|i| (i % 251) as u8).collect();
    let data_for_server = original_data.clone();

    let (server_len, client_ack) = tokio::join!(
        // 서버: 수신 후 확인 응답
        async {
            let incoming = server_ep.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();

            let received = recv.read_to_end(data_size + 1024).await.unwrap();
            assert_eq!(received.len(), data_for_server.len());
            assert_eq!(received, data_for_server);

            send.write_all(b"OK").await.unwrap();
            send.finish().unwrap();

            conn.closed().await;
            received.len()
        },
        // 클라이언트: 대용량 데이터 전송
        async {
            let conn = client_ep
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            let (mut send, mut recv) = conn.open_bi().await.unwrap();

            for chunk in original_data.chunks(64 * 1024) {
                send.write_all(chunk).await.unwrap();
            }
            send.finish().unwrap();

            let mut ack = vec![0u8; 16];
            let n = recv.read(&mut ack).await.unwrap().unwrap_or(0);
            let result = ack[..n].to_vec();

            conn.close(0u32.into(), b"done");
            result
        }
    );

    assert_eq!(server_len, data_size);
    assert_eq!(client_ack, b"OK");
}

/// 인증 토큰 전송/검증 시뮬레이션
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quic_auth_token_flow() {
    use quicsync::types::AuthToken;

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_ep = make_server_endpoint(addr);
    let server_addr = server_ep.local_addr().unwrap();
    let client_ep = make_client_endpoint();

    let expected_token = AuthToken::generate();
    let token_hex = expected_token.to_hex();

    let (server_valid, client_response) = tokio::join!(
        // 서버: 토큰 수신 및 검증
        async {
            let incoming = server_ep.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            let (mut send, recv) = conn.accept_bi().await.unwrap();

            let mut reader = tokio::io::BufReader::new(recv);
            let mut token_line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut token_line)
                .await
                .unwrap();

            let received_hex = token_line.trim();
            let received_token = AuthToken::from_hex(received_hex).unwrap();
            let valid = expected_token.verify(&received_token);

            let response: &[u8] = if valid { b"AUTH_OK" } else { b"AUTH_FAIL" };
            send.write_all(response).await.unwrap();
            send.finish().unwrap();

            conn.closed().await;
            valid
        },
        // 클라이언트: 토큰 전송
        async {
            let conn = client_ep
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            send.write_all(format!("{}\n", token_hex).as_bytes())
                .await
                .unwrap();

            let mut response = vec![0u8; 32];
            let n = recv.read(&mut response).await.unwrap().unwrap_or(0);
            let result = response[..n].to_vec();

            conn.close(0u32.into(), b"done");
            result
        }
    );

    assert!(server_valid);
    assert_eq!(client_response, b"AUTH_OK");
}

/// 잘못된 토큰 거부 테스트
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quic_auth_token_rejection() {
    use quicsync::types::AuthToken;

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_ep = make_server_endpoint(addr);
    let server_addr = server_ep.local_addr().unwrap();
    let client_ep = make_client_endpoint();

    let server_token = AuthToken::generate();
    let wrong_token = AuthToken::generate();

    let (server_valid, client_response) = tokio::join!(
        // 서버: 검증 실패 예상
        async {
            let incoming = server_ep.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            let (mut send, recv) = conn.accept_bi().await.unwrap();

            let mut reader = tokio::io::BufReader::new(recv);
            let mut token_line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut token_line)
                .await
                .unwrap();

            let received_hex = token_line.trim();
            let received_token = AuthToken::from_hex(received_hex).unwrap();
            let valid = server_token.verify(&received_token);

            let response: &[u8] = if valid { b"AUTH_OK" } else { b"AUTH_FAIL" };
            send.write_all(response).await.unwrap();
            send.finish().unwrap();

            conn.closed().await;
            valid
        },
        // 클라이언트: 잘못된 토큰 전송
        async {
            let conn = client_ep
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            send.write_all(format!("{}\n", wrong_token.to_hex()).as_bytes())
                .await
                .unwrap();

            let mut response = vec![0u8; 32];
            let n = recv.read(&mut response).await.unwrap().unwrap_or(0);
            let result = response[..n].to_vec();

            conn.close(0u32.into(), b"done");
            result
        }
    );

    assert!(!server_valid);
    assert_eq!(client_response, b"AUTH_FAIL");
}

/// 다중 스트림 동시 전송 테스트
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quic_multiple_streams() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_ep = make_server_endpoint(addr);
    let server_addr = server_ep.local_addr().unwrap();
    let client_ep = make_client_endpoint();

    let stream_count: u8 = 5;

    let (server_total, client_total) = tokio::join!(
        // 서버: 여러 스트림 수신
        async {
            let incoming = server_ep.accept().await.unwrap();
            let conn = incoming.await.unwrap();

            let mut handles = Vec::new();
            for _ in 0..stream_count {
                let conn = conn.clone();
                handles.push(tokio::spawn(async move {
                    let (mut send, mut recv) = conn.accept_bi().await.unwrap();
                    let data = recv.read_to_end(64 * 1024).await.unwrap();
                    send.write_all(&data).await.unwrap();
                    send.finish().unwrap();
                    data.len()
                }));
            }

            let mut total = 0;
            for h in handles {
                total += h.await.unwrap();
            }

            conn.closed().await;
            total
        },
        // 클라이언트: 여러 스트림 전송
        async {
            let conn = client_ep
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            let mut handles = Vec::new();
            for i in 0u8..stream_count {
                let conn = conn.clone();
                handles.push(tokio::spawn(async move {
                    let (mut send, mut recv) = conn.open_bi().await.unwrap();
                    let payload = vec![i; 1024];
                    send.write_all(&payload).await.unwrap();
                    send.finish().unwrap();

                    let mut response = vec![0u8; 2048];
                    let n = recv.read(&mut response).await.unwrap().unwrap_or(0);
                    assert_eq!(&response[..n], &payload[..]);
                    n
                }));
            }

            let mut total = 0;
            for h in handles {
                total += h.await.unwrap();
            }

            conn.close(0u32.into(), b"done");
            total
        }
    );

    assert_eq!(server_total, client_total);
    assert_eq!(server_total, stream_count as usize * 1024);
}

/// TcpProxy ↔ QUIC 통합
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_proxy_to_quic_integration() {
    use tokio::sync::mpsc;

    // 1. TcpProxy에서 데이터 수집
    let proxy = quicsync::tcp_proxy::TcpProxy::bind().await.unwrap();
    let proxy_port = proxy.port();

    let (fwd_tx, mut fwd_rx) = mpsc::channel::<Bytes>(64);
    let (_rev_tx, rev_rx) = mpsc::channel::<Bytes>(64);

    let proxy_handle = tokio::spawn(async move { proxy.relay(fwd_tx, rev_rx).await });

    let test_data = b"tcp-to-quic integration payload";

    // TCP 클라이언트 → proxy
    {
        let mut stream =
            tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
                .await
                .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, test_data)
            .await
            .unwrap();
    } // stream dropped → TCP FIN

    // channel에서 데이터 수집
    let mut collected = Vec::new();
    while let Some(chunk) = fwd_rx.recv().await {
        collected.extend_from_slice(&chunk);
        if collected.len() >= test_data.len() {
            break;
        }
    }
    assert_eq!(collected, test_data);

    // 2. QUIC를 통해 에코
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_ep = make_server_endpoint(addr);
    let server_addr = server_ep.local_addr().unwrap();
    let client_ep = make_client_endpoint();

    let (server_data, echoed) = tokio::join!(
        async {
            let incoming = server_ep.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let data = recv.read_to_end(64 * 1024).await.unwrap();
            send.write_all(&data).await.unwrap();
            send.finish().unwrap();
            conn.closed().await;
            data
        },
        async {
            let conn = client_ep
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            send.write_all(&collected).await.unwrap();
            send.finish().unwrap();

            let data = recv.read_to_end(64 * 1024).await.unwrap();
            conn.close(0u32.into(), b"done");
            data
        }
    );

    assert_eq!(server_data, test_data);
    assert_eq!(echoed, test_data);

    let _ = proxy_handle.await;
}
