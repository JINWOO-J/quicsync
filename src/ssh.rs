// SSH를 통한 원격 서버 실행 및 핸드셰이크

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::error::SshError;
use crate::types::{RemoteSpec, SshHandshake};

const HANDSHAKE_PREFIX: &str = "QUICSYNC_READY";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// SSH를 통해 원격 호스트에서 quicsync server를 실행하고 핸드셰이크를 수신한다.
///
/// 1. `ssh [user@]host quicsync --server` 명령어를 자식 프로세스로 실행
/// 2. stdout에서 `QUICSYNC_READY <port> <token>` 핸드셰이크 라인을 파싱
/// 3. SSH 프로세스는 터널로 유지 (종료하지 않음)
pub async fn launch_remote_server(remote: &RemoteSpec) -> Result<SshHandshake, SshError> {
    let ssh_target = match &remote.user {
        Some(user) => format!("{}@{}", user, remote.host),
        None => remote.host.clone(),
    };

    let mut child = Command::new("ssh")
        .arg(&ssh_target)
        .arg("PATH=$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:$PATH quicsync --server")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SshError::ConnectionFailed(format!("failed to spawn ssh: {}", e)))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SshError::ConnectionFailed("failed to capture stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SshError::ConnectionFailed("failed to capture stderr".into()))?;

    let mut stdout_reader = BufReader::new(stdout);
    let mut line = String::new();

    // stdout에서 핸드셰이크 라인 읽기 (타임아웃 적용)
    let read_result = timeout(HANDSHAKE_TIMEOUT, stdout_reader.read_line(&mut line)).await;

    match read_result {
        Ok(Ok(0)) | Ok(Err(_)) => {
            // stdout이 닫혔거나 읽기 오류 — stderr에서 원인 파악
            let stderr_msg = read_stderr(stderr).await;
            return Err(classify_ssh_error(&stderr_msg));
        }
        Err(_) => {
            // 타임아웃
            let _ = child.kill().await;
            return Err(SshError::HandshakeTimeout);
        }
        Ok(Ok(_)) => {} // 정상 읽기 완료
    }

    let info = parse_handshake(&line)?;

    Ok(SshHandshake {
        remote_port: info.port,
        auth_token: info.token,
        fingerprint: info.fingerprint,
        ssh_process: child,
    })
}

/// stderr 내용을 읽어 SSH 오류를 분류한다.
fn classify_ssh_error(stderr: &str) -> SshError {
    let lower = stderr.to_lowercase();
    if lower.contains("not found") || lower.contains("no such file") {
        SshError::BinaryNotFound(stderr.trim().to_string())
    } else {
        SshError::ConnectionFailed(stderr.trim().to_string())
    }
}

/// stderr에서 최대 4KB까지 읽어 진단 메시지를 반환한다.
async fn read_stderr(mut stderr: tokio::process::ChildStderr) -> String {
    let mut buf = vec![0u8; 4096];
    match timeout(Duration::from_secs(2), stderr.read(&mut buf)).await {
        Ok(Ok(n)) => String::from_utf8_lossy(&buf[..n]).to_string(),
        _ => String::new(),
    }
}

/// 핸드셰이크 파싱 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeInfo {
    pub port: u16,
    pub token: String,
    pub fingerprint: Option<String>,
}

