// Blake3 기반 데이터 무결성 검사

use crate::error::IntegrityError;

const HASH_LEN: usize = 32;

/// 데이터 청크의 Blake3 해시를 계산
pub fn compute_hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// 데이터와 기대 해시를 비교하여 검증
pub fn verify_hash(data: &[u8], expected: &[u8; 32]) -> bool {
    let actual = compute_hash(data);
    actual == *expected
}

/// 청크 프레이밍: [32바이트 Blake3 해시][데이터]
pub fn encode_chunk(data: &[u8]) -> Vec<u8> {
    let hash = compute_hash(data);
    let mut frame = Vec::with_capacity(HASH_LEN + data.len());
    frame.extend_from_slice(&hash);
    frame.extend_from_slice(data);
    frame
}

/// 청크 디프레이밍: 해시 추출 → 데이터 재해시 → 비교
pub fn decode_chunk(frame: &[u8]) -> Result<Vec<u8>, IntegrityError> {
    if frame.len() < HASH_LEN {
        return Err(IntegrityError::FrameTooShort(frame.len()));
    }

    let expected_hash: [u8; 32] = frame[..HASH_LEN]
        .try_into()
        .expect("slice length is HASH_LEN");
    let data = &frame[HASH_LEN..];
    let actual_hash = compute_hash(data);

    if expected_hash != actual_hash {
        return Err(IntegrityError::HashMismatch {
            expected: hex::encode(expected_hash),
            actual: hex::encode(actual_hash),
        });
    }

    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_roundtrip() {
        let data = b"";
        let frame = encode_chunk(data);
        assert_eq!(frame.len(), HASH_LEN);
        let decoded = decode_chunk(&frame).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn known_hash() {
        let data = b"hello";
        let hash = compute_hash(data);
        // blake3 해시는 결정적이므로 동일 입력 → 동일 해시
        assert_eq!(hash, compute_hash(data));
        assert!(verify_hash(data, &hash));
        assert!(!verify_hash(b"world", &hash));
    }

    #[test]
    fn roundtrip() {
        let data = b"quicsync integrity check test data";
        let frame = encode_chunk(data);
        assert_eq!(frame.len(), HASH_LEN + data.len());
        // 프레임 앞 32바이트가 원본 해시와 동일
        assert_eq!(&frame[..HASH_LEN], &compute_hash(data));
        let decoded = decode_chunk(&frame).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn frame_too_short() {
        let short = vec![0u8; 31];
        match decode_chunk(&short) {
            Err(IntegrityError::FrameTooShort(len)) => assert_eq!(len, 31),
            other => panic!("expected FrameTooShort, got {:?}", other),
        }
    }

    #[test]
    fn hash_mismatch_on_corruption() {
        let data = b"original data";
        let mut frame = encode_chunk(data);
        // 데이터 영역의 첫 바이트를 변조
        if frame.len() > HASH_LEN {
            frame[HASH_LEN] ^= 0xFF;
        }
        match decode_chunk(&frame) {
            Err(IntegrityError::HashMismatch { .. }) => {}
            other => panic!("expected HashMismatch, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    // Feature: quicsync-phase2-enhancements, Property 9: Blake3 청크 encode/decode 라운드트립
    // **Validates: Requirements 7.1, 7.2, 7.4, 7.6**
    proptest! {
        #[test]
        fn prop_encode_decode_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..65536)) {
            let frame = encode_chunk(&data);

            // 프레임 길이 = 32바이트 해시 + 데이터 길이
            prop_assert_eq!(frame.len(), HASH_LEN + data.len());

            // 프레임 앞 32바이트가 원본 데이터의 Blake3 해시와 동일
            let expected_hash = compute_hash(&data);
            prop_assert_eq!(&frame[..HASH_LEN], &expected_hash[..]);

            // decode 라운드트립: 원본 데이터 복원
            let decoded = decode_chunk(&frame).unwrap();
            prop_assert_eq!(decoded, data);
        }
    }

    // Feature: quicsync-phase2-enhancements, Property 10: Blake3 손상 감지
    // **Validates: Requirements 7.3**
    proptest! {
        #[test]
        fn prop_corruption_detected(
            data in proptest::collection::vec(any::<u8>(), 1..65536usize),
            corrupt_offset in any::<proptest::sample::Index>(),
            xor_val in 1u8..=255u8,
        ) {
            let mut frame = encode_chunk(&data);

            // 데이터 영역(32바이트 이후)에서 임의 1바이트 변조
            let data_region_len = frame.len() - HASH_LEN;
            let idx = HASH_LEN + corrupt_offset.index(data_region_len);
            frame[idx] ^= xor_val;

            // decode_chunk가 HashMismatch를 반환해야 함
            match decode_chunk(&frame) {
                Err(IntegrityError::HashMismatch { .. }) => {}
                other => prop_assert!(false, "expected HashMismatch, got {:?}", other),
            }
        }
    }
}
