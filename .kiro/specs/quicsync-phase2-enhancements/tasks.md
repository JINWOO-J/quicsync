# Implementation Plan: quicsync Phase 2/3 기능 확장

## Overview

기존 Phase 1 MVP 코드베이스(src/ 12개 모듈, 80개 테스트)를 확장하여 6개 신규 모듈(metrics, progress, stats, multi_stream, integrity, telemetry)을 추가하고, 기존 모듈(cli, ssh, quic, session, server, error)을 수정한다. 의존성 추가(blake3, serde, serde_json 등) → 핵심 유틸리티 → 보안 계층 → 관측성 → 멀티스트림 → 통합 순서로 진행한다.

## Tasks

- [ ] 1. 의존성 추가 및 에러 타입 확장
  - [ ] 1.1 Cargo.toml에 Phase 2/3 의존성 추가
    - `blake3 = "1"`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"` 추가
    - `[features]` 섹션에 `otel` feature 추가 (opentelemetry, opentelemetry-otlp, opentelemetry_sdk, tracing-opentelemetry)
    - _Requirements: 전체_

  - [ ] 1.2 error.rs에 새 오류 타입 추가
    - `IntegrityError`, `FingerprintError`, `StatsError`, `TelemetryError`, `MultiStreamError` enum 정의
    - `QuicsyncError`에 새 variant 추가 (`Integrity`, `Fingerprint`, `Stats`, `Telemetry`, `MultiStream`)
    - _Requirements: 3.4, 6.4, 7.3_

  - [ ] 1.3 types.rs에 새 타입 추가
    - `StatsFormat` enum (Text, Json), `HandshakeInfo`에 `fingerprint: Option<String>` 필드 추가
    - `FileEntry`, `StreamResult`, `MultiStreamReport` 구조체 정의
    - _Requirements: 4.5, 6.6, 3.6_

- [ ] 2. CLI 확장 (`src/cli.rs`)
  - [ ] 2.1 새 CLI 플래그 파싱 구현
    - `--no-progress`, `--streams <N>`, `--stats`, `--stats-format <text|json>`, `--otel-endpoint <URL>`, `--no-integrity` 플래그 추가
    - `--streams` 값 범위 검증 (1-64), 범위 밖이면 오류 메시지 출력 후 종료 코드 1
    - `show_progress` 기본값: stdout이 터미널이면 true, 아니면 false
    - _Requirements: 2.6, 2.7, 3.3, 3.4, 4.1, 4.5, 5.1, 5.6, 7.5_

  - [ ]* 2.2 Property 테스트: --streams 옵션 파싱 정확성
    - **Property 2: --streams 옵션 파싱 정확성**
    - proptest로 1-64 범위 정수 → 파싱 성공, 0 또는 65+ → 오류 반환 검증
    - **Validates: Requirements 3.3, 3.4**

- [ ] 3. Checkpoint - 의존성 및 CLI 확장 검증
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 4. Transfer Metrics 및 Progress UI 구현
  - [ ] 4.1 `src/metrics.rs` 신규 생성 - TransferMetrics 구현
    - `TransferMetrics` 구조체 (AtomicU64 기반 lock-free 메트릭)
    - `throughput_bps()`, `eta_secs()`, `rtt_snapshot()`, `snapshot()` 메서드 구현
    - `RttStats`, `MetricsSnapshot` 구조체 정의 (serde Serialize/Deserialize derive)
    - lib.rs에 `pub mod metrics;` 추가
    - _Requirements: 2.1, 2.2, 4.2, 4.3, 4.4_

  - [ ] 4.2 `src/progress.rs` 신규 생성 - Progress UI 구현
    - `ProgressUI` 구조체: `Arc<TransferMetrics>` 참조, `enabled` 플래그
    - `run()` 메서드: 500ms 주기로 stderr에 `\r` 캐리지 리턴으로 상태 갱신
    - `format_bytes()`, `format_speed()`, `format_eta()` 유틸리티 함수 구현
    - 표시 형식: `[모드] 속도 | ETA 시간 | 전송량 / 총량`
    - lib.rs에 `pub mod progress;` 추가
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 2.8, 2.9_

  - [ ]* 4.3 Property 테스트: 바이트/속도 포맷 함수 정확성
    - **Property 1: 바이트/속도 포맷 함수 정확성**
    - proptest로 임의 u64에 대해 `format_bytes` 단위 선택 규칙 검증 (0-999→B, 1000-999999→KB, 1000000-999999999→MB, 1000000000+→GB)
    - proptest로 임의 f64에 대해 `format_speed` 동일 규칙 + "/s" 접미사 검증
    - **Validates: Requirements 2.8, 2.9**

