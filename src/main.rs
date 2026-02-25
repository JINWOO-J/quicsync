use std::process::ExitCode;

use quicsync::cli::parse_args;
use quicsync::server::RemoteServer;
use quicsync::session::Session;

#[tokio::main]
async fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args().collect();

    // --connect를 먼저 확인한다.
    // rsync가 rsh로 호출할 때 "rsync --server ..." 인수가 뒤에 붙으므로,
    // --server보다 --connect를 먼저 검사해야 한다.
    if let Some(port) = parse_connect_flag(&raw_args) {
        // --connect 모드에서는 tracing을 초기화하지 않는다.
        // stdout/stderr가 rsync 프로토콜 통신에 사용되므로,
        // tracing 출력이 프로토콜 데이터를 오염시킬 수 있다.
        return run_connect(port, &raw_args).await;
    }

    if raw_args.iter().any(|a| a == "--server") {
        init_tracing(0); // 서버 모드는 warn 레벨
        return run_server().await;
    }

    let verbosity = count_verbose_flags(&raw_args);
    init_tracing(verbosity);

    tracing::debug!("quicsync v{}", env!("CARGO_PKG_VERSION"));

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
/// --connect PORT 플래그를 파싱한다. rsync의 --rsh에서 호출될 때 사용.
fn parse_connect_flag(args: &[String]) -> Option<u16> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--connect" {
            return iter.next().and_then(|p| p.parse().ok());
        }
    }
    None
}

