// CLI 인수 파싱 및 세션 오케스트레이션

use std::path::PathBuf;

use clap::{Arg, Command};

use std::io::IsTerminal;

use crate::error::CliError;
use crate::quic::window_bytes_from_env;
use crate::types::{CliArgs, RemoteSpec, StatsFormat, TransferDirection};

/// 경로가 원격 경로인지 판별한다.
/// rsync 관례: `/` 또는 `.`로 시작하지 않고 `:`를 포함하면 원격으로 간주한다.
fn is_remote(path: &str) -> bool {
    !path.starts_with('/') && !path.starts_with('.') && path.contains(':')
}

/// `[user@]host:path` 형태의 원격 경로를 파싱한다.
pub fn parse_remote(path: &str) -> Result<RemoteSpec, CliError> {
    let Some((host_part, remote_path)) = path.split_once(':') else {
        return Err(CliError::InvalidRemotePath(format!(
            "missing ':' in remote path: {path}"
        )));
    };

    if host_part.is_empty() {
        return Err(CliError::InvalidRemotePath(format!(
            "empty host in remote path: {path}"
        )));
    }

    let (user, host) = if let Some((user, host)) = host_part.split_once('@') {
        if user.is_empty() {
            return Err(CliError::InvalidRemotePath(format!(
                "empty user in remote path: {path}"
            )));
        }
        if host.is_empty() {
            return Err(CliError::InvalidRemotePath(format!(
                "empty host in remote path: {path}"
            )));
        }
        (Some(user.to_string()), host.to_string())
    } else {
        (None, host_part.to_string())
    };

    Ok(RemoteSpec {
        user,
        host,
        path: remote_path.to_string(),
    })
}

