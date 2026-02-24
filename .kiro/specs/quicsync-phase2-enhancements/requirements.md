# Requirements Document

## Introduction

quicsync Phase 2/3 기능 확장의 요구사항 문서이다. 기존 Phase 1 MVP(QUIC 터널 기반 rsync 가속)가 완성된 상태에서, 다음 4가지 영역을 추가한다:

1. **플랫폼 확장 및 Progress UI**: macOS(Apple Silicon/Intel) 크로스 컴파일 지원과 전송 상태를 실시간으로 표시하는 터미널 Progress UI
2. **멀티스트림 병렬 전송**: QUIC 멀티스트림을 활용한 디렉토리 내 파일 병렬 전송으로 대역폭 활용 극대화
3. **관측성(Observability)**: `--stats` 플래그를 통한 성능 리포트, JSON 출력 모드, OpenTelemetry 트레이싱 연동
4. **보안 계층 강화**: QUIC 세션 핀닝(서버 인증서 지문 검증)과 Blake3 체크섬 기반 전송 중 데이터 무결성 검사

기존 Phase 1 코드베이스의 인터페이스를 최대한 유지하면서 확장하는 것을 원칙으로 한다.

## Glossary

- **CLI**: quicsync의 Command-Line Interface. `quicsync [options] SRC DST` 형태로 실행하는 진입점
- **Progress_UI**: 터미널에 전송 속도, 예상 완료 시간, 현재 구동 모드(QUIC/TCP) 등을 실시간으로 표시하는 컴포넌트
- **Multi_Stream_Manager**: QUIC 연결 위에 여러 개의 양방향 스트림을 열어 파일들을 병렬로 전송하는 컴포넌트
- **Stats_Reporter**: 전송 완료 후 처리량, RTT, 큐 깊이 등의 성능 메트릭을 수집하고 리포트하는 컴포넌트
- **Telemetry_Exporter**: OpenTelemetry 프로토콜을 통해 트레이싱 데이터를 외부 수집기로 전송하는 컴포넌트
- **Session_Pinner**: QUIC 연결 수립 시 서버 인증서의 SHA-256 지문을 검증하여 MITM 공격을 방지하는 컴포넌트
- **Integrity_Checker**: Blake3 해시를 사용하여 전송 중 데이터 무결성을 검증하는 컴포넌트
- **QUIC_Tunnel**: 기존 Phase 1의 quinn 기반 QUIC 연결 컴포넌트
- **Remote_Server**: 원격 호스트에서 실행되는 quicsync 서버 프로세스
- **Transfer_Metrics**: 전송 중 수집되는 성능 데이터(처리량, RTT, 큐 깊이, 스트림 수 등)의 집합
- **Blake3**: 고속 암호학적 해시 함수. 데이터 무결성 검증에 사용
- **Certificate_Fingerprint**: 서버 TLS 인증서의 SHA-256 해시값. 세션 핀닝에 사용
- **OpenTelemetry**: 분산 트레이싱, 메트릭, 로그를 수집하는 관측성 프레임워크

## Requirements

### Requirement 1: macOS 플랫폼 지원

**User Story:** 운영자로서, macOS(Apple Silicon 및 Intel) 환경에서도 quicsync를 사용하고 싶다. 그래야 macOS 기반 개발/운영 환경에서도 장거리 파일 전송을 가속할 수 있다.

#### Acceptance Criteria

1. THE CLI SHALL `aarch64-apple-darwin`(Apple Silicon) 타겟으로 크로스 컴파일되어 정상 실행된다
2. THE CLI SHALL `x86_64-apple-darwin`(Intel Mac) 타겟으로 크로스 컴파일되어 정상 실행된다
3. WHEN macOS에서 실행될 때, THE CLI SHALL Linux 환경과 동일한 CLI 인수 및 동작을 제공한다
4. WHEN macOS에서 QUIC 터널을 수립할 때, THE QUIC_Tunnel SHALL Linux와 동일한 TLS 1.3 및 BBR 혼잡 제어를 적용한다
5. IF macOS 빌드에서 플랫폼 특화 API 호출이 실패하면, THEN THE CLI SHALL 플랫폼 호환성 오류 메시지를 출력하고 종료 코드 1로 종료한다

### Requirement 2: Progress UI

**User Story:** 운영자로서, 전송 중 현재 속도, 예상 완료 시간, 구동 모드를 실시간으로 확인하고 싶다. 그래야 전송 상태를 모니터링하고 문제를 조기에 감지할 수 있다.

#### Acceptance Criteria

