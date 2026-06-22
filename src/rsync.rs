// rsync 자식 프로세스 실행 및 관리

use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::error::RsyncError;
use crate::types::{RemoteSpec, TransferDirection};

pub struct RsyncChild {
    pub(crate) process: Child,
}

/// rsync `--log-file` 한 줄에서 추출한 전송 항목.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsyncLogItem {
    /// 일반 파일이면 true (디렉토리/심볼릭 등은 false)
    pub is_file: bool,
    /// 상대 경로 파일명
    pub name: String,
}

/// itemize 토큰인지 판별한다 (11자: update-type 1 + file-type 1 + 속성 9).
fn is_itemize_token(t: &str) -> bool {
    if t.len() != 11 {
        return false;
    }
    let b = t.as_bytes();
    matches!(b[0], b'<' | b'>' | b'c' | b'h' | b'.' | b'*')
        && matches!(b[1], b'f' | b'd' | b'L' | b'D' | b'S')
}

/// rsync `--log-file`(`--log-file-format='%i %n'`) 한 줄을 파싱한다.
///
/// 줄 형식: `YYYY/MM/DD HH:MM:SS [pid] <itemize 11자> <파일명>`
/// itemize(`%i`)는 11자 고정폭이며 속성 위치에 공백이 올 수 있으므로,
/// `[pid] ` 접두 뒤 정확히 11자를 잘라낸다(공백 split에 의존하지 않음).
/// 전송 항목이 아니면(시작/요약 메시지 등) None.
pub fn parse_log_file_line(line: &str) -> Option<RsyncLogItem> {
    // "...[pid] " 접두를 건너뛴다.
    let after_pid = line.split_once("] ")?.1;
    if after_pid.len() < 12 {
        return None;
    }
    let (itemize, rest) = after_pid.split_at(11);
    if !is_itemize_token(itemize) {
        return None;
    }
    let name = rest.strip_prefix(' ')?.trim_end();
    if name.is_empty() {
        return None;
    }
    Some(RsyncLogItem {
        is_file: itemize.as_bytes()[1] == b'f',
        name: name.to_string(),
    })
}

/// `QUICSYNC_DEFAULT_ARGS` 환경변수에서 기본 rsync 옵션을 읽는다.
/// 공백으로 분리하며, 미설정/공백이면 빈 벡터를 반환한다.
/// 코어는 rsync passthrough를 유지하되, 사용자가 매번 `-a` 등을 타이핑하지 않도록
/// 기본 옵션을 환경변수로만 선택 주입하는 용도다(설계 모델 C).
pub fn default_rsync_args_from_env() -> Vec<String> {
    std::env::var("QUICSYNC_DEFAULT_ARGS")
        .ok()
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

/// 디렉토리 전송 플래그가 옵션에 있는지 판별한다.
/// `-a`/`-r`/`-d`(단축 묶음 내부의 a/r/d 포함)와
/// `--archive`/`--recursive`/`--dirs`/`--files-from`을 인식한다.
/// 하나도 없으면 rsync가 디렉토리를 "skipping directory"로 건너뛴다.
pub fn has_recursive_flag(opts: &[String]) -> bool {
    opts.iter().any(|o| {
        if matches!(o.as_str(), "--archive" | "--recursive" | "--dirs")
            || o.starts_with("--files-from")
        {
            return true;
        }
        if o.starts_with("--") {
            return false;
        }
        // 단축 플래그 묶음(`-av`, `-rlt` 등): `=` 앞부분에서 a/r/d 탐색
        match o.strip_prefix('-') {
            Some(short) => short
                .split('=')
                .next()
                .unwrap_or("")
                .chars()
                .any(|c| c == 'a' || c == 'r' || c == 'd'),
            None => false,
        }
    })
}

/// rsync에 전달할 원격 경로 문자열 생성 (`[user@]host:path`)
fn format_remote_spec(remote: &RemoteSpec) -> String {
    match &remote.user {
        Some(user) => format!("{}@{}:{}", user, remote.host, remote.path),
        None => format!("{}:{}", remote.host, remote.path),
    }
}

/// rsync 명령어 인수를 구성한다 (테스트 가능한 순수 함수).
///
/// 반환값: rsync에 전달할 전체 인수 벡터
pub fn build_rsync_args(
    rsync_options: &[String],
    local_paths: &[PathBuf],
    remote: &RemoteSpec,
    proxy_port: u16,
    direction: TransferDirection,
) -> Vec<String> {
    let mut args = Vec::new();

    // --stats가 없으면 자동 추가하여 전송 요약을 항상 표시한다.
    let has_stats = rsync_options.iter().any(|opt| opt == "--stats");
    if !has_stats {
        args.push("--stats".to_string());
    }

    // 사용자 rsync 옵션을 그대로 전달 (Req 7.2)
    args.extend(rsync_options.iter().cloned());

    // --rsh 옵션으로 TCP_Proxy 포트를 통해 연결하도록 리다이렉션 (Req 7.1)
    // rsync는 --rsh 프로그램을 `PROGRAM host rsync --server ...` 형태로 호출한다.
    // quicsync --connect 모드가 localhost:proxy_port에 TCP 연결 후 stdin/stdout relay를 수행한다.
    // current_exe()로 현재 실행 중인 바이너리의 전체 경로를 사용하여
    // PATH에 설치된 다른 버전이 호출되는 문제를 방지한다.
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "quicsync".to_string());
    args.push(format!("--rsh={} --connect {}", exe, proxy_port));

    let remote_spec = format_remote_spec(remote);

    match direction {
        TransferDirection::Push => {
            // rsync [options] --rsh=... <local_paths...> <remote_spec>
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
            args.push(remote_spec);
        }
        TransferDirection::Pull => {
            // rsync [options] --rsh=... <remote_spec> <local_path>
            args.push(remote_spec);
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
        }
    }

    args
}