- [ ] 5. Stats Reporter 구현
  - [ ] 5.1 `src/stats.rs` 신규 생성 - StatsReporter 구현
    - `StatsReporter` 구조체: `format: StatsFormat`
    - `report()` 메서드: MetricsSnapshot을 텍스트 또는 JSON으로 stderr에 출력
    - `to_json()`, `from_json()` 함수 구현 (serde_json 사용)
    - 텍스트 리포트에 총 전송 바이트, 평균 처리량, 소요 시간, RTT(평균/최소/최대), 큐 깊이, backpressure 횟수 포함
    - lib.rs에 `pub mod stats;` 추가
    - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5, 4.6, 4.7_

  - [ ]* 5.2 Property 테스트: 텍스트 리포트 필수 필드 포함
    - **Property 4: 텍스트 리포트 필수 필드 포함**
    - proptest로 임의 MetricsSnapshot에 대해 텍스트 리포트 문자열이 모든 필수 필드를 포함하는지 검증
    - **Validates: Requirements 4.2, 4.3, 4.4**

  - [ ]* 5.3 Property 테스트: MetricsSnapshot JSON 라운드트립
    - **Property 5: MetricsSnapshot JSON 라운드트립**
    - proptest로 임의 MetricsSnapshot에 대해 `to_json` → `from_json` 라운드트립 동일성 검증
    - **Validates: Requirements 4.7**

- [ ] 6. Checkpoint - 메트릭/Progress/Stats 검증
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 7. Blake3 무결성 검사 구현
  - [ ] 7.1 `src/integrity.rs` 신규 생성 - Integrity Checker 구현
    - `compute_hash()`, `verify_hash()` 함수 (blake3 크레이트 사용)
    - `encode_chunk()`: [32바이트 Blake3 해시][데이터] 프레이밍
    - `decode_chunk()`: 프레임에서 해시 추출, 데이터 재해시하여 비교, 불일치 시 `IntegrityError::HashMismatch` 반환
    - 프레임 길이 32바이트 미만 시 `IntegrityError::FrameTooShort` 반환
    - lib.rs에 `pub mod integrity;` 추가
    - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.6_

  - [ ]* 7.2 Property 테스트: Blake3 청크 encode/decode 라운드트립
    - **Property 9: Blake3 청크 encode/decode 라운드트립**
    - proptest로 임의 바이트 시퀀스(0-64KB)에 대해 `encode_chunk` → `decode_chunk` 라운드트립 검증
    - 인코딩된 프레임의 처음 32바이트가 원본 데이터의 Blake3 해시와 동일한지 검증
    - **Validates: Requirements 7.1, 7.2, 7.4, 7.6**

  - [ ]* 7.3 Property 테스트: Blake3 손상 감지
    - **Property 10: Blake3 손상 감지**
    - proptest로 임의 바이트 시퀀스를 인코딩 후 데이터 영역(32바이트 이후)의 임의 1바이트 변경 → `decode_chunk`가 `IntegrityError::HashMismatch` 반환 검증
    - **Validates: Requirements 7.3**

- [ ] 8. QUIC 세션 핀닝 구현
  - [ ] 8.1 `src/quic.rs` 수정 - FingerprintVerifier 구현
    - `sha256_fingerprint()`: 인증서 DER 데이터의 SHA-256 지문 계산 (ring 크레이트 사용)
    - `fingerprint_to_hex()`, `fingerprint_from_hex()`: hex 인코딩/디코딩 (64자 문자열)
    - `FingerprintVerifier` 구조체: `ServerCertVerifier` trait 구현, 상수 시간 비교
    - 기존 `SkipServerVerification`을 `FingerprintVerifier`로 대체 (fingerprint가 Some일 때)
    - _Requirements: 6.2, 6.3, 6.4, 6.5_

  - [ ]* 8.2 Property 테스트: 인증서 지문 계산 및 hex 인코딩
    - **Property 7: 인증서 지문 계산 및 hex 인코딩**
    - proptest로 임의 바이트 시퀀스에 대해 `sha256_fingerprint`가 32바이트 반환 검증
    - proptest로 임의 32바이트 배열에 대해 `fingerprint_to_hex` → `fingerprint_from_hex` 라운드트립 검증
    - **Validates: Requirements 6.2, 6.5**

  - [ ]* 8.3 Property 테스트: 인증서 지문 검증
    - **Property 8: 인증서 지문 검증**
    - proptest로 동일한 32바이트 배열 → 검증 성공, 서로 다른 배열 → 검증 실패 검증
    - **Validates: Requirements 6.3, 6.4**

- [ ] 9. 확장 핸드셰이크 구현
  - [ ] 9.1 `src/ssh.rs` 수정 - 확장 핸드셰이크 파싱
    - `parse_handshake` 확장: `QUICSYNC_READY <port> <token> <fingerprint>` 4필드 파싱 지원
    - 3필드(기존 형식) 하위 호환 유지: fingerprint가 없으면 `None`
    - `HandshakeInfo` 구조체에 `fingerprint: Option<String>` 필드 추가
    - _Requirements: 6.1, 6.6_

  - [ ] 9.2 `src/server.rs` 수정 - 서버 핸드셰이크에 지문 포함
    - 서버 시작 시 생성된 인증서의 SHA-256 지문 계산
    - `QUICSYNC_READY <port> <token> <fingerprint>` 형태로 stdout에 출력
    - _Requirements: 6.1_

  - [ ]* 9.3 Property 테스트: 확장 핸드셰이크 파싱 라운드트립
    - **Property 6: 확장 핸드셰이크 파싱 라운드트립**
    - proptest로 임의 u16 포트 + 32바이트 토큰 + 32바이트 지문 → 직렬화 → `parse_handshake` 파싱 라운드트립 검증
    - 3필드 형식(지문 없음) 하위 호환 파싱 검증
    - **Validates: Requirements 6.6**

