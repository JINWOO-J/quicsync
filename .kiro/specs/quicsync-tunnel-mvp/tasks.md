# Implementation Plan: quicsync Tunnel MVP

## Overview

Rust + tokio + quinn 기반으로 rsync-over-QUIC 터널 MVP를 구현한다. 핵심 데이터 구조(Ring_Buffer, AuthToken)부터 시작하여 각 컴포넌트를 점진적으로 구현하고, 마지막에 Session Orchestrator로 전체를 연결한다.

## Tasks

- [x] 1. 프로젝트 초기화 및 핵심 타입 정의
  - [x] 1.1 Cargo 프로젝트 생성 및 의존성 설정
    - `cargo init` 실행, `Cargo.toml`에 quinn, tokio, proptest, clap, rcgen, ring 등 의존성 추가
    - `src/main.rs`, `src/lib.rs` 기본 구조 생성
    - _Requirements: N/A (프로젝트 기반)_

  - [x] 1.2 오류 타입 및 핵심 데이터 모델 정의
    - `src/error.rs`: `QuicsyncError`, `CliError`, `SshError`, `BufferError`, `QuicError`, `ServerError`, `RsyncError`, `SessionError` enum 정의
    - `src/types.rs`: `RemoteSpec`, `TransferDirection`, `CliArgs`, `AuthToken`, `SshHandshake` 구조체 정의
    - _Requirements: 1.1, 2.2, 6.2_

  - [x] 1.3 AuthToken property 테스트 작성
    - **Property 8: 인증 토큰 검증**
    - proptest로 임의 32바이트 배열 생성, `to_hex` → `from_hex` 라운드트립 검증, 동일/상이 토큰 `verify` 검증
    - **Validates: Requirements 6.2, 6.3**

- [x] 2. CLI 인수 파싱 구현
  - [x] 2.1 `parse_remote` 및 `parse_args` 함수 구현
    - `src/cli.rs`: clap 기반 인수 파싱, `user@host:path` 원격 경로 파싱, SRC/DST 방향 판별
    - `--help`, `--version` 플래그 처리
    - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6_

  - [x] 2.2 CLI 파싱 property 테스트 작성
    - **Property 1: CLI 파싱 정확성 — 유효 입력 보존**
    - proptest로 임의 user/host/path/옵션 생성, 파싱 후 원본 보존 검증
    - **Validates: Requirements 1.1, 1.2**

  - [x] 2.3 CLI 오류 거부 property 테스트 작성
    - **Property 2: CLI 파싱 거부 — 무효 입력 오류**
    - proptest로 양쪽 로컬/원격 조합, 잘못된 형식 문자열 생성, 오류 반환 검증
    - **Validates: Requirements 1.3, 1.4**

- [x] 3. Ring_Buffer 및 Buffer_Layer 구현
  - [x] 3.1 Ring_Buffer 구현
    - `src/buffer.rs`: `RingBuffer` 구조체, `new`, `write`, `read`, `len`, `is_empty`, `is_full`, `available` 메서드 구현
    - 환경변수 `QUICSYNC_BUFFER_SIZE` 파싱 로직 (`BufferLayer::from_env`)
    - _Requirements: 4.1, 4.2, 4.3, 4.5, 4.6_

  - [x] 3.2 Ring_Buffer 라운드트립 property 테스트 작성
    - **Property 6: Ring_Buffer write/read 라운드트립**
    - proptest로 임의 바이트 데이터 생성, write → read 후 데이터 동일성 및 len 변화 검증
    - **Validates: Requirements 4.3**

  - [x] 3.3 Backpressure property 테스트 작성
    - **Property 7: Backpressure 적용 및 해제**
    - proptest로 임의 capacity 생성, 가득 채운 후 BufferFull 오류 검증, 일부 read 후 write 재개 검증
    - **Validates: Requirements 4.5, 4.6**

  - [x] 3.4 환경변수 버퍼 크기 property 테스트 작성
    - **Property 5: 환경변수 버퍼 크기 설정**
    - proptest로 임의 양의 정수 생성, 환경변수 설정 후 `from_env` 결과 검증
    - **Validates: Requirements 4.2**

- [x] 4. Checkpoint — 핵심 데이터 구조 검증
  - 모든 테스트 통과 확인, 질문이 있으면 사용자에게 문의

