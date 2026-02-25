// AuthToken 연산 벤치마크
//
// generate, to_hex, from_hex, verify 각 연산의 성능을 측정한다.

use criterion::{Criterion, criterion_group, criterion_main};
use quicsync::types::AuthToken;

/// AuthToken::generate() 성능 — 암호학적 RNG 호출 포함
fn bench_generate(c: &mut Criterion) {
    c.bench_function("auth_token/generate", |b| {
        b.iter(|| AuthToken::generate());
    });
}

/// to_hex 변환 성능
fn bench_to_hex(c: &mut Criterion) {
    let token = AuthToken::generate();
    c.bench_function("auth_token/to_hex", |b| {
        b.iter(|| token.to_hex());
    });
}

/// from_hex 파싱 성능
fn bench_from_hex(c: &mut Criterion) {
    let token = AuthToken::generate();
    let hex = token.to_hex();
    c.bench_function("auth_token/from_hex", |b| {
        b.iter(|| AuthToken::from_hex(&hex).unwrap());
    });
}

/// to_hex → from_hex 라운드트립 성능
fn bench_hex_roundtrip(c: &mut Criterion) {
    let token = AuthToken::generate();
    c.bench_function("auth_token/hex_roundtrip", |b| {
        b.iter(|| {
            let hex = token.to_hex();
            AuthToken::from_hex(&hex).unwrap()
        });
    });
}

/// verify (동일 토큰) — 상수 시간 비교
fn bench_verify_same(c: &mut Criterion) {
    let token = AuthToken::generate();
    let clone = AuthToken::from_raw(*token.as_bytes());
    c.bench_function("auth_token/verify_same", |b| {
        b.iter(|| token.verify(&clone));
    });
}

/// verify (다른 토큰) — 상수 시간 비교
fn bench_verify_different(c: &mut Criterion) {
    let token_a = AuthToken::generate();
    let token_b = AuthToken::generate();
    c.bench_function("auth_token/verify_different", |b| {
        b.iter(|| token_a.verify(&token_b));
    });
}

criterion_group!(
    benches,
    bench_generate,
    bench_to_hex,
    bench_from_hex,
    bench_hex_roundtrip,
    bench_verify_same,
    bench_verify_different,
);
criterion_main!(benches);
