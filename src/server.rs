// 원격 측 QUIC 리스너 및 역방향 프록시
//
// Remote_Server는 원격 호스트에서 SSH를 통해 임시 실행된다.
// 단일 QUIC 연결을 수락하고, 인증 토큰 검증 후
// rsync 서버 프로세스를 spawn하여 양방향 데이터를 중계한다.

use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::error::ServerError;
use crate::quic::{
    build_server_endpoint, fingerprint_to_hex, generate_self_signed_cert, sha256_fingerprint,
};
use crate::types::AuthToken;

/// 원격 서버: QUIC 리스닝 → 토큰 검증 → rsync 중계
pub struct RemoteServer {
    pub endpoint: quinn::Endpoint,
    pub port: u16,
    pub auth_token: AuthToken,
    pub fingerprint: String,
}

impl RemoteServer {
    /// 서버 시작: 자체 서명 인증서 생성, UDP 포트 바인딩, QUIC 리스닝 준비
    pub async fn start() -> Result<Self, ServerError> {
        let (cert, key) = generate_self_signed_cert()
            .map_err(|e| ServerError::StartFailed(format!("cert generation: {e}")))?;

        let fp = sha256_fingerprint(cert.as_ref());
        let fingerprint = fingerprint_to_hex(&fp);

        let tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .map_err(|e| ServerError::StartFailed(format!("TLS config: {e}")))?;

        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let endpoint =
            build_server_endpoint(bind_addr, tls_config, crate::quic::window_bytes_from_env())
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
            fingerprint,
        })
    }

    /// stdout으로 핸드셰이크 정보를 출력한다.
    /// 형식: `QUICSYNC_READY <port> <token> <fingerprint>\n`
    pub fn emit_handshake(&self) {
        println!(
            "QUICSYNC_READY {} {} {}",
            self.port,
            self.auth_token.to_hex(),
            self.fingerprint
        );
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
        // 1. 단일 연결 수락 (SSH stdin EOF 감지 시 자동 종료)
        let stdin_closed = async {
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 1];
            // SSH가 끊어지면 stdin이 EOF를 반환한다
            let _ = tokio::io::AsyncReadExt::read(&mut stdin, &mut buf).await;
        };

        let incoming = tokio::select! {
            incoming = self.endpoint.accept() => {
                incoming.ok_or_else(|| ServerError::StartFailed("endpoint closed".into()))?
            }
            _ = stdin_closed => {
                // SSH 연결 끊김 → 정리 후 종료
                self.endpoint.close(0u32.into(), b"ssh disconnected");
                return Ok(0);
            }
        };

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

        // 클라이언트(--connect)는 rsync args를 JSON 배열로 전송한다(공백/quote 보존).
        // 서버도 동일하게 JSON으로 파싱해야 인자가 올바로 복원된다.
        let rsync_args = parse_rsync_args(&args_line)?;

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
        //
        // rsync_to_quic (stdout→QUIC) 완료를 기준으로 세션을 정리한다.
        // rsync stdout EOF = rsync가 보낼 데이터를 모두 보냄 → finish() 호출.
        //
        // quic_to_rsync (QUIC→stdin)는 클라이언트 finish()에 의존하는데,
        // 클라이언트는 서버 finish()를 기다리므로 순환 대기가 발생한다.
        // 따라서 rsync_to_quic 완료 후 quic_to_rsync를 abort한다.
        let quic_to_rsync = tokio::spawn(async move {
            let r = tokio::io::copy(&mut reader, &mut child_stdin).await;
            let _ = child_stdin.shutdown().await;
            r
        });

        let mut stdout_reader = tokio::io::BufReader::new(child_stdout);
        let rsync_to_quic = tokio::spawn(async move {
            let r = tokio::io::copy(&mut stdout_reader, &mut send_stream).await;
            if r.is_ok() {
                let _ = send_stream.finish();
            }
            r
        });

        // rsync stdout EOF → finish() 완료를 기다린다.
        // 이후 quic_to_rsync를 abort하여 순환 대기를 끊는다.
        let _ = rsync_to_quic.await;
        quic_to_rsync.abort();

        // 7. rsync 종료 대기
        let status = child
            .wait()
            .await
            .map_err(|e| ServerError::RelayError(format!("wait: {e}")))?;

        // 클라이언트가 모든 데이터를 수신한 후 연결을 닫을 때까지 대기.
        // send_stream.finish() 직후 endpoint.close()를 호출하면
        // QUIC CONNECTION_CLOSE가 스트림 데이터보다 먼저 도착하여
        // 클라이언트가 데이터를 유실할 수 있다.
        tokio::select! {
            _ = connection.closed() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                eprintln!("[quicsync-server] timeout waiting for client to close connection");
            }
        }

        // 리소스 정리: endpoint 종료
        self.endpoint.close(0u32.into(), b"done");

        Ok(status.code().unwrap_or(1))
    }
}

