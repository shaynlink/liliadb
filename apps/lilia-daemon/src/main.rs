use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use lilia_core::{CentralityType, ComponentType, ErrorCode, LiliaError};
use lilia_graph::DatabaseGraphExt;
use lilia_protocol::{
    read_frame, write_frame, Operation, Request, Response, ResponseValue, PROTOCOL_MAJOR,
    PROTOCOL_MINOR,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::{Notify, Semaphore};
use tracing::info;
use uuid::Uuid;

#[cfg(test)]
mod tests;

#[cfg(unix)]
mod ipc_unix;

#[cfg(windows)]
mod ipc_windows;

mod http;

#[derive(Debug, Parser)]
#[command(version, about = "LiliaDB local daemon")]
struct Arguments {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    endpoint: PathBuf,
    #[arg(long)]
    token_file: PathBuf,
    #[arg(long = "plugin")]
    plugins: Vec<PathBuf>,
    #[arg(long = "trusted-key")]
    trusted_keys: Vec<String>,
    #[arg(long)]
    plugin_root: Option<PathBuf>,
    #[arg(long)]
    allow_unsigned_plugins: bool,
    /// Explicitly enable development-only plugin policy exceptions.
    #[arg(long)]
    development: bool,
    /// Optional port for local loopback HTTP REST gateway (127.0.0.1:<PORT>).
    #[arg(long = "http-port")]
    http_port: Option<u16>,
    /// Optional custom socket address for local loopback HTTP REST gateway.
    #[arg(long = "http-bind")]
    http_bind: Option<std::net::SocketAddr>,
    /// Optional passphrase or key to enable AES-256-GCM encryption at rest.
    #[arg(long = "encryption-key")]
    encryption_key: Option<String>,
    /// Require all loaded plugins to be executed in an isolated WebAssembly sandbox.
    #[arg(long = "require-sandboxed-plugins")]
    require_sandboxed_plugins: bool,
    /// Maximum linear memory in megabytes for each WebAssembly plugin instance (default: 64).
    #[arg(long = "plugin-memory-limit-mb", default_value = "64")]
    plugin_memory_limit_mb: usize,
    /// Maximum CPU fuel units per WebAssembly plugin invocation (default: 10,000,000).
    #[arg(long = "plugin-fuel-limit", default_value = "10000000")]
    plugin_fuel_limit: u64,
    /// Enable sub-50ns zero-copy shared memory multi-process engine.
    #[arg(long = "shm")]
    shm: bool,
    /// Custom path for shared memory segment (defaults to <database>.shm).
    #[arg(long = "shm-path")]
    shm_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("lilia_daemon=info")
        .with_writer(std::io::stderr)
        .init();
    let arguments = Arguments::parse();
    let plugins = load_plugins(&arguments)?;
    // Claim the endpoint before rotating credentials; duplicate startup must not
    // invalidate the token of an already running daemon.
    #[cfg(unix)]
    let listener = ipc_unix::ListenerGuard::bind(&arguments.endpoint)?;
    #[cfg(windows)]
    let security = ipc_windows::UserSecurity::new()?;
    #[cfg(windows)]
    let listener = security.pipe(&arguments.endpoint, true)?;
    #[cfg(unix)]
    let token = create_token(&arguments.token_file)?;
    #[cfg(windows)]
    let credentials = security.credentials(&arguments.token_file)?;
    #[cfg(windows)]
    let token = credentials.token.clone();
    let mut options = DatabaseOptions::durable(&arguments.database);
    for plugin in &plugins {
        if let Some(codec) = plugin.codec() {
            info!(codec = %codec.name(), "enabled plugin storage codec");
            options = options.with_codec(Arc::new(codec));
            break;
        }
    }
    let encryption_key = arguments
        .encryption_key
        .clone()
        .or_else(|| std::env::var("LILIA_ENCRYPTION_KEY").ok());
    if let Some(ref key) = encryption_key {
        info!("enabled AES-256-GCM at-rest encryption codec");
        options = options.with_codec(Arc::new(lilia_core::Aes256GcmCodec::from_passphrase(key)));
    }
    if arguments.shm || arguments.shm_path.is_some() {
        let shm_backend = if let Some(ref path) = arguments.shm_path {
            lilia_shm::ShmBackend::FileBacked(path.clone())
        } else {
            lilia_shm::ShmBackend::Auto(arguments.database.clone())
        };
        info!(path = %shm_backend.resolve_path().display(), "enabled sub-50ns zero-copy shared memory sub-engine");
        options = options.with_shared_memory(lilia_shm::ShmOptions {
            backend: shm_backend,
            ..Default::default()
        });
    }
    let database = Arc::new(Database::open(options)?);
    let shutdown = Arc::new(Notify::new());
    info!(database = %arguments.database.display(), "daemon started");

    let http_addr = resolve_http_bind(&arguments)?;
    if let Some(addr) = http_addr {
        let http_listener = tokio::net::TcpListener::bind(addr).await?;
        info!(addr = %addr, "http gateway started");
        let http_database = database.clone();
        let http_token = token.clone();
        let http_shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) =
                http::serve_http(http_listener, http_database, http_token, http_shutdown).await
            {
                tracing::error!(error = %e, "http gateway error");
            }
        });
    }

    #[cfg(unix)]
    return serve(listener, database, token, shutdown).await;
    #[cfg(windows)]
    serve(
        &arguments.endpoint,
        listener,
        &security,
        database,
        token,
        shutdown,
    )
    .await
}

fn resolve_http_bind(arguments: &Arguments) -> anyhow::Result<Option<std::net::SocketAddr>> {
    if let Some(addr) = arguments.http_bind {
        if !addr.ip().is_loopback() && !arguments.development {
            anyhow::bail!(
                "HTTP gateway can only bind to loopback addresses (127.0.0.1 or ::1) unless in development mode"
            );
        }
        return Ok(Some(addr));
    }
    if let Some(port) = arguments.http_port {
        return Ok(Some(std::net::SocketAddr::from(([127, 0, 0, 1], port))));
    }
    Ok(None)
}

