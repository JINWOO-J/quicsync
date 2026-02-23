// 원격 측 QUIC 리스너 및 역방향 프록시
//
// Remote_Server는 원격 호스트에서 SSH를 통해 임시 실행된다.
// 단일 QUIC 연결을 수락하고, 인증 토큰 검증 후
// rsync 서버 프로세스를 spawn하여 양방향 데이터를 중계한다.

use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::error::ServerError;
use crate::quic::{build_server_endpoint, generate_self_signed_cert};
use crate::types::AuthToken;

/// 원격 서버: QUIC 리스닝 → 토큰 검증 → rsync 중계
pub struct RemoteServer {
    pub endpoint: quinn::Endpoint,
    pub port: u16,
    pub auth_token: AuthToken,
}

impl RemoteServer {
    /// 서버 시작: 자체 서명 인증서 생성, UDP 포트 바인딩, QUIC 리스닝 준비
    pub async fn start() -> Result<Self, ServerError> {
        let (cert, key) = generate_self_signed_cert()
            .map_err(|e| ServerError::StartFailed(format!("cert generation: {e}")))?;

        let tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .map_err(|e| ServerError::StartFailed(format!("TLS config: {e}")))?;

        let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let endpoint = build_server_endpoint(bind_addr, tls_config)
            .map_err(|e| ServerError::StartFailed(format!("endpoint: {e}")))?;

        let port = endpoint
            .local_addr()
            .map_err(|e| ServerError::StartFailed(format!("local_addr: {e}")))?
            .port();

        let auth_token = AuthToken::generate();

        Ok(Self {
            endpoint,
            port,
            auth_token,
        })
    }

    /// stdout으로 핸드셰이크 정보를 출력한다.
    /// 형식: `QUICSYNC_READY <port> <token>\n`
    pub fn emit_handshake(&self) {
        println!("QUICSYNC_READY {} {}", self.port, self.auth_token.to_hex());
    }

    /// 인증 토큰 검증: hex 문자열을 파싱하여 저장된 토큰과 비교
    fn verify_token(&self, token: &str) -> bool {
        match AuthToken::from_hex(token) {
            Ok(received) => self.auth_token.verify(&received),
            Err(_) => false,
        }
    }

    /// 단일 QUIC 연결을 수락하고 rsync 세션을 처리한다.
    ///
    /// 프로토콜:
    /// 1. QUIC 연결 수락
    /// 2. 첫 번째 양방향 스트림에서 토큰 라인 읽기 (`<64-hex>\n`)
    /// 3. 토큰 검증 (실패 시 연결 거부)
    /// 4. rsync args 라인 읽기 (스페이스 구분)
    /// 5. rsync 서버 프로세스 spawn
    /// 6. QUIC 스트림 ↔ rsync stdin/stdout 양방향 중계
    /// 7. rsync 종료 코드 반환
    pub async fn accept_and_serve(self) -> Result<i32, ServerError> {
        // 1. 단일 연결 수락
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| ServerError::StartFailed("endpoint closed".into()))?;

        let connection = incoming
            .await
            .map_err(|e| ServerError::StartFailed(format!("accept: {e}")))?;

        // 2. 첫 번째 양방향 스트림 수락
        let (mut send_stream, recv_stream) = connection
            .accept_bi()
            .await
            .map_err(|e| ServerError::RelayError(format!("accept_bi: {e}")))?;

        let mut reader = BufReader::new(recv_stream);

        // 3. 토큰 라인 읽기 및 검증
        let mut token_line = String::new();
        reader
            .read_line(&mut token_line)
            .await
            .map_err(|e| ServerError::RelayError(format!("read token: {e}")))?;

        let token = token_line.trim();
        if !self.verify_token(token) {
            eprintln!("[quicsync-server] invalid auth token received");
            connection.close(1u32.into(), b"invalid token");
            return Err(ServerError::InvalidToken);
        }

