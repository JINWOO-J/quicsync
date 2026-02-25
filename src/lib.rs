// quicsync는 Linux와 macOS만 지원한다.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("quicsync only supports Linux and macOS");

pub mod cli;
pub mod ssh;
pub mod tcp_proxy;
pub mod buffer;
pub mod quic;
pub mod server;
pub mod rsync;
pub mod session;
pub mod error;
pub mod types;
pub mod metrics;
pub mod progress;
pub mod stats;
pub mod integrity;
pub mod multi_stream;

#[cfg(feature = "otel")]
pub mod telemetry;
