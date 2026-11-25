#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use lilia_protocol::{read_frame, write_frame, Codec, Operation, Request, Response, ResponseValue};

/// How long the daemon may take to start, answer or stop. An upper bound, not a pace: a debug
/// build starting while other test binaries compile and run in parallel (`nx run-many`) has
/// taken more than 5 s; on an idle machine each cycle takes about 2 s.
const PROCESS_DEADLINE_SECS: u64 = 60;
const PROCESS_DEADLINE: Duration = Duration::from_secs(PROCESS_DEADLINE_SECS);

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lilia-daemon"));
    command
        .arg("--database")
        .arg(directory.join("test.lilia"))
        .arg("--endpoint")
        .arg(directory.join("daemon.sock"))
        .arg("--token-file")
        .arg(directory.join("token"));
    command
}

async fn request(directory: &Path, token: &str, operation: Operation) -> ResponseValue {
    let mut stream = tokio::net::UnixStream::connect(directory.join("daemon.sock"))
        .await
        .unwrap();
    Codec::Rkyv.announce(&mut stream).await.unwrap();
    let request = Request {
        id: uuid::Uuid::new_v4().to_string(),
        token: token.into(),
        deadline_ms: Some(PROCESS_DEADLINE_SECS * 1_000),
        operation,
    };
    write_frame(&mut stream, &request).await.unwrap();
    let response: Response =
        tokio::time::timeout(PROCESS_DEADLINE, read_frame::<Response>(&mut stream))
            .await
            .unwrap()
            .unwrap();
    response.result.unwrap()
}

#[tokio::test]
async fn duplicate_start_preserves_live_credentials_and_clean_restart_rotates_them() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let mut previous_token = None;
    for _ in 0..2 {
        let mut daemon = ChildGuard(
            command(&directory)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let token = tokio::time::timeout(PROCESS_DEADLINE, async {
            loop {
                assert!(
                    daemon.0.try_wait().unwrap().is_none(),
                    "daemon exited during startup"
                );
                if let Ok(token) = fs::read_to_string(directory.join("token")) {
                    if token.len() == 64 && previous_token.as_ref() != Some(&token) {
                        break token;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            request(&directory, &token, Operation::IntegrityCheck).await,
            ResponseValue::Bool(true)
        ));
        let duplicate = command(&directory).output().unwrap();
        assert!(!duplicate.status.success());
        assert!(!String::from_utf8_lossy(&duplicate.stderr).contains(&token));
        assert_eq!(fs::read_to_string(directory.join("token")).unwrap(), token);
        assert!(matches!(
            request(&directory, &token, Operation::Shutdown).await,
            ResponseValue::Shutdown
        ));
        tokio::time::timeout(PROCESS_DEADLINE, async {
            loop {
                if let Some(status) = daemon.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!directory.join("daemon.sock").exists());
        previous_token = Some(token);
    }
}