/// CLI 인수를 파싱한다.
/// `args`는 프로그램 이름을 포함한 전체 인수 목록이다.
/// 형식: `quicsync [rsync options] SRC DST`
pub fn parse_args(args: &[String]) -> Result<CliArgs, CliError> {
    let banner = format!(
        r#"
   __ _ _   _(_) ___ ___ _   _ _ __   ___
  / _` | | | | |/ __/ __| | | | '_ \ / __|
 | (_| | |_| | | (__\__ \ |_| | | | | (__
  \__, |\__,_|_|\___|___/\__, |_| |_|\___|
     |_|                 |___/        v{}

  rsync over QUIC tunnel — fast file sync for long-distance networks
"#,
        env!("CARGO_PKG_VERSION")
    );

    let cmd = Command::new("quicsync")
        .version(env!("CARGO_PKG_VERSION"))
        .before_help(banner)
        .override_usage("quicsync [rsync-options] SRC... DST")
        .after_help(
            "Examples:\n  \
             quicsync /local/dir user@remote:/remote/dir    # Push\n  \
             quicsync user@remote:/remote/dir /local/dir    # Pull\n  \
             quicsync -avz --delete /src user@host:/dst     # With rsync options\n  \
             quicsync ./* user@host:/dst                    # Multiple sources\n  \
             quicsync --window 128 /src user@host:/dst      # 128MB QUIC window",
        )
        .arg(
            Arg::new("args")
                .num_args(1..)
                .trailing_var_arg(true)
                .allow_hyphen_values(true),
        );

    let matches = cmd.try_get_matches_from(args).map_err(|e| {
        // --help, --version은 clap이 직접 출력하고 정상 종료한다.
        if e.use_stderr() {
            CliError::InvalidArgs(e.to_string())
        } else {
            e.exit();
        }
    })?;

    let trailing: Vec<&String> = matches
        .get_many::<String>("args")
        .map(|vals| vals.collect())
        .unwrap_or_default();

    if trailing.len() < 2 {
        return Err(CliError::InvalidArgs(
            "SRC and DST are required".to_string(),
        ));
    }

    // quicsync 자체 옵션을 먼저 추출한다.
    // 나머지는 rsync 옵션 + positional로 분류한다.
    let mut window_mb: Option<u64> = None;
    let mut no_progress = false;
    let mut streams: Option<u8> = None;
    let mut stats = false;
    let mut stats_format = StatsFormat::Text;
    let mut otel_endpoint: Option<String> = None;
    let mut no_integrity = false;
    let mut remaining: Vec<&str> = Vec::new();
    let mut iter = trailing.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--window" => {
                if let Some(val) = iter.next() {
                    window_mb = val.parse().ok();
                }
            }
            "--no-progress" => no_progress = true,
            "--streams" => {
                if let Some(val) = iter.next() {
                    match val.parse::<u8>() {
                        Ok(n) => streams = Some(n),
                        Err(_) => {
                            return Err(CliError::InvalidArgs(
                                format!("invalid value '{val}' for --streams: must be an integer between 1 and 64"),
                            ));
                        }
                    }
                }
            }
            "--stats" => stats = true,
            "--stats-format" => {
                if let Some(val) = iter.next() {
                    match val.as_str() {
                        "text" => stats_format = StatsFormat::Text,
                        "json" => stats_format = StatsFormat::Json,
                        _ => {
                            return Err(CliError::InvalidArgs(
                                format!("invalid value '{val}' for --stats-format: must be 'text' or 'json'"),
                            ));
                        }
                    }
                }
            }
            "--otel-endpoint" => {
                if let Some(val) = iter.next() {
                    otel_endpoint = Some(val.to_string());
                }
            }
            "--no-integrity" => no_integrity = true,
            _ if arg.starts_with("--window=") => {
                window_mb = arg.strip_prefix("--window=").unwrap().parse().ok();
            }
            _ if arg.starts_with("--streams=") => {
                let val = arg.strip_prefix("--streams=").unwrap();
                match val.parse::<u8>() {
                    Ok(n) => streams = Some(n),
                    Err(_) => {
                        return Err(CliError::InvalidArgs(
                            format!("invalid value '{val}' for --streams: must be an integer between 1 and 64"),
                        ));
                    }
                }
            }
            _ if arg.starts_with("--stats-format=") => {
                let val = arg.strip_prefix("--stats-format=").unwrap();
                match val {
                    "text" => stats_format = StatsFormat::Text,
                    "json" => stats_format = StatsFormat::Json,
                    _ => {
                        return Err(CliError::InvalidArgs(
                            format!("invalid value '{val}' for --stats-format: must be 'text' or 'json'"),
                        ));
                    }
                }
            }
            _ if arg.starts_with("--otel-endpoint=") => {
                otel_endpoint = Some(arg.strip_prefix("--otel-endpoint=").unwrap().to_string());
            }
            _ => remaining.push(arg.as_str()),
        }
    }

    // --streams 범위 검증 (1-64)
    let streams = streams.unwrap_or(4);
    if streams < 1 || streams > 64 {
        return Err(CliError::InvalidArgs(
            format!("invalid value '{streams}' for --streams: must be between 1 and 64"),
        ));
    }

    let show_progress = if no_progress {
        false
    } else {
        std::io::stdout().is_terminal()
    };

    let quic_window = window_mb
        .map(|mb| mb * 1024 * 1024)
        .unwrap_or_else(window_bytes_from_env);

    // trailing args를 rsync 옵션과 경로(positional)로 분리한다.
    // `-`로 시작하는 인수는 rsync 옵션, 나머지는 경로로 취급한다.
    let mut rsync_options = Vec::new();
    let mut positionals = Vec::new();
    for arg in &remaining {
        if arg.starts_with('-') {
            rsync_options.push(arg.to_string());
        } else {
            positionals.push(*arg);
        }
    }

    if positionals.len() < 2 {
        return Err(CliError::InvalidArgs(
            "SRC and DST are required".to_string(),
        ));
    }

    // 마지막 positional = DST, 나머지 = SRC(들)
    let dst = positionals.last().unwrap();
    let srcs = &positionals[..positionals.len() - 1];

    let dst_remote = is_remote(dst);
    // SRC 중 하나라도 remote이면 Pull 모드 (remote source는 1개만 허용)
    let remote_src_indices: Vec<usize> = srcs
        .iter()
        .enumerate()
        .filter(|(_, s)| is_remote(s))
        .map(|(i, _)| i)
        .collect();

    if !remote_src_indices.is_empty() && dst_remote {
        return Err(CliError::BothRemote);
    }

    if remote_src_indices.is_empty() && !dst_remote {
        return Err(CliError::BothLocal);
    }

    if dst_remote {
        // Push: 모든 SRC가 로컬, DST가 원격
        let remote = parse_remote(dst)?;
        let local_paths = srcs.iter().map(|s| PathBuf::from(*s)).collect();
        Ok(CliArgs {
            local_paths,
            remote,
            rsync_options,
            direction: TransferDirection::Push,
            quic_window,
            show_progress,
            streams,
            stats,
            stats_format,
            otel_endpoint,
            no_integrity,
        })
    } else {
        // Pull: SRC 중 하나가 원격, DST가 로컬
        if remote_src_indices.len() > 1 {
            return Err(CliError::BothRemote);
        }
        let remote_idx = remote_src_indices[0];
        let remote = parse_remote(srcs[remote_idx])?;
        // Pull에서 remote가 아닌 SRC가 있으면 에러
        if srcs.len() > 1 {
            return Err(CliError::InvalidArgs(
                "Pull mode supports only one remote source".to_string(),
            ));
        }
        Ok(CliArgs {
            local_paths: vec![PathBuf::from(*dst)],
            remote,
            rsync_options,
            direction: TransferDirection::Pull,
            quic_window,
            show_progress,
            streams,
            stats,
            stats_format,
            otel_endpoint,
            no_integrity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- parse_remote 단위 테스트 ---

    #[test]
    fn parse_remote_user_host_path() {
        let r = parse_remote("deploy@server.example.com:/var/www").unwrap();
        assert_eq!(r.user.as_deref(), Some("deploy"));
        assert_eq!(r.host, "server.example.com");
        assert_eq!(r.path, "/var/www");
    }

    #[test]
    fn parse_remote_host_path_only() {
        let r = parse_remote("myhost:data/backup").unwrap();
        assert_eq!(r.user, None);
        assert_eq!(r.host, "myhost");
        assert_eq!(r.path, "data/backup");
    }

    #[test]
    fn parse_remote_empty_path() {
        // rsync에서 host: 는 홈 디렉토리를 의미
        let r = parse_remote("server:").unwrap();
        assert_eq!(r.user, None);
        assert_eq!(r.host, "server");
        assert_eq!(r.path, "");
    }

    #[test]
    fn parse_remote_missing_colon() {
        assert!(parse_remote("no-colon-here").is_err());
    }

    #[test]
    fn parse_remote_empty_host() {
        assert!(parse_remote(":/path").is_err());
    }

    #[test]
    fn parse_remote_empty_user() {
        assert!(parse_remote("@host:/path").is_err());
    }

    #[test]
    fn parse_remote_empty_host_with_user() {
        assert!(parse_remote("user@:/path").is_err());
    }

    // --- parse_args 단위 테스트 ---

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_args_push_direction() {
        let a = args(&["quicsync", "/local/dir", "user@host:/remote/dir"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.local_paths, vec![PathBuf::from("/local/dir")]);
        assert_eq!(cli.remote.user.as_deref(), Some("user"));
        assert_eq!(cli.remote.host, "host");
        assert_eq!(cli.remote.path, "/remote/dir");
        assert_eq!(cli.direction, TransferDirection::Push);
        assert!(cli.rsync_options.is_empty());
    }

    #[test]
    fn parse_args_pull_direction() {
        let a = args(&["quicsync", "host:/data", "/local/data"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.local_paths, vec![PathBuf::from("/local/data")]);
        assert_eq!(cli.remote.host, "host");
        assert_eq!(cli.remote.path, "/data");
        assert_eq!(cli.direction, TransferDirection::Pull);
    }

    #[test]
    fn parse_args_with_rsync_options() {
        let a = args(&[
            "quicsync",
            "-avz",
            "--delete",
            "--exclude=*.tmp",
            "/src",
            "server:/dst",
        ]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.rsync_options, vec!["-avz", "--delete", "--exclude=*.tmp"]);
        assert_eq!(cli.local_paths, vec![PathBuf::from("/src")]);
        assert_eq!(cli.direction, TransferDirection::Push);
    }

    #[test]
    fn parse_args_both_local_error() {
        let a = args(&["quicsync", "/local/a", "/local/b"]);
        assert!(matches!(parse_args(&a), Err(CliError::BothLocal)));
    }

    #[test]
    fn parse_args_both_remote_error() {
        let a = args(&["quicsync", "host1:/a", "host2:/b"]);
        assert!(matches!(parse_args(&a), Err(CliError::BothRemote)));
    }

    #[test]
    fn parse_args_missing_dst() {
        let a = args(&["quicsync", "/only-one"]);
        assert!(matches!(parse_args(&a), Err(CliError::InvalidArgs(_))));
    }

    #[test]
    fn parse_args_no_args() {
        let a = args(&["quicsync"]);
        assert!(matches!(parse_args(&a), Err(CliError::InvalidArgs(_))));
    }

    #[test]
    fn parse_args_relative_local_path() {
        let a = args(&["quicsync", "./relative/path", "host:/remote"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.local_paths, vec![PathBuf::from("./relative/path")]);
        assert_eq!(cli.direction, TransferDirection::Push);
    }

    // --- Phase 2 CLI 플래그 단위 테스트 ---

    #[test]
    fn parse_args_default_new_fields() {
        let a = args(&["quicsync", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.streams, 4);
        assert!(!cli.stats);
        assert_eq!(cli.stats_format, StatsFormat::Text);
        assert_eq!(cli.otel_endpoint, None);
        assert!(!cli.no_integrity);
    }

    #[test]
    fn parse_args_no_progress_flag() {
        let a = args(&["quicsync", "--no-progress", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert!(!cli.show_progress);
    }

    #[test]
    fn parse_args_streams_valid() {
        let a = args(&["quicsync", "--streams", "8", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.streams, 8);
    }

    #[test]
    fn parse_args_streams_eq_syntax() {
        let a = args(&["quicsync", "--streams=16", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.streams, 16);
    }

    #[test]
    fn parse_args_streams_min_boundary() {
        let a = args(&["quicsync", "--streams", "1", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.streams, 1);
    }

    #[test]
    fn parse_args_streams_max_boundary() {
        let a = args(&["quicsync", "--streams", "64", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.streams, 64);
    }

    #[test]
    fn parse_args_streams_zero_error() {
        let a = args(&["quicsync", "--streams", "0", "/src", "host:/dst"]);
        assert!(matches!(parse_args(&a), Err(CliError::InvalidArgs(_))));
    }

    #[test]
    fn parse_args_streams_over_64_error() {
        let a = args(&["quicsync", "--streams", "65", "/src", "host:/dst"]);
        assert!(matches!(parse_args(&a), Err(CliError::InvalidArgs(_))));
    }

    #[test]
    fn parse_args_streams_non_numeric_error() {
        let a = args(&["quicsync", "--streams", "abc", "/src", "host:/dst"]);
        assert!(matches!(parse_args(&a), Err(CliError::InvalidArgs(_))));
    }

    #[test]
    fn parse_args_stats_flag() {
        let a = args(&["quicsync", "--stats", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert!(cli.stats);
    }

    #[test]
    fn parse_args_stats_format_json() {
        let a = args(&["quicsync", "--stats-format", "json", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.stats_format, StatsFormat::Json);
    }

    #[test]
    fn parse_args_stats_format_text() {
        let a = args(&["quicsync", "--stats-format", "text", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.stats_format, StatsFormat::Text);
    }

    #[test]
    fn parse_args_stats_format_eq_syntax() {
        let a = args(&["quicsync", "--stats-format=json", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.stats_format, StatsFormat::Json);
    }

    #[test]
    fn parse_args_stats_format_invalid_error() {
        let a = args(&["quicsync", "--stats-format", "xml", "/src", "host:/dst"]);
        assert!(matches!(parse_args(&a), Err(CliError::InvalidArgs(_))));
    }

    #[test]
    fn parse_args_otel_endpoint() {
        let a = args(&["quicsync", "--otel-endpoint", "http://localhost:4317", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.otel_endpoint, Some("http://localhost:4317".to_string()));
    }

    #[test]
    fn parse_args_otel_endpoint_eq_syntax() {
        let a = args(&["quicsync", "--otel-endpoint=http://collector:4317", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert_eq!(cli.otel_endpoint, Some("http://collector:4317".to_string()));
    }

    #[test]
    fn parse_args_no_integrity_flag() {
        let a = args(&["quicsync", "--no-integrity", "/src", "host:/dst"]);
        let cli = parse_args(&a).unwrap();
        assert!(cli.no_integrity);
    }

    #[test]
    fn parse_args_all_new_flags_combined() {
        let a = args(&[
            "quicsync", "--no-progress", "--streams", "16", "--stats",
            "--stats-format", "json", "--otel-endpoint", "http://otel:4317",
            "--no-integrity", "/src", "host:/dst",
        ]);
        let cli = parse_args(&a).unwrap();
        assert!(!cli.show_progress);
        assert_eq!(cli.streams, 16);
        assert!(cli.stats);
        assert_eq!(cli.stats_format, StatsFormat::Json);
        assert_eq!(cli.otel_endpoint, Some("http://otel:4317".to_string()));
        assert!(cli.no_integrity);
    }

    // --- Property-based 테스트 ---
    // Feature: quicsync-tunnel-mvp, Property 1: CLI 파싱 정확성 — 유효 입력 보존
    // **Validates: Requirements 1.1, 1.2**

    /// 유효한 user 문자열 생성기 (영숫자, 1~16자)
    fn valid_user() -> impl Strategy<Value = String> {
        "[a-zA-Z][a-zA-Z0-9]{0,15}".prop_map(|s| s)
    }

    /// 유효한 host 문자열 생성기 (영숫자+도트, `:` 없음, 1~32자)
    fn valid_host() -> impl Strategy<Value = String> {
        "[a-zA-Z][a-zA-Z0-9.]{0,31}"
            .prop_filter("host must not contain ':'", |s| !s.contains(':'))
    }

    /// 유효한 원격 path 생성기 (`/`로 시작, `:` 없음)
    fn valid_remote_path() -> impl Strategy<Value = String> {
        "/[a-zA-Z0-9/_.-]{1,32}"
            .prop_filter("path must not contain ':'", |s| !s.contains(':'))
    }

    /// 유효한 로컬 경로 생성기 (`/` 또는 `./`로 시작, `:` 없음)
    fn valid_local_path() -> impl Strategy<Value = String> {
        prop_oneof![
            "/[a-zA-Z0-9/_.]{1,32}",
            "\\./[a-zA-Z0-9/_.]{1,32}",
        ]
        .prop_filter("local path must not contain ':'", |s| !s.contains(':'))
    }

    /// 유효한 rsync 옵션 생성기 (`-`로 시작하는 문자열 벡터)
    fn valid_rsync_options() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(
            prop_oneof![
                Just("-a".to_string()),
                Just("-v".to_string()),
                Just("-z".to_string()),
                Just("-avz".to_string()),
                Just("--delete".to_string()),
                Just("--progress".to_string()),
                Just("--compress".to_string()),
                "--exclude=[a-zA-Z0-9*.]{1,16}".prop_map(|s| format!("--exclude={s}")),
            ],
            0..5,
        )
    }

    // --- Property 2 생성기 ---
    // Feature: quicsync-tunnel-mvp, Property 2: CLI 파싱 거부 — 무효 입력 오류
    // **Validates: Requirements 1.3, 1.4**

    /// 로컬 경로 생성기 (is_remote == false): `/` 또는 `.`로 시작, `:` 없음
    fn local_path_for_reject() -> impl Strategy<Value = String> {
        prop_oneof![
            "/[a-zA-Z0-9/_.-]{1,32}",
            "\\./[a-zA-Z0-9/_.-]{1,32}",
        ]
        .prop_filter("must not contain ':'", |s| !s.contains(':'))
    }

    /// 원격 경로 생성기 (is_remote == true): `/` `.`로 시작하지 않고 `:`를 포함
    fn remote_path_for_reject() -> impl Strategy<Value = String> {
        (
            "[a-zA-Z][a-zA-Z0-9.]{0,15}",
            "[a-zA-Z0-9/_.-]{0,32}",
        )
            .prop_map(|(host, path)| format!("{host}:{path}"))
            .prop_filter("must be detected as remote", |s| is_remote(s))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// 양쪽 모두 로컬 경로이면 BothLocal 오류를 반환한다.
        #[test]
        fn prop_parse_args_both_local_rejected(
            src in local_path_for_reject(),
            dst in local_path_for_reject(),
        ) {
            let argv = vec![
                "quicsync".to_string(),
                src,
                dst,
            ];
            let result = parse_args(&argv);
            prop_assert!(
                matches!(result, Err(CliError::BothLocal)),
                "expected BothLocal, got: {:?}", result
            );
        }

        /// 양쪽 모두 원격 경로이면 BothRemote 오류를 반환한다.
        #[test]
        fn prop_parse_args_both_remote_rejected(
            src in remote_path_for_reject(),
            dst in remote_path_for_reject(),
        ) {
            let argv = vec![
                "quicsync".to_string(),
                src,
                dst,
            ];
            let result = parse_args(&argv);
            prop_assert!(
                matches!(result, Err(CliError::BothRemote)),
                "expected BothRemote, got: {:?}", result
            );
        }
    }

    // --- Property 1 생성기 및 테스트 ---
    // Feature: quicsync-tunnel-mvp, Property 1: CLI 파싱 정확성 — 유효 입력 보존
    // **Validates: Requirements 1.1, 1.2**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Push 방향: 로컬 SRC + 원격 DST에서 user, host, path, rsync_options가 보존된다.
        #[test]
        fn prop_parse_args_push_preserves_fields(
            user in valid_user(),
            host in valid_host(),
            remote_path in valid_remote_path(),
            local_path in valid_local_path(),
            rsync_opts in valid_rsync_options(),
        ) {
            let remote_str = format!("{user}@{host}:{remote_path}");
            let mut argv: Vec<String> = vec!["quicsync".to_string()];
            argv.extend(rsync_opts.clone());
            argv.push(local_path.clone());
            argv.push(remote_str);

            let cli = parse_args(&argv).expect("valid input should parse");

            prop_assert_eq!(cli.remote.user.as_deref(), Some(user.as_str()));
            prop_assert_eq!(&cli.remote.host, &host);
            prop_assert_eq!(&cli.remote.path, &remote_path);
            prop_assert_eq!(cli.local_paths, vec![PathBuf::from(&local_path)]);
            prop_assert_eq!(&cli.rsync_options, &rsync_opts);
            prop_assert_eq!(cli.direction, TransferDirection::Push);
        }

        /// Pull 방향: 원격 SRC + 로컬 DST에서 user, host, path, rsync_options가 보존된다.
        #[test]
        fn prop_parse_args_pull_preserves_fields(
            user in valid_user(),
            host in valid_host(),
            remote_path in valid_remote_path(),
            local_path in valid_local_path(),
            rsync_opts in valid_rsync_options(),
        ) {
            let remote_str = format!("{user}@{host}:{remote_path}");
            let mut argv: Vec<String> = vec!["quicsync".to_string()];
            argv.extend(rsync_opts.clone());
            argv.push(remote_str);
            argv.push(local_path.clone());

            let cli = parse_args(&argv).expect("valid input should parse");

            prop_assert_eq!(cli.remote.user.as_deref(), Some(user.as_str()));
            prop_assert_eq!(&cli.remote.host, &host);
            prop_assert_eq!(&cli.remote.path, &remote_path);
            prop_assert_eq!(cli.local_paths, vec![PathBuf::from(&local_path)]);
            prop_assert_eq!(&cli.rsync_options, &rsync_opts);
            prop_assert_eq!(cli.direction, TransferDirection::Pull);
        }

        /// user 없는 Push: `host:path` 형태에서 user=None, host/path 보존.
        #[test]
        fn prop_parse_args_push_no_user(
            host in valid_host(),
            remote_path in valid_remote_path(),
            local_path in valid_local_path(),
            rsync_opts in valid_rsync_options(),
        ) {
            let remote_str = format!("{host}:{remote_path}");
            let mut argv: Vec<String> = vec!["quicsync".to_string()];
            argv.extend(rsync_opts.clone());
            argv.push(local_path.clone());
            argv.push(remote_str);

            let cli = parse_args(&argv).expect("valid input should parse");

            prop_assert_eq!(cli.remote.user, None);
            prop_assert_eq!(&cli.remote.host, &host);
            prop_assert_eq!(&cli.remote.path, &remote_path);
            prop_assert_eq!(&cli.rsync_options, &rsync_opts);
            prop_assert_eq!(cli.direction, TransferDirection::Push);
        }

        /// user 없는 Pull: `host:path` 형태에서 user=None, host/path 보존.
        #[test]
        fn prop_parse_args_pull_no_user(
            host in valid_host(),
            remote_path in valid_remote_path(),
            local_path in valid_local_path(),
            rsync_opts in valid_rsync_options(),
        ) {
            let remote_str = format!("{host}:{remote_path}");
            let mut argv: Vec<String> = vec!["quicsync".to_string()];
            argv.extend(rsync_opts.clone());
            argv.push(remote_str);
            argv.push(local_path.clone());

            let cli = parse_args(&argv).expect("valid input should parse");

            prop_assert_eq!(cli.remote.user, None);
            prop_assert_eq!(&cli.remote.host, &host);
            prop_assert_eq!(&cli.remote.path, &remote_path);
            prop_assert_eq!(&cli.rsync_options, &rsync_opts);
            prop_assert_eq!(cli.direction, TransferDirection::Pull);
        }
    }

    // Feature: quicsync-phase2-enhancements, Property 2: --streams 옵션 파싱 정확성
    // **Validates: Requirements 3.3, 3.4**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// 유효 범위(1-64)의 --streams 값은 파싱 성공하고 streams 필드에 보존된다.
        #[test]
        fn prop_streams_valid_range(n in 1u8..=64) {
            let argv = args(&["quicsync", "--streams", &n.to_string(), "/src", "host:/dst"]);
            let cli = parse_args(&argv).expect("valid streams value should parse");
            prop_assert_eq!(cli.streams, n);
        }

        /// 무효 범위(0 또는 65-255)의 --streams 값은 오류를 반환한다.
        #[test]
        fn prop_streams_invalid_range(n in prop_oneof![Just(0u8), 65u8..=255]) {
            let argv = args(&["quicsync", "--streams", &n.to_string(), "/src", "host:/dst"]);
            let result = parse_args(&argv);
            prop_assert!(
                matches!(result, Err(CliError::InvalidArgs(_))),
                "expected InvalidArgs for streams={}, got: {:?}", n, result
            );
        }
    }
}
