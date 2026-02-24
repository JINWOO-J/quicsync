// rsync 자식 프로세스 실행 및 관리

use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::error::RsyncError;
use crate::types::{RemoteSpec, TransferDirection};

pub struct RsyncChild {
    pub(crate) process: Child,
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

    // 사용자가 -a, -r, --archive, --recursive를 지정하지 않았으면 -a를 기본 추가.
    // rsync는 -r 없이 디렉토리를 건너뛰므로, quicsync의 합리적 기본값으로 -a를 사용한다.
    let has_recursive = rsync_options.iter().any(|opt| {
        opt == "-r"
            || opt == "-a"
            || opt == "--recursive"
            || opt == "--archive"
            || (opt.starts_with('-')
                && !opt.starts_with("--")
                && (opt.contains('r') || opt.contains('a')))
    });
    if !has_recursive {
        args.push("-a".to_string());
    }

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
    ) -> Result<Self, RsyncError> {
        let args = build_rsync_args(rsync_options, local_paths, remote, proxy_port, direction);
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

    // Feature: quicsync-tunnel-mvp, Property 9: rsync 명령어 구성 정확성
    // **Validates: Requirements 7.1, 7.2**

    /// 영문 소문자+숫자로 구성된 비어있지 않은 문자열 생성기
    fn alphanumeric_str(min: usize, max: usize) -> impl Strategy<Value = String> {
        prop::string::string_regex(&format!("[a-z0-9]{{{},{}}}", min, max))
            .expect("valid regex")
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

            // -a 및 --stats 자동 추가 여부 판별
            let has_recursive = options.iter().any(|opt| {
                opt == "-r"
                    || opt == "-a"
                    || opt == "--recursive"
                    || opt == "--archive"
                    || (opt.starts_with('-')
                        && !opt.starts_with("--")
                        && (opt.contains('r') || opt.contains('a')))
            });
            let has_stats = options.iter().any(|opt| opt == "--stats");
            let auto_prefix: usize = (!has_recursive as usize) + (!has_stats as usize);
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

        // 옵션 없으면 -a와 --stats가 자동 추가된다
        assert_eq!(args[0], "-a");
        assert_eq!(args[1], "--stats");
        assert!(args[2].starts_with("--rsh=") && args[2].ends_with(" --connect 8080"));
        assert_eq!(args[3], "/local");
        assert_eq!(args[4], "host:/path");
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn build_args_preserves_all_options() {
        let remote = RemoteSpec {
            user: Some("root".into()),
            host: "box".into(),
            path: "/".into(),
        };
        let options: Vec<String> = vec![
            "-a", "-v", "-z", "--delete", "--exclude=*.tmp", "--progress",
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
        assert!(args[options.len() + 1].starts_with("--rsh=") && args[options.len() + 1].ends_with(" --connect 9999"));
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
