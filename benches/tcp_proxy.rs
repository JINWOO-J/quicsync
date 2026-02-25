// TcpProxy relay 처리량 벤치마크
//
// 다양한 chunk 크기에서 TCP → channel, channel → TCP 처리량을 측정한다.

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

const TOTAL_BYTES: usize = 4 * 1024 * 1024; // 4MB

/// Forward 방향 처리량: TCP → channel
fn bench_forward(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("tcp_proxy/forward");

    for &chunk_size in &[1024, 64 * 1024, 256 * 1024] {
        group.throughput(Throughput::Bytes(TOTAL_BYTES as u64));
        group.bench_with_input(
            BenchmarkId::new("chunk", chunk_size),
            &chunk_size,
            |b, &cs| {
                b.iter(|| {
                    rt.block_on(async {
                        let proxy = quicsync::tcp_proxy::TcpProxy::bind().await.unwrap();
                        let port = proxy.port();

                        let (tx, mut rx) = mpsc::channel::<Bytes>(256);
                        let (_quic_tx, quic_rx) = mpsc::channel::<Bytes>(256);

                        let relay_handle =
                            tokio::spawn(async move { proxy.relay(tx, quic_rx).await });

                        // 데이터 전송
                        let data = vec![0xABu8; cs];
                        let send_handle = tokio::spawn(async move {
                            let mut stream = tokio::net::TcpStream::connect(format!(
                                "127.0.0.1:{port}"
                            ))
                            .await
                            .unwrap();
                            let mut sent = 0;
                            while sent < TOTAL_BYTES {
                                let to_send = cs.min(TOTAL_BYTES - sent);
                                stream.write_all(&data[..to_send]).await.unwrap();
                                sent += to_send;
                            }
                            drop(stream);
                        });

                        // 수신 측 drain
                        let mut received = 0;
                        while let Some(chunk) = rx.recv().await {
                            received += chunk.len();
                            if received >= TOTAL_BYTES {
                                break;
                            }
                        }

                        send_handle.await.unwrap();
                        let _ = relay_handle.await;
                        received
                    })
                });
            },
        );
    }

    group.finish();
}

/// Reverse 방향 처리량: channel → TCP
fn bench_reverse(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("tcp_proxy/reverse");

    for &chunk_size in &[1024, 64 * 1024, 256 * 1024] {
        group.throughput(Throughput::Bytes(TOTAL_BYTES as u64));
        group.bench_with_input(
            BenchmarkId::new("chunk", chunk_size),
            &chunk_size,
            |b, &cs| {
                b.iter(|| {
                    rt.block_on(async {
                        let proxy = quicsync::tcp_proxy::TcpProxy::bind().await.unwrap();
                        let port = proxy.port();

                        let (tx, _rx) = mpsc::channel::<Bytes>(256);
                        let (quic_tx, quic_rx) = mpsc::channel::<Bytes>(256);

                        let relay_handle =
                            tokio::spawn(async move { proxy.relay(tx, quic_rx).await });

                        // TCP 클라이언트 접속 & 수신
                        let recv_handle = tokio::spawn(async move {
                            let mut stream = tokio::net::TcpStream::connect(format!(
                                "127.0.0.1:{port}"
                            ))
                            .await
                            .unwrap();
                            let mut buf = vec![0u8; cs];
                            let mut received = 0;
                            loop {
                                match stream.read(&mut buf).await {
                                    Ok(0) => break,
                                    Ok(n) => received += n,
                                    Err(_) => break,
                                }
                            }
                            received
                        });

                        // channel → proxy 방향 데이터 전송
                        let data = vec![0xCDu8; cs];
                        let mut sent = 0;
                        while sent < TOTAL_BYTES {
                            let to_send = cs.min(TOTAL_BYTES - sent);
                            quic_tx
                                .send(Bytes::copy_from_slice(&data[..to_send]))
                                .await
                                .unwrap();
                            sent += to_send;
                        }
                        drop(quic_tx);

                        let received = recv_handle.await.unwrap();
                        let _ = relay_handle.await;
                        received
                    })
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_forward, bench_reverse,);
criterion_main!(benches);
