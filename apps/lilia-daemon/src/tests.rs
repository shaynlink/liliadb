use super::*;

use std::fs;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn unsigned_plugins_require_explicit_development_mode() {
    let arguments = Arguments {
        database: PathBuf::from("database.lilia"),
        endpoint: PathBuf::from("endpoint"),
        token_file: PathBuf::from("token"),
        plugins: Vec::new(),
        trusted_keys: Vec::new(),
        plugin_root: None,
        allow_unsigned_plugins: true,
        development: false,
        http_port: None,
        http_bind: None,
        encryption_key: None,
        require_sandboxed_plugins: false,
        plugin_memory_limit_mb: 64,
        plugin_fuel_limit: 10_000_000,
    };
    let error = load_plugins(&arguments).expect_err("unsigned mode must fail closed");
    assert!(error.to_string().contains("--development"));
}

#[test]
fn production_rejects_plugin_root_and_inline_key_overrides() {
    let mut arguments = Arguments {
        database: PathBuf::from("database.lilia"),
        endpoint: PathBuf::from("endpoint"),
        token_file: PathBuf::from("token"),
        plugins: Vec::new(),
        trusted_keys: Vec::new(),
        plugin_root: Some(PathBuf::from("custom")),
        allow_unsigned_plugins: false,
        development: false,
        http_port: None,
        http_bind: None,
        encryption_key: None,
        require_sandboxed_plugins: false,
        plugin_memory_limit_mb: 64,
        plugin_fuel_limit: 10_000_000,
    };
    assert!(load_plugins(&arguments)
        .expect_err("root override must fail")
        .to_string()
        .contains("--development"));
    arguments.plugin_root = None;
    arguments.trusted_keys.push("key".into());
    assert!(load_plugins(&arguments)
        .expect_err("key override must fail")
        .to_string()
        .contains("--development"));
}

#[test]
fn development_root_still_rejects_plugins_outside_it() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path().join("plugins");
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).expect("outside package");
    let arguments = Arguments {
        database: PathBuf::from("database.lilia"),
        endpoint: PathBuf::from("endpoint"),
        token_file: PathBuf::from("token"),
        plugins: vec![outside],
        trusted_keys: Vec::new(),
        plugin_root: Some(root),
        allow_unsigned_plugins: true,
        development: true,
        http_port: None,
        http_bind: None,
        encryption_key: None,
        require_sandboxed_plugins: false,
        plugin_memory_limit_mb: 64,
        plugin_fuel_limit: 10_000_000,
    };

    assert!(load_plugins(&arguments)
        .expect_err("outside package must fail")
        .to_string()
        .contains("outside the authorized plugin root"));
}

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
    let database = Arc::new(
        Database::open(DatabaseOptions::durable(
            directory.path().join("test.lilia"),
        ))
        .unwrap(),
    );
    (directory, database)
}

