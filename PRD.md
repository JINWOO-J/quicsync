# quicsync — Product Requirements Document

**Version:** 0.1 (Draft)
**작성일:** 2026-02-23
**상태:** Pre-development

---

## 1. 제품 개요 (Product Overview)

### 1.1 한 줄 정의

> `rsync` 명령어의 앞글자만 바꾸면, 장거리 네트워크에서 Rsync의 Delta-sync 성능을 완전히 유지하면서 QUIC(UDP) 기반의 고속 전송을 즉시 활용할 수 있는 단일 바이너리 가속기.

### 1.2 해결하는 문제

RTT 100ms 이상의 장거리 네트워크(LFN, Long Fat Network)에서 기존 `rsync over SSH(TCP)`는 TCP 윈도우 크기 제한으로 인해 이론 대역폭의 **10~20%만 활용**된다. 해외 서버 운영자, 글로벌 백업 담당자 등 실질적인 수요층이 존재하지만, 오픈소스로 "QUIC + Delta-sync"를 동시에 지원하는 도구는 현재 없다.

### 1.3 타겟 사용자

| 사용자 유형 | 상황 | 페인포인트 |
|-------------|------|------------|
| 해외 서버 운영자 | KR ↔ US/EU 서버 간 정기 동기화 | rsync 속도 저하, 대역폭 낭비 |
| DevOps 엔지니어 | CI/CD 아티팩트 배포 | 대용량 바이너리 반복 전송 |
| 글로벌 백업 담당자 | 원격 오프사이트 백업 | 백업 시간 초과, 창 부족 |
| 오픈소스 기여자 | 기존 rsync 스크립트 활용 | 새 도구 학습 비용 거부감 |

---

## 2. 핵심 가치 제안 (Value Proposition)

### 2.1 경쟁 포지셔닝

| 도구 | Delta-sync | QUIC | Zero-Config | 오픈소스 |
|------|:----------:|:----:|:-----------:|:--------:|
| rsync over SSH | ✅ | ❌ | ✅ | ✅ |
| rclone | ❌ | ❌ | △ | ✅ |
| Aspera | ✅ | ❌(독자 UDP) | ❌ | ❌ |
| WireGuard + rsync | ✅ | ❌ | ❌ | ✅ |
| **quicsync** | **✅** | **✅** | **✅** | **✅** |

### 2.2 킬러 피처 3가지

**① Delta-sync 완전 유지**
파일 전체를 재전송하는 타 QUIC 도구들과 달리, 내부적으로 실제 rsync 프로세스를 실행하여 변경 블록만 전송한다.

**② Zero-Config UX**
기존 백업 스크립트나 인프라 설정 변경 없이 `rsync` → `quicsync`로 명령어만 교체하면 동작한다. 별도 데몬 설치, 포트 포워딩 불필요.

**③ 하이브리드 보안**
인증·권한·포트 협상은 기존 SSH(TCP 22) 인프라를 그대로 활용하고, 데이터 전송만 일회용 QUIC(UDP) 터널로 우회한다.

---

## 3. 기술 아키텍처 (Technical Architecture)

### 3.1 기술 스택

- **언어:** Rust
- **비동기 런타임:** `tokio`
- **QUIC 엔진:** `quinn`
- **배포 형태:** 단일 정적 바이너리 (musl 링크, cross-compile)

### 3.2 핵심 동작 흐름

