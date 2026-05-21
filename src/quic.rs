// quinn 기반 QUIC 연결 수립 및 스트림 관리

use std::net::SocketAddr;
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{RecvStream, SendStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};

use crate::error::{FingerprintError, QuicError};

/// 기본 QUIC 윈도우 크기: 64MB
const DEFAULT_WINDOW_BYTES: u64 = 64 * 1024 * 1024;

/// QUIC 클라이언트 설정
pub struct QuicClientCfg {
    pub remote_addr: SocketAddr,
    pub auth_token: String,
    pub server_name: String,
    pub window_bytes: u64,
    /// SSH 핸드셰이크로 수신한 인증서 지문 (Some이면 핀닝 검증, None이면 기존 동작)
    pub fingerprint: Option<[u8; 32]>,
}

/// QUIC 연결 래퍼
pub struct QuicTunnel {
    connection: quinn::Connection,
}

impl QuicTunnel {
    /// QUIC 연결 수립 (클라이언트 측)
    pub async fn connect(config: QuicClientCfg) -> Result<Self, QuicError> {
        let mut endpoint = build_client_endpoint()?;
        endpoint.set_default_client_config(build_client_config_with_fingerprint(
            config.window_bytes,
            config.fingerprint,
        )?);

        let connection = endpoint
            .connect(config.remote_addr, &config.server_name)
            .map_err(|e| QuicError::ConnectionFailed(e.to_string()))?
            .await
            .map_err(|e| QuicError::ConnectionFailed(e.to_string()))?;

        Ok(Self { connection })
    }

    /// 양방향 스트림 열기
    pub async fn open_bi_stream(&self) -> Result<(SendStream, RecvStream), QuicError> {
        self.connection
            .open_bi()
            .await
            .map_err(|e| QuicError::StreamError(e.to_string()))
    }

    /// 연결 종료
    pub async fn close(self) -> Result<(), QuicError> {
        self.connection.close(0u32.into(), b"done");
        Ok(())
    }
}

// --- Endpoint 구성 ---

/// 자체 서명 인증서 생성 (일회성 세션용)
pub fn generate_self_signed_cert()
-> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), QuicError> {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .map_err(|e| QuicError::TlsError(e.to_string()))?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

    Ok((cert_der, key_der))
}

/// BBR 혼잡 제어 + 높은 BDP 네트워크를 위한 TransportConfig 생성
///
/// quinn 기본 윈도우(receive ~1.5MB, stream ~1MB)는 높은 RTT에서 병목이 된다.
/// BDP 예시: 1Gbps × 500ms RTT = 62.5MB → 기본 64MB.
/// `window_bytes`로 환경에 맞게 조절할 수 있다.
fn bbr_transport_config(window_bytes: u64) -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));

    let window = quinn::VarInt::from_u32(window_bytes.min(u32::MAX as u64) as u32);
    transport.receive_window(window);
    transport.stream_receive_window(window);
    transport.send_window(window_bytes);

    Arc::new(transport)
}

/// 환경변수 `QUICSYNC_WINDOW`에서 윈도우 크기(MB)를 읽는다.
/// 미설정이면 기본 64MB를 반환한다.
pub fn window_bytes_from_env() -> u64 {
    std::env::var("QUICSYNC_WINDOW")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|mb| mb * 1024 * 1024)
        .unwrap_or(DEFAULT_WINDOW_BYTES)
}

/// 클라이언트용 quinn ClientConfig 생성 (자체 서명 인증서 허용, BBR)
#[cfg(test)]
fn build_client_config(window_bytes: u64) -> Result<quinn::ClientConfig, QuicError> {
    let rustls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let quic_config = QuicClientConfig::try_from(rustls_config)
        .map_err(|e| QuicError::TlsError(e.to_string()))?;

    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
    client_config.transport_config(bbr_transport_config(window_bytes));

    Ok(client_config)
}