- [x] 5. QUIC 터널 구현
  - [x] 5.1 QUIC 클라이언트/서버 Endpoint 구성
    - `src/quic.rs`: `build_client_endpoint`, `build_server_endpoint` 함수 구현
    - BBR 혼잡 제어 설정, TLS 1.3 자체 서명 인증서 생성 (`generate_self_signed_cert`)
    - `QuicTunnel::connect`, `open_bi_stream`, `close` 구현
    - _Requirements: 5.1, 5.2, 5.3, 5.4_

  - [x] 5.2 핸드셰이크 프로토콜 구현
    - `src/ssh.rs`: `parse_handshake` 함수 — `QUICSYNC_READY <port> <token>` 파싱
    - `src/server.rs`: `RemoteServer::emit_handshake` — stdout으로 핸드셰이크 출력
    - _Requirements: 2.2_

  - [x] 5.3 핸드셰이크 라운드트립 property 테스트 작성
    - **Property 3: 핸드셰이크 프로토콜 라운드트립**
    - proptest로 임의 u16 포트 + 32바이트 토큰 생성, emit → parse 라운드트립 검증
    - **Validates: Requirements 2.2**

- [x] 6. SSH_Launcher 및 Remote_Server 구현
  - [x] 6.1 SSH_Launcher 구현
    - `src/ssh.rs`: `launch_remote_server` 함수 — SSH 명령어 구성, 자식 프로세스 실행, stdout에서 핸드셰이크 파싱
    - SSH 접속 실패 및 바이너리 미설치 오류 처리
    - _Requirements: 2.1, 2.3, 2.4, 2.5_

  - [x] 6.2 Remote_Server 구현
    - `src/server.rs`: `RemoteServer::start`, `accept_and_serve`, `verify_token` 구현
    - QUIC 리스닝, 토큰 검증, 원격 rsync 서버 프로세스 실행 및 양방향 데이터 중계
    - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5, 6.6_

- [x] 7. TCP_Proxy 및 Rsync_Child 구현
  - [x] 7.1 TCP_Proxy 구현
    - `src/tcp_proxy.rs`: `TcpProxy::bind`, `port`, `relay` 구현
    - 임시 TCP 포트 바인딩, rsync 연결 수락, 양방향 바이트 스트림 중계
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5_

  - [x] 7.2 Buffer_Layer 비동기 relay 구현
    - `src/buffer.rs`: `BufferLayer::relay_forward`, `relay_reverse` 구현
    - tokio 채널 기반 비동기 파이프라인, backpressure 적용
    - _Requirements: 4.3, 4.4, 4.5, 4.6, 4.7_

  - [x] 7.3 양방향 데이터 무결성 property 테스트 작성
    - **Property 4: 양방향 데이터 무결성**
    - proptest + tokio::test로 임의 바이트 시퀀스 생성, relay 파이프라인 통과 후 데이터 동일성 검증
    - **Validates: Requirements 3.3, 3.4**

  - [x] 7.4 Rsync_Child 구현
    - `src/rsync.rs`: `RsyncChild::spawn`, `wait` 구현
    - rsync `-e` 옵션을 활용한 프록시 포트 리다이렉션, 종료 코드 전파
    - _Requirements: 7.1, 7.2, 7.3, 7.4_

  - [x] 7.5 rsync 명령어 구성 property 테스트 작성
    - **Property 9: rsync 명령어 구성 정확성**
    - proptest로 임의 경로/포트/옵션/방향 생성, 구성된 명령어 인수 검증
    - **Validates: Requirements 7.1, 7.2**

  - [x] 7.6 종료 코드 전파 property 테스트 작성
    - **Property 10: 종료 코드 전파**
    - proptest로 임의 u8 종료 코드 생성, 전파 로직 검증
    - **Validates: Requirements 7.3, 7.4**

- [x] 8. Checkpoint — 개별 컴포넌트 검증
  - 모든 테스트 통과 확인, 질문이 있으면 사용자에게 문의

- [x] 9. Session Orchestrator 및 전체 연결
  - [x] 9.1 Session Orchestrator 구현
    - `src/session.rs`: `Session::start`, `run`, `shutdown`, `abort` 구현
    - 모든 컴포넌트를 순서대로 초기화하고 연결
    - SIGINT/SIGTERM 시그널 핸들러 등록 (`install_signal_handlers`)
    - _Requirements: 8.1, 8.2, 8.3, 8.4_

  - [x] 9.2 main.rs 진입점 연결
    - `src/main.rs`: CLI 파싱 → Session 시작 → 종료 코드 반환
    - `--server` 모드 분기 (Remote_Server 실행)
    - _Requirements: 1.1, 8.1_

- [x] 10. Final checkpoint — 전체 빌드 및 테스트
  - `cargo build`, `cargo test` 통과 확인
  - 질문이 있으면 사용자에게 문의

## Notes

- `*` 표시된 sub-task는 선택적이며 빠른 MVP를 위해 건너뛸 수 있다
- 각 task는 이전 task의 결과물에 의존하므로 순서대로 진행한다
- Property 테스트는 `proptest` 크레이트를 사용하며 최소 100회 반복으로 설정한다
- 통합 테스트(loopback 환경 end-to-end)는 모든 컴포넌트 구현 후 별도로 진행한다
