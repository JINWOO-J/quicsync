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
        endpoint.set_default_client_config(build_client_config(
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

/// 인증서 DER 바이트에서 SHA-256 지문을 계산한다.
pub fn sha256_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert_der);
    let mut fp = [0u8; 32];
    fp.copy_from_slice(digest.as_ref());
    fp
}

/// 지문 바이트를 hex 문자열로 변환한다.
pub fn fingerprint_to_hex(fp: &[u8; 32]) -> String {
    hex::encode(fp)
}

/// hex 문자열을 지문 바이트로 변환한다.
pub fn fingerprint_from_hex(s: &str) -> Result<[u8; 32], FingerprintError> {
    let bytes = hex::decode(s).map_err(|e| FingerprintError::InvalidHex(e.to_string()))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| FingerprintError::InvalidLength(v.len()))?;
    Ok(arr)
}

/// 클라이언트용 quinn ClientConfig 생성 (BBR)
///
/// `fingerprint`이 Some이면 FingerprintVerifier로 인증서를 검증한다.
/// None이면 SkipServerVerification을 사용한다.
fn build_client_config(
    window_bytes: u64,
    fingerprint: Option<[u8; 32]>,
) -> Result<quinn::ClientConfig, QuicError> {
    let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = match fingerprint {
        Some(fp) => Arc::new(FingerprintVerifier { expected: fp }),
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

// --- 인증서 지문 검증 verifier ---

/// SSH 핸드셰이크로 교환된 인증서 지문을 상수 시간 비교로 검증한다.
#[derive(Debug)]
struct FingerprintVerifier {
    expected: [u8; 32],
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
        // 상수 시간 비교: 모든 바이트를 항상 비교하여 타이밍 공격 방지
        let mut diff = 0u8;
        for (a, b) in actual.iter().zip(self.expected.iter()) {
            diff |= a ^ b;
        }
        if diff == 0 {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("fingerprint mismatch".into()))
        }
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
    use proptest::prelude::*;

    // --- window_bytes_from_env 테스트 ---

    #[test]
    fn window_bytes_default_when_unset() {
        // 환경변수가 없을 때 기본값 64MB
        unsafe { std::env::remove_var("QUICSYNC_WINDOW") };
        assert_eq!(window_bytes_from_env(), DEFAULT_WINDOW_BYTES);
    }

    #[test]
    fn window_bytes_from_env_valid() {
        unsafe { std::env::set_var("QUICSYNC_WINDOW", "128") };
        let result = window_bytes_from_env();
        unsafe { std::env::remove_var("QUICSYNC_WINDOW") };
        assert_eq!(result, 128 * 1024 * 1024);
    }

    #[test]
    fn window_bytes_from_env_invalid_falls_back() {
        unsafe { std::env::set_var("QUICSYNC_WINDOW", "not_a_number") };
        let result = window_bytes_from_env();
        unsafe { std::env::remove_var("QUICSYNC_WINDOW") };
        assert_eq!(result, DEFAULT_WINDOW_BYTES);
    }

    // --- bbr_transport_config 테스트 ---

    #[test]
    fn bbr_transport_config_builds_with_various_windows() {
        for &window in &[1024 * 1024, 32 * 1024 * 1024, 128 * 1024 * 1024] {
            let config = bbr_transport_config(window);
            // Arc<TransportConfig>가 생성되면 성공
            let _ = config;
        }
    }

    #[test]
    fn build_client_config_various_windows() {
        for &window in &[1024 * 1024, DEFAULT_WINDOW_BYTES, 256 * 1024 * 1024] {
            build_client_config(window, None).expect(&format!("should build with window {window}"));
        }
    }

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
        build_client_config(DEFAULT_WINDOW_BYTES, None).expect("client config should build");
    }

    #[test]
    fn build_client_config_with_fingerprint() {
        let fp = [0xABu8; 32];
        build_client_config(DEFAULT_WINDOW_BYTES, Some(fp))
            .expect("client config with fp should build");
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

    // --- fingerprint 테스트 ---

    #[test]
    fn sha256_fingerprint_deterministic() {
        let data = b"test certificate";
        let fp1 = sha256_fingerprint(data);
        let fp2 = sha256_fingerprint(data);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn sha256_fingerprint_different_input() {
        let fp1 = sha256_fingerprint(b"cert A");
        let fp2 = sha256_fingerprint(b"cert B");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn fingerprint_hex_roundtrip() {
        let fp = sha256_fingerprint(b"test cert");
        let hex_str = fingerprint_to_hex(&fp);
        let restored = fingerprint_from_hex(&hex_str).unwrap();
        assert_eq!(fp, restored);
    }

    #[test]
    fn fingerprint_from_hex_invalid() {
        assert!(fingerprint_from_hex("zzzz").is_err());
    }

    #[test]
    fn fingerprint_from_hex_wrong_length() {
        assert!(matches!(
            fingerprint_from_hex("aabb"),
            Err(FingerprintError::InvalidLength(2))
        ));
    }

    #[test]
    fn fingerprint_cert_roundtrip() {
        let (cert, _key) = generate_self_signed_cert().unwrap();
        let fp = sha256_fingerprint(cert.as_ref());
        let hex_str = fingerprint_to_hex(&fp);
        assert_eq!(hex_str.len(), 64);
        let restored = fingerprint_from_hex(&hex_str).unwrap();
        assert_eq!(fp, restored);
    }

    // Property 7: 지문 hex 라운드트립
    // Property 8: 상수 시간 비교
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_fingerprint_hex_roundtrip(bytes in prop::array::uniform32(any::<u8>())) {
            let hex_str = fingerprint_to_hex(&bytes);
            prop_assert_eq!(hex_str.len(), 64);
            let restored = fingerprint_from_hex(&hex_str).expect("valid hex should parse");
            prop_assert_eq!(bytes, restored);
        }

        #[test]
        fn prop_fingerprint_constant_time_comparison(
            a in prop::array::uniform32(any::<u8>()),
            b in prop::array::uniform32(any::<u8>()),
        ) {
            // 상수 시간 비교가 올바른 결과를 반환하는지 검증
            let mut diff = 0u8;
            for (x, y) in a.iter().zip(b.iter()) {
                diff |= x ^ y;
            }
            let ct_equal = diff == 0;
            let naive_equal = a == b;
            prop_assert_eq!(ct_equal, naive_equal);
        }
    }
}