/// 지문 핀닝이 적용된 클라이언트 ClientConfig 생성
/// fingerprint가 Some이면 FingerprintVerifier, None이면 SkipServerVerification 사용
pub fn build_client_config_with_fingerprint(
    window_bytes: u64,
    fingerprint: Option<[u8; 32]>,
) -> Result<quinn::ClientConfig, QuicError> {
    let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = match fingerprint {
        Some(fp) => Arc::new(FingerprintVerifier {
            expected_fingerprint: fp,
        }),
        None => Arc::new(SkipServerVerification),
    };

    let rustls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    let quic_config = QuicClientConfig::try_from(rustls_config)
        .map_err(|e| QuicError::TlsError(e.to_string()))?;

    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
    client_config.transport_config(bbr_transport_config(window_bytes));

    Ok(client_config)
}

/// 클라이언트 Endpoint 생성 (0.0.0.0:0 바인딩)
pub fn build_client_endpoint() -> Result<quinn::Endpoint, QuicError> {
    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    quinn::Endpoint::client(addr).map_err(|e| QuicError::ConnectionFailed(e.to_string()))
}

/// 서버 Endpoint 생성 (BBR 혼잡 제어 적용)
pub fn build_server_endpoint(
    bind_addr: SocketAddr,
    tls_config: rustls::ServerConfig,
    window_bytes: u64,
) -> Result<quinn::Endpoint, QuicError> {
    let quic_config =
        QuicServerConfig::try_from(tls_config).map_err(|e| QuicError::TlsError(e.to_string()))?;

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
    server_config.transport = bbr_transport_config(window_bytes);

    quinn::Endpoint::server(server_config, bind_addr)
        .map_err(|e| QuicError::ConnectionFailed(e.to_string()))
}

// --- 인증서 지문 계산 및 검증 ---

/// 인증서 DER 데이터의 SHA-256 지문 계산
pub fn sha256_fingerprint(cert: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert);
    let mut fp = [0u8; 32];
    fp.copy_from_slice(digest.as_ref());
    fp
}

/// SHA-256 지문을 hex 문자열(64자)로 변환
pub fn fingerprint_to_hex(fp: &[u8; 32]) -> String {
    hex::encode(fp)
}

/// hex 문자열을 SHA-256 지문으로 변환
pub fn fingerprint_from_hex(s: &str) -> Result<[u8; 32], FingerprintError> {
    let bytes = hex::decode(s).map_err(|e| FingerprintError::InvalidHex(e.to_string()))?;
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| FingerprintError::InvalidLength(len))
}

