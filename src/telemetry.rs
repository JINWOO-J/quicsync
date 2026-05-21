// OpenTelemetry 트레이싱 - 조건부 컴파일 (#[cfg(feature = "otel")])
//
// TelemetryExporter: OTLP 수집기 연결 및 TracerProvider 관리
// SessionSpan / ChildSpan: tracing span 래퍼

use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::TracerProvider;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;

use crate::error::TelemetryError;
use crate::metrics::TransferMetrics;
use crate::types::CliArgs;

/// OpenTelemetry OTLP 수집기로 트레이스를 전송하는 익스포터
pub struct TelemetryExporter {
    provider: TracerProvider,
}

impl TelemetryExporter {
    /// OpenTelemetry TracerProvider를 초기화한다.
    /// 수집기 연결 실패 시 TelemetryError를 반환한다 (호출자가 경고 로그 후 계속 진행 결정).
    pub fn init(endpoint: &str) -> Result<Self, TelemetryError> {
        let exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .map_err(|e| TelemetryError::InitFailed(e.to_string()))?;

        let provider = TracerProvider::builder()
            .with_simple_exporter(exporter)
            .with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                "quicsync",
            )]))
            .build();

        let tracer = provider.tracer("quicsync");
        let otel_layer = OpenTelemetryLayer::new(tracer);

        // tracing subscriber에 OpenTelemetry 레이어를 등록한다.
        // set_global_default가 이미 설정된 경우(테스트 등) 실패할 수 있으므로 무시한다.
        let subscriber = Registry::default().with(otel_layer);
        let _ = tracing::subscriber::set_global_default(subscriber);

        Ok(Self { provider })
    }

    /// 세션 전체를 감싸는 루트 span을 생성한다.
    pub fn start_session_span(&self, args: &CliArgs) -> SessionSpan {
        let span = tracing::info_span!(
            "quicsync.session",
            direction = ?args.direction,
            host = %args.remote.host,
            streams = args.streams,
        );
        SessionSpan { span }
    }

    /// TracerProvider를 종료하고 남은 span을 flush한다.
    pub fn shutdown(self) {
        if let Err(e) = self.provider.shutdown() {
            tracing::warn!("OpenTelemetry shutdown error: {e}");
        }
    }
}

/// 세션 루트 span 래퍼
pub struct SessionSpan {
    span: tracing::Span,
}

/// 하위 span 래퍼
pub struct ChildSpan {
    span: tracing::Span,
}

impl SessionSpan {
    /// SSH 핸드셰이크 단계의 하위 span
    pub fn ssh_span(&self) -> ChildSpan {
        let span = tracing::info_span!(parent: &self.span, "quicsync.ssh");
        ChildSpan { span }
    }

    /// QUIC 연결 단계의 하위 span
    pub fn quic_span(&self) -> ChildSpan {
        let span = tracing::info_span!(parent: &self.span, "quicsync.quic");
        ChildSpan { span }
    }

    /// 데이터 전송 단계의 하위 span (메트릭 attribute 포함)
    pub fn transfer_span(&self, metrics: &Arc<TransferMetrics>) -> ChildSpan {
        let snapshot = metrics.snapshot();
        let span = tracing::info_span!(
            parent: &self.span,
            "quicsync.transfer",
            bytes_transferred = snapshot.bytes_transferred,
            duration_secs = snapshot.duration_secs,
            streams_completed = snapshot.streams_completed,
        );
        ChildSpan { span }
    }
}

impl ChildSpan {
    /// span에 진입한다 (RAII guard 반환).
    pub fn enter(&self) -> tracing::span::Entered<'_> {
        self.span.enter()
    }
}