1. WHEN 전송이 진행 중일 때, THE Progress_UI SHALL 현재 전송 속도(bytes/sec)를 터미널에 표시한다
2. WHEN 전송이 진행 중일 때, THE Progress_UI SHALL 예상 완료 시간(ETA)을 터미널에 표시한다
3. WHEN 전송이 진행 중일 때, THE Progress_UI SHALL 현재 구동 모드(QUIC 또는 TCP)를 터미널에 표시한다
4. WHEN 전송이 진행 중일 때, THE Progress_UI SHALL 전송된 바이트 수와 총 바이트 수를 터미널에 표시한다
5. THE Progress_UI SHALL 500ms 이하의 주기로 표시 정보를 갱신한다
6. WHEN `--no-progress` 플래그가 지정되면, THE CLI SHALL Progress_UI 출력을 비활성화한다
7. WHEN 표준 출력이 터미널이 아닌 경우(파이프 등), THE Progress_UI SHALL 자동으로 비활성화된다
8. WHEN Progress_UI가 메트릭을 포맷할 때, THE Progress_UI SHALL 전송 속도를 사람이 읽기 쉬운 단위(B/s, KB/s, MB/s, GB/s)로 자동 변환하여 표시한다
9. WHEN Progress_UI가 메트릭을 포맷할 때, THE Progress_UI SHALL 전송된 바이트 수를 사람이 읽기 쉬운 단위(B, KB, MB, GB)로 자동 변환하여 표시한다

### Requirement 3: 멀티스트림 병렬 전송

**User Story:** 운영자로서, 디렉토리 내 여러 파일을 전송할 때 QUIC 멀티스트림을 활용하여 병렬로 전송하고 싶다. 그래야 대역폭 활용을 극대화하고 전체 전송 시간을 단축할 수 있다.

#### Acceptance Criteria

1. WHEN 디렉토리 전송 시 여러 파일이 존재하면, THE Multi_Stream_Manager SHALL 각 파일에 대해 별도의 QUIC 스트림을 열어 병렬로 전송한다
2. THE Multi_Stream_Manager SHALL 동시 활성 스트림 수를 기본 4개로 제한한다
3. WHERE `--streams <N>` 옵션이 지정되면, THE Multi_Stream_Manager SHALL 동시 활성 스트림 수를 N개로 설정한다
4. IF `--streams` 옵션의 값이 1 미만이거나 64 초과이면, THEN THE CLI SHALL 유효 범위(1-64) 오류 메시지를 출력하고 종료 코드 1로 종료한다
5. WHEN 병렬 전송 중 하나의 스트림에서 오류가 발생하면, THE Multi_Stream_Manager SHALL 해당 스트림만 종료하고 나머지 스트림의 전송을 계속한다
6. WHEN 모든 스트림의 전송이 완료되면, THE Multi_Stream_Manager SHALL 각 스트림의 전송 결과(성공/실패)를 집계하여 보고한다
7. THE Multi_Stream_Manager SHALL 단일 QUIC 연결 위에서 멀티스트림을 운용하여 추가 핸드셰이크 오버헤드 없이 병렬 전송한다

### Requirement 4: 성능 통계 리포트

**User Story:** 운영자로서, 전송 완료 후 처리량, RTT, 큐 깊이 등의 상세 성능 리포트를 확인하고 싶다. 그래야 네트워크 성능을 분석하고 최적화 포인트를 파악할 수 있다.

#### Acceptance Criteria

1. WHEN `--stats` 플래그가 지정되면, THE Stats_Reporter SHALL 전송 완료 후 성능 리포트를 표준 오류(stderr)에 출력한다
2. WHEN 성능 리포트를 출력할 때, THE Stats_Reporter SHALL 총 전송 바이트 수, 평균 처리량(bytes/sec), 전송 소요 시간을 포함한다
3. WHEN 성능 리포트를 출력할 때, THE Stats_Reporter SHALL 평균 RTT, 최소 RTT, 최대 RTT를 포함한다
4. WHEN 성능 리포트를 출력할 때, THE Stats_Reporter SHALL 최대 큐 깊이와 backpressure 발생 횟수를 포함한다
5. WHEN `--stats-format json` 옵션이 지정되면, THE Stats_Reporter SHALL 성능 리포트를 JSON 형식으로 출력한다
6. WHEN JSON 형식으로 출력할 때, THE Stats_Reporter SHALL 모든 메트릭 필드를 포함하는 유효한 JSON 객체를 생성한다
7. THE Stats_Reporter SHALL Transfer_Metrics를 JSON으로 직렬화한 뒤 역직렬화하면 원래 데이터와 동일한 값을 복원한다

