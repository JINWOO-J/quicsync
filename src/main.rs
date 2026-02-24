use std::process::ExitCode;

use quicsync::cli::parse_args;
use quicsync::server::RemoteServer;
use quicsync::session::Session;

#[tokio::main]
async fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args().collect();

    // --server 플래그는 parse_args 전에 감지한다.
    if raw_args.iter().any(|a| a == "--server") {
        init_tracing(0); // 서버 모드는 warn 레벨
        return run_server().await;
    }

    let verbosity = count_verbose_flags(&raw_args);
    init_tracing(verbosity);

    run_client(&raw_args).await
}

/// -v 플래그 개수를 센다. `-v`, `-vv`, `-vvv` 및 개별 `-v -v -v` 모두 지원.
fn count_verbose_flags(args: &[String]) -> usize {
    args.iter()
        .skip(1) // 프로그램 이름 제외
        .filter(|a| a.starts_with('-') && !a.starts_with("--") && !a.contains('='))
        .map(|a| a.chars().filter(|&c| c == 'v').count())
        .sum()
}

/// verbosity 레벨에 따라 tracing을 초기화한다.
/// RUST_LOG 환경변수가 설정되어 있으면 그것을 우선한다.
fn init_tracing(verbosity: usize) {
    use tracing_subscriber::EnvFilter;

    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else {
        let level = match verbosity {
            0 => "warn",
            1 => "quicsync=info",
            2 => "quicsync=debug",
            _ => "quicsync=trace",
        };
        EnvFilter::new(level)
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();
}

/// --server 모드: 원격 호스트에서 SSH를 통해 실행된다.
async fn run_server() -> ExitCode {
    match server_main().await {
        Ok(code) => exit_code(code),
        Err(e) => {
            eprintln!("[quicsync-server] error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn server_main() -> Result<i32, quicsync::error::ServerError> {
    let server = RemoteServer::start().await?;
    server.emit_handshake();
    server.accept_and_serve().await
}

/// 일반 CLI 모드: 로컬에서 사용자가 직접 실행한다.
async fn run_client(raw_args: &[String]) -> ExitCode {
    let args = match parse_args(raw_args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("quicsync: {e}");
            return ExitCode::FAILURE;
        }
    };

    let session = match Session::start(args).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("quicsync: {e}");
            return ExitCode::FAILURE;
        }
    };

    match session.run().await {
        Ok(code) => exit_code(code),
        Err(e) => {
            eprintln!("quicsync: {e}");
            ExitCode::FAILURE
        }
    }
}

/// i32 종료 코드를 ExitCode로 변환한다.
fn exit_code(code: i32) -> ExitCode {
    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code as u8)
    }
}
