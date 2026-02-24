// rsync 자식 프로세스 실행 및 관리

use std::path::Path;
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
    local_path: &Path,
    remote: &RemoteSpec,
    proxy_port: u16,
    direction: TransferDirection,
) -> Vec<String> {
    let mut args = Vec::new();

    // 사용자 rsync 옵션을 그대로 전달 (Req 7.2)
    args.extend(rsync_options.iter().cloned());

    // --rsh 옵션으로 TCP_Proxy 포트를 통해 연결하도록 리다이렉션 (Req 7.1)
    // rsync는 --rsh 프로그램을 `PROGRAM host rsync --server ...` 형태로 호출한다.
    // quicsync --connect 모드가 localhost:proxy_port에 TCP 연결 후 stdin/stdout relay를 수행한다.
    // rsync가 추가하는 인수(host, --server 등)는 자동으로 무시된다.
    args.push(format!("--rsh=quicsync --connect {}", proxy_port));

    let remote_spec = format_remote_spec(remote);
    let local = local_path.to_string_lossy().to_string();

    match direction {
        TransferDirection::Push => {
            // rsync [options] --rsh=... <local_path> <remote_spec>
            args.push(local);
            args.push(remote_spec);
        }
        TransferDirection::Pull => {
            // rsync [options] --rsh=... <remote_spec> <local_path>
            args.push(remote_spec);
            args.push(local);
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
        local_path: &Path,
        remote: &RemoteSpec,
        proxy_port: u16,
        direction: TransferDirection,
    ) -> Result<Self, RsyncError> {
        let args = build_rsync_args(rsync_options, local_path, remote, proxy_port, direction);

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
                Path::new(&local_path),
                &remote,
                port,
                direction,
            );

            let n = options.len();

            // (1) 사용자 옵션이 순서대로 args 앞부분에 위치
            for (i, opt) in options.iter().enumerate() {
                prop_assert_eq!(&args[i], opt, "option at index {} mismatch", i);
            }

            // (2) --rsh 옵션이 사용자 옵션 바로 뒤에 위치하며 올바른 포트 포함
            let rsh = &args[n];
            let expected_rsh = format!("--rsh=quicsync --connect {}", port);
            prop_assert_eq!(rsh, &expected_rsh);

            // (3) remote spec 포맷 검증
            let expected_remote = match &user {
                Some(u) => format!("{}@{}:{}", u, host, remote_path),
                None => format!("{}:{}", host, remote_path),
            };

            // (4) 방향에 따른 인수 순서 검증
            match direction {
                TransferDirection::Push => {
                    // Push: local_path, remote_spec
                    prop_assert_eq!(&args[n + 1], &local_path);
                    prop_assert_eq!(&args[n + 2], &expected_remote);
                }
                TransferDirection::Pull => {
                    // Pull: remote_spec, local_path
                    prop_assert_eq!(&args[n + 1], &expected_remote);
                    prop_assert_eq!(&args[n + 2], &local_path);
                }
            }

            // 총 인수 개수: options + --rsh + local + remote = n + 3
            prop_assert_eq!(args.len(), n + 3);
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
            Path::new("/local/src"),
            &remote,
            12345,
            TransferDirection::Push,
        );

        assert_eq!(args[0], "-avz");
        assert_eq!(args[1], "--delete");
        assert_eq!(args[2], "--rsh=quicsync --connect 12345");
        assert_eq!(args[3], "/local/src");
        assert_eq!(args[4], "deploy@server.example.com:/data/backup");
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
            Path::new("/local/dest"),
            &remote,
            54321,
            TransferDirection::Pull,
        );

        assert_eq!(args[0], "-r");
        assert_eq!(args[1], "--rsh=quicsync --connect 54321");
        // Pull: remote first, then local
        assert_eq!(args[2], "10.0.0.1:/remote/files");
        assert_eq!(args[3], "/local/dest");
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
            Path::new("/local"),
            &remote,
            8080,
            TransferDirection::Push,
        );

        assert_eq!(args[0], "--rsh=quicsync --connect 8080");
        assert_eq!(args[1], "/local");
        assert_eq!(args[2], "host:/path");
        assert_eq!(args.len(), 3);
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
            Path::new("/src"),
            &remote,
            9999,
            TransferDirection::Push,
        );

        // 모든 사용자 옵션이 순서대로 보존되어야 한다 (Req 7.2)
        for (i, opt) in options.iter().enumerate() {
            assert_eq!(&args[i], opt);
        }
        // --rsh 옵션이 사용자 옵션 뒤에 위치
        assert_eq!(args[options.len()], "--rsh=quicsync --connect 9999");
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
