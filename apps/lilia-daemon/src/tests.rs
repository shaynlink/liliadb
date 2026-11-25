use super::*;

#[cfg(unix)]
use std::fs;

use lilia_protocol::{PROTOCOL_MAJOR, PROTOCOL_MINOR};

mod gateway;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[cfg(unix)]
#[test]
fn token_symlink_is_rejected_without_touching_target() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let target = directory.join("valuable-file");
    fs::write(&target, b"preserve me").unwrap();
    let token = directory.join("token");
    std::os::unix::fs::symlink(&target, &token).unwrap();
    assert!(create_token(&token).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"preserve me");
}

fn mutation() -> Operation {
    Operation::Batch {
        operations: vec![lilia_core::BatchOperation::KvSet {
            namespace: "test".into(),
            key: b"key".to_vec(),
            value: b"value".to_vec(),
            if_version: Some(0),
            expires_at_ms: None,
        }],
    }
}

fn database() -> (tempfile::TempDir, Arc<Database>) {
    let directory = tempfile::tempdir().unwrap();
    let options = OpenOptions {
        durability: Some("performance".into()),
        profile: Some("constrained".into()),
        ..OpenOptions::default()
    };
    let database = Arc::new(lilia_protocol::open(directory.path(), &options).unwrap());
    (directory, database)
}

/// The value of key `key` of namespace `test`.
fn stored(database: &Database) -> Option<Vec<u8>> {
    database
        .system()
        .kv_get("test", b"key")
        .unwrap()
        .map(|entry| entry.value)
}

/// A client connection to a daemon handler, in `codec`.
async fn connect(
    database: Arc<Database>,
    shutdown: Arc<Notify>,
    codec: Codec,
) -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (mut client, server) = tokio::io::duplex(8192);
    let handler =
        tokio::spawn(async move { handle_stream(server, database, TOKEN, shutdown).await });
    codec.announce(&mut client).await.unwrap();
    (client, handler)
}

async fn exchange(
    stream: &mut tokio::io::DuplexStream,
    token: &str,
    operation: Operation,
    deadline_ms: Option<u64>,
) -> Response {
    let id = Uuid::new_v4().to_string();
    let request = Request {
        id: id.clone(),
        token: token.into(),
        deadline_ms,
        operation,
    };
    lilia_protocol::write_frame(stream, &request).await.unwrap();
    let response: Response =
        tokio::time::timeout(Duration::from_secs(5), lilia_protocol::read_frame(stream))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(response.id, id);
    if let Err(error) = &response.result {
        assert_eq!(error.request_id.to_string(), id);
        assert!(!error.message.contains(TOKEN));
    }
    response
}

/// One request of a JS client: its JSON response and the segments it refers to.
async fn exchange_js(
    stream: &mut tokio::io::DuplexStream,
    token: &str,
    operation: Operation,
) -> (serde_json::Value, Vec<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = lilia_protocol::to_msgpack(&Request {
        id: Uuid::new_v4().to_string(),
        token: token.into(),
        deadline_ms: None,
        operation,
    })
    .unwrap();
    stream
        .write_u32(u32::try_from(request.len()).unwrap())
        .await
        .unwrap();
    stream.write_all(&request).await.unwrap();
    let length = stream.read_u32().await.unwrap() as usize;
    let mut frame = vec![0; length];
    stream.read_exact(&mut frame).await.unwrap();
    let u32_at = |at: usize| u32::from_le_bytes(frame[at..at + 4].try_into().unwrap()) as usize;
    let json = u32_at(0);
    let value = serde_json::from_slice(&frame[4..4 + json]).unwrap();
    let count = u32_at(4 + json);
    let bytes = 8 + json + count * 8;
    let segments = (0..count)
        .map(|i| {
            let (offset, len) = (u32_at(8 + json + i * 8), u32_at(12 + json + i * 8));
            frame[bytes + offset..bytes + offset + len].to_vec()
        })
        .collect();
    (value, segments)
}

