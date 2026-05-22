# quicsync

[English](README.md) | **한국어**

rsync의 delta-sync를 그대로 쓰면서 전송만 QUIC(UDP) 터널로 흘려보내는 단일 Rust 바이너리. 장거리 네트워크에서 빠르게 동기화한다.

## 왜 quicsync인가?

RTT 100ms가 넘는 장거리 네트워크(LFN)에서 `rsync over SSH`는 회선을 거의 놀린다. TCP 윈도우 한계 탓에 대역폭의 10~20%밖에 못 쓴다. quicsync는 rsync를 건드리지 않고 이 문제를 푼다. rsync의 TCP 트래픽을 로컬에서 가로채 QUIC 터널로 중계한다.

- rsync delta-sync 알고리즘을 그대로 사용
- TCP↔QUIC bounded relay와 QUIC flow control로 장거리 병목 완화
- QUIC BBR 혼잡 제어로 회선 대역폭을 최대한 활용
- 인증은 기존 SSH 인프라를 그대로 사용

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

바이너리 하나가 양쪽을 다 맡는다. 로컬에서는 CLI 모드, 원격에서는 `--server` 모드로 동작한다. 데몬도, 포트 포워딩도 필요 없다.

## 설치

### 소스에서 빌드

```bash
# Rust 1.85+ 필요 (edition 2024)
git clone https://github.com/JINWOO-J/quicsync.git
cd quicsync
cargo build --release

# 바이너리를 PATH에 복사
cp target/release/quicsync /usr/local/bin/
```

