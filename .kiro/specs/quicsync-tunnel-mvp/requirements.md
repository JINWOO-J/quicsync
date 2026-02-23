# Requirements Document

## Introduction

quicsync Phase 1 (터널 검증 MVP)의 요구사항 문서이다. 양측 모두 quicsync가 설치된 환경에서, rsync의 Delta-sync를 완전히 유지하면서 QUIC(UDP) 기반 터널을 통해 장거리 네트워크(LFN) 전송 성능을 개선하는 것이 목표이다. Linux x86_64/ARM64 환경만 대상으로 하며, UDP 폴백, Zero-Config, macOS 지원은 Phase 2 이후로 제외한다.

## Glossary

- **CLI**: quicsync의 Command-Line Interface. 사용자가 `quicsync [rsync options] SRC DST` 형태로 실행하는 진입점
- **SSH_Launcher**: SSH를 통해 원격 호스트에 접속하여 Remote_Server 프로세스를 임시 실행하는 컴포넌트
- **TCP_Proxy**: 로컬 OS에 임시 TCP 리스닝 포트를 생성하여 rsync 트래픽을 수신하는 컴포넌트
- **Buffer_Layer**: TCP_Proxy와 QUIC_Tunnel 사이에서 Ring Buffer 기반 무상태 버퍼링을 수행하는 레이어. rsync TCP 패킷에 즉각 ACK를 응답하고, QUIC 전송 속도와 TCP 수신 속도를 논리적으로 단절한다
- **QUIC_Tunnel**: quinn 라이브러리 기반의 UDP 멀티스트림 전송 채널. TLS 1.3 암호화와 BBR 혼잡 제어를 적용한다
- **Remote_Server**: 원격 호스트에서 임시로 실행되는 quicsync 서버 프로세스. QUIC 스트림을 수신하여 TCP로 역변환한 뒤 원격 rsync 프로세스에 전달한다
- **Rsync_Child**: quicsync가 자식 프로세스로 실행하는 rsync 인스턴스. 목적지가 로컬 TCP_Proxy 포트로 강제 리다이렉션된다
- **Ring_Buffer**: Buffer_Layer 내부의 고정 크기 순환 메모리 큐 (기본 256MB)
- **Backpressure**: Ring_Buffer 오버플로우 시 TCP_Proxy의 수신을 일시 중단하여 rsync 전송 속도를 제한하는 메커니즘
- **LFN**: Long Fat Network. RTT가 높고 대역폭이 큰 네트워크 경로
- **BBR**: Bottleneck Bandwidth and Round-trip propagation time. Google이 개발한 혼잡 제어 알고리즘

## Requirements

### Requirement 1: CLI 인수 파싱 및 rsync 호환

**User Story:** 운영자로서, 기존 rsync 명령어와 동일한 형태로 quicsync를 실행하고 싶다. 그래야 기존 스크립트를 최소한으로 수정할 수 있다.

#### Acceptance Criteria

1. WHEN 사용자가 `quicsync [rsync options] SRC DST` 형태로 명령을 입력하면, THE CLI SHALL SRC와 DST를 파싱하여 로컬 경로와 원격 경로(`user@host:path`)를 식별한다
2. WHEN 사용자가 rsync 옵션 플래그(예: `-avz`, `--delete`, `--exclude`)를 전달하면, THE CLI SHALL 해당 옵션을 변경 없이 Rsync_Child에 그대로 전달한다
3. IF SRC와 DST 모두 원격 경로이거나 모두 로컬 경로이면, THEN THE CLI SHALL 명확한 오류 메시지를 출력하고 종료 코드 1로 종료한다
4. IF 원격 경로의 형식이 `user@host:path` 패턴에 맞지 않으면, THEN THE CLI SHALL 형식 오류를 설명하는 메시지를 출력하고 종료 코드 1로 종료한다
5. THE CLI SHALL `--help` 플래그에 대해 사용법 안내를 출력한다
6. THE CLI SHALL `--version` 플래그에 대해 현재 바이너리 버전을 출력한다

### Requirement 2: SSH를 통한 원격 서버 실행

**User Story:** 운영자로서, 원격 호스트에 별도 데몬 설정 없이 SSH를 통해 quicsync 서버가 자동으로 실행되길 원한다. 그래야 추가 인프라 설정이 불필요하다.

#### Acceptance Criteria