        // 4. rsync args 라인 읽기
        let mut args_line = String::new();
        reader
            .read_line(&mut args_line)
            .await
            .map_err(|e| ServerError::RelayError(format!("read args: {e}")))?;

        let rsync_args: Vec<&str> = args_line.trim().split_whitespace().collect();

        // 5. rsync 서버 프로세스 spawn (stdin/stdout 파이프)
        let mut child = Command::new("rsync")
            .args(&rsync_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ServerError::RsyncSpawnFailed(e.to_string()))?;

        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| ServerError::RsyncSpawnFailed("failed to capture stdin".into()))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| ServerError::RsyncSpawnFailed("failed to capture stdout".into()))?;

        // 6. 양방향 중계: QUIC recv → rsync stdin, rsync stdout → QUIC send
        let quic_to_rsync = async {
            tokio::io::copy(&mut reader, &mut child_stdin).await?;
            child_stdin.shutdown().await?;
            Ok::<_, std::io::Error>(())
        };

        let mut stdout_reader = tokio::io::BufReader::new(child_stdout);
        let rsync_to_quic = async {
            tokio::io::copy(&mut stdout_reader, &mut send_stream).await?;
            send_stream.finish().map_err(|e| std::io::Error::other(e))?;
            Ok::<_, std::io::Error>(())
        };

        // 양방향 동시 실행 — 어느 한쪽이 끝나면 다른 쪽도 종료
        tokio::select! {
            result = quic_to_rsync => {
                if let Err(e) = result {
                    eprintln!("[quicsync-server] quic→rsync relay error: {e}");
                }
            }
            result = rsync_to_quic => {
                if let Err(e) = result {
                    eprintln!("[quicsync-server] rsync→quic relay error: {e}");
                }
            }
        }

        // 7. rsync 종료 대기 및 종료 코드 반환
        let status = child
            .wait()
            .await
            .map_err(|e| ServerError::RelayError(format!("wait: {e}")))?;

        // 리소스 정리: endpoint 종료
        self.endpoint.close(0u32.into(), b"done");

        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::parse_handshake;

    /// 테스트용 더미 endpoint 생성 (tokio runtime 필요)
    fn test_endpoint() -> quinn::Endpoint {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (cert, key) = generate_self_signed_cert().unwrap();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        build_server_endpoint(addr, tls).unwrap()
    }

    fn test_server(token: AuthToken) -> RemoteServer {
        RemoteServer {
            port: 8080,
            auth_token: token,
            endpoint: test_endpoint(),
        }
    }

    #[tokio::test]
    async fn emit_parse_roundtrip() {
        let token = AuthToken::generate();
        let server = RemoteServer {
            port: 45231,
            auth_token: token.clone(),
            endpoint: test_endpoint(),
        };

        let handshake_line = format!(
            "QUICSYNC_READY {} {}",
            server.port,
            server.auth_token.to_hex()
        );

        let (parsed_port, parsed_token) = parse_handshake(&handshake_line).unwrap();
        assert_eq!(parsed_port, server.port);
        assert_eq!(parsed_token, server.auth_token.to_hex());
    }

    #[tokio::test]
    async fn verify_token_valid() {
        let token = AuthToken::generate();
        let server = test_server(token.clone());
        assert!(server.verify_token(&token.to_hex()));
    }

    #[tokio::test]
    async fn verify_token_invalid_hex() {
        let server = test_server(AuthToken::generate());
        assert!(!server.verify_token("not-valid-hex"));
    }

    #[tokio::test]
    async fn verify_token_wrong_token() {
        let server = test_server(AuthToken::generate());
        let other = AuthToken::generate();
        assert!(!server.verify_token(&other.to_hex()));
    }

    #[tokio::test]
    async fn start_binds_port_and_generates_token() {
        let server = RemoteServer::start().await.expect("server should start");
        assert!(server.port > 0);
        assert_eq!(server.auth_token.to_hex().len(), 64);
    }
}