/// fallback용 순정 rsync-over-SSH 인수를 구성한다.
pub fn build_direct_rsync_args(
    rsync_options: &[String],
    local_paths: &[PathBuf],
    remote: &RemoteSpec,
    direction: TransferDirection,
) -> Vec<String> {
    let mut args = Vec::new();

    let has_stats = rsync_options.iter().any(|opt| opt == "--stats");
    if !has_stats {
        args.push("--stats".to_string());
    }

    args.extend(rsync_options.iter().cloned());
    let remote_spec = format_remote_spec(remote);

    match direction {
        TransferDirection::Push => {
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
            args.push(remote_spec);
        }
        TransferDirection::Pull => {
            args.push(remote_spec);
            for p in local_paths {
                args.push(p.to_string_lossy().to_string());
            }
        }
    }

    args
}

impl RsyncChild {
    /// rsync 자식 프로세스를 실행한다.
    ///
    /// proxy_port를 사용하여 원격 목적지를 로컬 TCP_Proxy로 리다이렉션한다.
    /// stderr를 캡처하여 비정상 종료 시 사용자에게 표시할 수 있도록 한다.
    pub fn spawn(
        rsync_options: &[String],
        local_paths: &[PathBuf],
        remote: &RemoteSpec,
        proxy_port: u16,
        direction: TransferDirection,
        log_path: Option<&std::path::Path>,
    ) -> Result<Self, RsyncError> {
        let mut args = build_rsync_args(rsync_options, local_paths, remote, proxy_port, direction);

        // --web 모니터링용 파일 이벤트 로그. stdout(사용자 출력)은 건드리지 않고
        // 별도 로그 파일에 파일 단위 항목을 기록하게 한다.
        if let Some(p) = log_path {
            args.push(format!("--log-file={}", p.display()));
            args.push("--log-file-format=%i %n".to_string());
        }

        tracing::debug!("rsync spawn: rsync {}", args.join(" "));

        let process = Command::new("rsync")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RsyncError::SpawnFailed(e.to_string()))?;

