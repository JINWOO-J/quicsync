// quicsync는 Linux와 macOS만 지원한다.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("quicsync only supports Linux and macOS");

pub mod buffer;
pub mod cli;
pub mod doctor;
pub mod error;
pub mod integrity;
pub mod metrics;
pub mod multi_stream;
pub mod progress;
pub mod quic;
pub mod remote_install;
pub mod rsync;
pub mod server;
pub mod session;
pub mod ssh;
pub mod stats;
pub mod tcp_proxy;
pub mod types;
pub mod update;
pub mod web;

#[cfg(feature = "otel")]
pub mod telemetry;