```
[사용자]
  quicsync user@remote:/path /local/path
       │
       ▼
[로컬 quicsync 프로세스]
  1. SSH로 원격 접속 → 원격 측 quicsync 서버 프로세스 임시 실행
  2. 로컬 OS에 임시 TCP 프록시 포트 생성
  3. rsync를 자식 프로세스로 실행
     → 목적지를 원격이 아닌 '로컬 프록시 포트'로 강제
       │
       ▼
[무상태 버퍼링 레이어 (핵심)]
  - 로컬 rsync TCP 패킷에 즉각 ACK 응답
  - 패킷을 메모리 큐(Ring Buffer)에 수신
  - QUIC 전송 속도와 TCP 수신 속도를 논리적으로 단절
       │
       ▼
[QUIC 터널 (quinn)]
  - UDP 기반 멀티스트림 전송
  - 혼잡 제어: BBR 우선, CUBIC 폴백
  - TLS 1.3 기본 적용
       │
       ▼
[원격 quicsync 서버 (임시)]
  - QUIC → TCP 역변환
  - 원격 rsync 서버 프로세스로 전달
  - 세션 종료 시 자동 정리
```

### 3.3 메모리 큐 설계 원칙

- 큐 크기: 기본 256MB (환경변수로 조정 가능)
- 큐 오버플로우 시: 백프레셔(backpressure) 적용하여 rsync TCP 수신 일시 중단
- 큐 언더플로우 시: QUIC idle 유지, keep-alive 전송

---

## 4. 비기능 요구사항 (Non-functional Requirements)

| 항목 | 목표 |
|------|------|
| 처리량 | RTT 100ms 환경에서 순정 rsync 대비 **3배 이상** 처리량 |
| 지연 오버헤드 | QUIC 터널 설정 시간 < 500ms |
| 메모리 사용 | 기본 설정 기준 < 512MB |
| 이진 크기 | 단일 바이너리 < 20MB |
| 플랫폼 | Linux x86_64 / ARM64, macOS (ARM/Intel) |
| 라이선스 | MIT 또는 Apache-2.0 |

---

## 5. 알려진 불확실성 및 리스크

| 리스크 | 설명 | 대응 방안 |
|--------|------|-----------|
| TCP Meltdown 완전 회피 여부 | 극단적 패킷 손실 구간에서 QUIC 혼잡 제어와 내부 TCP 윈도우 충돌 가능 | 실측 데이터 수집 후 BBR 파라미터 튜닝 |
| 기업 방화벽 UDP 차단 | UDP 인바운드 전면 차단 시 QUIC 터널 실패 | TCP 폴백 자동 전환 구현 (Phase 2) |
| rsync 프로토콜 양방향성 | 단순 단방향 스트림이 아니라 핸드셰이크·협상 패킷 존재 | 프로토콜 분석 후 양방향 프록시 구현 필요 |
| 원격 측 quicsync 미설치 | 상대 서버에 quicsync 없으면 동작 불가 | 패키지 배포 전략 + SSH one-liner 설치 지원 |

---

## 6. 개발 로드맵 (Phased Plan)

---

### Phase 1 — 터널 검증 (MVP)

**목표:** "양측 모두 quicsync 설치" 전제 하에, rsync-over-QUIC 터널이 실제로 순정 rsync보다 빠름을 수치로 증명한다.

**기간:** 6~8주

**범위:**

- SSH를 통한 원격 quicsync 서버 자동 실행
- 로컬 임시 TCP 프록시 ↔ QUIC 터널 ↔ 원격 TCP 역변환 기본 구현
- 무상태 버퍼링 레이어 초기 버전 (고정 큐 크기)
- rsync 자식 프로세스 실행 및 목적지 강제 리다이렉션
- 기본 CLI: `quicsync [rsync options] SRC DST`
- BBR 혼잡 제어 기본 적용
- TLS 1.3 기본 암호화

**성공 기준:**

- RTT 100ms 환경에서 순정 rsync 대비 처리량 **2배 이상**
- 전송 결과가 순정 rsync와 **바이트 단위로 동일**
- 10GB 이상 파일 전송 시 메모리 누수 없음

**이 단계에서 의도적으로 제외하는 것:**

- UDP 차단 환경 폴백
- Zero-Config (양측 설치 필수)
- macOS 지원 (Linux only)

---

### Phase 2 — Zero-Config & 폴백

**목표:** 원격 서버에 quicsync가 없어도 동작하고, UDP 차단 환경에서도 자동으로 순정 rsync로 폴백한다.