1. WHEN CLI가 원격 경로를 감지하면, THE SSH_Launcher SHALL SSH를 통해 원격 호스트에 접속하여 Remote_Server 프로세스를 실행한다
2. WHEN Remote_Server가 시작되면, THE Remote_Server SHALL QUIC 리스닝에 사용할 UDP 포트 번호와 인증 토큰을 SSH 표준 출력으로 로컬에 전달한다
3. WHEN SSH_Launcher가 원격으로부터 포트 번호와 인증 토큰을 수신하면, THE SSH_Launcher SHALL 해당 정보를 사용하여 QUIC_Tunnel 연결을 개시한다
4. IF SSH 접속이 실패하면, THEN THE SSH_Launcher SHALL SSH 오류 메시지를 포함한 진단 정보를 출력하고 종료 코드 1로 종료한다
5. IF 원격 호스트에 quicsync 바이너리가 존재하지 않으면, THEN THE SSH_Launcher SHALL 바이너리 미설치를 안내하는 오류 메시지를 출력하고 종료 코드 1로 종료한다

### Requirement 3: 로컬 TCP 프록시

**User Story:** 운영자로서, rsync가 로컬 TCP 포트를 통해 데이터를 전송하도록 하고 싶다. 그래야 rsync 프로세스를 수정 없이 그대로 활용할 수 있다.

#### Acceptance Criteria

1. WHEN QUIC_Tunnel 연결이 수립되면, THE TCP_Proxy SHALL 로컬 OS에서 사용 가능한 임시 TCP 포트를 바인딩하고 리스닝을 시작한다
2. WHEN TCP_Proxy가 리스닝을 시작하면, THE TCP_Proxy SHALL 바인딩된 포트 번호를 Rsync_Child 실행 모듈에 전달한다
3. WHEN rsync로부터 TCP 연결이 수립되면, THE TCP_Proxy SHALL 수신한 바이트 스트림을 Buffer_Layer로 전달한다
4. WHEN Remote_Server로부터 QUIC 스트림을 통해 응답 데이터가 도착하면, THE TCP_Proxy SHALL 해당 데이터를 rsync TCP 연결로 역방향 전달한다
5. WHEN rsync TCP 연결이 종료되면, THE TCP_Proxy SHALL 리스닝 소켓을 정리하고 관련 리소스를 해제한다

### Requirement 4: 무상태 버퍼링 레이어

**User Story:** 운영자로서, 로컬 rsync의 TCP 전송 속도가 장거리 QUIC 전송 속도에 의해 제한되지 않길 원한다. 그래야 TCP 윈도우 크기 제한 문제를 우회할 수 있다.

#### Acceptance Criteria

1. THE Buffer_Layer SHALL 기본 256MB 크기의 Ring_Buffer를 할당한다
2. WHERE 환경변수 `QUICSYNC_BUFFER_SIZE`가 설정되면, THE Buffer_Layer SHALL 해당 값을 Ring_Buffer 크기로 사용한다
3. WHEN TCP_Proxy로부터 데이터를 수신하면, THE Buffer_Layer SHALL 데이터를 Ring_Buffer에 저장하고 TCP_Proxy에 즉각 ACK를 반환한다
4. WHEN Ring_Buffer에 데이터가 존재하면, THE Buffer_Layer SHALL 데이터를 QUIC_Tunnel로 비동기 전송한다
5. IF Ring_Buffer가 가용 용량의 100%에 도달하면, THEN THE Buffer_Layer SHALL Backpressure를 적용하여 TCP_Proxy의 수신을 일시 중단한다
6. WHEN Ring_Buffer 사용량이 Backpressure 해제 임계값 이하로 감소하면, THE Buffer_Layer SHALL TCP_Proxy의 수신을 재개한다
7. WHEN QUIC_Tunnel이 유휴 상태이고 Ring_Buffer가 비어 있으면, THE Buffer_Layer SHALL QUIC 연결 유지를 위한 keep-alive를 전송한다

### Requirement 5: QUIC 터널

**User Story:** 운영자로서, 장거리 네트워크에서 UDP 기반 QUIC 프로토콜을 통해 데이터를 전송하고 싶다. 그래야 TCP 윈도우 크기 제한 없이 높은 처리량을 달성할 수 있다.

#### Acceptance Criteria

1. WHEN SSH_Launcher로부터 원격 포트와 인증 토큰을 수신하면, THE QUIC_Tunnel SHALL quinn 라이브러리를 사용하여 원격 Remote_Server에 QUIC 연결을 수립한다
2. THE QUIC_Tunnel SHALL TLS 1.3을 사용하여 모든 전송 데이터를 암호화한다
3. THE QUIC_Tunnel SHALL BBR 혼잡 제어 알고리즘을 기본으로 적용한다
4. WHEN Buffer_Layer로부터 데이터를 수신하면, THE QUIC_Tunnel SHALL 양방향 QUIC 스트림을 통해 데이터를 원격으로 전송한다
5. WHEN 원격으로부터 응답 데이터를 수신하면, THE QUIC_Tunnel SHALL 해당 데이터를 Buffer_Layer를 거쳐 TCP_Proxy로 전달한다
6. IF QUIC 연결이 예기치 않게 끊어지면, THEN THE QUIC_Tunnel SHALL 오류 원인을 로그에 기록하고 전체 세션을 정리한다
7. WHEN QUIC 연결 수립 시간이 500ms를 초과하면, THE QUIC_Tunnel SHALL 타임아웃 경고를 로그에 기록한다