/// SSH stdout에서 핸드셰이크 라인을 파싱한다.
///
/// 형식 (3-part, 하위 호환): `QUICSYNC_READY <port> <token>\n`
/// 형식 (4-part, 지문 포함): `QUICSYNC_READY <port> <token> <fingerprint>\n`
/// - port: 1–65535 범위의 UDP 포트 번호
/// - token: 64자 hex 인코딩된 인증 토큰
/// - fingerprint: 64자 hex 인코딩된 인증서 SHA-256 지문 (선택)
pub fn parse_handshake(stdout_line: &str) -> Result<HandshakeInfo, SshError> {
    let line = stdout_line.trim();
    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.len() != 3 && parts.len() != 4 {
        return Err(SshError::HandshakeParseFailed(format!(
            "expected 3 or 4 parts (QUICSYNC_READY <port> <token> [fingerprint]), got {}",
            parts.len()
        )));
    }

    if parts[0] != HANDSHAKE_PREFIX {
        return Err(SshError::HandshakeParseFailed(format!(
            "expected prefix '{}', got '{}'",
            HANDSHAKE_PREFIX, parts[0]
        )));
    }

    let port: u16 = parts[1]
        .parse()
        .map_err(|_| SshError::HandshakeParseFailed(format!("invalid port: '{}'", parts[1])))?;

    if port == 0 {
        return Err(SshError::HandshakeParseFailed(
            "port must be 1-65535, got 0".to_string(),
        ));
    }

    let token = parts[2];
    if token.len() != 64 {
        return Err(SshError::HandshakeParseFailed(format!(
            "token must be 64 hex chars, got {} chars",
            token.len()
        )));
    }

    if !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SshError::HandshakeParseFailed(
            "token contains non-hex characters".to_string(),
        ));
    }

    let fingerprint = if parts.len() == 4 {
        let fp = parts[3];
        if fp.len() != 64 {
            return Err(SshError::HandshakeParseFailed(format!(
                "fingerprint must be 64 hex chars, got {} chars",
                fp.len()
            )));
        }
        if !fp.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(SshError::HandshakeParseFailed(
                "fingerprint contains non-hex characters".to_string(),
            ));
        }
        Some(fp.to_string())
    } else {
        None
    };

    Ok(HandshakeInfo {
        port,
        token: token.to_string(),
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AuthToken;
    use proptest::prelude::*;

    #[test]
    fn parse_valid_handshake() {
        let token = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let line = format!("QUICSYNC_READY 45231 {}", token);
        let info = parse_handshake(&line).unwrap();
        assert_eq!(info.port, 45231);
        assert_eq!(info.token, token);
        assert_eq!(info.fingerprint, None);
    }

    #[test]
    fn parse_handshake_with_trailing_newline() {
        let token = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let line = format!("QUICSYNC_READY 8080 {}\n", token);
        let info = parse_handshake(&line).unwrap();
        assert_eq!(info.port, 8080);
    }

    #[test]
    fn parse_handshake_4_part_with_fingerprint() {
        let token = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let fp = "ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00";
        let line = format!("QUICSYNC_READY 9090 {} {}", token, fp);
        let info = parse_handshake(&line).unwrap();
        assert_eq!(info.port, 9090);
        assert_eq!(info.token, token);
        assert_eq!(info.fingerprint.as_deref(), Some(fp));
    }

    #[test]
    fn parse_handshake_missing_prefix() {
        let result = parse_handshake("WRONG_PREFIX 8080 aabbccdd");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("expected prefix"));
    }

    #[test]
    fn parse_handshake_invalid_port() {
        let token = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let result = parse_handshake(&format!("QUICSYNC_READY 99999 {}", token));
        assert!(result.is_err());
    }

    #[test]
    fn parse_handshake_port_zero() {
        let token = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let result = parse_handshake(&format!("QUICSYNC_READY 0 {}", token));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("port must be 1-65535"));
    }

    #[test]
    fn parse_handshake_short_token() {
        let result = parse_handshake("QUICSYNC_READY 8080 aabbccdd");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("64 hex chars"));
    }

    #[test]
    fn parse_handshake_non_hex_token() {
        // 64 chars but contains 'g' which is not hex
        let bad_token = "g1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let result = parse_handshake(&format!("QUICSYNC_READY 8080 {}", bad_token));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("non-hex"));
    }

    #[test]
    fn parse_handshake_too_few_parts() {
        let result = parse_handshake("QUICSYNC_READY 8080");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("expected 3 or 4 parts"));
    }

    #[test]
    fn parse_handshake_too_many_parts() {
        let token = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let fp = "ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00";
        let result = parse_handshake(&format!("QUICSYNC_READY 8080 {} {} extra", token, fp));
        assert!(result.is_err());
    }

    // Feature: quicsync-tunnel-mvp, Property 3: 핸드셰이크 프로토콜 라운드트립
    // **Validates: Requirements 2.2**

    // --- classify_ssh_error 테스트 ---

    #[test]
    fn classify_not_found() {
        let err = classify_ssh_error("bash: quicsync: command not found");
        assert!(matches!(err, SshError::BinaryNotFound(_)));
    }

    #[test]
    fn classify_no_such_file() {
        let err = classify_ssh_error("No such file or directory");
        assert!(matches!(err, SshError::BinaryNotFound(_)));
    }

    #[test]
    fn classify_generic_error() {
        let err = classify_ssh_error("Connection refused");
        assert!(matches!(err, SshError::ConnectionFailed(_)));
    }

    #[test]
    fn classify_empty_string() {
        let err = classify_ssh_error("");
        assert!(matches!(err, SshError::ConnectionFailed(_)));
    }

    #[test]
    fn classify_mixed_case_not_found() {
        let err = classify_ssh_error("ERROR: Not Found on remote host");
        assert!(matches!(err, SshError::BinaryNotFound(_)));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// 3-part: 임의의 유효 포트(1–65535)와 32바이트 토큰으로 핸드셰이크 문자열을 구성한 뒤
        /// parse_handshake로 파싱하면 원래 포트와 토큰이 복원되어야 한다.
        #[test]
        fn handshake_roundtrip(
            port in 1u16..=65535,
            token_bytes in prop::array::uniform32(any::<u8>()),
        ) {
            let token = AuthToken::from_raw(token_bytes);
            let token_hex = token.to_hex();

            let handshake_line = format!("QUICSYNC_READY {} {}", port, token_hex);

            let info = parse_handshake(&handshake_line)
                .expect("valid handshake should parse");

            prop_assert_eq!(info.port, port);
            prop_assert_eq!(info.token, token_hex);
            prop_assert_eq!(info.fingerprint, None);
        }

        /// 4-part: 지문 포함 핸드셰이크 라운드트립 (Property 6)
        #[test]
        fn handshake_roundtrip_with_fingerprint(
            port in 1u16..=65535,
            token_bytes in prop::array::uniform32(any::<u8>()),
            fp_bytes in prop::array::uniform32(any::<u8>()),
        ) {
            let token = AuthToken::from_raw(token_bytes);
            let token_hex = token.to_hex();
            let fp_hex = hex::encode(fp_bytes);

            let handshake_line = format!("QUICSYNC_READY {} {} {}", port, token_hex, fp_hex);

            let info = parse_handshake(&handshake_line)
                .expect("valid 4-part handshake should parse");

            prop_assert_eq!(info.port, port);
            prop_assert_eq!(info.token, token_hex);
            prop_assert_eq!(info.fingerprint, Some(fp_hex));
        }
    }
}