- [ ] 10. Checkpoint - 보안 계층 검증
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 11. 멀티스트림 병렬 전송 구현
  - [ ] 11.1 `src/multi_stream.rs` 신규 생성 - MultiStreamManager 구현
    - `MultiStreamManager` 구조체: `Connection`, `max_streams: u8`, `Arc<TransferMetrics>`
    - `transfer_files()` 메서드: `tokio::sync::Semaphore`로 동시 스트림 수 제한
    - 각 파일을 독립 QUIC 양방향 스트림에서 전송, 개별 스트림 실패 시 나머지 계속
    - `StreamResult`, `MultiStreamReport` 집계 로직 구현
    - lib.rs에 `pub mod multi_stream;` 추가
    - _Requirements: 3.1, 3.2, 3.5, 3.6, 3.7_

  - [ ]* 11.2 Property 테스트: 스트림 결과 집계 정확성
    - **Property 3: 스트림 결과 집계 정확성**
    - proptest로 임의 `Vec<StreamResult>`에 대해 `total_success` + `total_failed` == 전체 길이 검증
    - `total_success` == `success == true` 개수, `total_failed` == `success == false` 개수 검증
    - **Validates: Requirements 3.6**

- [ ] 12. OpenTelemetry 트레이싱 구현
  - [ ] 12.1 `src/telemetry.rs` 신규 생성 - TelemetryExporter 구현
    - `TelemetryExporter` 구조체: `init()`, `shutdown()` 메서드
    - `SessionSpan`: 루트 span 생성, `ssh_span()`, `quic_span()`, `transfer_span()` 하위 span
    - `transfer_span`에 전송 바이트 수, 소요 시간, 스트림 수를 attribute로 포함
    - 수집기 연결 실패 시 경고 로그만 기록, 전송은 계속
    - `#[cfg(feature = "otel")]` 조건부 컴파일
    - lib.rs에 `#[cfg(feature = "otel")] pub mod telemetry;` 추가
    - _Requirements: 5.1, 5.2, 5.3, 5.4, 5.5, 5.6_

- [ ] 13. Session 통합 - 모든 컴포넌트 연결
  - [ ] 13.1 `src/session.rs` 수정 - 메트릭 및 컴포넌트 주입
    - `Session`에 `Arc<TransferMetrics>` 주입
    - 전송 시작 시 `ProgressUI::run()` tokio task 스폰 (show_progress가 true일 때)
    - 전송 완료 후 `StatsReporter::report()` 호출 (stats가 true일 때)
    - QUIC 연결 수립 시 `FingerprintVerifier` 사용 (fingerprint가 Some일 때)
    - 데이터 전송 시 `encode_chunk`/`decode_chunk` 적용 (no_integrity가 false일 때)
    - OpenTelemetry span 생성/종료 (otel_endpoint가 Some일 때)
    - _Requirements: 2.1, 2.3, 2.6, 4.1, 5.1, 6.3, 7.1, 7.5_

  - [ ] 13.2 멀티스트림 전송 경로 통합
    - 디렉토리 전송 시 `MultiStreamManager::transfer_files()` 호출
    - 단일 파일 전송 시 기존 단일 스트림 경로 유지
    - 전송 완료 후 `MultiStreamReport` 결과 출력
    - _Requirements: 3.1, 3.5, 3.6, 3.7_

- [ ] 14. macOS 크로스 컴파일 지원
  - [ ] 14.1 빌드 설정 및 플랫폼 조건부 코드
    - Makefile 또는 빌드 스크립트에 `aarch64-apple-darwin`, `x86_64-apple-darwin` 타겟 추가
    - 플랫폼 특화 API 호출에 `#[cfg(target_os)]` 조건부 컴파일 적용
    - macOS에서 플랫폼 호환성 오류 시 적절한 오류 메시지 출력 + 종료 코드 1
    - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5_

- [ ] 15. Final Checkpoint - 전체 통합 검증
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional and can be skipped for faster MVP
- 각 태스크는 특정 requirements를 참조하여 추적 가능
- Property-based 테스트는 design.md의 Correctness Properties 1-10을 모두 커버
- OpenTelemetry는 `otel` feature flag로 조건부 컴파일되므로, 기본 빌드에는 영향 없음
- 기존 Phase 1의 80개 테스트가 모든 단계에서 계속 통과해야 함
