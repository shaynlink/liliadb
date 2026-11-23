//! The HTTP gateway: authentication, and a session over every route family.

use super::*;

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

#[allow(
    clippy::too_many_lines,
    reason = "one gateway session, checked request by request"
)]
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
