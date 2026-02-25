// RingBuffer 처리량 벤치마크
//
// 다양한 chunk 크기 및 capacity에서의 write/read 성능을 측정한다.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use quicsync::buffer::RingBuffer;

/// write-only 처리량: capacity를 가득 채울 때까지 기록
fn bench_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer/write");

    for &chunk_size in &[64, 1024, 64 * 1024, 256 * 1024] {
        let capacity = 4 * 1024 * 1024; // 4MB
        let data = vec![0xABu8; chunk_size];

        group.throughput(Throughput::Bytes(capacity as u64));
        group.bench_with_input(
            BenchmarkId::new("chunk", chunk_size),
            &chunk_size,
            |b, _| {
                b.iter(|| {
                    let mut buf = RingBuffer::new(capacity);
                    let mut written_total = 0;
                    while written_total < capacity {
                        match buf.write(&data) {
                            Ok(n) => written_total += n,
                            Err(_) => break,
                        }
                    }
                    written_total
                });
            },
        );
    }

    group.finish();
}

/// read-only 처리량: 가득 찬 버퍼를 비울 때까지 읽기
fn bench_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer/read");

    for &chunk_size in &[64, 1024, 64 * 1024, 256 * 1024] {
        let capacity = 4 * 1024 * 1024;

        group.throughput(Throughput::Bytes(capacity as u64));
        group.bench_with_input(
            BenchmarkId::new("chunk", chunk_size),
            &chunk_size,
            |b, _| {
                b.iter_batched(
                    || {
                        let mut buf = RingBuffer::new(capacity);
                        let fill = vec![0xCDu8; capacity];
                        buf.write(&fill).unwrap();
                        buf
                    },
                    |mut buf| {
                        let mut out = vec![0u8; chunk_size];
                        let mut read_total = 0;
                        while read_total < capacity {
                            let n = buf.read(&mut out);
                            if n == 0 {
                                break;
                            }
                            read_total += n;
                        }
                        read_total
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// write→read 왕복 처리량: 한 청크 쓰고 한 청크 읽기 반복
fn bench_write_read_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer/roundtrip");

    for &chunk_size in &[64, 1024, 64 * 1024] {
        let total_bytes = 4 * 1024 * 1024u64;
        let capacity = chunk_size * 4; // 4 청크분의 버퍼
        let data = vec![0xEFu8; chunk_size];

        group.throughput(Throughput::Bytes(total_bytes));
        group.bench_with_input(
            BenchmarkId::new("chunk", chunk_size),
            &chunk_size,
            |b, _| {
                b.iter(|| {
                    let mut buf = RingBuffer::new(capacity);
                    let mut out = vec![0u8; chunk_size];
                    let iterations = total_bytes as usize / chunk_size;

                    for _ in 0..iterations {
                        buf.write(&data).unwrap();
                        buf.read(&mut out);
                    }
                });
            },
        );
    }

    group.finish();
}

/// 다양한 capacity에서의 write 성능 비교
fn bench_capacity_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer/capacity_scaling");
    let chunk_size = 4096;
    let data = vec![0xABu8; chunk_size];

    for &capacity in &[64 * 1024, 1024 * 1024, 16 * 1024 * 1024] {
        group.throughput(Throughput::Bytes(capacity as u64));
        group.bench_with_input(
            BenchmarkId::new("capacity", capacity),
            &capacity,
            |b, &cap| {
                b.iter(|| {
                    let mut buf = RingBuffer::new(cap);
                    let mut total = 0;
                    while total < cap {
                        match buf.write(&data) {
                            Ok(n) => total += n,
                            Err(_) => break,
                        }
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_write,
    bench_read,
    bench_write_read_roundtrip,
    bench_capacity_scaling,
);
criterion_main!(benches);
