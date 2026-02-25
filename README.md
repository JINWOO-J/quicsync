# quicsync

rsync의 Delta-sync를 그대로 유지하면서, QUIC(UDP) 터널을 통해 장거리 네트워크 전송 성능을 개선하는 단일 Rust 바이너리.

## 왜 quicsync인가?

RTT 100ms 이상의 장거리 네트워크(LFN)에서 `rsync over SSH(TCP)`는 TCP 윈도우 크기 제한으로 이론 대역폭의 10~20%만 활용한다. quicsync는 rsync 프로세스를 수정하지 않고, TCP 트래픽을 로컬에서 가로채어 QUIC 터널로 중계하는 투명 프록시 방식으로 이 문제를 해결한다.

- rsync Delta-sync 알고리즘 100% 유지
- TCP 윈도우 크기 제한을 무상태 버퍼링으로 우회
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
    │  rsync ←TCP→ TCP_Proxy ←Buffer→ QUIC_Tunnel ←QUIC→ Remote_Server ←TCP→ rsync(server)
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
```

### 지원하는 경로 형식

| 형식 | 설명 |
|------|------|
| `user@host:path` | 사용자 지정 원격 경로 |
| `host:path` | 현재 사용자로 원격 접속 |
| `/absolute/path` | 로컬 절대 경로 |
| `./relative/path` | 로컬 상대 경로 |

## 벤치마크

RTT가 높은 네트워크에서 quicsync의 성능 이점을 측정한 결과다. `tc netem delay 50ms`로 RTT 100ms를 시뮬레이션했다.

### RTT 100ms 환경 (장거리 네트워크 시뮬레이션)

| 시나리오 | rsync+ssh | quicsync | 개선 |
|---|---|---|---|
| 256MB 단일 파일 | 24.5s (10.5 MB/s) | 14.6s (17.5 MB/s) | **1.68x 빠름** |
| 1000개 × 100KB | 10.2s (9.6 MB/s) | 6.5s (15.1 MB/s) | **1.57x 빠름** |
| 혼합 (96MB+소파일) | 9.1s (12.8 MB/s) | 6.1s (16.1 MB/s) | **1.49x 빠름** |
| 증분 (초기 전송) | 8.7s (11.3 MB/s) | 6.0s (16.5 MB/s) | **1.46x 빠름** |

### LAN 환경 (RTT < 1ms)

| 시나리오 | rsync+ssh | quicsync | 비고 |
|---|---|---|---|
| 256MB 단일 파일 | 4.4s (57.8 MB/s) | 5.3s (48.8 MB/s) | rsync+ssh가 빠름 |
| 1000개 × 100KB | 1.9s (51.2 MB/s) | 2.2s (45.2 MB/s) | rsync+ssh가 빠름 |

LAN에서는 TCP 윈도우 크기 제한이 병목이 아니므로 rsync+ssh가 더 빠르다. QUIC의 userspace 처리 + TLS 오버헤드가 추가되기 때문이다. RTT가 높아질수록 TCP throughput은 `window_size / RTT`로 급격히 떨어지지만, QUIC(BBR)은 완만하게 유지된다.

### 벤치마크 실행

```bash
brew install gnu-time  # gtime 필요
./bench/run.sh user@host:/remote/path 3

# RTT 시뮬레이션 (원격 서버가 Linux인 경우)
ssh user@host 'tc qdisc add dev eth0 root netem delay 50ms'
./bench/run.sh user@host:/remote/path 3
ssh user@host 'tc qdisc del dev eth0 root'
```

## 환경변수

| 변수 | 기본값 | 설명 |
|------|--------|------|
| `QUICSYNC_BUFFER_SIZE` | `268435456` (256MB) | Ring Buffer 크기 (바이트) |
| `QUICSYNC_LOG` | `warn` | 로그 레벨 (`trace`, `debug`, `info`, `warn`, `error`) |

```bash
# 버퍼 크기를 512MB로 설정
QUICSYNC_BUFFER_SIZE=536870912 quicsync /src user@host:/dst

# 디버그 로그 활성화
RUST_LOG=debug quicsync /src user@host:/dst
```

## 빌드 및 테스트

```bash
# 빌드
cargo build

# 전체 테스트 실행 (75개 테스트, property-based 테스트 포함)
cargo test

# 릴리스 빌드
cargo build --release
```

테스트는 `proptest` 기반 property-based testing을 포함하며, CLI 파싱, Ring Buffer, 핸드셰이크 프로토콜, 데이터 무결성, 인증 토큰, rsync 명령어 구성, 종료 코드 전파 등 10개 correctness property를 검증한다.

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
└── types.rs       # 핵심 데이터 모델
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
- Linux x86_64/ARM64 대상 (macOS 지원은 Phase 2)
- UDP 차단 환경에서의 TCP 폴백 미지원
- rsync daemon mode (`rsync://`) 미지원

## 라이선스

MIT
