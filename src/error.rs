// 오류 타입 계층

use std::fmt;
use thiserror::Error;

/// quicsync 최상위 오류 타입
#[derive(Debug, Error)]
pub enum QuicsyncError {
    #[error("CLI error: {0}")]
    Cli(#[from] CliError),

    #[error("SSH error: {0}")]
    Ssh(#[from] SshError),

    #[error("Proxy error: {0}")]
    Proxy(#[from] ProxyError),

    #[error("Buffer error: {0}")]
    Buffer(#[from] BufferError),

    #[error("QUIC error: {0}")]
    Quic(#[from] QuicError),

    #[error("Server error: {0}")]
    Server(#[from] ServerError),

    #[error("Rsync error: {0}")]
    Rsync(#[from] RsyncError),

    #[error("Session error: {0}")]
    Session(#[from] SessionError),
}

/// CLI 인수 파싱 오류
#[derive(Debug, Error)]
pub enum CliError {
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("invalid remote path: {0}")]
    InvalidRemotePath(String),

    #[error("both paths are local")]
    BothLocal,

    #[error("both paths are remote")]
    BothRemote,
}

/// SSH 관련 오류
#[derive(Debug, Error)]
pub enum SshError {
    #[error("SSH connection failed: {0}")]
    ConnectionFailed(String),

    #[error("binary not found on remote: {0}")]
    BinaryNotFound(String),

    #[error("handshake timed out")]
    HandshakeTimeout,

    #[error("handshake parse failed: {0}")]
    HandshakeParseFailed(String),
}

/// TCP 프록시 오류
#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("bind failed: {0}")]
    BindFailed(String),

    #[error("relay error: {0}")]
    RelayError(String),
}

/// 버퍼 관련 오류
#[derive(Debug, Error)]
pub enum BufferError {
    #[error("invalid buffer size: {0}")]
    InvalidSize(String),

    #[error("buffer full")]
    Full,
}

/// QUIC 터널 오류
#[derive(Debug, Error)]
pub enum QuicError {
    #[error("QUIC connection failed: {0}")]
    ConnectionFailed(String),

    #[error("QUIC stream error: {0}")]
    StreamError(String),

    #[error("QUIC connection timed out")]
    Timeout,

    #[error("TLS error: {0}")]
    TlsError(String),
}

/// 원격 서버 오류
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("server start failed: {0}")]
    StartFailed(String),

    #[error("invalid auth token")]
    InvalidToken,

    #[error("rsync server spawn failed: {0}")]
    RsyncSpawnFailed(String),

    #[error("relay error: {0}")]
    RelayError(String),
}

/// rsync 자식 프로세스 오류
#[derive(Debug, Error)]
pub enum RsyncError {
    #[error("rsync spawn failed: {0}")]
    SpawnFailed(String),

    #[error("rsync exited with code {0}")]
    ExitCode(i32),

    #[error("rsync terminated by signal")]
    Signal,
}

/// 세션 관리 오류
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session init failed: {0}")]
    InitFailed(String),

    #[error("signal received: {0}")]
    SignalReceived(String),

    #[error("component error: {0}")]
    ComponentError(String),
}

/// AuthToken 파싱 오류
#[derive(Debug, Error)]
pub enum TokenError {
    #[error("invalid hex string: {0}")]
    InvalidHex(String),

    #[error("invalid token length: expected 32 bytes, got {0}")]
    InvalidLength(usize),
}

/// Ring_Buffer가 가득 찼을 때 반환하는 오류
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferFull;

impl fmt::Display for BufferFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "buffer full")
    }
}

impl std::error::Error for BufferFull {}