**기간:** 8~10주

**범위:**

- **UDP 차단 폴백:** QUIC 터널 실패 시 자동으로 순정 rsync(SSH+TCP) 전환, 사용자 알림
- **SSH one-liner 원격 설치:** 원격 측에 quicsync 미설치 시, SSH로 단일 바이너리 자동 업로드 후 실행
- **동적 큐 크기 조정:** 네트워크 조건에 따라 버퍼 크기 자동 튜닝
- **macOS 지원** (Apple Silicon + Intel)
- **기본 Progress UI:** 전송 속도, 예상 완료 시간, 현재 모드(QUIC/TCP) 표시
- **환경변수 및 설정 파일** (`~/.config/quicsync/config.toml`) 지원

**성공 기준:**

- UDP 완전 차단 환경에서 사용자 개입 없이 TCP 폴백 완료 (< 3초 내 전환)
- 원격 quicsync 미설치 환경에서 자동 설치 후 정상 동작
- 기존 rsync 스크립트에서 명령어만 교체하여 동작 (인수 호환성 95% 이상)

---

### Phase 3 — 프로덕션 강화 & 생태계

**목표:** 기업 운영 환경에서 안정적으로 사용할 수 있는 수준의 관측성, 보안, 패키지 배포를 완성한다.

**기간:** 10~12주

**범위:**

- **다중 스트림 병렬 전송:** 단일 디렉토리 내 파일을 QUIC 멀티스트림으로 병렬 전송
- **관측성(Observability):**
  - `--stats` 플래그: 전송 후 상세 성능 리포트 (처리량, RTT, 큐 깊이, 폴백 여부)
  - JSON 출력 모드 (`--output-format json`)
  - OpenTelemetry 트레이싱 옵션
- **보안 강화:**
  - QUIC 세션 핀닝 (서버 인증서 지문 검증)
  - 전송 중 무결성 검사 (Blake3 체크섬)
- **패키지 배포:**
  - Homebrew Tap (macOS)
  - APT/RPM 패키지 (Debian/RHEL 계열)
  - GitHub Releases 자동 배포 (cross-compile CI)
  - Docker 이미지
- **문서화:**
  - 영문/한국어 README
  - 아키텍처 다이어그램
  - 성능 벤치마크 공개 (AWS, GCP 실측)
  - rsync → quicsync 마이그레이션 가이드

**성공 기준:**

- 10,000 파일 / 100GB 전송 무중단 완료
- RTT 200ms 환경에서 처리량 순정 rsync 대비 **4배 이상**
- GitHub Stars 500+ (오픈소스 커뮤니티 반응 검증)

---

## 7. 마일스톤 요약

```
Week 0        Week 8        Week 18       Week 30
  │             │             │             │
  ├─── Phase 1 ─┤             │             │
  │    터널 MVP │             │             │
  │             ├─── Phase 2 ─┤             │
  │             │  Zero-Config│             │
  │             │  + 폴백     ├─── Phase 3 ─┤
  │             │             │  프로덕션   │
  │             │             │  강화       │
```

| 마일스톤 | 예상 시점 | 주요 산출물 |
|----------|-----------|-------------|
| Phase 1 완료 | Week 8 | rsync-over-QUIC 동작 + 벤치마크 |
| Phase 2 완료 | Week 18 | Zero-Config + 폴백 + macOS 지원 |
| Phase 3 완료 | Week 30 | 패키지 배포 + 공개 벤치마크 |

---

## 8. 기술 부채 및 향후 과제 (Out of Scope v1)

- Windows 지원
- GUI 클라이언트
- rsync daemon mode (`rsync://`) 지원
- 클라우드 스토리지 직접 연동 (S3, GCS 등)
- 멀티홉 릴레이 서버

---

*이 문서는 살아있는 문서입니다. Phase 1 완료 후 실측 데이터를 기반으로 Phase 2/3 계획을 재조정할 수 있습니다.*