### Requirement 6: 원격 서버 (Remote Server)

**User Story:** 운영자로서, 원격 호스트에서 QUIC 스트림을 수신하여 로컬 rsync 서버 프로세스에 전달하는 역방향 프록시가 동작하길 원한다. 그래야 양방향 rsync 프로토콜이 정상 작동한다.

#### Acceptance Criteria

1. WHEN Remote_Server가 시작되면, THE Remote_Server SHALL 사용 가능한 UDP 포트를 바인딩하고 QUIC 리스닝을 시작한다
2. WHEN Remote_Server가 QUIC 연결을 수락하면, THE Remote_Server SHALL 인증 토큰을 검증한 후 데이터 스트림 처리를 시작한다
3. IF 인증 토큰이 유효하지 않으면, THEN THE Remote_Server SHALL 연결을 즉시 거부하고 오류를 로그에 기록한다
4. WHEN QUIC 스트림으로부터 데이터를 수신하면, THE Remote_Server SHALL 해당 데이터를 로컬 rsync 서버 프로세스의 TCP 연결로 전달한다
5. WHEN 로컬 rsync 서버 프로세스로부터 응답 데이터를 수신하면, THE Remote_Server SHALL 해당 데이터를 QUIC 스트림을 통해 로컬로 전송한다
6. WHEN QUIC 연결이 종료되거나 세션이 완료되면, THE Remote_Server SHALL rsync 서버 프로세스를 종료하고 바인딩된 포트와 메모리를 해제한다

### Requirement 7: rsync 자식 프로세스 관리

**User Story:** 운영자로서, quicsync가 내부적으로 실제 rsync를 실행하여 Delta-sync를 수행하길 원한다. 그래야 변경된 블록만 전송하는 rsync의 핵심 기능을 그대로 활용할 수 있다.

#### Acceptance Criteria

1. WHEN TCP_Proxy가 리스닝을 시작하면, THE Rsync_Child SHALL rsync를 자식 프로세스로 실행하되, 원격 목적지를 TCP_Proxy의 로컬 포트로 리다이렉션한다
2. THE Rsync_Child SHALL CLI에서 전달받은 모든 rsync 옵션을 자식 프로세스에 그대로 전달한다
3. WHEN rsync 자식 프로세스가 종료되면, THE Rsync_Child SHALL rsync의 종료 코드를 quicsync의 종료 코드로 전파한다
4. IF rsync 자식 프로세스가 비정상 종료하면, THEN THE Rsync_Child SHALL rsync의 stderr 출력을 사용자에게 표시하고 해당 종료 코드로 종료한다
5. THE Rsync_Child SHALL 전송 완료 후 rsync 결과가 순정 rsync 실행과 바이트 단위로 동일한 결과를 생성한다

### Requirement 8: 세션 생명주기 및 리소스 정리

**User Story:** 운영자로서, quicsync 세션이 정상 종료든 비정상 종료든 모든 리소스가 확실히 정리되길 원한다. 그래야 좀비 프로세스나 포트 누수가 발생하지 않는다.

#### Acceptance Criteria

1. WHEN rsync 전송이 정상 완료되면, THE CLI SHALL TCP_Proxy, QUIC_Tunnel, Remote_Server를 순차적으로 종료하고 모든 리소스를 해제한다
2. IF 사용자가 SIGINT(Ctrl+C) 또는 SIGTERM 시그널을 전송하면, THEN THE CLI SHALL 진행 중인 전송을 중단하고 모든 리소스를 정리한 뒤 종료한다
3. IF 네트워크 오류로 QUIC_Tunnel이 끊어지면, THEN THE CLI SHALL 로컬 Rsync_Child와 TCP_Proxy를 종료하고 오류 메시지를 출력한다
4. WHEN Remote_Server의 QUIC 연결이 종료되면, THE Remote_Server SHALL 원격 rsync 프로세스를 종료하고 UDP 포트를 해제한다
5. THE CLI SHALL 10GB 이상 파일 전송 시 메모리 사용량이 기본 설정 기준 512MB를 초과하지 않는다