        Ok(Self { process })
    }

    /// 순정 rsync-over-SSH fallback 프로세스를 실행한다.
    pub fn spawn_direct(
        rsync_options: &[String],
        local_paths: &[PathBuf],
        remote: &RemoteSpec,
        direction: TransferDirection,
    ) -> Result<Self, RsyncError> {
        let args = build_direct_rsync_args(rsync_options, local_paths, remote, direction);
        tracing::debug!("rsync fallback spawn: rsync {}", args.join(" "));

        let process = Command::new("rsync")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RsyncError::SpawnFailed(e.to_string()))?;

        Ok(Self { process })
    }

    /// rsync 종료를 비동기로 대기하고 종료 코드를 반환한다.
    ///
    /// - 종료 코드 0: Ok(0) 반환
    /// - 종료 코드 != 0: stderr를 출력하고 RsyncError::ExitCode 반환 (Req 7.4)
    /// - 시그널로 종료: stderr를 출력하고 RsyncError::Signal 반환
    pub async fn wait(self) -> Result<i32, RsyncError> {
        let output = self
            .process
            .wait_with_output()
            .await
            .map_err(|e| RsyncError::SpawnFailed(e.to_string()))?;

        match output.status.code() {
            Some(0) => Ok(0),
            Some(code) => {
                // 비정상 종료: stderr를 사용자에게 표시 (Req 7.4)
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    eprintln!("{}", stderr);
                }
                Err(RsyncError::ExitCode(code))
            }
            None => {
                // 시그널에 의한 종료
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    eprintln!("{}", stderr);
                }
                Err(RsyncError::Signal)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- has_recursive_flag 단위 테스트 ---

    #[test]
    fn has_recursive_flag_present() {
        for o in [
            "-a", "-avz", "-r", "-rlt", "-d", "--archive", "--recursive", "--dirs",
            "--files-from=list.txt",
        ] {
            assert!(has_recursive_flag(&[o.to_string()]), "expected true for {o}");
        }
        // 다른 옵션 사이에 섞여 있어도 인식한다
        assert!(has_recursive_flag(&["-v".into(), "--delete".into(), "-a".into()]));
    }

    #[test]
    fn has_recursive_flag_absent() {
        assert!(!has_recursive_flag(&[]));
        for o in [
            "-v", "-vvv", "-z", "-P", "--delete", "--stats", "--progress", "--exclude=*.tmp",
        ] {
            assert!(!has_recursive_flag(&[o.to_string()]), "expected false for {o}");
        }
    }

    // --- parse_log_file_line 단위 테스트 ---

    #[test]
    fn parse_log_line_transferred_file() {
        let line = "2026/05/22 12:34:56 [12345] >f+++++++++ dir/file.txt";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(item.is_file);
        assert_eq!(item.name, "dir/file.txt");
    }

    #[test]
    fn parse_log_line_directory_is_not_file() {
        let line = "2026/05/22 12:34:56 [12345] cd+++++++++ subdir/";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(!item.is_file);
        assert_eq!(item.name, "subdir/");
    }

    #[test]
    fn parse_log_line_preserves_spaces_in_name() {
        let line = "2026/05/22 12:34:56 [99] >f+++++++++ my file with spaces.bin";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(item.is_file);
        assert_eq!(item.name, "my file with spaces.bin");
    }

    #[test]
    fn parse_log_line_unchanged_file_still_counts() {
        // 변경 없는 파일(.f.........)도 처리 대상이므로 파일로 센다. (itemize는 11자 고정폭)
        let line = "2026/05/22 12:34:56 [1] .f......... unchanged.txt";
        let item = parse_log_file_line(line).expect("should parse");
        assert!(item.is_file);
        assert_eq!(item.name, "unchanged.txt");
    }

    #[test]
    fn parse_log_line_non_transfer_message_is_none() {
        assert!(parse_log_file_line("2026/05/22 12:34:56 [1] building file list").is_none());
        assert!(parse_log_file_line("2026/05/22 12:34:56 [1] sent 1000 bytes").is_none());
        assert!(parse_log_file_line("").is_none());
    }

    // Feature: quicsync-tunnel-mvp, Property 9: rsync 명령어 구성 정확성
    // **Validates: Requirements 7.1, 7.2**

    /// 영문 소문자+숫자로 구성된 비어있지 않은 문자열 생성기
    fn alphanumeric_str(min: usize, max: usize) -> impl Strategy<Value = String> {
        prop::string::string_regex(&format!("[a-z0-9]{{{},{}}}", min, max)).expect("valid regex")
    }

    /// rsync 옵션 생성기: `-` 로 시작하는 문자열
    fn rsync_option() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("-a".to_string()),
            Just("-v".to_string()),
            Just("-z".to_string()),
            Just("-r".to_string()),
            Just("--delete".to_string()),
            Just("--progress".to_string()),
            Just("--exclude=*.tmp".to_string()),
            Just("--compress".to_string()),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Property 9: 임의의 유효한 입력에 대해 build_rsync_args가 구성하는 인수가
        /// (1) 모든 사용자 옵션을 순서대로 포함하고
        /// (2) --rsh에 올바른 포트가 포함되며
        /// (3) remote spec이 올바르게 포맷되어야 한다.
        #[test]
        fn rsync_args_construction_correctness(
            user in prop::option::of(alphanumeric_str(1, 8)),
            host in alphanumeric_str(1, 12),
            remote_path in alphanumeric_str(1, 10).prop_map(|s| format!("/{}", s)),
            local_path in alphanumeric_str(1, 10).prop_map(|s| format!("/{}", s)),
            port in 1u16..=65535u16,
            direction in prop_oneof![
                Just(TransferDirection::Push),
                Just(TransferDirection::Pull),
            ],
            options in prop::collection::vec(rsync_option(), 0..6),
        ) {
            let remote = RemoteSpec {
                user: user.clone(),
                host: host.clone(),
                path: remote_path.clone(),
            };

            let args = build_rsync_args(
                &options,
                &[PathBuf::from(&local_path)],
                &remote,
                port,
                direction,
            );

            // --stats 자동 추가 여부 판별
            let has_stats = options.iter().any(|opt| opt == "--stats");
            let auto_prefix: usize = !has_stats as usize;
            let n = options.len();

            // (1) 사용자 옵션이 순서대로 args에 위치 (auto_prefix 오프셋 적용)
            for (i, opt) in options.iter().enumerate() {
                prop_assert_eq!(&args[auto_prefix + i], opt, "option at index {} mismatch", i);
            }

            // (2) --rsh 옵션이 사용자 옵션 바로 뒤에 위치하며 올바른 포트 포함
            let rsh = &args[auto_prefix + n];
            let expected_suffix = format!(" --connect {}", port);
            prop_assert!(rsh.starts_with("--rsh="), "rsh should start with --rsh=, got: {}", rsh);
            prop_assert!(rsh.ends_with(&expected_suffix), "rsh should end with '{}', got: {}", expected_suffix, rsh);

            // (3) remote spec 포맷 검증
            let expected_remote = match &user {
                Some(u) => format!("{}@{}:{}", u, host, remote_path),
                None => format!("{}:{}", host, remote_path),
            };

            // (4) 방향에 따른 인수 순서 검증
            match direction {
                TransferDirection::Push => {
                    prop_assert_eq!(&args[auto_prefix + n + 1], &local_path);
                    prop_assert_eq!(&args[auto_prefix + n + 2], &expected_remote);
                }
                TransferDirection::Pull => {
                    prop_assert_eq!(&args[auto_prefix + n + 1], &expected_remote);
                    prop_assert_eq!(&args[auto_prefix + n + 2], &local_path);
                }
            }

            // 총 인수 개수: (auto_prefix) + options + --rsh + local + remote
            prop_assert_eq!(args.len(), auto_prefix + n + 3);
        }
    }

    #[test]
    fn build_args_push_with_user() {
        let remote = RemoteSpec {
            user: Some("deploy".into()),
            host: "server.example.com".into(),
            path: "/data/backup".into(),
        };
        let args = build_rsync_args(
            &["-avz".into(), "--delete".into()],
            &[PathBuf::from("/local/src")],
            &remote,
            12345,
            TransferDirection::Push,
        );

        // -avz에 'a' 포함 → -a 자동 추가 안 됨, --stats 자동 추가됨
        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "-avz");
        assert_eq!(args[2], "--delete");
        assert!(args[3].starts_with("--rsh=") && args[3].ends_with(" --connect 12345"));
        assert_eq!(args[4], "/local/src");
        assert_eq!(args[5], "deploy@server.example.com:/data/backup");
    }

    #[test]
    fn build_args_pull_without_user() {
        let remote = RemoteSpec {
            user: None,
            host: "10.0.0.1".into(),
            path: "/remote/files".into(),
        };
        let args = build_rsync_args(
            &["-r".into()],
            &[PathBuf::from("/local/dest")],
            &remote,
            54321,
            TransferDirection::Pull,
        );

        // -r에 'r' 포함 → -a 자동 추가 안 됨, --stats 자동 추가됨
        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "-r");
        assert!(args[2].starts_with("--rsh=") && args[2].ends_with(" --connect 54321"));
        // Pull: remote first, then local
        assert_eq!(args[3], "10.0.0.1:/remote/files");
        assert_eq!(args[4], "/local/dest");
    }

    #[test]
    fn build_args_no_options() {
        let remote = RemoteSpec {
            user: None,
            host: "host".into(),
            path: "/path".into(),
        };
        let args = build_rsync_args(
            &[],
            &[PathBuf::from("/local")],
            &remote,
            8080,
            TransferDirection::Push,
        );

        // 옵션 없으면 --stats만 자동 추가된다
        assert_eq!(args[0], "--stats");
        assert!(args[1].starts_with("--rsh=") && args[1].ends_with(" --connect 8080"));
        assert_eq!(args[2], "/local");
        assert_eq!(args[3], "host:/path");
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn build_direct_args_push_has_no_rsh() {
        let remote = RemoteSpec {
            user: Some("deploy".into()),
            host: "server".into(),
            path: "/dst".into(),
        };
        let args = build_direct_rsync_args(
            &["-a".into()],
            &[PathBuf::from("/src")],
            &remote,
            TransferDirection::Push,
        );

        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "-a");
        assert_eq!(args[2], "/src");
        assert_eq!(args[3], "deploy@server:/dst");
        assert!(!args.iter().any(|a| a.starts_with("--rsh=")));
    }

    #[test]
    fn build_direct_args_pull_has_no_rsh() {
        let remote = RemoteSpec {
            user: None,
            host: "server".into(),
            path: "/src".into(),
        };
        let args = build_direct_rsync_args(
            &[],
            &[PathBuf::from("/dst")],
            &remote,
            TransferDirection::Pull,
        );

        assert_eq!(args[0], "--stats");
        assert_eq!(args[1], "server:/src");
        assert_eq!(args[2], "/dst");
        assert!(!args.iter().any(|a| a.starts_with("--rsh=")));
    }

    #[test]
    fn build_args_preserves_all_options() {
        let remote = RemoteSpec {
            user: Some("root".into()),
            host: "box".into(),
            path: "/".into(),
        };
        let options: Vec<String> = vec![
            "-a",
            "-v",
            "-z",
            "--delete",
            "--exclude=*.tmp",
            "--progress",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let args = build_rsync_args(
            &options,
            &[PathBuf::from("/src")],
            &remote,
            9999,
            TransferDirection::Push,
        );

        // -a 포함 → -a 자동 추가 안 됨, --stats 자동 추가됨 (인덱스 +1)
        // 모든 사용자 옵션이 순서대로 보존되어야 한다 (Req 7.2)
        assert_eq!(args[0], "--stats");
        for (i, opt) in options.iter().enumerate() {
            assert_eq!(&args[i + 1], opt);
        }
        // --rsh 옵션이 사용자 옵션 뒤에 위치
        assert!(
            args[options.len() + 1].starts_with("--rsh=")
                && args[options.len() + 1].ends_with(" --connect 9999")
        );
    }

    #[test]
    fn format_remote_spec_with_user() {
        let remote = RemoteSpec {
            user: Some("admin".into()),
            host: "srv".into(),
            path: "/data".into(),
        };
        assert_eq!(format_remote_spec(&remote), "admin@srv:/data");
    }

    #[test]
    fn format_remote_spec_without_user() {
        let remote = RemoteSpec {
            user: None,
            host: "srv".into(),
            path: "/data".into(),
        };
        assert_eq!(format_remote_spec(&remote), "srv:/data");
    }

    // Feature: quicsync-tunnel-mvp, Property 10: 종료 코드 전파
    // **Validates: Requirements 7.3, 7.4**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Property 10: 임의의 u8 종료 코드(0–255)에 대해,
        /// 자식 프로세스가 해당 코드로 종료하면 wait()가 올바르게 전파해야 한다.
        /// - 코드 0: Ok(0)
        /// - 코드 1–255: Err(RsyncError::ExitCode(code))
        #[test]
        fn exit_code_propagation(code in 0u8..=255u8) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let process = Command::new("sh")
                    .arg("-c")
                    .arg(format!("exit {}", code))
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("failed to spawn sh");

                let child = RsyncChild { process };
                let result = child.wait().await;
                let code_i32 = code as i32;

                match code {
                    0 => {
                        prop_assert!(result.is_ok(), "exit 0 should return Ok, got {:?}", result);
                        prop_assert_eq!(result.unwrap(), 0);
                    }
                    _ => {
                        prop_assert!(result.is_err(), "exit {} should return Err, got {:?}", code, result);
                        match result.unwrap_err() {
                            RsyncError::ExitCode(c) => {
                                prop_assert_eq!(c, code_i32, "exit code mismatch: expected {}, got {}", code_i32, c);
                            }
                            other => {
                                prop_assert!(false, "expected ExitCode({}), got {:?}", code_i32, other);
                            }
                        }
                    }
                }

                Ok::<_, proptest::test_runner::TestCaseError>(())
            })?;
        }
    }
}
