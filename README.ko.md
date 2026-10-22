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

시작할 때 전송 방향, 경로, 고른 전송 경로를 출력한다. 터미널에서는 진행 줄 하나를 갱신하다가 끝나면 요약 한 줄로 바꾼다.

```text
quicsync 0.6.1 · push ~/work/app → user@host:/dst
경로: QUIC (RTT 12.3ms)
⠹ QUIC 54% · 829.1 MB · 129.5 MB/s · 8s · 파일 3,319/4,344+
✓ 완료 · 17.9s · 파일 45,503개 · 2.3 GB (전송 1.1 GB, 압축 2.2배) · 평균 59.9 MB/s · QUIC
```

진행 줄은 rsync의 `--info=progress2`를 읽으므로 로컬 rsync가 3.1 이상이어야 한다. 전송량은 압축 전 파일 크기 기준이고, 파일 수 뒤의 `+`는 rsync가 아직 파일 목록을 만드는 중이라는 뜻이다.

출력이 터미널이 아니거나 `--no-progress`를 주면 예전처럼 rsync의 `--stats` 요약 전체가 나온다. `-v`를 줘도 전체 요약이 나오고, `-P`, `--progress`, `--info=...`를 주면 rsync 자체 진행 출력을 그대로 보인다. quicsync 자체 전송 통계는 `--stats`로 본다. `NO_COLOR`를 설정하면 색을 쓰지 않는다.

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
| `--transport auto\|quic` | `auto`는 RTT가 10ms 미만이면 rsync over SSH로 전송, `quic`은 항상 QUIC 터널 사용 | auto (`QUICSYNC_TRANSPORT`) |
| `--install-remote` | 원격 `quicsync`가 없으면 설치 후 1회 재시도 | false |

나머지 인수는 rsync 옵션이나 경로로 처리된다. 아직 실제 전송 경로에 연결되지 않은 실험 옵션은 성공한 척 무시하지 않고 그대로 거부한다.

### 전송 경로 선택

RTT가 낮으면 QUIC 터널보다 순정 rsync over SSH가 빠를 수 있다. 기본값인 `--transport=auto`에서는 전송 전에 RTT를 확인한다.

1. 원격 호스트의 22번 포트로 TCP 연결을 한 번 맺는다.
2. 10ms 안에 연결되면 순정 rsync over SSH로 전송한다.
3. 그렇지 않으면 QUIC 터널로 전송한다.

rsync over SSH로 바꿀 때는 stderr에 다음 알림을 출력한다.

```text
quicsync: RTT가 0.4ms로 10ms보다 낮아 rsync over SSH로 전송합니다.
          손실이 있는 회선이면 QUIC이 더 빠를 수 있습니다: --transport=quic
```

다음 경우에는 항상 QUIC 터널을 쓴다.

- `--transport=quic`을 준 경우
- `--web`이나 `--stats`를 준 경우. 두 기능은 QUIC 터널에서만 동작한다.
- 22번 포트가 10ms 안에 응답하지 않는 경우. 예를 들어 방화벽이 연결을 버리면 이렇게 된다.