### Requirement 5: OpenTelemetry 트레이싱

**User Story:** 운영자로서, quicsync의 전송 과정을 OpenTelemetry 트레이싱으로 추적하고 싶다. 그래야 분산 환경에서 전송 병목을 진단하고 모니터링 시스템과 통합할 수 있다.

#### Acceptance Criteria

1. WHEN `--otel-endpoint <URL>` 옵션이 지정되면, THE Telemetry_Exporter SHALL 해당 URL의 OpenTelemetry 수집기로 트레이스를 전송한다
2. WHEN OpenTelemetry가 활성화되면, THE Telemetry_Exporter SHALL 세션 전체를 하나의 루트 span으로 생성한다
3. WHEN OpenTelemetry가 활성화되면, THE Telemetry_Exporter SHALL SSH 핸드셰이크, QUIC 연결, 데이터 전송 각 단계를 하위 span으로 기록한다
4. WHEN 전송 span이 기록될 때, THE Telemetry_Exporter SHALL 전송 바이트 수, 소요 시간, 스트림 수를 span attribute로 포함한다
5. IF OpenTelemetry 수집기 연결이 실패하면, THEN THE Telemetry_Exporter SHALL 경고를 로그에 기록하고 전송 작업은 정상적으로 계속한다
6. WHEN `--otel-endpoint` 옵션이 지정되지 않으면, THE CLI SHALL OpenTelemetry 관련 초기화를 수행하지 않는다

### Requirement 6: QUIC 세션 핀닝

**User Story:** 운영자로서, QUIC 연결 수립 시 서버 인증서 지문을 검증하여 MITM 공격을 방지하고 싶다. 그래야 신뢰할 수 없는 네트워크에서도 안전하게 파일을 전송할 수 있다.

#### Acceptance Criteria

1. WHEN Remote_Server가 시작될 때, THE Remote_Server SHALL 서버 인증서의 SHA-256 지문을 SSH stdout 핸드셰이크에 포함하여 전달한다
2. WHEN QUIC 연결 수립 시, THE Session_Pinner SHALL 서버가 제시하는 인증서의 SHA-256 지문을 계산한다
3. WHEN 인증서 지문을 검증할 때, THE Session_Pinner SHALL SSH를 통해 수신한 지문과 서버 인증서 지문을 상수 시간 비교한다
4. IF 인증서 지문이 일치하지 않으면, THEN THE Session_Pinner SHALL QUIC 연결을 즉시 거부하고 지문 불일치 오류를 출력한다
5. THE Session_Pinner SHALL SHA-256 지문을 hex 인코딩하여 64자 문자열로 표현한다
6. WHEN 핸드셰이크 프로토콜이 지문을 포함할 때, THE SSH_Launcher SHALL `QUICSYNC_READY <port> <token> <fingerprint>` 형태로 확장된 핸드셰이크를 파싱한다

### Requirement 7: Blake3 데이터 무결성 검사

**User Story:** 운영자로서, 전송 중 데이터가 손상되지 않았는지 Blake3 체크섬으로 검증하고 싶다. 그래야 네트워크 오류로 인한 데이터 손상을 감지할 수 있다.

#### Acceptance Criteria

1. WHEN 데이터 청크를 전송할 때, THE Integrity_Checker SHALL 각 청크의 Blake3 해시를 계산하여 데이터와 함께 전송한다
2. WHEN 데이터 청크를 수신할 때, THE Integrity_Checker SHALL 수신한 데이터의 Blake3 해시를 재계산하여 전송된 해시와 비교한다
3. IF 해시가 일치하지 않으면, THEN THE Integrity_Checker SHALL 해당 청크의 무결성 오류를 보고하고 전송을 중단한다
4. THE Integrity_Checker SHALL Blake3 해시를 32바이트(256비트) 길이로 계산한다
5. WHEN `--no-integrity` 플래그가 지정되면, THE CLI SHALL 무결성 검사를 비활성화하여 CPU 오버헤드를 제거한다
6. THE Integrity_Checker SHALL 임의의 바이트 데이터에 대해 Blake3 해시를 계산한 뒤 동일한 데이터로 재계산하면 동일한 해시를 반환한다
7. WHEN 무결성 검사가 활성화된 상태에서 전송이 완료되면, THE Integrity_Checker SHALL 검증된 총 청크 수와 총 바이트 수를 로그에 기록한다
