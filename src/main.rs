use std::process::ExitCode;

use quicsync::cli::parse_args;
use quicsync::server::RemoteServer;
use quicsync::session::Session;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let raw_args: Vec<String> = std::env::args().collect();

    // --server 플래그는 parse_args 전에 감지한다.
    // parse_args는 SRC DST 형식을 기대하므로 --server 모드와 호환되지 않는다.
    if raw_args.iter().any(|a| a == "--server") {
        return run_server().await;
    }

    run_client(&raw_args).await
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
