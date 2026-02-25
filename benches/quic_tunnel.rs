// QUIC 터널 성능 벤치마크
//
// localhost에서 QUIC 핸드셰이크 레이턴시, 단방향/양방향 스트림 처리량을 측정한다.

use std::net::SocketAddr;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::ServerName;

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

fn make_server_endpoint(window_bytes: u64) -> quinn::Endpoint {
    let (cert, key) = quicsync::quic::generate_self_signed_cert().unwrap();
    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    quicsync::quic::build_server_endpoint(addr, tls_config, window_bytes).unwrap()
}

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

/// QUIC 핸드셰이크 레이턴시 (연결 수립만)
fn bench_handshake(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("quic/handshake", |b| {
        b.iter(|| {
            rt.block_on(async {
                let server_ep = make_server_endpoint(8 * 1024 * 1024);
                let server_addr = server_ep.local_addr().unwrap();
                let client_ep = make_client_endpoint();

                let server_task = tokio::spawn(async move {
                    let incoming = server_ep.accept().await.unwrap();
                    let _conn = incoming.await.unwrap();
                });

                let conn = client_ep
                    .connect(server_addr, "localhost")
                    .unwrap()
                    .await
                    .unwrap();

                server_task.await.unwrap();

                conn.close(0u32.into(), b"done");
                client_ep.wait_idle().await;
            });
        });
    });
}

/// 단방향 스트림 처리량: 클라이언트 → 서버
fn bench_unidirectional_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("quic/unidirectional");

    for &data_size in &[256 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        group.throughput(Throughput::Bytes(data_size as u64));
        group.bench_with_input(
            BenchmarkId::new("size", data_size),
            &data_size,
            |b, &size| {
                b.iter(|| {
                    rt.block_on(async {
                        let server_ep = make_server_endpoint(16 * 1024 * 1024);
                        let server_addr = server_ep.local_addr().unwrap();
                        let client_ep = make_client_endpoint();

                        let server_task = tokio::spawn(async move {
                            let incoming = server_ep.accept().await.unwrap();
                            let conn = incoming.await.unwrap();
                            let (_send, mut recv) = conn.accept_bi().await.unwrap();
                            let data = recv.read_to_end(size + 1024).await.unwrap();
                            data.len()
                        });

                        let conn = client_ep
                            .connect(server_addr, "localhost")
                            .unwrap()
                            .await
                            .unwrap();

                        let (mut send, _recv) = conn.open_bi().await.unwrap();
                        let payload = vec![0xABu8; size];
                        for chunk in payload.chunks(64 * 1024) {
                            send.write_all(chunk).await.unwrap();
                        }
                        send.finish().unwrap();

                        let received = server_task.await.unwrap();
                        assert_eq!(received, size);

                        conn.close(0u32.into(), b"done");
                        client_ep.wait_idle().await;
                    });
                });
            },
        );
    }

    group.finish();
}

/// 양방향 스트림 처리량: 동시 전송
fn bench_bidirectional_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data_size = 1024 * 1024; // 1MB 양방향

    c.bench_function("quic/bidirectional_1mb", |b| {
        b.iter(|| {
            rt.block_on(async {
                let server_ep = make_server_endpoint(16 * 1024 * 1024);
                let server_addr = server_ep.local_addr().unwrap();
                let client_ep = make_client_endpoint();

                let (server_result, client_result) = tokio::join!(
                    async {
                        let incoming = server_ep.accept().await.unwrap();
                        let conn = incoming.await.unwrap();
                        let (mut send, mut recv) = conn.accept_bi().await.unwrap();

                        // 수신
                        let data = recv.read_to_end(data_size + 1024).await.unwrap();

                        // 에코
                        send.write_all(&data).await.unwrap();
                        send.finish().unwrap();

                        // 클라이언트가 연결을 닫을 때까지 대기
                        conn.closed().await;
                        data.len()
                    },
                    async {
                        let conn = client_ep
                            .connect(server_addr, "localhost")
                            .unwrap()
                            .await
                            .unwrap();

                        let (mut send, mut recv) = conn.open_bi().await.unwrap();

                        // 전송
                        let payload = vec![0xCDu8; data_size];
                        send.write_all(&payload).await.unwrap();
                        send.finish().unwrap();

                        // 에코 수신
                        let echoed = recv.read_to_end(data_size + 1024).await.unwrap();
                        assert_eq!(echoed.len(), data_size);

                        conn.close(0u32.into(), b"done");
                        client_ep.wait_idle().await;
                        echoed.len()
                    }
                );

                assert_eq!(server_result, data_size);
                assert_eq!(client_result, data_size);
            });
        });
    });
}

/// 윈도우 크기에 따른 처리량 비교
fn bench_window_size_comparison(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("quic/window_size");
    let data_size = 2 * 1024 * 1024; // 2MB

    for &window_mb in &[1, 8, 64] {
        let window_bytes = window_mb as u64 * 1024 * 1024;
        group.throughput(Throughput::Bytes(data_size as u64));
        group.bench_with_input(
            BenchmarkId::new("window_mb", window_mb),
            &window_bytes,
            |b, &wb| {
                b.iter(|| {
                    rt.block_on(async {
                        let server_ep = make_server_endpoint(wb);
                        let server_addr = server_ep.local_addr().unwrap();
                        let client_ep = make_client_endpoint();

                        let (server_result, _client_result) = tokio::join!(
                            async {
                                let incoming = server_ep.accept().await.unwrap();
                                let conn = incoming.await.unwrap();
                                let (_send, mut recv) = conn.accept_bi().await.unwrap();
                                let len = recv.read_to_end(data_size + 1024).await.unwrap().len();
                                conn.closed().await;
                                len
                            },
                            async {
                                let conn = client_ep
                                    .connect(server_addr, "localhost")
                                    .unwrap()
                                    .await
                                    .unwrap();

                                let (mut send, _recv) = conn.open_bi().await.unwrap();
                                let payload = vec![0xEFu8; data_size];
                                send.write_all(&payload).await.unwrap();
                                send.finish().unwrap();

                                // 서버가 데이터를 다 읽을 시간을 주기 위해 잠시 대기
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                                conn.close(0u32.into(), b"done");
                                client_ep.wait_idle().await;
                            }
                        );

                        assert_eq!(server_result, data_size);
                    });
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_handshake,
    bench_unidirectional_throughput,
    bench_bidirectional_throughput,
    bench_window_size_comparison,
);
criterion_main!(benches);
