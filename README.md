# quicsync

rsync의 Delta-sync를 그대로 유지하면서, QUIC(UDP) 터널을 통해 장거리 네트워크 전송 성능을 개선하는 단일 Rust 바이너리.

## 왜 quicsync인가?

RTT 100ms 이상의 장거리 네트워크(LFN)에서 `rsync over SSH(TCP)`는 TCP 윈도우 크기 제한으로 이론 대역폭의 10~20%만 활용한다. quicsync는 rsync 프로세스를 수정하지 않고, TCP 트래픽을 로컬에서 가로채어 QUIC 터널로 중계하는 투명 프록시 방식으로 이 문제를 해결한다.

- rsync Delta-sync 알고리즘 100% 유지
- TCP/QUIC 사이 bounded relay와 QUIC flow control로 장거리 전송 병목 완화
- QUIC BBR 혼잡 제어로 LFN 대역폭 최대 활용
- 기존 SSH 인프라를 인증 채널로 그대로 활용

## 동작 원리

```
quicsync user@remote:/path /local/path
    │
    ├─ 1. SSH로 원격 quicsync server 실행 (포트+토큰 수신)
    ├─ 2. QUIC 터널 수립 (quinn, TLS 1.3, BBR)
    ├─ 3. 로컬 TCP 프록시 포트 바인딩
    ├─ 4. rsync 자식 프로세스 실행 (목적지 → 로컬 프록시)
    │
    │  rsync ←TCP→ TCP_Proxy ←Relay→ QUIC_Tunnel ←QUIC→ Remote_Server ←TCP→ rsync(server)
    │
    └─ 5. 전송 완료 → 리소스 정리
```

동일한 `quicsync` 바이너리가 로컬에서는 CLI 모드로, 원격에서는 `--server` 모드로 동작한다. 별도 데몬 설치나 포트 포워딩이 필요 없다.

## 설치

### 소스에서 빌드

```bash
# Rust 1.85+ 필요 (edition 2024)
git clone https://github.com/your-org/quicsync.git
cd quicsync
cargo build --release

# 바이너리를 PATH에 복사
cp target/release/quicsync /usr/local/bin/
```

양측 호스트 모두에 `quicsync` 바이너리가 설치되어 있어야 한다.

## 사용법

rsync와 동일한 형식으로 사용한다:

```bash
# Push: 로컬 → 원격
quicsync /local/dir user@remote:/remote/dir

# Pull: 원격 → 로컬
quicsync user@remote:/remote/dir /local/dir

# rsync 옵션 전달
quicsync -avz --delete --exclude='*.tmp' /src user@server:/dst

# 멀티소스 경로 (glob 패턴)
quicsync ./* user@host:/dst

# QUIC 윈도우 크기 설정 (기본 64MB, 높은 RTT에서 증가 권장)
quicsync --window 128 /src user@host:/dst

# 사전 진단
quicsync doctor user@host

# 원격 quicsync가 없을 때 현재 로컬 바이너리 설치
quicsync install-remote user@host

# 전송 중 원격 quicsync 누락 시 설치 후 1회 재시도
quicsync --install-remote /src user@host:/dst

# QUIC 초기화 실패 시 명시적으로 rsync-over-SSH fallback
quicsync --fallback=rsync /src user@host:/dst

# 자체 업데이트 확인/실행
quicsync update --check
quicsync update
```

rsync에는 `--stats`가 자동으로 추가되어 전송 완료 시 rsync 통계 요약이 표시된다. quicsync 자체 전송 통계가 필요하면 `--stats`를 명시한다. 전송 시작 시 방향과 경로를 표시하고, 완료 시 경과 시간을 출력한다.

### 지원하는 경로 형식

| 형식 | 설명 |
|------|------|
| `user@host:path` | 사용자 지정 원격 경로 |
| `host:path` | 현재 사용자로 원격 접속 |
| `/absolute/path` | 로컬 절대 경로 |
| `./relative/path` | 로컬 상대 경로 |

Push 모드에서는 여러 소스 경로를 지정할 수 있다 (glob 패턴 포함). Pull 모드에서는 원격 소스 1개만 지원한다.

### quicsync 전용 옵션

| 옵션 | 설명 | 기본값 |
|------|------|--------|
| `--window MB` | QUIC flow control 윈도우 크기 (MB) | 64 |
| `--no-progress` | TTY 진행률 표시 비활성화 | false |
| `--stats` | quicsync 자체 전송 통계 출력 | false |
| `--stats-format text\|json` | quicsync 통계 출력 형식 | text |
| `--fallback none\|rsync` | QUIC 세션 초기화 실패 시 rsync-over-SSH로 재시도 | none |
| `--install-remote` | 원격 `quicsync`가 없으면 현재 로컬 바이너리를 `$HOME/.local/bin/quicsync`로 설치 후 1회 재시도 | false |