RTT 확인으로는 패킷 손실을 알 수 없다. 손실이 있는 회선에서는 RTT가 낮아도 rsync over SSH가 크게 느려질 수 있으므로, 그런 회선에서는 `--transport=quic`을 준다. 측정값은 [낮은 RTT 회선](#낮은-rtt-회선)에 있다.

RTT 확인은 항상 22번 포트를 쓰며, SSH 설정의 포트는 읽지 않는다. rsync over SSH로 바꾼 경우 원격 sshd 로그에 RTT 확인용 연결이 종료된 기록이 한 줄 남을 수 있다.

### SSH 연결 재사용

quicsync는 전송할 때마다 SSH 연결을 새로 맺으며, RTT가 높으면 이 단계에 왕복이 많이 든다. quicsync는 `ssh`를 별도 옵션 없이 실행하므로 `~/.ssh/config` 설정을 그대로 따른다. 같은 호스트로 반복해서 보낸다면 ControlMaster 설정으로 SSH 연결 하나를 재사용할 수 있다.

```sshconfig
Host example.com
    ControlMaster auto
    ControlPath ~/.ssh/cm-%C
    ControlPersist 10m
```

첫 전송이 공유 연결을 열고, 이후 같은 호스트로 가는 전송은 `ControlPersist` 시간이 지날 때까지 그 연결을 쓴다. `--transport=auto`가 고른 rsync over SSH도 같은 연결을 쓴다. 전송이 끝나도 백그라운드 ssh 프로세스가 남으며, 바로 닫으려면 `ssh -O exit example.com`을 실행한다.

아래는 RTT 200ms에서 10KB 파일 3개를 pull로 1회씩 보낸 결과다.

| | 재사용 없음 | 재사용(두 번째 실행) |
|---|---|---|
| quicsync | 4.63초 | 2.35초 |
| rsync+ssh | 4.36초 | 2.08초 |

재사용을 켠 첫 실행은 공유 연결을 여느라 4.63초가 걸렸다.

`Host`에는 quicsync에 넘기는 호스트 이름과 맞는 패턴을 써야 한다. quicsync는 QUIC 연결을 맺을 때 SSH 별칭을 해석하지 않는다.

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

이 측정에서는 RTT 0ms일 때 두 도구가 비슷했다. 다른 환경에서 측정했을 때는 RTT가 약 12ms보다 낮으면 rsync+ssh가 빨랐다([낮은 RTT 회선](#낮은-rtt-회선) 참고). RTT가 올라갈수록 격차가 벌어진다. TCP는 `throughput ≈ window_size / RTT`에 묶이는 반면, QUIC(BBR) + 64MB 윈도우는 회선을 가득 채운다.

### 낮은 RTT 회선

VM이 bare-metal 호스트로 보내는(pull) 구성이다. 지연과 손실은 VM 안에서 tc netem으로 넣었다. 값은 2회 평균이며 단위는 초다.

| RTT | 1GB 파일 rsync+ssh | 1GB 파일 quicsync | 10KB × 1만 개 rsync+ssh | 10KB × 1만 개 quicsync |
|-----|------|------|------|------|
| 0.3ms | 3.7 | 8.3 | 1.6 | 1.8 |
| 1.4ms | 4.1 | 7.9 | 1.5 | 1.8 |
| 2.4ms | 4.2 | 7.9 | 1.6 | 1.8 |
| 5.5ms | 4.3 | 8.0 | 1.6 | 1.8 |
| 10.4ms | 6.8 | 8.1 | 1.7 | 2.0 |
| 20.5ms | 12.6 | 8.3 | 2.2 | 2.2 |

두 도구의 시간이 같아지는 RTT는 1GB 파일에서 약 12ms, 작은 파일에서 약 20ms다. 이 구성에서는 VM의 CPU 코어 하나가 QUIC 송신을 약 125MB/s로 제한한다.

패킷 손실이 있으면 결과가 달라진다. 아래는 RTT 5ms에서 1GB 파일을 1회 보낸 결과이며, 60초 제한을 두었다.

| 손실 | rsync+ssh | quicsync |
|------|-----------|----------|
| 0.5% | 14.3 MB/s (60초에서 중단) | 127.8 MB/s (8.0초) |
| 1% | 5.2 MB/s (60초에서 중단) | 128.8 MB/s (8.0초) |

### 점보 프레임 링크

Mac(M4 Max)과 Mac(M1 Pro)을 Thunderbolt 브리지로 연결해 측정했다. 링크 MTU는 9000, RTT는 0.7ms다. 값은 1GB 파일 하나를 보낸 시간(초)이며, 범위는 2~4회 측정값이고 rsync+ssh는 1회 측정했다.

| | pull | push |
|---|---|---|
| rsync+ssh | 2.28 | 1.95 |
| quicsync 0.6.0 (`--transport=quic`) | 14.2~15.4 | 14.4~21.7 |
| quicsync, MTU 상한 8952 (`--transport=quic`) | 3.23~3.30 | 3.11~3.15 |

macOS에는 UDP GSO가 없어 quicsync가 패킷마다 시스템 호출을 한다. quinn 기본 상한인 1452바이트 패킷으로는 송신 측 CPU가 먼저 한계에 닿는다. 그래서 MTU 탐색 상한을 8952바이트로 올리고 UDP 소켓 버퍼를 8MB로 늘렸다. 수신 버퍼가 커지면서, 버퍼가 넘쳐 생긴 손실 때문에 quinn이 MTU를 1200으로 낮추던 현상도 사라졌다.

양쪽 호스트 모두 이 변경이 들어간 quicsync를 써야 한다. 이 RTT에서는 `--transport=auto`가 rsync over SSH를 고르며, 이 링크에서는 그쪽이 여전히 빠르다.

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
| `QUICSYNC_TRANSPORT` | `auto` | `--transport`의 기본값(`auto` 또는 `quic`). `--transport` 옵션을 주면 그 값이 우선한다 |
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
