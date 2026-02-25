// OpenTelemetry 텔레메트리 (feature-gated: #[cfg(feature = "otel")])

use crate::error::TelemetryError;

/// OpenTelemetry 텔레메트리 익스포터
pub struct TelemetryExporter {
    endpoint: String,
}

impl TelemetryExporter {
    /// OTLP 엔드포인트로 텔레메트리 익스포터를 초기화한다.
    pub fn init(endpoint: &str) -> Result<Self, TelemetryError> {
        #[cfg(feature = "otel")]
        {
            use opentelemetry_sdk::trace::TracerProvider;
            use opentelemetry_otlp::WithExportConfig;

            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()
                .map_err(|e| TelemetryError::InitFailed(e.to_string()))?;

            let provider = TracerProvider::builder()
                .with_batch_exporter(exporter)
                .build();

            opentelemetry::global::set_tracer_provider(provider);
        }

        Ok(Self {
            endpoint: endpoint.to_string(),
        })
    }

    /// 텔레메트리 프로바이더를 종료한다.
    pub fn shutdown(&self) {
        #[cfg(feature = "otel")]
        {
            opentelemetry::global::shutdown_tracer_provider();
        }
        let _ = &self.endpoint; // suppress unused warning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_exporter_init_without_otel_feature() {
        // otel feature 없이도 init은 성공해야 한다 (no-op)
        let exporter = TelemetryExporter::init("http://localhost:4317");
        assert!(exporter.is_ok());
    }

    #[test]
    fn telemetry_exporter_shutdown_is_safe() {
        let exporter = TelemetryExporter::init("http://localhost:4317").unwrap();
        exporter.shutdown(); // should not panic
    }
}