위 옵션을 제외한 인수는 rsync 옵션 또는 경로로 처리된다. 아직 실제 전송 경로에 연결되지 않은 실험 옵션은 성공처럼 무시하지 않고 명시적으로 거부한다.

### 진단

`doctor`는 전송 전에 로컬/원격 의존성과 QUIC 터널 수립 가능성을 확인한다.

```bash
quicsync doctor user@host
quicsync doctor --json user@host
```

확인 항목은 로컬 `rsync`, 로컬 `quicsync`, SSH 접속, 원격 `quicsync`, 원격 `rsync`, QUIC tunnel handshake다.
실패한 항목은 가능한 경우 원인별 `hint`를 함께 출력하며, `--json` 출력에도 같은 hint가 포함된다.

### 원격 설치

원격 호스트에 `quicsync`가 없으면 현재 실행 중인 로컬 바이너리를 SSH stdin으로 전송해 설치할 수 있다.

```bash
quicsync install-remote user@host
quicsync install-remote --dir /usr/local/bin user@host
quicsync --install-remote /src user@host:/dst
```

기본 설치 경로는 `$HOME/.local/bin/quicsync`다. `--install-remote`는 전송 시작 시 원격 `quicsync` 누락이 감지될 때만 설치하고 한 번 재시도한다. 이 방식은 로컬/원격 OS와 CPU architecture가 같은 환경을 전제로 한다. 다른 architecture의 원격 호스트에는 release asset 또는 패키지 관리자를 통해 맞는 바이너리를 설치해야 한다.

### 자체 업데이트

`update`는 `git-kit`의 self-update 흐름을 quicsync에 맞게 축소해 수용했다.

```bash
quicsync update --check
quicsync update
quicsync update --to v0.2.0
```

동작은 현재 실행 중인 바이너리 위치에 따라 분기한다. Homebrew 경로면 `brew upgrade quicsync`에 위임하고, Cargo install 경로면 `cargo install --git https://github.com/jinwoo-j/quicsync quicsync --force` 명령을 안내한다. 그 외 manual install은 GitHub Releases의 `quicsync_<os>_<arch>.tar.gz`와 `checksums.txt`를 받아 sha256 검증 후 현재 바이너리를 atomic replace한다. `--check`는 새 버전이 있으면 exit 1을 반환한다.

## 벤치마크

Docker 컨테이너 간 pumba(tc netem)로 양방향 지연을 주입하여 측정한 결과다. 128MB 단일 파일, 3라운드 평균.

### RTT별 성능 비교

| RTT | rsync+ssh | quicsync | 배수 |
|-----|-----------|----------|------|
| 0ms | 0.96s (156 MB/s) | 0.99s (147 MB/s) | 0.97x |
| 50ms | 11.14s (12.4 MB/s) | 3.03s (43.0 MB/s) | **3.67x** |
| 100ms | 22.05s (6.1 MB/s) | 5.09s (25.2 MB/s) | **4.33x** |
| 200ms | 63.24s (2.2 MB/s) | 9.31s (13.7 MB/s) | **6.79x** |
| 500ms | 106.14s (1.3 MB/s) | 20.51s (6.2 MB/s) | **5.18x** |

LAN(RTT 0ms)에서는 QUIC userspace 오버헤드로 rsync+ssh와 동등하다. RTT가 높아질수록 TCP의 `throughput ≈ window_size / RTT` 제한이 심해지는 반면, QUIC(BBR) + 64MB 윈도우는 대역폭을 훨씬 효율적으로 활용한다.

### 벤치마크 실행

```bash
# Docker 기반 latency sweep (pumba 사용)
docker compose -f bench/docker/compose.yml up -d --build
bash bench/docker/setup_ssh.sh
bash bench/docker/run_latency_sweep.sh 3
docker compose -f bench/docker/compose.yml down

# 실서버 벤치마크 (gtime 필요)
brew install gnu-time
./bench/run.sh user@host:/remote/path 3
```

## 환경변수

