// Ring Buffer 기반 무상태 버퍼링 및 backpressure 제어

use std::env;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use quinn::{RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::error::{BufferError, BufferFull};
use crate::metrics::TransferMetrics;

/// 기본 버퍼 크기: 256MB
const DEFAULT_BUFFER_SIZE: usize = 256 * 1024 * 1024;

/// 고정 크기 순환 메모리 큐
pub struct RingBuffer {
    data: Vec<u8>,
    head: usize, // 읽기 위치
    tail: usize, // 쓰기 위치
    len: usize,
    capacity: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0u8; capacity],
            head: 0,
            tail: 0,
            len: 0,
            capacity,
        }
    }

    /// 데이터를 버퍼에 기록한다.
    /// 공간이 부족하면 가능한 만큼 기록하고 기록한 바이트 수를 반환한다.
    /// 버퍼가 완전히 가득 차면 BufferFull 오류를 반환한다.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, BufferFull> {
        if self.len == self.capacity {
            return Err(BufferFull);
        }

        let available = self.capacity - self.len;
        let to_write = data.len().min(available);

        for &byte in &data[..to_write] {
            self.data[self.tail] = byte;
            self.tail = (self.tail + 1) % self.capacity;
        }
        self.len += to_write;

        Ok(to_write)
    }

    /// 버퍼에서 데이터를 읽어 buf에 채운다. 읽은 바이트 수를 반환한다.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let to_read = buf.len().min(self.len);

        for slot in buf.iter_mut().take(to_read) {
            *slot = self.data[self.head];
            self.head = (self.head + 1) % self.capacity;
        }
        self.len -= to_read;

        to_read
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len == self.capacity
    }

    /// 남은 쓰기 가능 바이트 수
    pub fn available(&self) -> usize {
        self.capacity - self.len
    }
}

/// TCP_Proxy와 QUIC_Tunnel 사이의 버퍼링 레이어
pub struct BufferLayer {
    buffer: RingBuffer,
    backpressure_threshold: usize,
}

impl BufferLayer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: RingBuffer::new(capacity),
            backpressure_threshold: capacity, // 100% → backpressure
        }
    }

    /// 환경변수 `QUICSYNC_BUFFER_SIZE`에서 버퍼 크기를 읽어 생성한다.
    /// 미설정이면 기본 256MB를 사용한다.
    pub fn from_env() -> Self {
        let raw = env::var("QUICSYNC_BUFFER_SIZE").ok();
        Self::new(parse_buffer_size(raw.as_deref()))
    }

    pub fn is_backpressure_active(&self) -> bool {
        self.buffer.len() >= self.backpressure_threshold
    }

    /// TCP → Buffer → QUIC 방향 비동기 중계
    ///
    /// tcp_rx 채널에서 데이터를 수신하여 QUIC SendStream으로 전달한다.
    /// Backpressure는 bounded mpsc 채널이 자연스럽게 제공한다:
    /// QUIC 전송이 느려지면 이 함수가 tcp_rx.recv()를 호출하지 않게 되고,
    /// 채널이 가득 차면 TcpProxy의 send().await가 블로킹된다.
    ///
    /// keep-alive: 30초간 데이터가 없으면 빈 바이트를 전송하여 QUIC 연결을 유지한다.
    pub async fn relay_forward(
        &self,
        mut tcp_rx: mpsc::Receiver<Bytes>,
        mut quic_tx: SendStream,
        metrics: Arc<TransferMetrics>,
    ) -> Result<(), BufferError> {
        let keepalive_interval = Duration::from_secs(30);
        let mut total_bytes = 0u64;

        loop {
            tokio::select! {
                maybe_data = tcp_rx.recv() => {
                    match maybe_data {
                        Some(data) => {
                            total_bytes += data.len() as u64;
                            metrics
                                .bytes_transferred
                                .fetch_add(data.len() as u64, Ordering::Relaxed);
                            quic_tx.write_all(&data).await.map_err(|e| {
                                BufferError::InvalidSize(format!("quic write: {e}"))
                            })?;
                        }
                        None => {
                            // tcp_rx 종료 — 모든 데이터 전송 완료
                            tracing::debug!("relay_forward: tcp_rx closed after {total_bytes} bytes, calling quic_tx.finish()");
                            quic_tx.finish().map_err(|e| {
                                BufferError::InvalidSize(format!("quic finish: {e}"))
                            })?;
                            return Ok(());
                        }
                    }
                }
                _ = tokio::time::sleep(keepalive_interval) => {
                    // Req 4.7: QUIC 유휴 + 버퍼 비어있음 → keep-alive
                    quic_tx.write_all(&[]).await.map_err(|e| {
                        BufferError::InvalidSize(format!("keepalive: {e}"))
                    })?;
                }
            }
        }
    }

    /// QUIC → Buffer → TCP 방향 비동기 중계 (역방향)
    ///
    /// QUIC RecvStream에서 데이터를 읽어 tcp_tx 채널로 전달한다.
    /// 이 방향은 QUIC → localhost TCP이므로 속도 차이가 적어
    /// RingBuffer 없이 직접 중계한다.
    pub async fn relay_reverse(
        &self,
        mut quic_rx: RecvStream,
        tcp_tx: mpsc::Sender<Bytes>,
        metrics: Arc<TransferMetrics>,
    ) -> Result<(), BufferError> {
        let mut buf = vec![0u8; 256 * 1024]; // 256KB 읽기 버퍼
        let mut total_bytes = 0u64;
        let mut chunk_count = 0u64;

        loop {
            let n = quic_rx.read(&mut buf).await
                .map_err(|e| {
                    tracing::error!("relay_reverse: QUIC read error after {total_bytes} bytes ({chunk_count} chunks): {e}");
                    BufferError::InvalidSize(format!("quic read: {e}"))
                })?
                .unwrap_or(0);

            if n == 0 {
                // QUIC 스트림 종료
                tracing::debug!(
                    "relay_reverse: QUIC recv_stream EOF after {total_bytes} bytes ({chunk_count} chunks)"
                );
                return Ok(());
            }

            chunk_count += 1;
            total_bytes += n as u64;
            metrics
                .bytes_transferred
                .fetch_add(n as u64, Ordering::Relaxed);

            tcp_tx.send(Bytes::copy_from_slice(&buf[..n])).await
                .map_err(|_| {
                    tracing::error!("relay_reverse: tcp_tx channel closed after {total_bytes} bytes ({chunk_count} chunks)");
                    BufferError::InvalidSize("tcp_tx channel closed".into())
                })?;
        }
    }
}

