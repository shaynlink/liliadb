//! The `LiliaDB` local daemon: one database served to the processes of its user over a local
//! socket (a named pipe on Windows) and, optionally, a loopback HTTP gateway. Every request runs
//! through [`lilia_protocol::execute`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use lilia_core::{ErrorCode, LiliaError};
use lilia_db::Database;
use lilia_protocol::{Codec, OpenOptions, Operation, Request, Response, ResponseValue};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, Semaphore};
use tracing::info;
use uuid::Uuid;

#[cfg(test)]
mod tests;

#[cfg(unix)]
mod ipc_unix;

mod http;

/// How long a connected client may stay silent before the daemon drops it.
const CLIENT_IDLE: Duration = Duration::from_secs(30);
/// How long closing waits for running requests.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Parser)]
#[command(version, about = "LiliaDB local daemon")]
struct Arguments {
    /// Directory of the database; created if missing.
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    endpoint: PathBuf,
    #[arg(long)]
    token_file: PathBuf,
    /// `durable` (default), `balanced` or `performance`.
    #[arg(long)]
    durability: Option<String>,
    /// `server` (default), `mobile` or `constrained`.
    #[arg(long)]
    profile: Option<String>,
    /// Keep a tamper-evident ledger of every record write.
    #[arg(long)]
    ledger: bool,
    /// How long old versions stay readable, in milliseconds (a week by default).
    #[arg(long)]
    history_retention_ms: Option<u64>,
    /// Commits that can be undone (none by default).
    #[arg(long, default_value_t = 0)]
    undo_depth: u32,
    /// Allow the HTTP gateway to bind addresses other than loopback.
    #[arg(long)]
    development: bool,
    /// Port of a loopback HTTP gateway (127.0.0.1:<PORT>).
    #[arg(long = "http-port")]
    http_port: Option<u16>,
    /// Address of the HTTP gateway.
    #[arg(long = "http-bind")]
    http_bind: Option<std::net::SocketAddr>,
}

impl Arguments {
    fn open_options(&self) -> OpenOptions {
        OpenOptions {
            durability: self.durability.clone(),
            profile: self.profile.clone(),
            ledger: self.ledger,
            history_retention_ms: self.history_retention_ms,
            undo_depth: self.undo_depth,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("lilia_daemon=info")
        .with_writer(std::io::stderr)
        .init();
    let arguments = Arguments::parse();
    let http_addr = resolve_http_bind(&arguments)?;
    // Claim the endpoint before rotating credentials; duplicate startup must not
    // invalidate the token of an already running daemon.
    #[cfg(unix)]
    let listener = ipc_unix::ListenerGuard::bind(&arguments.endpoint)?;
    #[cfg(windows)]
    let security = lilia_io::ipc::UserSecurity::new()?;
    #[cfg(windows)]
    let listener = security.pipe(&arguments.endpoint, true)?;
    #[cfg(unix)]
    let token = create_token(&arguments.token_file)?;
    #[cfg(windows)]
    let credentials = security.credentials(&arguments.token_file)?;
    #[cfg(windows)]
    let token = credentials.token.clone();
    let path = arguments.database.clone();
    let options = arguments.open_options();
    let database =
        Arc::new(tokio::task::spawn_blocking(move || lilia_protocol::open(path, &options)).await??);
    let shutdown = Arc::new(Notify::new());
    info!(database = %arguments.database.display(), "daemon started");

    if let Some(addr) = http_addr {
        let http_listener = tokio::net::TcpListener::bind(addr).await?;
        info!(addr = %addr, "http gateway started");
        let http_database = Arc::clone(&database);
        let http_token = token.clone();
        let http_shutdown = Arc::clone(&shutdown);
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
    security: &lilia_io::ipc::UserSecurity,
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
    tokio::task::spawn_blocking(move || lilia_protocol::close(database, CLOSE_TIMEOUT)).await??;
    Ok(())
}

async fn handle_stream<S>(
    mut stream: S,
    database: Arc<Database>,
    token: &str,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let codec = match tokio::time::timeout(CLIENT_IDLE, Codec::accept(&mut stream)).await {
        Err(_) => return Err(anyhow::anyhow!("client idle timeout")),
        Ok(codec) => codec?,
    };
    loop {
        let request: Request =
            match tokio::time::timeout(CLIENT_IDLE, codec.read_request(&mut stream)).await {
                Err(_) => return Err(anyhow::anyhow!("client idle timeout")),
                Ok(Ok(request)) => request,
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(())
                }
                Ok(Err(error)) => return Err(error.into()),
            };
        let operation_name = request.operation.name();
        let request_id = request.id.clone();
        let should_shutdown = matches!(request.operation, Operation::Shutdown);
        let started = Instant::now();
        let request_uuid = Uuid::parse_str(&request.id).unwrap_or_else(|_| Uuid::new_v4());
        let result = if bool::from(request.token.as_bytes().ct_eq(token.as_bytes())) {
            run_blocking(
                Arc::clone(&database),
                request.operation,
                request.deadline_ms.map(|ms| (started, ms)),
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
        codec
            .write_response(
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

/// Fails once `deadline` (a start and a budget in milliseconds) has passed.
fn check_deadline(deadline: Option<(Instant, u64)>) -> lilia_core::Result<()> {
    if deadline.is_some_and(|(started, ms)| started.elapsed() >= Duration::from_millis(ms)) {
        return Err(LiliaError::new(
            ErrorCode::Timeout,
            "request deadline exceeded",
            true,
        ));
    }
    Ok(())
}

/// Runs `operation` on a blocking worker, unless `deadline` passes before it starts: waiting
/// for a worker counts, but once the operation starts it runs to its outcome, since timing out
/// a running write could hide a committed mutation.
pub(crate) async fn run_blocking(
    database: Arc<Database>,
    operation: Operation,
    deadline: Option<(Instant, u64)>,
) -> lilia_core::Result<ResponseValue> {
    check_deadline(deadline)?;
    tokio::task::spawn_blocking(move || {
        check_deadline(deadline)?;
        lilia_protocol::execute(&database, operation)
    })
    .await
    .map_err(|_| LiliaError::new(ErrorCode::Storage, "request worker failed", false))?
}