양쪽 호스트 모두에 `quicsync` 바이너리가 필요하다. 원격에 없으면 quicsync가 맞는 바이너리를 대신 배포한다([원격 설치](#원격-설치) 참고).

### 자체 업데이트

```bash
quicsync update --check
quicsync update
```

`update`는 GitHub Releases에서 현재 플랫폼에 맞는 자산을 받아 SHA-256을 확인하고, 실행 중인 바이너리를 제자리에서 교체한다. 자세한 내용은 [자체 업데이트](#자체-업데이트) 참고.

## 사용법

rsync와 같은 형식이다.

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

# 전송 중 실시간 웹 모니터
quicsync --web /src user@host:/dst

# 사전 진단
quicsync doctor user@host

# 원격에 quicsync가 없으면 설치 (OS/arch 자동 선택)
quicsync install-remote user@host

# 전송 중 원격 quicsync 누락 시 설치 후 1회 재시도
quicsync --install-remote /src user@host:/dst

# QUIC 초기화 실패 시 명시적으로 rsync-over-SSH fallback
quicsync --fallback=rsync /src user@host:/dst
```

quicsync가 rsync에 `--stats`를 대신 붙여 주므로, 전송이 끝나면 rsync 요약이 출력된다. quicsync 자체 통계도 보고 싶으면 `--stats`를 직접 붙인다. 시작할 때 방향과 경로를, 끝날 때 경과 시간을 출력한다.

### 지원하는 경로 형식

| 형식 | 설명 |
|------|------|
| `user@host:path` | 사용자 지정 원격 경로 |
| `host:path` | 현재 사용자로 원격 접속 |
| `/absolute/path` | 로컬 절대 경로 |
| `./relative/path` | 로컬 상대 경로 |

Push 모드는 여러 소스 경로를 받는다(glob 포함). Pull 모드는 원격 소스 하나만 받는다.

### quicsync 전용 옵션

| 옵션 | 설명 | 기본값 |
|------|------|--------|
| `--window MB` | QUIC flow control 윈도우 크기 (MB) | 64 |
| `--web` | 전송 중 localhost에 실시간 모니터링 대시보드 제공 | false |
| `--no-progress` | TTY 진행률 표시 비활성화 | false |
| `--stats` | quicsync 자체 전송 통계 출력 | false |
| `--stats-format text\|json` | quicsync 통계 출력 형식 | text |
| `--fallback none\|rsync` | QUIC 세션 초기화 실패 시 rsync-over-SSH로 재시도 | none |
| `--install-remote` | 원격 `quicsync`가 없으면 설치 후 1회 재시도 | false |

나머지 인수는 rsync 옵션이나 경로로 처리된다. 아직 실제 전송 경로에 연결되지 않은 실험 옵션은 성공한 척 무시하지 않고 그대로 거부한다.

### 웹 모니터

`--web`을 붙이면 전송을 브라우저에서 지켜볼 수 있다. 전송이 도는 동안 quicsync가 localhost 전용 HTTP 서버를 띄우며, 외부 의존성은 없다. `127.0.0.1`의 임의 포트에 바인딩하고, URL을 stderr에 출력한 뒤 브라우저를 대신 열어 준다.

```bash
quicsync --web /src user@host:/dst
# quicsync: web monitor → http://127.0.0.1:51550
```

대시보드는 500ms마다 `/api/metrics`를 polling해 다음을 보여준다.

- 처리량, 전송량, 경과 시간, 전송 모드(QUIC/TCP)
- 파일 진행: 현재 파일, 완료/전체 파일 수, 진행률 바

**push** 모드에서는 로컬 소스를 walk해 전체 파일 수를 구하고 이 값으로 퍼센트 바를 그린다. **pull** 모드에서는 완료 수만 표시한다. 전송 없이 대시보드만 보려면:

```bash
cargo run --example web_dashboard
```

### 진단

`doctor`는 전송 전에 로컬·원격 의존성과 QUIC 터널 수립 가능 여부를 확인한다.

```bash
quicsync doctor user@host
quicsync doctor --json user@host
```

확인 항목은 로컬 `rsync`, 로컬 `quicsync`, SSH 접속, 원격 `quicsync`, 원격 `rsync`, QUIC handshake다. 실패한 항목에는 해당되는 경우 원인별 `hint`가 붙고, `--json` 출력에도 같은 hint가 담긴다.

### 원격 설치

원격에 `quicsync`가 없으면 quicsync가 직접 배포한다. `uname`으로 원격 OS와 architecture를 읽어 맞는 바이너리를 고른다.

- **로컬과 같은 플랫폼** → 현재 바이너리를 SSH로 그대로 흘려보낸다(다운로드 없음)
- **다른 플랫폼** → 맞는 release 자산(`quicsync_<os>_<arch>.tar.gz`)을 받아 SHA-256을 확인하고 SSH로 설치한다

```bash
quicsync install-remote user@host
quicsync install-remote --dir /usr/local/bin user@host
quicsync --install-remote /src user@host:/dst
```

설치 경로는 기본값이 `$HOME/.local/bin/quicsync`다. `--install-remote`는 전송 시작 시 원격 `quicsync` 누락이 감지될 때만 설치하고 한 번 재시도한다. 크로스 아키텍처 설치는 로컬 바이너리와 **같은 버전**의 자산을 받으므로, 그 버전 release가 GitHub Releases에 있어야 한다.

### 자체 업데이트

```bash
quicsync update --check
quicsync update
quicsync update --to v0.4.0
```

`update`의 동작은 실행 중인 바이너리 위치에 따라 갈린다. Homebrew 경로면 `brew upgrade quicsync`로 넘기고, Cargo install 경로면 `cargo install --git https://github.com/jinwoo-j/quicsync quicsync --force` 명령을 안내한다. 그 외 manual install이면 GitHub Releases의 `quicsync_<os>_<arch>.tar.gz`와 `checksums.txt`를 받아 SHA-256을 확인한 뒤 현재 바이너리를 atomic하게 교체한다. `--check`는 새 버전이 있으면 exit 1을 반환한다.

## 벤치마크

Docker 컨테이너 사이에 pumba(tc netem)로 양방향 지연을 주입해 측정했다. 128MB 단일 파일, 3라운드 평균.

| RTT | rsync+ssh | quicsync | 배수 |
|-----|-----------|----------|------|
| 0ms | 0.96s (156 MB/s) | 0.99s (147 MB/s) | 0.97x |
| 50ms | 11.14s (12.4 MB/s) | 3.03s (43.0 MB/s) | **3.67x** |
| 100ms | 22.05s (6.1 MB/s) | 5.09s (25.2 MB/s) | **4.33x** |
| 200ms | 63.24s (2.2 MB/s) | 9.31s (13.7 MB/s) | **6.79x** |
| 500ms | 106.14s (1.3 MB/s) | 20.51s (6.2 MB/s) | **5.18x** |

LAN(RTT 0ms)에서는 QUIC userspace 오버헤드 때문에 rsync+ssh와 비슷하다. RTT가 올라갈수록 격차가 벌어진다. TCP는 `throughput ≈ window_size / RTT`에 묶이는 반면, QUIC(BBR) + 64MB 윈도우는 회선을 가득 채운다.

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
| `QUICSYNC_BUFFER_SIZE` | `268435456` (256MB) | 내부 buffer layer 할당 크기 (바이트). relay는 주로 bounded channel backpressure에 기대므로, 성능 튜닝에는 `--window`를 먼저 쓴다 |
| `QUICSYNC_WINDOW` | `64` | QUIC 윈도우 크기 (MB). 높은 RTT에서 올리면 처리량이 늘어난다 |
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

테스트에는 `proptest` 기반 property test가 들어 있다. CLI 파싱, Ring Buffer, 핸드셰이크 프로토콜, 데이터 무결성, 인증 토큰, rsync 명령어 구성, 종료 코드 전파를 검증한다. E2E smoke harness는 `ssh localhost` 무암호 접속과 원격 PATH의 `quicsync`가 갖춰진 환경에서만 돌고, 조건이 안 맞으면 exit 77로 skip한다.

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
├── web.rs         # 실시간 모니터링 웹 서버 (--web)
├── remote_install.rs # 원격 설치 (동일 arch 복사 / 크로스 arch 다운로드)
├── update.rs      # 자체 업데이트 + 공용 release 다운로더
├── integrity.rs   # Blake3 무결성 유틸리티 (전송 경로 미연결)
├── multi_stream.rs # 멀티스트림 실험 인프라 (전송 경로 미연결)
└── telemetry.rs   # OpenTelemetry 실험 인프라 (feature-gated)
examples/
└── web_dashboard.rs # 더미 메트릭으로 --web 대시보드 미리보기
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

## 제한사항

- 양쪽 호스트 모두에 `quicsync` 필요. 원격에 없으면 `install-remote` / `--install-remote`로 배포할 수 있고, release 자산을 통해 OS/architecture가 달라도 배포된다.
- Linux(x86_64/aarch64)와 macOS(x86_64/aarch64) 대상.
- UDP가 막혔거나 QUIC 초기화가 실패하는 환경에서는 `--fallback=rsync`로 TCP/SSH rsync로 재시도한다.
- rsync daemon mode (`rsync://`) 미지원.
- 멀티스트림 파일 전송, OpenTelemetry export, per-chunk Blake3 전송 검증은 아직 실제 전송 경로에 연결되지 않았다.

## 라이선스

MIT