/// 환경변수 문자열을 버퍼 크기로 파싱한다. None이거나 파싱 실패 시 기본값 반환.
fn parse_buffer_size(raw: Option<&str>) -> usize {
    match raw {
        Some(val) => val.parse::<usize>().unwrap_or(DEFAULT_BUFFER_SIZE),
        None => DEFAULT_BUFFER_SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- RingBuffer 단위 테스트 ---

    #[test]
    fn new_buffer_is_empty() {
        let buf = RingBuffer::new(16);
        assert!(buf.is_empty());
        assert!(!buf.is_full());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.available(), 16);
    }

    #[test]
    fn write_and_read_basic() {
        let mut buf = RingBuffer::new(8);
        let written = buf.write(b"hello").unwrap();
        assert_eq!(written, 5);
        assert_eq!(buf.len(), 5);

        let mut out = [0u8; 5];
        let read = buf.read(&mut out);
        assert_eq!(read, 5);
        assert_eq!(&out, b"hello");
        assert!(buf.is_empty());
    }

    #[test]
    fn write_returns_buffer_full_when_full() {
        let mut buf = RingBuffer::new(4);
        buf.write(b"abcd").unwrap();
        assert!(buf.is_full());

        let result = buf.write(b"x");
        assert!(result.is_err());
    }

    #[test]
    fn partial_write_when_space_limited() {
        let mut buf = RingBuffer::new(4);
        buf.write(b"ab").unwrap();
        let written = buf.write(b"cdef").unwrap();
        assert_eq!(written, 2); // 4 - 2 = 2 바이트만 기록 가능
        assert!(buf.is_full());
    }

    #[test]
    fn wraparound_write_and_read() {
        let mut buf = RingBuffer::new(4);
        buf.write(b"ab").unwrap();

        let mut out = [0u8; 2];
        buf.read(&mut out);
        assert_eq!(&out, b"ab");

        // head=2, tail=2 상태에서 wraparound 기록
        buf.write(b"cdef").unwrap();
        assert_eq!(buf.len(), 4);

        let mut out2 = [0u8; 4];
        buf.read(&mut out2);
        assert_eq!(&out2, b"cdef");
    }

    #[test]
    fn read_from_empty_returns_zero() {
        let mut buf = RingBuffer::new(4);
        let mut out = [0u8; 4];
        assert_eq!(buf.read(&mut out), 0);
    }

    #[test]
    fn read_partial_when_buf_larger_than_data() {
        let mut buf = RingBuffer::new(4);
        buf.write(b"ab").unwrap();

        let mut out = [0u8; 8];
        let read = buf.read(&mut out);
        assert_eq!(read, 2);
        assert_eq!(&out[..2], b"ab");
    }

    // --- BufferLayer 단위 테스트 ---

    #[test]
    fn buffer_layer_default_capacity() {
        let layer = BufferLayer::new(DEFAULT_BUFFER_SIZE);
        assert_eq!(layer.buffer.capacity, 256 * 1024 * 1024);
        assert!(!layer.is_backpressure_active());
    }

    #[test]
    fn parse_buffer_size_none_returns_default() {
        assert_eq!(parse_buffer_size(None), DEFAULT_BUFFER_SIZE);
    }

    #[test]
    fn parse_buffer_size_valid_value() {
        assert_eq!(parse_buffer_size(Some("1024")), 1024);
    }

    #[test]
    fn parse_buffer_size_invalid_falls_back() {
        assert_eq!(parse_buffer_size(Some("not_a_number")), DEFAULT_BUFFER_SIZE);
    }

    #[test]
    fn parse_buffer_size_empty_string_falls_back() {
        assert_eq!(parse_buffer_size(Some("")), DEFAULT_BUFFER_SIZE);
    }

    // Feature: quicsync-tunnel-mvp, Property 5: 환경변수 버퍼 크기 설정
    // **Validates: Requirements 4.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_env_buffer_size_valid_positive_integer(
            value in 1usize..=1_000_000,
        ) {
            // 임의의 양의 정수를 문자열로 변환 후 parse_buffer_size에 전달하면
            // 해당 값이 그대로 반환되어야 한다
            let s = value.to_string();
            let result = parse_buffer_size(Some(&s));
            prop_assert_eq!(result, value);
        }

        #[test]
        fn prop_env_buffer_size_none_returns_default(
            _dummy in 0u8..1,
        ) {
            // None이면 항상 DEFAULT_BUFFER_SIZE를 반환해야 한다
            let result = parse_buffer_size(None);
            prop_assert_eq!(result, DEFAULT_BUFFER_SIZE);
        }
    }

    // Feature: quicsync-tunnel-mvp, Property 6: Ring_Buffer write/read 라운드트립
    // **Validates: Requirements 4.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_ring_buffer_write_read_roundtrip(
            capacity in 1usize..=1024,
            data in proptest::collection::vec(any::<u8>(), 0..=1024),
        ) {
            // data.len() <= capacity 으로 제한
            let data = if data.len() > capacity { &data[..capacity] } else { &data[..] };

            let mut buf = RingBuffer::new(capacity);

            // write 전 len == 0
            prop_assert_eq!(buf.len(), 0);

            // write — data.len() <= capacity이므로 전체 기록 성공
            let written = buf.write(data).unwrap();
            prop_assert_eq!(written, data.len());
            prop_assert_eq!(buf.len(), data.len());

            // read
            let mut out = vec![0u8; data.len()];
            let read_count = buf.read(&mut out);
            prop_assert_eq!(read_count, data.len());
            prop_assert_eq!(buf.len(), 0);

            // 데이터 동일성
            prop_assert_eq!(&out[..], data);
        }
    }

    // Feature: quicsync-tunnel-mvp, Property 7: Backpressure 적용 및 해제
    // **Validates: Requirements 4.5, 4.6**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_backpressure_apply_and_release(
            capacity in 1usize..=512,
            read_count in 1usize..=512,
        ) {
            let read_count = read_count.min(capacity);
            let mut buf = RingBuffer::new(capacity);

            // capacity 바이트를 채워 버퍼를 가득 채운다
            let fill_data = vec![0xABu8; capacity];
            let written = buf.write(&fill_data).unwrap();
            prop_assert_eq!(written, capacity);

            // is_full() == true
            prop_assert!(buf.is_full());
            prop_assert_eq!(buf.len(), capacity);
            prop_assert_eq!(buf.available(), 0);

            // 추가 write → BufferFull 오류
            let extra = buf.write(&[0xFF]);
            prop_assert!(extra.is_err());

            // 일부 read → backpressure 해제
            let mut read_buf = vec![0u8; read_count];
            let actually_read = buf.read(&mut read_buf);
            prop_assert_eq!(actually_read, read_count);

            // is_full() == false, write 재개 가능
            prop_assert!(!buf.is_full());
            prop_assert_eq!(buf.available(), read_count);

            let resume = buf.write(&[0xCD]);
            prop_assert!(resume.is_ok());
            prop_assert_eq!(resume.unwrap(), 1);
        }
    }
}