| 변수 | 기본값 | 설명 |
|------|--------|------|
| `QUICSYNC_BUFFER_SIZE` | `268435456` (256MB) | 내부 buffer layer 할당 크기 (바이트). 현재 relay는 bounded channel backpressure를 주로 사용하므로 일반 성능 튜닝 용도로는 `--window`를 우선 사용 |
| `QUICSYNC_WINDOW` | `64` | QUIC 윈도우 크기 (MB). 높은 RTT 환경에서 증가시키면 처리량 향상 |
| `RUST_LOG` | unset | 로그 필터. 예: `RUST_LOG=debug`, `RUST_LOG=quicsync=trace` |

```bash
# QUIC 윈도우를 128MB로 설정 (RTT가 매우 높은 환경)
quicsync --window 128 /src user@host:/dst

# 또는 환경변수로 설정 (서버 측에도 적용됨)
QUICSYNC_WINDOW=128 quicsync /src user@host:/dst

# 버퍼 크기를 512MB로 설정
QUICSYNC_BUFFER_SIZE=536870912 quicsync /src user@host:/dst

# 디버그 로그 활성화
RUST_LOG=debug quicsync /src user@host:/dst
```

## 빌드 및 테스트

```bash
# 빌드
cargo build

# 전체 테스트 실행 (unit, integration, property-based 테스트 포함)
cargo test

# 릴리스 빌드
cargo build --release

# 로컬 E2E smoke 테스트
scripts/e2e-local.sh localhost
```

테스트는 `proptest` 기반 property-based testing을 포함하며 CLI 파싱, Ring Buffer, 핸드셰이크 프로토콜, 데이터 무결성, 인증 토큰, rsync 명령어 구성, 종료 코드 전파를 검증한다.
E2E smoke harness는 `ssh localhost` 무암호 접속과 원격 PATH의 `quicsync`가 준비된 환경에서만 실행되며, 조건이 맞지 않으면 exit 77로 skip한다.

### 안정화 변경 검증 기준

```bash
cargo test
git diff --check
rustfmt --check --edition 2024 src/cli.rs src/types.rs src/main.rs src/server.rs src/error.rs src/rsync.rs src/stats.rs src/doctor.rs src/lib.rs
```

전체 repository formatting은 기존 drift가 있으므로 별도 정리 작업으로 다룬다.

## 프로젝트 구조

```
src/
├── main.rs        # 진입점 (CLI / --server 모드 분기)
├── lib.rs         # 모듈 선언
├── cli.rs         # CLI 인수 파싱 (clap)
├── ssh.rs         # SSH 원격 서버 실행 및 핸드셰이크
├── quic.rs        # QUIC 터널 (quinn, BBR, TLS 1.3)
├── tcp_proxy.rs   # 로컬 TCP 프록시
├── buffer.rs      # Ring Buffer 및 비동기 relay
├── server.rs      # 원격 QUIC 서버
├── rsync.rs       # rsync 자식 프로세스 관리
├── session.rs     # 세션 오케스트레이터 및 시그널 핸들링
├── error.rs       # 오류 타입 계층
├── types.rs       # 핵심 데이터 모델
├── metrics.rs     # 전송 메트릭
├── progress.rs    # TTY 진행률 표시
├── stats.rs       # quicsync 통계 출력
├── integrity.rs   # Blake3 무결성 유틸리티 (전송 경로 미연결)
├── multi_stream.rs # 멀티스트림 실험 인프라 (전송 경로 미연결)
└── telemetry.rs   # OpenTelemetry 실험 인프라 (feature-gated)
```

## 주요 의존성

| 크레이트 | 용도 |
|----------|------|
| `quinn` | QUIC 프로토콜 구현 |
| `tokio` | 비동기 런타임 |
| `rustls` | TLS 1.3 |
| `clap` | CLI 인수 파싱 |
| `rcgen` | 자체 서명 인증서 생성 |
| `ring` | 암호화 프리미티브 |

## 제한사항 (Phase 1 MVP)

- 양측 호스트 모두에 `quicsync` 설치 필요
- 같은 OS/architecture 환경에서는 `quicsync install-remote` 또는 `--install-remote`로 원격 설치 가능
- Linux x86_64/ARM64 대상. macOS는 로컬/원격 모두 추가 검증 필요
- UDP 차단 또는 QUIC 초기화 실패 환경에서는 `--fallback=rsync`를 명시해야 TCP/SSH 기반 rsync로 재시도
- rsync daemon mode (`rsync://`) 미지원
- 다른 OS/architecture 원격 호스트에 맞는 바이너리 자동 선택 설치는 아직 미지원
- 멀티스트림 파일 전송, OpenTelemetry export, per-chunk Blake3 전송 검증은 아직 실제 전송 경로에 연결되지 않음

## 라이선스

MIT