/// 인증서 지문 기반 검증기
/// SSH 핸드셰이크로 수신한 지문과 서버 인증서 지문을 상수 시간 비교
#[derive(Debug)]
struct FingerprintVerifier {
    expected_fingerprint: [u8; 32],
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual = sha256_fingerprint(end_entity.as_ref());
        // Constant-time comparison: XOR all bytes, OR into accumulator
        let mismatch = actual
            .iter()
            .zip(self.expected_fingerprint.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if mismatch != 0 {
            return Err(rustls::Error::General(format!(
                "certificate fingerprint mismatch: expected {}, actual {}",
                fingerprint_to_hex(&self.expected_fingerprint),
                fingerprint_to_hex(&actual),
            )));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// --- 자체 서명 인증서 허용을 위한 커스텀 verifier ---
// SSH 채널로 교환된 인증 토큰으로 상호 인증하므로, 인증서 신뢰 체인은 불필요하다.

#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_self_signed_cert_succeeds() {
        let (cert, key) = generate_self_signed_cert().expect("cert generation should succeed");
        assert!(!cert.is_empty());
        match &key {
            PrivateKeyDer::Pkcs8(k) => assert!(!k.secret_pkcs8_der().is_empty()),
            _ => panic!("expected PKCS8 key"),
        }
    }

    #[test]
    fn build_client_config_succeeds() {
        build_client_config(DEFAULT_WINDOW_BYTES).expect("client config should build");
    }

    #[tokio::test]
    async fn build_client_endpoint_succeeds() {
        build_client_endpoint().expect("client endpoint should bind");
    }

    #[tokio::test]
    async fn build_server_endpoint_succeeds() {
        let (cert, key) = generate_self_signed_cert().unwrap();
        let tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .expect("server TLS config should build");

        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        build_server_endpoint(addr, tls_config, DEFAULT_WINDOW_BYTES)
            .expect("server endpoint should bind");
    }

    #[test]
    fn sha256_fingerprint_returns_32_bytes() {
        let data = b"test certificate data";
        let fp = sha256_fingerprint(data);
        assert_eq!(fp.len(), 32);
    }

    #[test]
    fn sha256_fingerprint_deterministic() {
        let data = b"same input";
        assert_eq!(sha256_fingerprint(data), sha256_fingerprint(data));
    }

    #[test]
    fn fingerprint_hex_roundtrip() {
        let fp = sha256_fingerprint(b"test");
        let hex_str = fingerprint_to_hex(&fp);
        assert_eq!(hex_str.len(), 64);
        let decoded = fingerprint_from_hex(&hex_str).unwrap();
        assert_eq!(decoded, fp);
    }

    #[test]
    fn fingerprint_from_hex_invalid_hex() {
        let result = fingerprint_from_hex("not-valid-hex!");
        assert!(matches!(result, Err(FingerprintError::InvalidHex(_))));
    }

    #[test]
    fn fingerprint_from_hex_wrong_length() {
        let result = fingerprint_from_hex("aabb");
        assert!(matches!(result, Err(FingerprintError::InvalidLength(2))));
    }

    #[test]
    fn build_client_config_with_fingerprint_none() {
        build_client_config_with_fingerprint(DEFAULT_WINDOW_BYTES, None)
            .expect("should build with no fingerprint");
    }

    #[test]
    fn build_client_config_with_fingerprint_some() {
        let fp = [0xABu8; 32];
        build_client_config_with_fingerprint(DEFAULT_WINDOW_BYTES, Some(fp))
            .expect("should build with fingerprint");
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    /// Replicate the constant-time XOR fold comparison used by FingerprintVerifier
    fn verify_fingerprint(expected: &[u8; 32], actual: &[u8; 32]) -> bool {
        let mismatch = actual
            .iter()
            .zip(expected.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        mismatch == 0
    }

    // Feature: quicsync-phase2-enhancements, Property 7: 인증서 지문 계산 및 hex 인코딩
    // **Validates: Requirements 6.2, 6.5**
    proptest! {
        #[test]
        fn prop_sha256_fingerprint_returns_32_bytes(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
            let fp = sha256_fingerprint(&data);
            prop_assert_eq!(fp.len(), 32);
        }

        #[test]
        fn prop_fingerprint_hex_roundtrip(fp in proptest::collection::vec(any::<u8>(), 32..=32)) {
            let arr: [u8; 32] = fp.try_into().unwrap();
            let hex_str = fingerprint_to_hex(&arr);
            prop_assert_eq!(hex_str.len(), 64);
            let decoded = fingerprint_from_hex(&hex_str).unwrap();
            prop_assert_eq!(decoded, arr);
        }
    }

    // Feature: quicsync-phase2-enhancements, Property 8: 인증서 지문 검증
    // **Validates: Requirements 6.3, 6.4**
    proptest! {
        #[test]
        fn prop_same_fingerprint_verifies(fp in proptest::collection::vec(any::<u8>(), 32..=32)) {
            let arr: [u8; 32] = fp.try_into().unwrap();
            prop_assert!(verify_fingerprint(&arr, &arr));
        }

        #[test]
        fn prop_different_fingerprint_rejects(
            a in proptest::collection::vec(any::<u8>(), 32..=32),
            b in proptest::collection::vec(any::<u8>(), 32..=32),
        ) {
            let arr_a: [u8; 32] = a.try_into().unwrap();
            let arr_b: [u8; 32] = b.try_into().unwrap();
            if arr_a != arr_b {
                prop_assert!(!verify_fingerprint(&arr_a, &arr_b));
            }
        }
    }
}
