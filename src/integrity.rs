// Blake3 기반 데이터 무결성 검증

use crate::error::IntegrityError;

/// Blake3 해시 크기 (32 bytes)
const HASH_SIZE: usize = 32;

/// Blake3 해시 계산
pub fn compute_hash(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

/// Blake3 해시 검증
pub fn verify_hash(data: &[u8], expected: &[u8; 32]) -> bool {
    let actual = compute_hash(data);
    actual == *expected
}

/// 데이터를 [32-byte Blake3 해시][data] 형태로 인코딩
pub fn encode_chunk(data: &[u8]) -> Vec<u8> {
    let hash = compute_hash(data);
    let mut frame = Vec::with_capacity(HASH_SIZE + data.len());
    frame.extend_from_slice(&hash);
    frame.extend_from_slice(data);
    frame
}

/// [32-byte Blake3 해시][data] 프레임에서 해시를 검증하고 데이터를 반환
pub fn decode_chunk(frame: &[u8]) -> Result<Vec<u8>, IntegrityError> {
    if frame.len() < HASH_SIZE {
        return Err(IntegrityError::FrameTooShort(frame.len()));
    }

    let (hash_bytes, data) = frame.split_at(HASH_SIZE);
    let expected: [u8; 32] = hash_bytes.try_into().unwrap();

    if !verify_hash(data, &expected) {
        return Err(IntegrityError::HashMismatch);
    }

    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn compute_hash_deterministic() {
        let h1 = compute_hash(b"hello");
        let h2 = compute_hash(b"hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_hash_different_input() {
        let h1 = compute_hash(b"hello");
        let h2 = compute_hash(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn verify_hash_valid() {
        let data = b"test data";
        let hash = compute_hash(data);
        assert!(verify_hash(data, &hash));
    }

    #[test]
    fn verify_hash_invalid() {
        let data = b"test data";
        let hash = [0u8; 32];
        assert!(!verify_hash(data, &hash));
    }

    #[test]
    fn encode_decode_roundtrip() {
        let data = b"hello world";
        let frame = encode_chunk(data);
        let decoded = decode_chunk(&frame).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_frame_too_short() {
        let frame = [0u8; 31];
        let result = decode_chunk(&frame);
        assert!(matches!(result, Err(IntegrityError::FrameTooShort(31))));
    }

    #[test]
    fn decode_corrupted_data() {
        let data = b"hello world";
        let mut frame = encode_chunk(data);
        // 데이터 부분을 변조
        if let Some(last) = frame.last_mut() {
            *last ^= 0xFF;
        }
        let result = decode_chunk(&frame);
        assert!(matches!(result, Err(IntegrityError::HashMismatch)));
    }

    #[test]
    fn decode_empty_data() {
        // 32바이트 해시 + 0바이트 데이터
        let frame = encode_chunk(b"");
        let decoded = decode_chunk(&frame).unwrap();
        assert!(decoded.is_empty());
    }

    // Property 9: Blake3 encode/decode 라운드트립
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_encode_decode_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..=4096)) {
            let frame = encode_chunk(&data);
            prop_assert_eq!(frame.len(), 32 + data.len());
            let decoded = decode_chunk(&frame).expect("valid frame should decode");
            prop_assert_eq!(decoded, data);
        }
    }

    // Property 10: 1바이트 변조 감지
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_single_byte_corruption_detected(
            data in proptest::collection::vec(any::<u8>(), 1..=4096),
            pos in any::<proptest::sample::Index>(),
            flip in 1u8..=255,
        ) {
            let frame = encode_chunk(&data);
            let mut corrupted = frame.clone();
            let idx = pos.index(corrupted.len());
            corrupted[idx] ^= flip;
            let result = decode_chunk(&corrupted);
            prop_assert!(result.is_err(), "corruption at index {idx} should be detected");
        }
    }
}
