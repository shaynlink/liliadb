use super::*;

use std::fs;

use lilia_protocol::{PROTOCOL_MAJOR, PROTOCOL_MINOR};

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

async fn exchange_in(
    codec: Codec,
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
    codec.write_request(stream, &request).await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), codec.read_response(stream))
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

async fn exchange(
    stream: &mut tokio::io::DuplexStream,
    token: &str,
    operation: Operation,
    deadline_ms: Option<u64>,
) -> Response {
    exchange_in(Codec::Rkyv, stream, token, operation, deadline_ms).await
}

#[tokio::test]
async fn message_pack_clients_are_served_like_rkyv_clients() {
    let (_directory, database) = database();
    let shutdown = Arc::new(Notify::new());
    let (mut client, handler) = connect(Arc::clone(&database), shutdown, Codec::MessagePack).await;
    let codec = Codec::MessagePack;
    assert!(matches!(
        exchange_in(codec, &mut client, TOKEN, mutation(), None)
            .await
            .result,
        Ok(ResponseValue::Mutations(_))
    ));
    let response = exchange_in(
        codec,
        &mut client,
        TOKEN,
        Operation::KvGet {
            namespace: "test".into(),
            key: b"key".to_vec(),
        },
        None,
    )
    .await;
    assert!(
        matches!(response.result, Ok(ResponseValue::Kv(Some(entry))) if entry.value == b"value")
    );
    let refused = exchange_in(codec, &mut client, "wrong", Operation::IntegrityCheck, None).await;
    assert_eq!(refused.result.unwrap_err().code, ErrorCode::Unauthorized);
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

#[tokio::test]
async fn http_gateway_health_is_unauthenticated() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (_dir, db) = database();
    let shutdown = Arc::new(Notify::new());
    let state = http::HttpState {
        database: db,
        token: TOKEN.into(),
        shutdown,
    };
    let app = http::router(state);

    let req = axum::http::Request::builder()
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], "running");
}

#[tokio::test]
async fn http_gateway_rejects_missing_or_bad_token() {
    use tower::ServiceExt;

    let (_dir, db) = database();
    let shutdown = Arc::new(Notify::new());
    let state = http::HttpState {
        database: db,
        token: TOKEN.into(),
        shutdown,
    };
    let app = http::router(state.clone());

    // 1. Missing Authorization header
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/kv/get")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(r#"{"namespace":"test","key":"k"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);

    // 2. Invalid Token
    let app = http::router(state);
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/kv/get")
        .header("Authorization", "Bearer wrong_token_value")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(r#"{"namespace":"test","key":"k"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn http_gateway_kv_json_sql_lifecycle() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (_dir, db) = database();
    let shutdown = Arc::new(Notify::new());
    let state = http::HttpState {
        database: db,
        token: TOKEN.into(),
        shutdown: shutdown.clone(),
    };

    let send = |req: axum::http::Request<axum::body::Body>| {
        let app = http::router(state.clone());
        async move {
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, json)
        }
    };

    let auth_header = format!("Bearer {TOKEN}");

    // 1. KV SET
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/kv/set")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"namespace":"users","key":"alice","value":"developer"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    let kv_version = body["version"].clone();
    assert!(kv_version.is_string(), "a version is a decimal string");

    // 2. KV GET
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/kv/get")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"namespace":"users","key":"alice"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["entry"]["value"], "developer");
    assert_eq!(body["entry"]["version"], kv_version);

    // 3. JSON PUT
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/json/put")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"space":"profiles","id":"u1","value":{"name":"Alice","role":"admin"}}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert!(body["version"].is_string());

    // 4. JSON GET via Path
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/json/profiles/u1")
        .header("Authorization", &auth_header)
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["document"]["value"]["name"], "Alice");

    // 4b. BATCH OPERATIONS
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/batch")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"operations":[{"type":"kv_set","namespace":"users","key":"bob","value":"tester"},{"type":"json_put","space":"profiles","id":"u2","value":{"name":"Bob"}}]}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert!(body["results"][0]["version"].is_string());
    assert!(body["results"][1]["version"].is_string());

    // 5. SQL QUERY
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/sql")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"query":"SELECT id, json_get_str(value, 'name') AS name FROM lilia_json WHERE space = 'profiles' ORDER BY id"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["columns"], serde_json::json!(["id", "name"]));
    assert_eq!(
        body["rows"],
        serde_json::json!([["u1", "Alice"], ["u2", "Bob"]])
    );

    // 5b. GRAPH ENDPOINTS
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/graph/node")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"graph":"social","id":"alice","kind":"user","metadata":{"name":"Alice"}}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/graph/node")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"graph":"social","id":"bob","kind":"user"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/graph/edge")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"graph":"social","source":"alice","target":"bob","kind":"follows"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/graph/neighbors")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"graph":"social","node":"alice","direction":"outgoing"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["neighbors"], serde_json::json!(["bob"]));

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/graph/reachability")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"graph":"social","start_nodes":["alice"],"direction":"outgoing"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["reachable"], serde_json::json!(["alice", "bob"]));

    // 5c. FTS ENDPOINTS
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/fts/index")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"collection":"docs","doc_id":"doc1","content":"liliadb full text search integration"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/fts/search")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"collection":"docs","query":"liliadb"}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["doc_id"], "doc1");

    // FTS Suggest (typo: "liliad" -> "liliadb")
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/fts/suggest")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"collection":"docs","prefix":"liliad","max_distance":1}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    let suggs = body["suggestions"].as_array().unwrap();
    assert_ne!(*suggs, Vec::<serde_json::Value>::new());
    assert_eq!(suggs[0]["term"], "liliadb");

    // FTS Fuzzy Search (typo: "liliad" finds "doc1")
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/fts/fuzzy-search")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"collection":"docs","query":"liliad","max_distance":1}"#,
        ))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
    let fuzzy_hits = body["hits"].as_array().unwrap();
    assert_eq!(fuzzy_hits.len(), 1);
    assert_eq!(fuzzy_hits[0]["doc_id"], "doc1");

    // 6. DOCTOR
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/doctor")
        .header("Authorization", &auth_header)
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);

    // 7. SHUTDOWN
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/shutdown")
        .header("Authorization", &auth_header)
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["ok"], true);
}