/// --connect 모드: rsync의 --rsh 트랜스포트로 동작한다.
/// localhost:PORT에 TCP 연결 후, rsync 서버 인수를 전송하고 stdin/stdout ↔ TCP 양방향 relay.
///
/// rsync는 rsh 프로그램을 다음과 같이 호출한다:
///   quicsync --connect PORT [-l user] host rsync --server [flags] . path
/// "rsync" 이후의 인수를 원격 서버에 전달하여 올바른 프로토콜 협상이 이루어지도록 한다.
async fn run_connect(port: u16, raw_args: &[String]) -> ExitCode {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    let rsync_server_args = extract_rsync_server_args(raw_args);

    let stream = match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("quicsync --connect: failed to connect to port {port}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // tokio::io::split()을 사용한다 (TcpStream::into_split() 대신).
    //
    // into_split()의 OwnedWriteHalf는 drop 시 TCP FIN을 전송한다.
    // pull 모드에서 fwd 태스크가 완료되어 tcp_write가 drop되면:
    //   FIN → proxy tcp_read EOF → relay_forward quic_tx.finish()
    //   → server child_stdin.shutdown() → 원격 rsync sender 중단
    // 이로 인해 역방향 데이터 전송이 조기 종료된다.
    //
    // io::split()은 내부적으로 Arc<Mutex<TcpStream>>을 사용하므로
    // write half가 drop되어도 underlying 소켓은 닫히지 않는다.
    // TCP 연결은 --connect 프로세스 종료 시에만 닫힌다.
    let (mut tcp_read, mut tcp_write) = tokio::io::split(stream);

    // rsync 서버 인수를 TCP 프록시를 통해 원격 서버로 전송
    let args_line = format!("{rsync_server_args}\n");
    if let Err(e) = tcp_write.write_all(args_line.as_bytes()).await {
        eprintln!("quicsync --connect: failed to send rsync args: {e}");
        return ExitCode::FAILURE;
    }

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // stdin → tcp: rsync가 보내는 데이터를 TCP 프록시로 전달.
    //
    // tokio::spawn으로 분리하는 이유: tokio::io::stdin()이 blocking thread를
    // 사용하므로, rev가 먼저 완료될 때 libc::close(0)으로 stdin을 닫아
    // blocking read를 중단시켜야 한다.
    // stdout/stdin이 non-blocking 모드로 설정되어 있으면 blocking으로 복원.
    // tokio 런타임이 fd를 non-blocking으로 설정할 수 있으며,
    // tokio::io::copy가 pipe에 EAGAIN을 만나면 에러로 처리될 수 있다.
    unsafe {
        for fd in [0, 1] {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 && (flags & libc::O_NONBLOCK) != 0 {
                libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
            }
        }
    }
    let fwd = tokio::spawn(async move {
        tokio::io::copy(&mut stdin, &mut tcp_write).await
    });

    // tcp → stdout: 서버에서 오는 데이터를 rsync에 전달.
    let rev = async {
        let r = tokio::io::copy(&mut tcp_read, &mut stdout).await;
        let _ = stdout.flush().await;
        r
    };

    tokio::pin!(rev);

    // 양방향 중계를 select!로 실행한다.
    //
    // - fwd 완료 시: local rsync가 stdout을 닫음 (보통 프로세스 종료 시).
    //   rev가 완료될 때까지 대기하여 역방향 데이터를 모두 수신한다.
    //
    // - rev 완료 시: TCP 읽기 EOF (서버에서 모든 데이터 수신 완료) 또는
    //   stdout EPIPE (local rsync 종료).
    //   fd 0을 닫아 tokio blocking stdin reader를 중단시키고 정상 종료한다.
    tokio::select! {
        join_r = fwd => {
            let r = match join_r {
                Ok(inner) => inner,
                Err(e) => Err(std::io::Error::other(format!("fwd task: {e}"))),
            };
            let rev_r = rev.await;
            if r.is_err() || rev_r.is_err() { ExitCode::FAILURE } else { ExitCode::SUCCESS }
        }
        r = &mut rev => {
            // rev 완료 = 서버에서 모든 데이터 수신 완료.
            // fwd의 tokio blocking stdin reader는 close(0)으로 깨어나지 않을 수 있다
            // (Docker socketpair 등). 프로세스를 즉시 종료한다.
            let code = if r.is_err() { 1 } else { 0 };
            std::process::exit(code);
        }
    }
}

/// rsync --rsh 인수에서 rsync 서버 명령 부분을 추출한다.
///
/// rsync는 rsh 프로그램을 다음과 같이 호출한다:
///   PROGRAM --connect PORT [-l user] host rsync --server [flags] . path
///
/// "rsync" 인수를 찾아 그 이후의 모든 인수(`--server [flags] . path`)를 반환한다.
fn extract_rsync_server_args(raw_args: &[String]) -> String {
    // --connect PORT 이후의 인수를 수집
    let mut iter = raw_args.iter().skip(1); // 프로그램 이름 건너뜀
    let mut after_connect = Vec::new();
    while let Some(arg) = iter.next() {
        if arg == "--connect" {
            iter.next(); // PORT 건너뜀
            // 나머지 인수를 모두 수집
            after_connect = iter.map(|s| s.as_str()).collect();
            break;
        }
    }

    // "rsync" 인수를 찾아 그 다음부터의 모든 인수를 반환
    if let Some(pos) = after_connect.iter().position(|&a| a == "rsync") {
        after_connect[pos + 1..].join(" ")
    } else {
        String::new()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extract_rsync_server_args_typical_push() {
        // rsync이 push 시 rsh에 전달하는 전형적인 인수
        let args = s(&[
            "quicsync", "--connect", "54220", "-l", "root", "jwserver68",
            "rsync", "--server", "-vvve.LsfxCIvu", ".", "/app/upload-test",
        ]);
        assert_eq!(
            extract_rsync_server_args(&args),
            "--server -vvve.LsfxCIvu . /app/upload-test"
        );
    }

    #[test]
    fn extract_rsync_server_args_no_login_flag() {
        // user 없이 호출되는 경우 (-l 플래그 없음)
        let args = s(&[
            "quicsync", "--connect", "12345", "myhost",
            "rsync", "--server", "-e.LsfxC", ".", "/data",
        ]);
        assert_eq!(
            extract_rsync_server_args(&args),
            "--server -e.LsfxC . /data"
        );
    }

    #[test]
    fn extract_rsync_server_args_pull_with_sender() {
        // pull 시 rsync가 --sender 플래그를 추가
        let args = s(&[
            "quicsync", "--connect", "8080", "-l", "user", "host",
            "rsync", "--server", "--sender", "-vvve.LsfxCIvu", ".", "/remote/path",
        ]);
        assert_eq!(
            extract_rsync_server_args(&args),
            "--server --sender -vvve.LsfxCIvu . /remote/path"
        );
    }

    #[test]
    fn extract_rsync_server_args_no_rsync_found() {
        // rsync 인수가 없는 비정상 케이스 → 빈 문자열 반환
        let args = s(&["quicsync", "--connect", "54220", "-l", "root", "host"]);
        assert_eq!(extract_rsync_server_args(&args), "");
    }

    #[test]
    fn extract_rsync_server_args_no_connect_flag() {
        // --connect 없는 경우 → 빈 문자열 반환
        let args = s(&["quicsync", "rsync", "--server", ".", "/path"]);
        assert_eq!(extract_rsync_server_args(&args), "");
    }
}