fn load_plugins(arguments: &Arguments) -> anyhow::Result<Vec<lilia_plugin_api::LoadedPlugin>> {
    use base64::Engine;

    if arguments.allow_unsigned_plugins && !arguments.development {
        anyhow::bail!("--allow-unsigned-plugins requires explicit --development mode");
    }
    if arguments.plugin_root.is_some() && !arguments.development {
        anyhow::bail!("--plugin-root requires explicit --development mode");
    }
    if !arguments.trusted_keys.is_empty() && !arguments.development {
        anyhow::bail!("--trusted-key requires explicit --development mode");
    }
    if arguments.plugins.is_empty() {
        return Ok(Vec::new());
    }

    let root = arguments
        .plugin_root
        .clone()
        .map_or_else(lilia_plugin_api::default_plugin_root, Ok)?;
    let mut trusted_keys = vec![lilia_plugin_api::official_release_key()];
    if let Ok(store) = lilia_plugin_api::TrustStore::open(&root) {
        if let Ok(keys) = store.keys() {
            trusted_keys.extend(keys);
        }
    }
    trusted_keys.extend(
        arguments
            .trusted_keys
            .iter()
            .map(|encoded| {
                let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                let bytes: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("trusted keys must be 32 bytes"))?;
                ed25519_dalek::VerifyingKey::from_bytes(&bytes).map_err(Into::into)
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    );
    let canonical_root = root.canonicalize()?;
    arguments
        .plugins
        .iter()
        .map(|path| {
            let resolved_path = if path.exists() {
                if path.join("package").join("manifest.json").is_file() {
                    path.join("package")
                } else {
                    path.clone()
                }
            } else if let Some(installed) =
                lilia_plugin_api::find_installed(&root, path.to_str().unwrap_or_default())?
            {
                installed.package_path
            } else {
                anyhow::bail!(
                    "plugin `{}` not found as a file or directory, and not installed in `{}`",
                    path.display(),
                    root.display()
                );
            };

            let canonical_plugin = resolved_path.canonicalize()?;
            if !canonical_plugin.starts_with(&canonical_root) {
                anyhow::bail!("plugin path is outside the authorized plugin root");
            }
            let manifest = lilia_plugin_api::verify_package(
                &canonical_plugin,
                &trusted_keys,
                arguments.allow_unsigned_plugins,
            )?;
            load_trusted_descriptor(&canonical_plugin, &manifest, arguments)
        })
        .collect()
}

#[allow(unsafe_code)]
fn load_trusted_descriptor(
    path: &Path,
    manifest: &lilia_plugin_api::PluginManifest,
    arguments: &Arguments,
) -> anyhow::Result<lilia_plugin_api::LoadedPlugin> {
    let is_wasm = lilia_plugin_api::is_wasm_target(&manifest.target)
        || std::path::Path::new(&manifest.library)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wasm"));
    if arguments.require_sandboxed_plugins && !is_wasm {
        anyhow::bail!(
            "plugin `{}` is a native dynamic library; --require-sandboxed-plugins requires WebAssembly sandboxed plugins",
            manifest.name
        );
    }
    let wasm_config = lilia_plugin_api::WasmPluginConfig {
        memory_limit_bytes: arguments.plugin_memory_limit_mb.saturating_mul(1024 * 1024),
        fuel_limit: arguments.plugin_fuel_limit,
    };
    let plugin = unsafe {
        lilia_plugin_api::load_descriptor_with_config(path, manifest, wasm_config)
            .map_err(Into::<anyhow::Error>::into)?
    };
    if arguments.require_sandboxed_plugins && !plugin.is_sandboxed() {
        anyhow::bail!(
            "plugin `{}` is a native dynamic library; --require-sandboxed-plugins requires WebAssembly sandboxed plugins",
            manifest.name
        );
    }
    if plugin.is_sandboxed() {
        info!(plugin = %plugin.name(), "loaded sandboxed WebAssembly plugin (Wasmtime)");
    } else {
        info!(plugin = %plugin.name(), "loaded native C-ABI plugin (libloading)");
    }
    Ok(plugin)
}

#[cfg(unix)]
fn create_token(path: &Path) -> std::io::Result<String> {
    ipc_unix::create_token(path)
}

#[cfg(unix)]
async fn serve(
    listener: ipc_unix::ListenerGuard,
    database: Arc<Database>,
    token: String,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()> {
    let connections = Arc::new(Semaphore::new(64));
    loop {
        tokio::select! {
            accepted = listener.listener.accept() => {
                let (stream, _) = accepted?;
                if !stream.peer_cred().is_ok_and(|peer| ipc_unix::same_user(peer.uid())) {
                    continue;
                }
                let database = Arc::clone(&database);
                let token = token.clone();
                let shutdown = Arc::clone(&shutdown);
                let Ok(permit) = connections.clone().try_acquire_owned() else { continue };
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_stream(stream, database, &token, shutdown).await {
                        tracing::warn!(%error, "client disconnected with error");
                    }
                });
            }
            () = shutdown.notified() => break,
        }
    }
    close_database(database).await
}

#[cfg(windows)]
async fn serve(
    endpoint: &Path,
    mut server: tokio::net::windows::named_pipe::NamedPipeServer,
    security: &ipc_windows::UserSecurity,
    database: Arc<Database>,
    token: String,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()> {
    let connections = Arc::new(Semaphore::new(64));
    loop {
        tokio::select! {
            connected = server.connect() => {
                connected?;
                // Keep an instance alive continuously, even when rejecting a
                // connection under load. Never release the pipe namespace.
                let next = security.pipe(endpoint, false)?;
                let accepted = std::mem::replace(&mut server, next);
                let database = Arc::clone(&database);
                let token = token.clone();
                let shutdown = Arc::clone(&shutdown);
                let Ok(permit) = connections.clone().try_acquire_owned() else { continue };
                tokio::spawn(async move {
                    let _permit = permit;
                    handle_stream(accepted, database, &token, shutdown).await.ok();
                });
            }
            () = shutdown.notified() => break,
        }
    }
    close_database(database).await
}

async fn close_database(database: Arc<Database>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || database.close(std::time::Duration::from_secs(5)))
        .await??;
    Ok(())
}

async fn verify_stream_token<S>(
    stream: &mut S,
    request_id: &str,
    supplied_token: &str,
    expected_token: &str,
    request_uuid: Uuid,
) -> std::io::Result<bool>
where
    S: AsyncWrite + Unpin,
{
    if bool::from(supplied_token.as_bytes().ct_eq(expected_token.as_bytes())) {
        return Ok(true);
    }
    let err = LiliaError::new(ErrorCode::Unauthorized, "invalid daemon token", false)
        .with_request_id(request_uuid);
    write_frame(
        stream,
        &Response {
            id: request_id.to_owned(),
            result: Err(err),
        },
    )
    .await?;
    Ok(false)
}

