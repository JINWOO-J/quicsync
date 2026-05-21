// 핵심 데이터 모델 및 타입 정의

use std::path::PathBuf;
use tokio::process::Child;

use ring::rand::SecureRandom;

use crate::error::TokenError;

/// 원격 경로 파싱 결과 (`user@host:path`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    pub user: Option<String>,
    pub host: String,
    pub path: String,
}

/// 전송 방향
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// 로컬 → 원격
    Push,
    /// 원격 → 로컬
    Pull,
}

/// QUIC 초기화 실패 시 fallback 동작
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackMode {
    None,
    Rsync,
}

/// CLI 인수 파싱 결과
#[derive(Debug, Clone)]
pub struct CliArgs {
    pub local_paths: Vec<PathBuf>,
    pub remote: RemoteSpec,
    pub rsync_options: Vec<String>,
    pub direction: TransferDirection,
    /// QUIC 윈도우 크기 (바이트). --window 옵션 또는 QUICSYNC_WINDOW 환경변수로 설정.
    pub quic_window: u64,

    // Phase 2/3 신규 필드
    /// --no-progress로 비활성화 (기본: stdout이 터미널이면 true)
    pub show_progress: bool,
    /// --streams N (기본: 4, 범위: 1-64)
    pub streams: u8,
    /// --stats
    pub stats: bool,
    /// --stats-format json|text (기본: text)
    pub stats_format: StatsFormat,
    /// --otel-endpoint URL
    pub otel_endpoint: Option<String>,
    /// --no-integrity (기본: false, 즉 무결성 검사 활성)
    pub no_integrity: bool,
    /// QUIC 초기화 실패 시 fallback 동작
    pub fallback: FallbackMode,
    /// 원격 quicsync가 없을 때 현재 로컬 바이너리를 원격에 설치하고 한 번 재시도
    pub install_remote: bool,
}

/// 32바이트 랜덤 인증 토큰 (hex 인코딩 시 64자)
#[derive(Clone)]
pub struct AuthToken([u8; 32]);

impl AuthToken {
    /// 암호학적으로 안전한 랜덤 토큰 생성
    pub fn generate() -> Self {
        let rng = ring::rand::SystemRandom::new();
        let mut bytes = [0u8; 32];
        rng.fill(&mut bytes).expect("system random should not fail");
        Self(bytes)
    }

    /// hex 문자열로 변환 (64자)
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// hex 문자열에서 복원
    pub fn from_hex(s: &str) -> Result<Self, TokenError> {
        let bytes = hex::decode(s).map_err(|e| TokenError::InvalidHex(e.to_string()))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|v: Vec<u8>| TokenError::InvalidLength(v.len()))?;
        Ok(Self(arr))
    }

    /// 상수 시간 비교로 토큰 검증
    pub fn verify(&self, other: &Self) -> bool {
        // 상수 시간 비교: 모든 바이트를 항상 비교하여 타이밍 공격 방지
        let mut diff = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }

    /// 내부 바이트 배열 참조
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 바이트 배열에서 직접 생성 (테스트용)
    pub fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuthToken([REDACTED])")
    }
}

impl PartialEq for AuthToken {
    fn eq(&self, other: &Self) -> bool {
        self.verify(other)
    }
}

impl Eq for AuthToken {}

/// SSH 핸드셰이크 결과
#[derive(Debug)]
pub struct SshHandshake {
    pub remote_port: u16,
    pub auth_token: String,
    pub ssh_process: Child,
    pub fingerprint: Option<String>,
}

/// 통계 출력 형식
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsFormat {
    Text,
    Json,
}

/// 전송 대상 파일 정보
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

/// 개별 스트림의 전송 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamResult {
    pub stream_id: usize,
    pub success: bool,
    pub bytes_transferred: u64,
    pub error: Option<String>,
}

/// 전체 병렬 전송 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiStreamReport {
    pub results: Vec<StreamResult>,
    pub total_success: usize,
    pub total_failed: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Feature: quicsync-tunnel-mvp, Property 8: 인증 토큰 검증
    // **Validates: Requirements 6.2, 6.3**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// to_hex → from_hex 라운드트립: 임의 32바이트 배열에서 생성한 토큰이
        /// hex 인코딩 후 디코딩하면 원래 토큰과 동일해야 한다.
        #[test]
        fn auth_token_hex_roundtrip(bytes in prop::array::uniform32(any::<u8>())) {
            let token = AuthToken(bytes);
            let hex_str = token.to_hex();
            let restored = AuthToken::from_hex(&hex_str).expect("valid hex should parse");
            prop_assert_eq!(token.as_bytes(), restored.as_bytes());
        }

        /// 동일 토큰 verify: 같은 바이트로 만든 두 토큰은 verify가 true여야 한다.
        #[test]
        fn auth_token_verify_same(bytes in prop::array::uniform32(any::<u8>())) {
            let token = AuthToken(bytes);
            let clone = AuthToken(bytes);
            prop_assert!(token.verify(&clone));
        }

        /// 상이 토큰 verify: 서로 다른 바이트 배열의 토큰은 verify가 false여야 한다.
        #[test]
        fn auth_token_verify_different(
            bytes_a in prop::array::uniform32(any::<u8>()),
            bytes_b in prop::array::uniform32(any::<u8>()),
        ) {
            prop_assume!(bytes_a != bytes_b);
            let token_a = AuthToken(bytes_a);
            let token_b = AuthToken(bytes_b);
            prop_assert!(!token_a.verify(&token_b));
        }
    }
}