/// 클라이언트(--connect)가 보낸 JSON 배열 형식의 rsync 서버 인자를 파싱한다.
/// `main.rs`의 run_connect가 `serde_json::to_string`으로 인코딩한 것과 짝을 이룬다.
fn parse_rsync_args(line: &str) -> Result<Vec<String>, ServerError> {
    serde_json::from_str(line.trim())
        .map_err(|e| ServerError::RelayError(format!("parse rsync args: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::parse_handshake;

    #[test]
    fn parse_rsync_args_json_roundtrip() {
        // main.rs run_connect가 보내는 형식
        let line = r#"["--server","-vve.LsfxCIvu","--stats",".","/tmp/qs-base/"]"#;
        let args = parse_rsync_args(line).unwrap();
        assert_eq!(
            args,
            vec!["--server", "-vve.LsfxCIvu", "--stats", ".", "/tmp/qs-base/"]
        );
    }

    #[test]
    fn parse_rsync_args_preserves_spaces() {
        let line = r#"["--server",".","/remote/path with spaces"]"#;
        let args = parse_rsync_args(line).unwrap();
        assert_eq!(args[2], "/remote/path with spaces");
    }

    #[test]
    fn parse_rsync_args_trailing_newline() {
        let line = "[\"--server\",\".\",\"/dst\"]\n";
        let args = parse_rsync_args(line).unwrap();
        assert_eq!(args, vec!["--server", ".", "/dst"]);
    }

    #[test]
    fn parse_rsync_args_invalid_json_errors() {
        assert!(parse_rsync_args("--server . /dst").is_err());
    }

    /// 테스트용 더미 endpoint 생성 (tokio runtime 필요)
    fn test_endpoint() -> quinn::Endpoint {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (cert, key) = generate_self_signed_cert().unwrap();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        build_server_endpoint(addr, tls, 64 * 1024 * 1024).unwrap()
    }

    fn test_server(token: AuthToken) -> RemoteServer {
        let (cert, _key) = generate_self_signed_cert().unwrap();
        let fp = crate::quic::sha256_fingerprint(cert.as_ref());
        let fingerprint = crate::quic::fingerprint_to_hex(&fp);
        RemoteServer {
            port: 8080,
            auth_token: token,
            endpoint: test_endpoint(),
            fingerprint,
        }
    }

    #[tokio::test]
    async fn emit_parse_roundtrip() {
        let token = AuthToken::generate();
        let (cert, _key) = generate_self_signed_cert().unwrap();
        let fp = crate::quic::sha256_fingerprint(cert.as_ref());
        let fingerprint = crate::quic::fingerprint_to_hex(&fp);
        let server = RemoteServer {
            port: 45231,
            auth_token: token.clone(),
            endpoint: test_endpoint(),
            fingerprint: fingerprint.clone(),
        };

        let handshake_line = format!(
            "QUICSYNC_READY {} {} {}",
            server.port,
            server.auth_token.to_hex(),
            server.fingerprint,
        );

        let (parsed_port, parsed_token, parsed_fp) = parse_handshake(&handshake_line).unwrap();
        assert_eq!(parsed_port, server.port);
        assert_eq!(parsed_token, server.auth_token.to_hex());
        assert_eq!(parsed_fp, Some(fingerprint));
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
        assert_eq!(server.fingerprint.len(), 64);
        assert!(server.fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