fn current_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[allow(clippy::too_many_lines)]
async fn handle_stream<S>(
    mut stream: S,
    database: Arc<Database>,
    token: &str,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let request: Request =
            match tokio::time::timeout(Duration::from_secs(30), read_frame(&mut stream)).await {
                Err(_) => return Err(anyhow::anyhow!("client idle timeout")),
                Ok(Ok(request)) => request,
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(())
                }
                Ok(Err(error)) => return Err(error.into()),
            };
        let operation_name = operation_name(&request.operation);
        let request_id = request.id.clone();
        let should_shutdown = matches!(request.operation, Operation::Shutdown);
        let started = Instant::now();
        let request_uuid = Uuid::parse_str(&request.id).unwrap_or_else(|_| Uuid::new_v4());

        if let Operation::Subscribe { namespace } = request.operation {
            if !verify_stream_token(
                &mut stream,
                &request.id,
                &request.token,
                token,
                request_uuid,
            )
            .await?
            {
                return Ok(());
            }
            write_frame(
                &mut stream,
                &Response {
                    id: request.id.clone(),
                    result: Ok(ResponseValue::Ack),
                },
            )
            .await?;
            info!(
                request_id,
                operation = "subscribe",
                namespace = namespace.as_deref().unwrap_or("*"),
                "client subscription started"
            );
            let mut rx = database.subscribe();
            let mut read_buf = [0u8; 1];
            loop {
                tokio::select! {
                    evt = rx.recv() => {
                        match evt {
                            Ok(event) => {
                                if event.matches_filter(namespace.as_deref())
                                    && write_frame(
                                        &mut stream,
                                        &Response {
                                            id: request.id.clone(),
                                            result: Ok(ResponseValue::Event(event)),
                                        },
                                    )
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "ipc subscriber lagged behind events");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    res = stream.read(&mut read_buf) => {
                        match res {
                            Ok(0) | Err(_) => break,
                            _ => {}
                        }
                    }
                    () = shutdown.notified() => break,
                }
            }
            info!(request_id, "client subscription closed");
            return Ok(());
        }

        if let Operation::LiveQuerySubscribe { sql, params } = request.operation {
            if !verify_stream_token(
                &mut stream,
                &request.id,
                &request.token,
                token,
                request_uuid,
            )
            .await?
            {
                return Ok(());
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let db_for_live = Arc::clone(&database);
            let handle = match tokio::task::spawn_blocking(move || {
                db_for_live.live_query(&sql, &params, move |diff| {
                    let _ = tx.send(diff);
                })
            })
            .await
            {
                Ok(Ok(handle)) => handle,
                Ok(Err(err)) => {
                    write_frame(
                        &mut stream,
                        &Response {
                            id: request.id,
                            result: Err(err.with_request_id(request_uuid)),
                        },
                    )
                    .await?;
                    return Ok(());
                }
                Err(join_err) => {
                    let err = LiliaError::new(
                        ErrorCode::Storage,
                        format!("live query worker panicked: {join_err}"),
                        false,
                    )
                    .with_request_id(request_uuid);
                    write_frame(
                        &mut stream,
                        &Response {
                            id: request.id,
                            result: Err(err),
                        },
                    )
                    .await?;
                    return Ok(());
                }
            };
            write_frame(
                &mut stream,
                &Response {
                    id: request.id.clone(),
                    result: Ok(ResponseValue::Ack),
                },
            )
            .await?;
            info!(
                request_id,
                operation = "live_query_subscribe",
                subscription_id = handle.id,
                "live query subscription started"
            );
            let mut read_buf = [0u8; 1];
            loop {
                tokio::select! {
                    diff = rx.recv() => {
                        match diff {
                            Some(diff) => {
                                if write_frame(
                                    &mut stream,
                                    &Response {
                                        id: request.id.clone(),
                                        result: Ok(ResponseValue::LiveQueryDiff(diff)),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    res = stream.read(&mut read_buf) => {
                        match res {
                            Ok(0) | Err(_) => break,
                            _ => {}
                        }
                    }
                    () = shutdown.notified() => break,
                }
            }
            handle.unsubscribe();
            info!(
                request_id,
                subscription_id = handle.id,
                "live query subscription closed"
            );
            return Ok(());
        }

        let result = if bool::from(request.token.as_bytes().ct_eq(token.as_bytes())) {
            execute(
                Arc::clone(&database),
                request.operation,
                started,
                request.deadline_ms,
            )
            .await
        } else {
            Err(LiliaError::new(
                ErrorCode::Unauthorized,
                "invalid daemon token",
                false,
            ))
        };
        let result = result.map_err(|error| error.with_request_id(request_uuid));
        let success = result.is_ok();
        write_frame(
            &mut stream,
            &Response {
                id: request.id,
                result,
            },
        )
        .await?;
        info!(
            request_id,
            operation = operation_name,
            success,
            elapsed_us = started.elapsed().as_micros(),
            "request completed"
        );
        if should_shutdown && success {
            shutdown.notify_one();
            return Ok(());
        }
    }
}

fn check_deadline(started: Instant, deadline_ms: Option<u64>) -> lilia_core::Result<()> {
    if deadline_ms.is_some_and(|ms| started.elapsed() >= Duration::from_millis(ms)) {
        return Err(LiliaError::new(
            ErrorCode::Timeout,
            "request deadline exceeded",
            true,
        ));
    }
    Ok(())
}

async fn execute(
    database: Arc<Database>,
    operation: Operation,
    started: Instant,
    deadline_ms: Option<u64>,
) -> lilia_core::Result<ResponseValue> {
    check_deadline(started, deadline_ms)?;
    tokio::task::spawn_blocking(move || {
        // Include worker and FIFO waiting time. Once the transaction starts, return
        // its actual outcome: timing out a running write could hide a committed mutation.
        check_deadline(started, deadline_ms)?;
        if let Operation::Batch { operations } = operation {
            let deadline = deadline_ms
                .and_then(|ms| started.checked_add(std::time::Duration::from_millis(ms)));
            return database
                .batch_with_deadline(&operations, deadline)
                .map(ResponseValue::Mutations);
        }
        dispatch(&database, operation)
    })
    .await
    .map_err(|_| LiliaError::new(ErrorCode::Storage, "request worker failed", false))?
}

#[allow(clippy::too_many_lines)]
fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::Handshake { .. } => "handshake",
        Operation::Shutdown => "shutdown",
        Operation::IntegrityCheck => "integrity_check",
        Operation::Backup { .. } => "backup",
        Operation::KvGet { .. } => "kv_get",
        Operation::KvScan { .. } => "kv_scan",
        Operation::JsonGet { .. } => "json_get",
        Operation::JsonScan { .. } => "json_scan",
        Operation::JsonCreateIndex { .. } => "json_create_index",
        Operation::JsonDropIndex { .. } => "json_drop_index",
        Operation::JsonListIndexes { .. } => "json_list_indexes",
        Operation::JsonScanByIndex { .. } => "json_scan_by_index",
        Operation::JsonFind { .. } => "json_find",
        Operation::Batch { .. } => "batch",
        Operation::SqlQuery { .. } => "sql_query",
        Operation::GraphNeighbors { .. } => "graph_neighbors",
        Operation::GraphReachability { .. } => "graph_reachability",
        Operation::FtsSearch { .. } => "fts_search",
        Operation::VectorGet { .. } => "vector_get",
        Operation::VectorSearch { .. } => "vector_search",
        Operation::HybridSearch { .. } => "hybrid_search",
        Operation::Subscribe { .. } => "subscribe",
        Operation::CrdtCounterGet { .. } => "crdt_counter_get",
        Operation::CrdtSetGet { .. } => "crdt_set_get",
        Operation::CrdtJsonGet { .. } => "crdt_json_get",
        Operation::QuickCheck => "quick_check",
        Operation::Checkpoint => "checkpoint",
        Operation::CheckpointStats => "checkpoint_stats",
        Operation::StorageStats => "storage_stats",
        Operation::FtsDelete { .. } => "fts_delete",
        Operation::JsonExtract { .. } => "json_extract",
        Operation::JsonScanPrefix { .. } => "json_scan_prefix",
        Operation::JsonGetAtVersion { .. } => "json_get_at_version",
        Operation::JsonGetAtTimestamp { .. } => "json_get_at_timestamp",
        Operation::JsonHistory { .. } => "json_history",
        Operation::JsonDiff { .. } => "json_diff",
        Operation::JsonTrainDictionary { .. } => "json_train_dictionary",
        Operation::JsonSetAutoTrain { .. } => "json_set_auto_train",
        Operation::JsonIsAutoTrainEnabled { .. } => "json_is_auto_train_enabled",
        Operation::JsonPutHlc { .. } => "json_put_hlc",
        Operation::JsonGetHlc { .. } => "json_get_hlc",
        Operation::GraphGetNode { .. } => "graph_get_node",
        Operation::GraphGetNodeDocument { .. } => "graph_get_node_document",
        Operation::GraphDeleteNode { .. } => "graph_delete_node",
        Operation::GraphListNodes { .. } => "graph_list_nodes",
        Operation::GraphRemoveEdge { .. } => "graph_remove_edge",
        Operation::GraphListEdges { .. } => "graph_list_edges",
        Operation::GraphImpactAnalysis { .. } => "graph_impact_analysis",
        Operation::GraphShortestPath { .. } => "graph_shortest_path",
        Operation::GraphDijkstra { .. } => "graph_dijkstra",
        Operation::GraphAllShortestPaths { .. } => "graph_all_shortest_paths",
        Operation::GraphAstarSpatial { .. } => "graph_astar_spatial",
        Operation::GraphPageRank { .. } => "graph_pagerank",
        Operation::GraphCentrality { .. } => "graph_centrality",
        Operation::GraphHasCycle { .. } => "graph_has_cycle",
        Operation::GraphFindCycles { .. } => "graph_find_cycles",
        Operation::GraphConnectedComponents { .. } => "graph_connected_components",
        Operation::GraphCypher { .. } => "graph_cypher",
        Operation::VectorCreateIndex { .. } => "vector_create_index",
        Operation::VectorGetIndexInfo { .. } => "vector_get_index_info",
        Operation::VectorDropIndex { .. } => "vector_drop_index",
        Operation::VectorSearchFiltered { .. } => "vector_search_filtered",
        Operation::VectorSearchInGraphNeighbors { .. } => "vector_search_in_graph_neighbors",
        Operation::VectorSearchByJsonIndex { .. } => "vector_search_by_json_index",
        Operation::TsAppendPoints { .. } => "ts_append_points",
        Operation::TsQueryRange { .. } => "ts_query_range",
        Operation::TsQueryAuto { .. } => "ts_query_auto",
        Operation::TsQueryStats { .. } => "ts_query_stats",
        Operation::TsSetPolicy { .. } => "ts_set_policy",
        Operation::TsGetPolicy { .. } => "ts_get_policy",
        Operation::TsListPolicies => "ts_list_policies",
        Operation::TsDeletePolicy { .. } => "ts_delete_policy",
        Operation::TsApplyRetention { .. } => "ts_apply_retention",
        Operation::GeoAddFence { .. } => "geo_add_fence",
        Operation::GeoRemoveFence { .. } => "geo_remove_fence",
        Operation::GeoContainsPoint { .. } => "geo_contains_point",
        Operation::GeoFindFencesForPoint { .. } => "geo_find_fences_for_point",
        Operation::GeoRecordGpsPoints { .. } => "geo_record_gps_points",
        Operation::GeoGetTrackPoints { .. } => "geo_get_track_points",
        Operation::GeoTrackDistance { .. } => "geo_track_distance",
        Operation::CrdtCounterState { .. } => "crdt_counter_state",
        Operation::CrdtCounterMerge { .. } => "crdt_counter_merge",
        Operation::CrdtSetContains { .. } => "crdt_set_contains",
        Operation::CrdtSetState { .. } => "crdt_set_state",
        Operation::CrdtSetPrune { .. } => "crdt_set_prune",
        Operation::CrdtSetMerge { .. } => "crdt_set_merge",
        Operation::CrdtJsonState { .. } => "crdt_json_state",
        Operation::CrdtJsonMerge { .. } => "crdt_json_merge",
        Operation::HlcNext => "hlc_next",
        Operation::HlcObserve { .. } => "hlc_observe",
        Operation::ListConflicts { .. } => "list_conflicts",
        Operation::ResolveConflict { .. } => "resolve_conflict",
        Operation::Undo => "undo",
        Operation::Redo => "redo",
        Operation::UndoHistory => "undo_history",
        Operation::CreateMaterializedView { .. } => "create_materialized_view",
        Operation::DropMaterializedView { .. } => "drop_materialized_view",
        Operation::RefreshMaterializedView { .. } => "refresh_materialized_view",
        Operation::ListMaterializedViews => "list_materialized_views",
        Operation::LiveQuerySubscribe { .. } => "live_query_subscribe",
        Operation::P2pSync { .. } => "p2p_sync",
    }
}

#[allow(clippy::too_many_lines)]
fn dispatch(database: &Database, operation: Operation) -> lilia_core::Result<ResponseValue> {
    Ok(match operation {
        Operation::Handshake { major, minor: _ } if major == PROTOCOL_MAJOR => {
            ResponseValue::Handshake {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
                capabilities: vec![
                    "kv".into(),
                    "json".into(),
                    "batch".into(),
                    "sql".into(),
                    "graph".into(),
                    "fts".into(),
                    "vector".into(),
                    "hybrid".into(),
                    "watch".into(),
                    "crdt".into(),
                    "timeseries".into(),
                    "geo".into(),
                    "hlc".into(),
                    "undo".into(),
                    "ivm".into(),
                    "live_query".into(),
                    "diagnostics".into(),
                ],
            }
        }
        Operation::Handshake { .. } => {
            return Err(LiliaError::new(
                ErrorCode::Unsupported,
                "incompatible protocol major",
                false,
            ));
        }
        Operation::Shutdown => ResponseValue::Shutdown,
        Operation::IntegrityCheck => ResponseValue::Integrity(database.integrity_check()?),
        Operation::Backup { destination } => {
            database.backup(destination)?;
            ResponseValue::Ack
        }
        Operation::Subscribe { .. } | Operation::LiveQuerySubscribe { .. } => ResponseValue::Ack,
        Operation::KvGet { namespace, key } => {
            ResponseValue::Kv(database.kv_get(&namespace, &key)?)
        }
        Operation::KvScan {
            namespace,
            after,
            limit,
        } => ResponseValue::KvPage(database.kv_scan(&namespace, after.as_deref(), limit)?),
        Operation::JsonGet { space, id } => ResponseValue::Json(database.json_get(&space, &id)?),
        Operation::JsonScan {
            space,
            after,
            limit,
        } => ResponseValue::JsonPage(database.json_scan(&space, after.as_deref(), limit)?),
        Operation::JsonCreateIndex {
            space,
            name,
            path,
            filter,
        } => {
            let options = lilia_core::IndexOptions { filter };
            database.create_json_index_with_options(&space, &name, &path, options)?;
            ResponseValue::Ack
        }
        Operation::JsonDropIndex { space, name } => {
            ResponseValue::IndexDropped(database.drop_json_index(&space, &name)?)
        }
        Operation::JsonListIndexes { space } => {
            ResponseValue::JsonIndexes(database.list_json_indexes(&space)?)
        }
        Operation::JsonScanByIndex {
            space,
            index_name,
            value,
            after,
            limit,
        } => ResponseValue::JsonPage(database.json_scan_by_index(
            &space,
            &index_name,
            &value,
            after.as_deref(),
            limit,
        )?),
        Operation::JsonFind {
            space,
            query,
            after,
            limit,
        } => {
            let query_map = query.as_object().ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    "query must be a JSON object",
                    false,
                )
            })?;
            ResponseValue::JsonPage(database.json_find(
                &space,
                query_map,
                after.as_deref(),
                limit,
            )?)
        }
        Operation::Batch { operations } => ResponseValue::Mutations(database.batch(&operations)?),
        Operation::SqlQuery { sql, params } => {
            ResponseValue::Sql(database.sql_query(&sql, &params)?)
        }
        Operation::GraphNeighbors {
            graph,
            node,
            direction,
            kind,
        } => ResponseValue::GraphNeighbors(database.graph_neighbors(
            &graph,
            &node,
            direction,
            kind.as_deref(),
        )?),
        Operation::GraphReachability {
            graph,
            start_nodes,
            direction,
        } => ResponseValue::GraphReachability(database.graph_reachability(
            &graph,
            &start_nodes,
            direction,
        )?),
        Operation::FtsSearch {
            collection,
            query,
            limit,
        } => ResponseValue::FtsHits(database.fts_search(&collection, &query, limit)?),
        Operation::VectorGet { collection, id } => {
            let res = database
                .vector_get(&collection, &id)?
                .map(|(dim, et, vec)| (dim, et, lilia_protocol::ByteBuf::from(vec)));
            ResponseValue::Vector(res)
        }
        Operation::VectorSearch {
            collection,
            vector,
            dim,
            element_type,
            metric,
            limit,
        } => ResponseValue::VectorHits(database.vector_search(
            &collection,
            &vector,
            dim,
            element_type,
            metric,
            limit,
        )?),
        Operation::HybridSearch {
            collection,
            fts_query,
            vector,
            dim,
            element_type,
            metric,
            rrf_k,
            fts_weight,
            vector_weight,
            limit,
        } => {
            let vec_arg = match (vector, dim, element_type, metric) {
                (Some(v), Some(d), Some(et), Some(m)) => Some((v, d, et, m)),
                _ => None,
            };
            let vec_ref = vec_arg
                .as_ref()
                .map(|(v, d, et, m)| (v.as_slice(), *d, *et, *m));
            ResponseValue::HybridHits(database.hybrid_search(
                &collection,
                fts_query.as_deref(),
                vec_ref,
                rrf_k,
                fts_weight,
                vector_weight,
                limit,
            )?)
        }
        Operation::CrdtCounterGet { space, id } => {
            ResponseValue::CrdtCounter(database.crdt().counter(&space, &id).get()?)
        }
        Operation::CrdtSetGet { space, id } => {
            let elements = database
                .crdt()
                .set(&space, &id)
                .read()?
                .into_iter()
                .collect();
            ResponseValue::CrdtSet(elements)
        }
        Operation::CrdtJsonGet { space, id } => {
            ResponseValue::CrdtJson(database.crdt().doc(&space, &id).get()?)
        }
        Operation::QuickCheck => ResponseValue::Bool(database.quick_check()?),
        Operation::Checkpoint => {
            database.checkpoint()?;
            ResponseValue::Ack
        }
        Operation::CheckpointStats => ResponseValue::CheckpointStats(database.checkpoint_stats()),
        Operation::StorageStats => ResponseValue::TableStorageStats(database.storage_stats()?),
        Operation::FtsDelete { collection, doc_id } => {
            ResponseValue::Bool(database.fts_delete(&collection, &doc_id)?)
        }
        Operation::JsonExtract { space, id, path } => {
            ResponseValue::JsonExtracted(database.json_extract(&space, &id, &path)?)
        }
        Operation::JsonScanPrefix {
            space,
            prefix,
            after_id,
            limit,
        } => ResponseValue::JsonPage(database.json_scan_prefix(
            &space,
            &prefix,
            after_id.as_deref(),
            limit,
        )?),
        Operation::JsonGetAtVersion { space, id, version } => {
            ResponseValue::Json(database.json_get_at_version(&space, &id, version)?)
        }
        Operation::JsonGetAtTimestamp {
            space,
            id,
            timestamp_ms,
        } => ResponseValue::Json(database.json_get_at_timestamp(&space, &id, timestamp_ms)?),
        Operation::JsonHistory { space, id } => {
            ResponseValue::JsonRevisions(database.json_history(&space, &id)?)
        }
        Operation::JsonDiff {
            space,
            id,
            from_version,
            to_version,
        } => ResponseValue::JsonValue(database.json_diff(&space, &id, from_version, to_version)?),
        Operation::JsonTrainDictionary {
            space,
            sample_count,
            dict_size,
        } => ResponseValue::DictStats(database.train_json_dictionary(
            &space,
            sample_count,
            dict_size,
        )?),
        Operation::JsonSetAutoTrain { space, enabled } => {
            database.set_space_auto_train(&space, enabled)?;
            ResponseValue::Ack
        }
        Operation::JsonIsAutoTrainEnabled { space } => {
            ResponseValue::Bool(database.is_space_auto_train_enabled(&space))
        }
        Operation::JsonPutHlc {
            space,
            id,
            value,
            hlc,
            parent_hlc,
            strategy,
        } => ResponseValue::HlcOutcome(
            database.json_put_hlc(&space, &id, value, hlc, parent_hlc, strategy)?,
        ),
        Operation::JsonGetHlc { space, id } => {
            ResponseValue::HlcRecord(database.json_get_hlc(&space, &id)?)
        }
        Operation::GraphGetNode { graph, id } => {
            ResponseValue::GraphNode(database.graph_get_node(&graph, &id)?)
        }
        Operation::GraphGetNodeDocument { graph, id } => {
            ResponseValue::JsonExtracted(database.graph_get_node_document(&graph, &id)?)
        }
        Operation::GraphDeleteNode { graph, id } => {
            ResponseValue::Bool(database.graph_delete_node(&graph, &id)?)
        }
        Operation::GraphListNodes { graph, kind, limit } => {
            ResponseValue::GraphNodes(database.graph_list_nodes(&graph, kind.as_deref(), limit)?)
        }
        Operation::GraphRemoveEdge {
            graph,
            source,
            target,
            kind,
        } => ResponseValue::Bool(database.graph_remove_edge(&graph, &source, &target, &kind)?),
        Operation::GraphListEdges {
            graph,
            source,
            target,
            kind,
            limit,
        } => ResponseValue::GraphEdges(database.graph_list_edges(
            &graph,
            source.as_deref(),
            target.as_deref(),
            kind.as_deref(),
            limit,
        )?),
        Operation::GraphImpactAnalysis {
            graph,
            changed_nodes,
        } => ResponseValue::GraphImpact(database.graph(&graph).impact_analysis(&changed_nodes)?),
        Operation::GraphShortestPath {
            graph,
            start,
            target,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPath(database.graph(&graph).shortest_path(
            &start,
            &target,
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphDijkstra {
            graph,
            start,
            target,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPath(database.graph(&graph).dijkstra(
            &start,
            target.as_deref(),
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphAllShortestPaths {
            graph,
            start,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPathMap(database.graph(&graph).all_shortest_paths(
            &start,
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphAstarSpatial {
            graph,
            start,
            target,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPath(database.graph(&graph).astar_spatial(
            &start,
            &target,
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphPageRank { graph, config } => {
            ResponseValue::NodeScores(database.graph(&graph).pagerank(&config)?)
        }
        Operation::GraphCentrality {
            graph,
            centrality,
            direction,
            kind_filter,
            normalized,
        } => {
            let store = database.graph(&graph);
            let scores = match centrality {
                CentralityType::Betweenness => {
                    store.betweenness_centrality(direction, kind_filter.as_deref(), normalized)?
                }
                CentralityType::Closeness => {
                    store.closeness_centrality(direction, kind_filter.as_deref())?
                }
                CentralityType::Degree => {
                    store.degree_centrality(direction, kind_filter.as_deref())?
                }
            };
            ResponseValue::NodeScores(scores)
        }
        Operation::GraphHasCycle {
            graph,
            direction,
            kind_filter,
        } => ResponseValue::Bool(
            database
                .graph(&graph)
                .has_cycle(direction, kind_filter.as_deref())?,
        ),
        Operation::GraphFindCycles {
            graph,
            max_cycles,
            kind_filter,
        } => ResponseValue::CycleList(
            database
                .graph(&graph)
                .find_cycles(max_cycles, kind_filter.as_deref())?,
        ),
        Operation::GraphConnectedComponents {
            graph,
            component_type,
            kind_filter,
        } => {
            let store = database.graph(&graph);
            let components = match component_type {
                ComponentType::StronglyConnected => {
                    store.strongly_connected_components(kind_filter.as_deref())?
                }
                ComponentType::WeaklyConnected => {
                    store.weakly_connected_components(kind_filter.as_deref())?
                }
            };
            ResponseValue::CycleList(components)
        }
        Operation::GraphCypher { graph, query } => {
            ResponseValue::CypherResult(database.graph(&graph).cypher(&query)?)
        }
        Operation::VectorCreateIndex { collection, config } => {
            database.vector_create_index(&collection, config)?;
            ResponseValue::Ack
        }
        Operation::VectorGetIndexInfo { collection } => {
            ResponseValue::VectorIndexInfo(database.vector_get_index_info(&collection)?)
        }
        Operation::VectorDropIndex { collection } => {
            ResponseValue::Bool(database.vector_drop_index(&collection)?)
        }
        Operation::VectorSearchFiltered {
            collection,
            query_vector,
            dim,
            element_type,
            metric,
            candidate_ids,
            limit,
        } => {
            let refs: Vec<&str> = candidate_ids.iter().map(String::as_str).collect();
            ResponseValue::VectorHits(database.vector_search_filtered(
                &collection,
                &query_vector,
                dim,
                element_type,
                metric,
                &refs,
                limit,
            )?)
        }
        Operation::VectorSearchInGraphNeighbors {
            collection,
            graph,
            start_node,
            direction,
            query_vector,
            dim,
            element_type,
            metric,
            limit,
        } => ResponseValue::VectorHits(database.vector_search_in_graph_neighbors(
            &collection,
            &graph,
            &start_node,
            direction,
            &query_vector,
            dim,
            element_type,
            metric,
            limit,
        )?),
        Operation::VectorSearchByJsonIndex {
            collection,
            space,
            index_name,
            index_value,
            query_vector,
            dim,
            element_type,
            metric,
            limit,
        } => ResponseValue::VectorHits(database.vector_search_by_json_index(
            &collection,
            &space,
            &index_name,
            &index_value,
            &query_vector,
            dim,
            element_type,
            metric,
            limit,
        )?),
        Operation::TsAppendPoints {
            series_id,
            points,
            resolution_ms,
        } => {
            if let Some(res_ms) = resolution_ms {
                database.ts_append_chunk(&series_id, res_ms, &points)?;
            } else {
                database.ts_append_points(&series_id, &points)?;
            }
            ResponseValue::Ack
        }
        Operation::TsQueryRange {
            series_id,
            start_time,
            end_time,
            resolution_ms,
        } => {
            let points = match resolution_ms {
                Some(res_ms) => database
                    .ts_query_range_with_resolution(&series_id, res_ms, start_time, end_time)?,
                None => database.ts_query_range(&series_id, start_time, end_time)?,
            };
            ResponseValue::TsPoints(points)
        }
        Operation::TsQueryAuto {
            series_id,
            start_time,
            end_time,
            max_points,
        } => ResponseValue::TsPoints(
            database.ts_query_auto(&series_id, start_time, end_time, max_points)?,
        ),
        Operation::TsQueryStats {
            series_id,
            start_time,
            end_time,
            resolution_ms,
        } => {
            let stats = match resolution_ms {
                Some(res_ms) => database
                    .ts_query_stats_with_resolution(&series_id, res_ms, start_time, end_time)?,
                None => database.ts_query_stats(&series_id, start_time, end_time)?,
            };
            ResponseValue::TsStats(stats)
        }
        Operation::TsSetPolicy { policy } => {
            database.ts_set_policy(&policy)?;
            ResponseValue::Ack
        }
        Operation::TsGetPolicy { series_pattern } => {
            ResponseValue::TsPolicy(database.ts_get_policy(&series_pattern)?)
        }
        Operation::TsListPolicies => ResponseValue::TsPolicies(database.ts_list_policies()?),
        Operation::TsDeletePolicy { series_pattern } => {
            ResponseValue::Bool(database.ts_delete_policy(&series_pattern)?)
        }
        Operation::TsApplyRetention {
            series_id,
            policy,
            now_ms,
        } => {
            let stats = match (series_id, policy) {
                (Some(sid), Some(pol)) => database.ts_apply_retention_for_series(
                    &sid,
                    &pol,
                    now_ms.unwrap_or_else(current_epoch_ms),
                )?,
                (Some(_), None) | (None, Some(_)) => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "ts_apply_retention requires both series_id and policy together, or neither",
                        false,
                    ));
                }
                (None, None) => match now_ms {
                    Some(now) => database.ts_apply_retention_at(now)?,
                    None => database.ts_apply_retention()?,
                },
            };
            ResponseValue::TsRetentionStats(stats)
        }
        Operation::GeoAddFence {
            id,
            name,
            category,
            polygon_json,
            metadata,
        } => {
            database.geo_add_fence(&id, &name, &category, &polygon_json, metadata.as_deref())?;
            ResponseValue::Ack
        }
        Operation::GeoRemoveFence { id } => ResponseValue::Bool(database.geo_remove_fence(&id)?),
        Operation::GeoContainsPoint {
            polygon_json,
            lon,
            lat,
        } => ResponseValue::Bool(database.geo_contains_point(&polygon_json, lon, lat)?),
        Operation::GeoFindFencesForPoint { lon, lat } => {
            ResponseValue::GeofenceMatches(database.geo_find_fences_for_point(lon, lat)?)
        }
        Operation::GeoRecordGpsPoints { track_id, points } => {
            ResponseValue::GeofenceMatches(database.geo_record_gps_points(&track_id, &points)?)
        }
        Operation::GeoGetTrackPoints {
            track_id,
            start_time,
            end_time,
        } => ResponseValue::GpsPoints(
            database.geo_get_track_points(&track_id, start_time, end_time)?,
        ),
        Operation::GeoTrackDistance { track_id } => {
            ResponseValue::F64(database.geo_track_distance_meters(&track_id)?)
        }
        Operation::CrdtCounterState { space, id } => {
            ResponseValue::CrdtCounterState(database.crdt().counter(&space, &id).state()?)
        }
        Operation::CrdtCounterMerge { space, id, remote } => {
            ResponseValue::CrdtCounter(database.crdt().counter(&space, &id).merge(&remote)?)
        }
        Operation::CrdtSetContains { space, id, element } => {
            ResponseValue::Bool(database.crdt().set(&space, &id).contains(&element)?)
        }
        Operation::CrdtSetState { space, id } => {
            ResponseValue::CrdtSetState(database.crdt().set(&space, &id).state()?)
        }
        Operation::CrdtSetPrune {
            space,
            id,
            cutoff_ms,
        } => ResponseValue::Usize(database.crdt().set(&space, &id).prune(cutoff_ms)?),
        Operation::CrdtSetMerge { space, id, remote } => {
            database.crdt().set(&space, &id).merge(&remote)?;
            ResponseValue::Ack
        }
        Operation::CrdtJsonState { space, id } => {
            ResponseValue::CrdtJsonState(database.crdt().doc(&space, &id).state()?)
        }
        Operation::CrdtJsonMerge { space, id, remote } => {
            ResponseValue::U64(database.crdt().doc(&space, &id).merge(&remote)?)
        }
        Operation::HlcNext => ResponseValue::Hlc(database.next_hlc()),
        Operation::HlcObserve { remote } => ResponseValue::Hlc(database.observe_hlc(&remote)?),
        Operation::ListConflicts { space } => {
            ResponseValue::Conflicts(database.list_conflicts(space.as_deref())?)
        }
        Operation::ResolveConflict {
            conflict_id,
            resolved_value,
        } => {
            database.resolve_conflict(conflict_id, resolved_value)?;
            ResponseValue::Ack
        }
        Operation::Undo => ResponseValue::UndoResult(database.undo()?),
        Operation::Redo => ResponseValue::RedoResult(database.redo()?),
        Operation::UndoHistory => ResponseValue::UndoEntries(database.undo_history()?),
        Operation::CreateMaterializedView { name, query } => {
            database.create_materialized_view(&name, &query)?;
            ResponseValue::Ack
        }
        Operation::DropMaterializedView { name } => {
            database.drop_materialized_view(&name)?;
            ResponseValue::Ack
        }
        Operation::RefreshMaterializedView { name } => {
            database.refresh_materialized_view(&name)?;
            ResponseValue::Ack
        }
        Operation::ListMaterializedViews => {
            ResponseValue::MaterializedViews(database.list_materialized_views()?)
        }
        Operation::P2pSync { message } => match message {
            lilia_protocol::P2pSyncMessage::EstimateRequest { space, strata } => {
                let local_strata = database.p2p_strata(space.as_deref())?;
                let estimate = strata.estimate_difference(&local_strata).map_err(|e| {
                    lilia_core::LiliaError::new(
                        lilia_core::ErrorCode::Storage,
                        e.to_string(),
                        false,
                    )
                })?;

                if estimate.fully_decoded {
                    let diff = estimate.direct_diff.unwrap_or_default();
                    let (_, id_map) = database.p2p_collect_items(space.as_deref())?;
                    let changesets = database.p2p_export_payloads(&diff.b_minus_a, &id_map)?;
                    let requested_id_hashes =
                        diff.a_minus_b.iter().map(|&x| (x >> 64) as u64).collect();
                    ResponseValue::P2pSync(lilia_protocol::P2pSyncMessage::FastReconcileResponse {
                        space,
                        changesets,
                        requested_id_hashes,
                    })
                } else {
                    let ibf = database.p2p_ibf(
                        space.as_deref(),
                        lilia_core::InvertibleBloomFilter::with_expected_diff(
                            estimate.estimated_diff,
                        )
                        .map_err(|e| {
                            lilia_core::LiliaError::new(
                                lilia_core::ErrorCode::Storage,
                                e.to_string(),
                                false,
                            )
                        })?
                        .len(),
                    )?;
                    ResponseValue::P2pSync(lilia_protocol::P2pSyncMessage::IbfNegotiation {
                        space,
                        estimated_diff: estimate.estimated_diff,
                        ibf,
                    })
                }
            }
            lilia_protocol::P2pSyncMessage::FastReconcileResponse {
                space,
                changesets,
                requested_id_hashes,
            } => {
                database.p2p_apply_changesets(
                    &changesets,
                    lilia_core::ConflictResolutionStrategy::ThreeWayMerge,
                )?;
                let (_, id_map) = database.p2p_collect_items(space.as_deref())?;
                let req_elements: Vec<u128> = requested_id_hashes
                    .into_iter()
                    .map(|h| u128::from(h) << 64)
                    .collect();
                let outgoing = database.p2p_export_payloads(&req_elements, &id_map)?;
                ResponseValue::P2pSync(lilia_protocol::P2pSyncMessage::ChangesetDelivery {
                    space,
                    changesets: outgoing,
                })
            }
            lilia_protocol::P2pSyncMessage::IbfNegotiation {
                space,
                estimated_diff: _,
                ibf,
            } => {
                let mut local_ibf = database.p2p_ibf(space.as_deref(), ibf.len())?;
                local_ibf.subtract(&ibf).map_err(|e| {
                    lilia_core::LiliaError::new(
                        lilia_core::ErrorCode::Storage,
                        e.to_string(),
                        false,
                    )
                })?;
                let diff = local_ibf.decode().map_err(|e| {
                    lilia_core::LiliaError::new(
                        lilia_core::ErrorCode::Storage,
                        e.to_string(),
                        false,
                    )
                })?;
                let (_, id_map) = database.p2p_collect_items(space.as_deref())?;
                let changesets = database.p2p_export_payloads(&diff.a_minus_b, &id_map)?;
                let requested_id_hashes =
                    diff.b_minus_a.iter().map(|&x| (x >> 64) as u64).collect();
                ResponseValue::P2pSync(lilia_protocol::P2pSyncMessage::IbfReconcileResponse {
                    space,
                    changesets,
                    requested_id_hashes,
                })
            }
            lilia_protocol::P2pSyncMessage::IbfReconcileResponse {
                space,
                changesets,
                requested_id_hashes,
            } => {
                database.p2p_apply_changesets(
                    &changesets,
                    lilia_core::ConflictResolutionStrategy::ThreeWayMerge,
                )?;
                let (_, id_map) = database.p2p_collect_items(space.as_deref())?;
                let req_elements: Vec<u128> = requested_id_hashes
                    .into_iter()
                    .map(|h| u128::from(h) << 64)
                    .collect();
                let outgoing = database.p2p_export_payloads(&req_elements, &id_map)?;
                ResponseValue::P2pSync(lilia_protocol::P2pSyncMessage::ChangesetDelivery {
                    space,
                    changesets: outgoing,
                })
            }
            lilia_protocol::P2pSyncMessage::ChangesetDelivery {
                space: _,
                changesets,
            } => {
                let stats = database.p2p_apply_changesets(
                    &changesets,
                    lilia_core::ConflictResolutionStrategy::ThreeWayMerge,
                )?;
                ResponseValue::Usize(stats.applied_count)
            }
        },
    })
}