async fn exchange(
    stream: &mut tokio::io::DuplexStream,
    token: &str,
    operation: Operation,
    deadline_ms: Option<u64>,
) -> Response {
    let id = Uuid::new_v4().to_string();
    write_frame(
        stream,
        &Request {
            id: id.clone(),
            token: token.into(),
            deadline_ms,
            operation,
        },
    )
    .await
    .unwrap();
    let response: Response = tokio::time::timeout(Duration::from_secs(5), read_frame(stream))
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

#[tokio::test]
async fn rejected_shutdown_never_signals_and_connection_remains_usable() {
    let (_directory, database) = database();
    let shutdown = Arc::new(Notify::new());
    let (mut client, server) = tokio::io::duplex(8192);
    let handler = tokio::spawn(handle_stream(server, database, TOKEN, shutdown.clone()));
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
        Ok(ResponseValue::Integrity(true))
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
    let (mut client, server) = tokio::io::duplex(8192);
    let handler = tokio::spawn(handle_stream(
        server,
        database.clone(),
        TOKEN,
        Arc::new(Notify::new()),
    ));
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
        assert!(database.kv_get("test", b"key").unwrap().is_none());
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
    assert_eq!(
        database.kv_get("test", b"key").unwrap().unwrap().value,
        b"value"
    );
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
        let execution = execute(database.clone(), mutation(), Instant::now(), Some(100));
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
        assert!(database.kv_get("test", b"key").unwrap().is_none());
        assert!(matches!(
            execute(database, mutation(), Instant::now(), None).await,
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
        database: db.clone(),
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
    assert_eq!(body["version"], "1");

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
    assert_eq!(body["entry"]["version"], "1");

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
    assert_eq!(body["version"], "1");

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
    assert_eq!(body["results"][0]["version"], "1");
    assert_eq!(body["results"][1]["version"], "1");

    // 5. SQL QUERY
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/sql")
        .header("Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            r#"{"query":"SELECT id, json_extract(value, '$.name') as name FROM lilia_json WHERE space = 'profiles'"}"#,
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

#[tokio::test]
async fn test_ipc_streaming_subscription() {
    use lilia_core::{BatchOperation, DatabaseEvent};
    use lilia_protocol::{read_frame, write_frame, Operation, Request, Response, ResponseValue};

    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        Database::open(DatabaseOptions::durable(dir.path().join("ipc_watch.lilia"))).unwrap(),
    );
    let token = "test_secret_token";
    let shutdown = Arc::new(Notify::new());

    let (mut client, server) = tokio::io::duplex(8_192);

    let db_clone = Arc::clone(&db);
    let shutdown_clone = Arc::clone(&shutdown);
    let token_clone = token.to_string();
    tokio::spawn(async move {
        let _ = handle_stream(server, db_clone, &token_clone, shutdown_clone).await;
    });

    // Send Subscribe request
    let sub_req = Request {
        id: "sub-1".into(),
        token: token.into(),
        deadline_ms: None,
        operation: Operation::Subscribe {
            namespace: Some("live_ns".into()),
        },
    };
    write_frame(&mut client, &sub_req).await.unwrap();

    // Read Ack response
    let ack_resp: Response = read_frame(&mut client).await.unwrap();
    assert_eq!(ack_resp.id, "sub-1");
    assert!(matches!(ack_resp.result.unwrap(), ResponseValue::Ack));

    // Perform mutations on database (one matching, one non-matching)
    db.batch(&[
        BatchOperation::KvSet {
            namespace: "ignored_ns".into(),
            key: b"k1".to_vec(),
            value: b"v1".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
        BatchOperation::KvSet {
            namespace: "live_ns".into(),
            key: b"order_99".to_vec(),
            value: b"created".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
    ])
    .unwrap();

    // Client should receive only the live_ns event
    let event_resp: Response = read_frame(&mut client).await.unwrap();
    assert_eq!(event_resp.id, "sub-1");
    match event_resp.result.unwrap() {
        ResponseValue::Event(DatabaseEvent::KvMutated {
            namespace,
            key,
            version,
            deleted,
        }) => {
            assert_eq!(namespace, "live_ns");
            assert_eq!(key, b"order_99");
            assert_eq!(version, Some(1));
            assert!(!deleted);
        }
        other => panic!("expected KvMutated event, got {other:?}"),
    }

    // Drop client to verify clean shutdown
    drop(client);
    shutdown.notify_one();
}

#[tokio::test]
async fn test_http_sse_watch_endpoint() {
    use tower::ServiceExt;

    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        Database::open(DatabaseOptions::durable(
            dir.path().join("http_watch.lilia"),
        ))
        .unwrap(),
    );
    let token = "test_http_token".to_string();
    let shutdown = Arc::new(Notify::new());

    let app = http::router(http::HttpState {
        database: Arc::clone(&db),
        token: token.clone(),
        shutdown: Arc::clone(&shutdown),
    });

    // 1. Unauthenticated request should fail
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/watch")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);

    // 2. Request with token in query string (for EventSource) should succeed with text/event-stream
    let req = axum::http::Request::builder()
        .method("GET")
        .uri(format!("/v1/watch?namespace=orders&token={token}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));
}

#[test]
fn require_sandboxed_plugins_accepts_wasm_plugin() {
    const CODEC_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "lilia_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "lilia_dealloc") (param i32) (param i32))
  (func (export "codec_encode") (param i32) (param i32) (param i32) (param i32) (param i32) (result i32) (i32.const 0))
  (func (export "codec_decode") (param i32) (param i32) (param i32) (param i32) (param i32) (result i32) (i32.const 0))
)
"#;
    let temp = tempfile::tempdir().unwrap();
    let wasm_file = temp.path().join("plugin.wasm");
    fs::write(&wasm_file, CODEC_WAT.as_bytes()).unwrap();

    let pkg_out = temp.path().join("pkg");
    let pack_opts = lilia_plugin_api::PackOptions {
        name: "sandboxed-test".into(),
        version: "1.0.0".into(),
        capabilities: vec![lilia_plugin_api::Capability::Codec],
        engine_requirement: None,
        target: Some(lilia_plugin_api::WASM_TARGET_WASIP1.into()),
    };
    let packed = lilia_plugin_api::pack_plugin(&wasm_file, &pkg_out, &pack_opts).unwrap();

    let sk = lilia_plugin_api::official_release_signing_key();
    let vk = lilia_plugin_api::official_release_key();
    lilia_plugin_api::sign_package(&packed, &sk).unwrap();

    let root = temp.path().join("plugin_root");
    let installed =
        lilia_plugin_api::install_verified(&packed, &root, &[vk], false, false).unwrap();

    let arguments = Arguments {
        database: temp.path().join("db.lilia"),
        endpoint: temp.path().join("ipc.sock"),
        token_file: temp.path().join("token"),
        plugins: vec![installed],
        trusted_keys: Vec::new(),
        plugin_root: Some(root),
        allow_unsigned_plugins: true,
        development: true,
        http_port: None,
        http_bind: None,
        encryption_key: None,
        require_sandboxed_plugins: true,
        plugin_memory_limit_mb: 64,
        plugin_fuel_limit: 10_000_000,
    };

    let plugins =
        load_plugins(&arguments).expect("wasm plugin must be accepted when sandboxing required");
    assert_eq!(plugins.len(), 1);
    assert!(plugins[0].is_sandboxed());
    assert_eq!(plugins[0].name(), "sandboxed-test");
}

#[test]
fn require_sandboxed_plugins_rejects_native_plugin() {
    let temp = tempfile::tempdir().unwrap();
    let lib_file = temp.path().join(if cfg!(target_os = "windows") {
        "plugin.dll"
    } else if cfg!(target_os = "macos") {
        "plugin.dylib"
    } else {
        "plugin.so"
    });
    fs::write(&lib_file, b"fake native shared library content").unwrap();

    let pkg_out = temp.path().join("pkg");
    let pack_opts = lilia_plugin_api::PackOptions {
        name: "native-test".into(),
        version: "1.0.0".into(),
        capabilities: vec![lilia_plugin_api::Capability::Codec],
        engine_requirement: None,
        target: Some(lilia_plugin_api::host_target().into()),
    };
    let packed = lilia_plugin_api::pack_plugin(&lib_file, &pkg_out, &pack_opts).unwrap();

    let sk = lilia_plugin_api::official_release_signing_key();
    let vk = lilia_plugin_api::official_release_key();
    lilia_plugin_api::sign_package(&packed, &sk).unwrap();

    let root = temp.path().join("plugin_root");
    let installed =
        lilia_plugin_api::install_verified(&packed, &root, &[vk], false, false).unwrap();

    let arguments = Arguments {
        database: temp.path().join("db.lilia"),
        endpoint: temp.path().join("ipc.sock"),
        token_file: temp.path().join("token"),
        plugins: vec![installed],
        trusted_keys: Vec::new(),
        plugin_root: Some(root),
        allow_unsigned_plugins: true,
        development: true,
        http_port: None,
        http_bind: None,
        encryption_key: None,
        require_sandboxed_plugins: true,
        plugin_memory_limit_mb: 64,
        plugin_fuel_limit: 10_000_000,
    };

    let err = load_plugins(&arguments)
        .expect_err("native plugin must be rejected when sandboxing required");
    assert!(
        err.to_string()
            .contains("requires WebAssembly sandboxed plugins"),
        "error should reject native plugin: {err}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_ipc_extended_multimodel_dispatch() {
    use lilia_core::{
        circle_to_geopoly_json, BatchOperation, CentralityType, ComponentType,
        ConflictResolutionStrategy, Direction, GpsPoint, HnswConfig, PageRankConfig,
        TimeSeriesPoint, VectorElementType, VectorMetric,
    };
    use lilia_protocol::{Operation, ResponseValue};

    let (_dir, db) = database();
    let shutdown = Arc::new(Notify::new());
    let (mut client, server) = tokio::io::duplex(65_536);
    let handler = tokio::spawn(handle_stream(
        server,
        Arc::clone(&db),
        TOKEN,
        Arc::clone(&shutdown),
    ));

    // 1. Diagnostics
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::QuickCheck, None)
            .await
            .result
            .unwrap(),
        ResponseValue::Bool(true)
    ));
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::Checkpoint, None)
            .await
            .result
            .unwrap(),
        ResponseValue::Ack
    ));
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::CheckpointStats, None)
            .await
            .result
            .unwrap(),
        ResponseValue::CheckpointStats(_)
    ));
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::StorageStats, None)
            .await
            .result
            .unwrap(),
        ResponseValue::TableStorageStats(stats) if !stats.is_empty()
    ));

    // 2. JSON Extended + Time-Travel + Zstd Dict + Undo/Redo + FTS Delete
    exchange(
        &mut client,
        TOKEN,
        Operation::Batch {
            operations: vec![
                BatchOperation::JsonPut {
                    space: "users".into(),
                    id: "u1".into(),
                    value: serde_json::json!({"name": "Alice", "role": "admin", "city": "Paris"}),
                    if_version: None,
                },
                BatchOperation::JsonPut {
                    space: "users".into(),
                    id: "u1".into(),
                    value: serde_json::json!({"name": "Alice", "role": "owner", "city": "Paris"}),
                    if_version: None,
                },
            ],
        },
        None,
    )
    .await
    .result
    .unwrap();

    match exchange(
        &mut client,
        TOKEN,
        Operation::JsonExtract {
            space: "users".into(),
            id: "u1".into(),
            path: "$.role".into(),
        },
        None,
    )
    .await
    .result
    .unwrap()
    {
        ResponseValue::JsonExtracted(Some(v)) => assert_eq!(v, serde_json::json!("owner")),
        other => panic!("unexpected json extract: {other:?}"),
    }
    match exchange(
        &mut client,
        TOKEN,
        Operation::JsonScanPrefix {
            space: "users".into(),
            prefix: "u".into(),
            after_id: None,
            limit: 10,
        },
        None,
    )
    .await
    .result
    .unwrap()
    {
        ResponseValue::JsonPage(page) => assert_eq!(page.len(), 1),
        other => panic!("unexpected scan prefix: {other:?}"),
    }
    match exchange(
        &mut client,
        TOKEN,
        Operation::JsonGetAtVersion {
            space: "users".into(),
            id: "u1".into(),
            version: 1,
        },
        None,
    )
    .await
    .result
    .unwrap()
    {
        ResponseValue::Json(Some(entry)) => {
            assert_eq!(entry.value()["role"], serde_json::json!("admin"));
        }
        other => panic!("unexpected get_at_version: {other:?}"),
    }
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::JsonHistory {
                space: "users".into(),
                id: "u1".into()
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::JsonRevisions(revs) if revs.len() == 2
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::JsonDiff {
                space: "users".into(),
                id: "u1".into(),
                from_version: 1,
                to_version: 2,
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::JsonValue(diff) if diff.as_array().is_some_and(|a| !a.is_empty())
    ));
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::UndoHistory, None)
            .await
            .result
            .unwrap(),
        ResponseValue::UndoEntries(entries) if !entries.is_empty()
    ));
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::Undo, None)
            .await
            .result
            .unwrap(),
        ResponseValue::UndoResult(Some(_))
    ));
    assert!(matches!(
        exchange(&mut client, TOKEN, Operation::Redo, None)
            .await
            .result
            .unwrap(),
        ResponseValue::RedoResult(Some(_))
    ));
    exchange(
        &mut client,
        TOKEN,
        Operation::Batch {
            operations: vec![BatchOperation::FtsIndex {
                collection: "docs".into(),
                doc_id: "d1".into(),
                content: "lilia rust sqlite multimodel".into(),
            }],
        },
        None,
    )
    .await
    .result
    .unwrap();
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::FtsDelete {
                collection: "docs".into(),
                doc_id: "d1".into(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::Bool(true)
    ));

    // 3. Graph Extended, Analytics & Cypher + Vector HNSW & Filtered
    exchange(
        &mut client,
        TOKEN,
        Operation::Batch {
            operations: vec![
                BatchOperation::GraphSetNode {
                    graph: "g1".into(),
                    id: "a".into(),
                    kind: "service".into(),
                    metadata: serde_json::json!({"x": 0.0, "y": 0.0}),
                    doc_space: Some("users".into()),
                    doc_id: Some("u1".into()),
                },
                BatchOperation::GraphSetNode {
                    graph: "g1".into(),
                    id: "b".into(),
                    kind: "service".into(),
                    metadata: serde_json::json!({"x": 1.0, "y": 1.0}),
                    doc_space: None,
                    doc_id: None,
                },
                BatchOperation::GraphAddEdge {
                    graph: "g1".into(),
                    source: "a".into(),
                    target: "b".into(),
                    kind: "calls".into(),
                    weight: 2.5,
                },
            ],
        },
        None,
    )
    .await
    .result
    .unwrap();

    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::GraphGetNodeDocument {
                graph: "g1".into(),
                id: "a".into(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::JsonExtracted(Some(_))
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::GraphShortestPath {
                graph: "g1".into(),
                start: "a".into(),
                target: "b".into(),
                direction: Direction::Outgoing,
                kind_filter: None,
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::WeightedPath(Some(p)) if (p.total_weight - 2.5).abs() < 1e-6
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::GraphPageRank {
                graph: "g1".into(),
                config: PageRankConfig::default(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::NodeScores(scores) if scores.len() == 2
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::GraphCentrality {
                graph: "g1".into(),
                centrality: CentralityType::Degree,
                direction: Direction::Outgoing,
                kind_filter: None,
                normalized: false,
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::NodeScores(scores) if scores.len() == 2
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::GraphConnectedComponents {
                graph: "g1".into(),
                component_type: ComponentType::WeaklyConnected,
                kind_filter: None,
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::CycleList(comps) if comps.len() == 1
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::GraphCypher {
                graph: "g1".into(),
                query: "MATCH (n:service) RETURN n.id".into(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::CypherResult(res) if res.row_count() == 2
    ));

    let v0 = [1.0_f32, 0.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect::<Vec<u8>>();
    exchange(
        &mut client,
        TOKEN,
        Operation::Batch {
            operations: vec![BatchOperation::VectorSet {
                collection: "emb".into(),
                id: "b".into(),
                dim: 2,
                element_type: VectorElementType::Float32,
                vector: v0.clone(),
            }],
        },
        None,
    )
    .await
    .result
    .unwrap();
    exchange(
        &mut client,
        TOKEN,
        Operation::VectorCreateIndex {
            collection: "emb".into(),
            config: HnswConfig {
                dim: 2,
                element_type: VectorElementType::Float32,
                metric: VectorMetric::Cosine,
                ..HnswConfig::default()
            },
        },
        None,
    )
    .await
    .result
    .unwrap();
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::VectorGetIndexInfo {
                collection: "emb".into(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::VectorIndexInfo(Some(_))
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::VectorSearchInGraphNeighbors {
                collection: "emb".into(),
                graph: "g1".into(),
                start_node: "a".into(),
                direction: Direction::Outgoing,
                query_vector: v0,
                dim: 2,
                element_type: VectorElementType::Float32,
                metric: VectorMetric::Cosine,
                limit: 5,
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::VectorHits(hits) if hits.len() == 1 && hits[0].id == "b"
    ));

    // 4. Time-Series, Geo, CRDT, HLC & IVM
    exchange(
        &mut client,
        TOKEN,
        Operation::TsAppendPoints {
            series_id: "cpu.0".into(),
            points: vec![
                TimeSeriesPoint {
                    timestamp: 1_000,
                    value: 10.0,
                },
                TimeSeriesPoint {
                    timestamp: 2_000,
                    value: 20.0,
                },
            ],
            resolution_ms: None,
        },
        None,
    )
    .await
    .result
    .unwrap();
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::TsQueryRange {
                series_id: "cpu.0".into(),
                start_time: 0,
                end_time: 5_000,
                resolution_ms: None,
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::TsPoints(pts) if pts.len() == 2
    ));

    let poly = circle_to_geopoly_json(2.3522, 48.8566, 500.0, 12);
    exchange(
        &mut client,
        TOKEN,
        Operation::GeoAddFence {
            id: "paris".into(),
            name: "Paris Center".into(),
            category: "city".into(),
            polygon_json: poly,
            metadata: None,
        },
        None,
    )
    .await
    .result
    .unwrap();
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::GeoRecordGpsPoints {
                track_id: "trk1".into(),
                points: vec![
                    GpsPoint {
                        timestamp: 1_000,
                        latitude: 48.8566,
                        longitude: 2.3522,
                        altitude: None,
                        speed: None,
                        accuracy: None,
                    },
                ],
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::GeofenceMatches(matches) if !matches.is_empty()
    ));

    let hlc1 = match exchange(&mut client, TOKEN, Operation::HlcNext, None)
        .await
        .result
        .unwrap()
    {
        ResponseValue::Hlc(hlc) => hlc,
        other => panic!("unexpected Hlc: {other:?}"),
    };
    let hlc2 = lilia_core::Hlc::new(hlc1.millis + 1, 0, "remote");
    let outcome = exchange(
        &mut client,
        TOKEN,
        Operation::JsonPutHlc {
            space: "collab".into(),
            id: "doc1".into(),
            value: serde_json::json!({"v": 1}),
            hlc: Some(hlc1),
            parent_hlc: None,
            strategy: ConflictResolutionStrategy::Manual,
        },
        None,
    )
    .await
    .result
    .unwrap();
    assert!(matches!(outcome, ResponseValue::HlcOutcome(_)));
    let _ = exchange(
        &mut client,
        TOKEN,
        Operation::JsonPutHlc {
            space: "collab".into(),
            id: "doc1".into(),
            value: serde_json::json!({"v": 2}),
            hlc: Some(hlc2),
            parent_hlc: None,
            strategy: ConflictResolutionStrategy::Manual,
        },
        None,
    )
    .await
    .result
    .unwrap();
    let conflicts = match exchange(
        &mut client,
        TOKEN,
        Operation::ListConflicts {
            space: Some("collab".into()),
        },
        None,
    )
    .await
    .result
    .unwrap()
    {
        ResponseValue::Conflicts(c) => c,
        other => panic!("unexpected conflicts: {other:?}"),
    };
    assert_eq!(conflicts.len(), 1);
    exchange(
        &mut client,
        TOKEN,
        Operation::ResolveConflict {
            conflict_id: conflicts[0].id.expect("conflict id"),
            resolved_value: serde_json::json!({"v": 3}),
        },
        None,
    )
    .await
    .result
    .unwrap();

    exchange(
        &mut client,
        TOKEN,
        Operation::Batch {
            operations: vec![
                BatchOperation::CrdtPnCounterIncrement {
                    space: "metrics".into(),
                    id: "hits".into(),
                    actor_id: "n1".into(),
                    amount: 3,
                },
                BatchOperation::CrdtOrSetAdd {
                    space: "tags".into(),
                    id: "t1".into(),
                    actor_id: "n1".into(),
                    element: "rust".into(),
                },
                BatchOperation::CrdtJsonSetField {
                    space: "cfg".into(),
                    id: "c1".into(),
                    actor_id: "n1".into(),
                    path: "mode".into(),
                    value: serde_json::json!("fast"),
                    timestamp_ms: Some(1_000),
                },
            ],
        },
        None,
    )
    .await
    .result
    .unwrap();
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::CrdtCounterState {
                space: "metrics".into(),
                id: "hits".into(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::CrdtCounterState(state) if state.value() == 3
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::CrdtSetContains {
                space: "tags".into(),
                id: "t1".into(),
                element: "rust".into(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::Bool(true)
    ));
    assert!(matches!(
        exchange(
            &mut client,
            TOKEN,
            Operation::CrdtJsonState {
                space: "cfg".into(),
                id: "c1".into(),
            },
            None
        )
        .await
        .result
        .unwrap(),
        ResponseValue::CrdtJsonState(Some(_))
    ));

    drop(client);
    shutdown.notify_waiters();
    handler.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_ipc_live_query_streaming() {
    use lilia_core::BatchOperation;
    use lilia_protocol::{read_frame, write_frame, Operation, Request, Response, ResponseValue};

    let (_dir, db) = database();
    let shutdown = Arc::new(Notify::new());
    let (mut client, server) = tokio::io::duplex(16_384);
    let db_clone = Arc::clone(&db);
    let shutdown_clone = Arc::clone(&shutdown);
    let handler = tokio::spawn(async move {
        let _ = handle_stream(server, db_clone, TOKEN, shutdown_clone).await;
    });

    write_frame(
        &mut client,
        &Request {
            id: "lq-1".into(),
            token: TOKEN.into(),
            deadline_ms: None,
            operation: Operation::LiveQuerySubscribe {
                sql: "SELECT id, value FROM lilia_json WHERE space = 'tasks'".into(),
                params: Vec::new(),
            },
        },
    )
    .await
    .unwrap();

    let ack: Response = read_frame(&mut client).await.unwrap();
    assert!(matches!(ack.result.unwrap(), ResponseValue::Ack));

    let init_frame: Response = read_frame(&mut client).await.unwrap();
    match init_frame.result.unwrap() {
        ResponseValue::LiveQueryDiff(diff) => assert!(diff.is_initial),
        other => panic!("expected initial LiveQueryDiff, got {other:?}"),
    }

    db.batch(&[BatchOperation::JsonPut {
        space: "tasks".into(),
        id: "t1".into(),
        value: serde_json::json!({"title": "Ship SDK"}),
        if_version: None,
    }])
    .unwrap();

    let delta_frame: Response = read_frame(&mut client).await.unwrap();
    match delta_frame.result.unwrap() {
        ResponseValue::LiveQueryDiff(diff) => {
            assert!(!diff.is_initial);
            assert_eq!(diff.added.len(), 1);
        }
        other => panic!("expected delta LiveQueryDiff, got {other:?}"),
    }

    drop(client);
    shutdown.notify_one();
    let _ = handler.await;
}