#[tokio::test]
async fn js_clients_are_served_like_rkyv_clients() {
    let (_directory, database) = database();
    let shutdown = Arc::new(Notify::new());
    let (mut client, handler) = connect(Arc::clone(&database), shutdown, Codec::Js).await;
    let (written, _) = exchange_js(&mut client, TOKEN, mutation()).await;
    assert_eq!(written["result"]["Ok"]["type"], "mutations");
    let (read, segments) = exchange_js(
        &mut client,
        TOKEN,
        Operation::KvGet {
            namespace: "test".into(),
            key: b"key".to_vec(),
        },
    )
    .await;
    let entry = &read["result"]["Ok"]["value"];
    let value = usize::try_from(entry["value"].as_u64().unwrap()).unwrap();
    assert_eq!(segments[value], b"value");
    assert!(entry["version"].is_string(), "versions are decimal strings");
    let (refused, _) = exchange_js(&mut client, "wrong", Operation::IntegrityCheck).await;
    assert_eq!(refused["result"]["Err"]["code"], "UNAUTHORIZED");
    drop(client);
    handler.await.unwrap().unwrap();
}

#[tokio::test]
async fn rejected_shutdown_never_signals_and_connection_remains_usable() {
    let (_directory, database) = database();
    let shutdown = Arc::new(Notify::new());
    let (mut client, handler) = connect(database, Arc::clone(&shutdown), Codec::Rkyv).await;
    for token in [
        "",
        "wrong",
        "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        let error = exchange(&mut client, token, Operation::Shutdown, None)
            .await
            .result
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Unauthorized);
        assert!(!error.retryable);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), shutdown.notified())
                .await
                .is_err()
        );
    }
    let error = exchange(&mut client, TOKEN, Operation::Shutdown, Some(0))
        .await
        .result
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.retryable);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), shutdown.notified())
            .await
            .is_err()
    );
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::IntegrityCheck, None)
            .await
            .result,
        Ok(ResponseValue::Bool(true))
    ));
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::Shutdown, None)
            .await
            .result,
        Ok(ResponseValue::Shutdown)
    ));
    tokio::time::timeout(Duration::from_secs(1), shutdown.notified())
        .await
        .unwrap();
    handler.await.unwrap().unwrap();
}

#[tokio::test]
async fn rejected_mutations_have_no_effect_and_valid_requests_still_work() {
    let (_directory, database) = database();
    let (mut client, handler) =
        connect(Arc::clone(&database), Arc::new(Notify::new()), Codec::Rkyv).await;
    for (token, deadline, expected) in [
        ("wrong", None, ErrorCode::Unauthorized),
        ("wrong", Some(0), ErrorCode::Unauthorized),
        (TOKEN, Some(0), ErrorCode::Timeout),
    ] {
        assert_eq!(
            exchange(&mut client, token, mutation(), deadline)
                .await
                .result
                .unwrap_err()
                .code,
            expected
        );
        assert!(stored(&database).is_none());
    }
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::Handshake {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR
            },
            None
        )
        .await
        .result,
        Ok(ResponseValue::Handshake { .. })
    ));
    assert_eq!(
        exchange(
            &mut client,
            TOKEN,
            Operation::Handshake {
                major: PROTOCOL_MAJOR + 1,
                minor: 0
            },
            None
        )
        .await
        .result
        .unwrap_err()
        .code,
        ErrorCode::Unsupported
    );
    assert!(matches!(
        exchange(&mut client, TOKEN, mutation(), Some(u64::MAX))
            .await
            .result,
        Ok(ResponseValue::Mutations(_))
    ));
    assert_eq!(stored(&database).as_deref(), Some(&b"value"[..]));
    drop(client);
    handler.await.unwrap().unwrap();
}

#[test]
fn request_expiring_in_worker_queue_never_mutates_database() {
    let (_directory, database) = database();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (release, wait) = std::sync::mpsc::channel();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.await.unwrap();
        let execution = run_blocking(database.clone(), mutation(), Some((Instant::now(), 100)));
        tokio::pin!(execution);
        tokio::select! {
            result = &mut execution => panic!("request must wait for worker: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(150)) => {},
        }
        release.send(()).unwrap();
        blocker.await.unwrap();
        let error = execution.await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
        assert!(stored(&database).is_none());
        assert!(matches!(
            run_blocking(database, mutation(), None).await,
            Ok(ResponseValue::Mutations(_))
        ));
    });
}
