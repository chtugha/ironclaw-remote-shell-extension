use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::Router;
use clap::Parser;
use constant_time_eq::constant_time_eq;
use russh::client;
use russh::ChannelMsg;
use russh_keys::key::PrivateKeyWithHashAlg;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

const DEFAULT_MAX_SESSIONS: usize = 64;
const DEFAULT_SESSION_TTL_SECS: u64 = 3600;
const DEFAULT_SSH_PORT: u16 = 22;
const SSH_INACTIVITY_TIMEOUT_SECS: u64 = 300;
const SSH_KEEPALIVE_INTERVAL_SECS: u64 = 30;
const SSH_KEEPALIVE_MAX: usize = 3;
const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;
const MIN_TIMEOUT_SECS: u64 = 1;
const MAX_TIMEOUT_SECS: u64 = 3600;
const MAX_INPUT_LENGTH: usize = 65536;
const MAX_HOSTNAME_LENGTH: usize = 253;

#[derive(Parser)]
#[command(
    name = "remote-shell-gateway",
    about = "SSH gateway for IronClaw remote-shell extension"
)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long, default_value = "9022")]
    port: u16,

    #[arg(long, default_value_t = DEFAULT_MAX_SESSIONS)]
    max_sessions: usize,

    #[arg(long, default_value_t = DEFAULT_SESSION_TTL_SECS)]
    session_ttl_secs: u64,
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<RwLock<HashMap<String, Arc<SshSession>>>>,
    bearer_token: Option<String>,
    max_sessions: usize,
    session_ttl: Duration,
}

struct SshSession {
    handle: client::Handle<SshHandler>,
    host: String,
    port: u16,
    username: String,
    created_at: Instant,
}

struct SshHandler {
    expected_fingerprint: Option<String>,
}

#[async_trait::async_trait]
impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match &self.expected_fingerprint {
            Some(expected) => {
                let fp = server_public_key.fingerprint(ssh_key::HashAlg::Sha256);
                let actual = fp.to_string();
                if actual == *expected {
                    Ok(true)
                } else {
                    error!(
                        expected = %expected,
                        actual = %actual,
                        "host key fingerprint mismatch"
                    );
                    Ok(false)
                }
            }
            None => {
                warn!("host key verification skipped — insecure_ignore_host_key was set");
                Ok(true)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConnectRequest {
    session_id: Option<String>,
    host: String,
    port: Option<u16>,
    username: String,
    #[serde(default)]
    auth: AuthMethod,
    host_key_fingerprint: Option<String>,
    #[serde(default)]
    insecure_ignore_host_key: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AuthMethod {
    Password {
        password: String,
    },
    PrivateKey {
        key_pem: String,
        passphrase: Option<String>,
    },
}

impl Default for AuthMethod {
    fn default() -> Self {
        AuthMethod::Password {
            password: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ExecuteRequest {
    session_id: String,
    command: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 30;

fn default_timeout() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_SECS
}

#[derive(Debug, Deserialize)]
struct DisconnectRequest {
    session_id: String,
}

#[derive(Debug, Serialize)]
struct ConnectResponse {
    session_id: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct ExecuteResponse {
    stdout: String,
    stderr: String,
    exit_code: Option<u32>,
}

#[derive(Debug, Serialize)]
struct SessionInfo {
    session_id: String,
    host: String,
    port: u16,
    username: String,
    age_secs: u64,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
struct MessageResponse {
    message: String,
}

fn error_json(status: StatusCode, msg: String) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::to_value(ErrorResponse { error: msg }).expect("serialize error")),
    )
}

async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: middleware::Next,
) -> impl IntoResponse {
    if let Some(expected) = &state.bearer_token {
        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match provided {
            Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {}
            _ => {
                return error_json(
                    StatusCode::UNAUTHORIZED,
                    "Invalid or missing bearer token".into(),
                )
                .into_response();
            }
        }
    }
    next.run(request).await.into_response()
}

async fn reap_expired_sessions(state: &AppState) {
    let expired: Vec<(String, Arc<SshSession>)> = {
        let mut sessions = state.sessions.write().await;
        let mut expired = Vec::new();
        sessions.retain(|id, s| {
            let alive = s.created_at.elapsed() < state.session_ttl;
            if !alive {
                info!(session_id = %id, "session expired, reaping");
                expired.push((id.clone(), Arc::clone(s)));
            }
            alive
        });
        expired
    };
    for (id, session) in &expired {
        let _ = session
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
        debug!(session_id = %id, "expired session disconnected");
    }
}

async fn handle_connect(
    State(state): State<AppState>,
    Json(req): Json<ConnectRequest>,
) -> impl IntoResponse {
    reap_expired_sessions(&state).await;

    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let port = req.port.unwrap_or(DEFAULT_SSH_PORT);

    if req.host.is_empty() || req.host.len() > MAX_HOSTNAME_LENGTH {
        return error_json(
            StatusCode::BAD_REQUEST,
            format!("Hostname must be 1-{MAX_HOSTNAME_LENGTH} characters"),
        );
    }
    if req.host.contains(' ') || req.host.contains('\n') || req.host.contains('\r') {
        return error_json(
            StatusCode::BAD_REQUEST,
            "Hostname contains invalid characters".into(),
        );
    }
    if req.username.is_empty() || req.username.len() > MAX_INPUT_LENGTH {
        return error_json(
            StatusCode::BAD_REQUEST,
            "Username must be 1-65536 characters".into(),
        );
    }

    {
        let sessions = state.sessions.read().await;
        if sessions.contains_key(&session_id) {
            return error_json(
                StatusCode::CONFLICT,
                format!("Session '{}' already exists. Disconnect it first or use a different session_id.", session_id),
            );
        }
        if sessions.len() >= state.max_sessions {
            return error_json(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Maximum number of sessions ({}) reached. Disconnect unused sessions first.",
                    state.max_sessions
                ),
            );
        }
    }

    let expected_fingerprint = if req.insecure_ignore_host_key {
        warn!(host = %req.host, "connecting with host key verification DISABLED — vulnerable to MITM");
        None
    } else if let Some(fp) = req.host_key_fingerprint {
        Some(fp)
    } else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "Either 'host_key_fingerprint' must be provided for secure connections, \
             or 'insecure_ignore_host_key' must be set to true. \
             Get the fingerprint with: ssh-keyscan <host> | ssh-keygen -lf -"
                .into(),
        );
    };

    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(SSH_INACTIVITY_TIMEOUT_SECS)),
        keepalive_interval: Some(Duration::from_secs(SSH_KEEPALIVE_INTERVAL_SECS)),
        keepalive_max: SSH_KEEPALIVE_MAX,
        ..Default::default()
    });

    let addr = format!("{}:{}", req.host, port);
    debug!(addr = %addr, "connecting to SSH server");

    let handler = SshHandler {
        expected_fingerprint,
    };

    let mut handle = match client::connect(config, &addr, handler).await {
        Ok(h) => h,
        Err(e) => {
            error!(error = %e, "SSH connection failed");
            return error_json(
                StatusCode::BAD_GATEWAY,
                format!("SSH connection failed: {e}"),
            );
        }
    };

    let auth_result = match &req.auth {
        AuthMethod::Password { password } => {
            handle.authenticate_password(&req.username, password).await
        }
        AuthMethod::PrivateKey {
            key_pem,
            passphrase,
        } => {
            let key_pair = match russh_keys::decode_secret_key(key_pem, passphrase.as_deref()) {
                Ok(k) => k,
                Err(e) => {
                    error!(error = %e, "failed to decode private key");
                    return error_json(
                        StatusCode::BAD_REQUEST,
                        format!("Invalid private key: {e}"),
                    );
                }
            };
            let key_with_hash = match PrivateKeyWithHashAlg::new(Arc::new(key_pair), None) {
                Ok(k) => k,
                Err(e) => {
                    error!(error = %e, "failed to prepare private key");
                    return error_json(
                        StatusCode::BAD_REQUEST,
                        format!("Failed to prepare private key: {e}"),
                    );
                }
            };
            handle
                .authenticate_publickey(&req.username, key_with_hash)
                .await
        }
    };

    match auth_result {
        Ok(true) => {}
        Ok(false) => {
            return error_json(
                StatusCode::UNAUTHORIZED,
                "Authentication failed: credentials rejected".into(),
            );
        }
        Err(e) => {
            error!(error = %e, "SSH authentication error");
            return error_json(
                StatusCode::UNAUTHORIZED,
                format!("Authentication error: {e}"),
            );
        }
    }

    let session = Arc::new(SshSession {
        handle,
        host: req.host.clone(),
        port,
        username: req.username.clone(),
        created_at: Instant::now(),
    });

    {
        let mut sessions = state.sessions.write().await;
        if sessions.contains_key(&session_id) {
            drop(sessions);
            let _ = session
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            return error_json(
                StatusCode::CONFLICT,
                format!("Session '{}' already exists. Disconnect it first or use a different session_id.", session_id),
            );
        }
        if sessions.len() >= state.max_sessions {
            drop(sessions);
            let _ = session
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            return error_json(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Maximum number of sessions ({}) reached. Disconnect unused sessions first.",
                    state.max_sessions
                ),
            );
        }
        sessions.insert(session_id.clone(), session);
    }

    info!(session_id = %session_id, host = %req.host, "SSH session established");

    (
        StatusCode::OK,
        Json(
            serde_json::to_value(ConnectResponse {
                session_id,
                message: format!("Connected to {}:{} as {}", req.host, port, req.username),
            })
            .expect("serialize response"),
        ),
    )
}

async fn handle_execute(
    State(state): State<AppState>,
    Json(req): Json<ExecuteRequest>,
) -> impl IntoResponse {
    if req.session_id.is_empty() || req.session_id.len() > MAX_INPUT_LENGTH {
        return error_json(StatusCode::BAD_REQUEST, "Invalid session_id length".into());
    }
    if req.command.is_empty() || req.command.len() > MAX_INPUT_LENGTH {
        return error_json(
            StatusCode::BAD_REQUEST,
            format!("Command must be 1-{MAX_INPUT_LENGTH} characters"),
        );
    }
    if req.timeout_secs < MIN_TIMEOUT_SECS || req.timeout_secs > MAX_TIMEOUT_SECS {
        return error_json(
            StatusCode::BAD_REQUEST,
            format!("timeout_secs must be {MIN_TIMEOUT_SECS}-{MAX_TIMEOUT_SECS}"),
        );
    }

    let session = {
        let sessions = state.sessions.read().await;
        match sessions.get(&req.session_id) {
            Some(s) => {
                if s.created_at.elapsed() >= state.session_ttl {
                    drop(sessions);
                    let mut sessions = state.sessions.write().await;
                    sessions.remove(&req.session_id);
                    return error_json(
                        StatusCode::GONE,
                        format!("Session '{}' has expired", req.session_id),
                    );
                }
                Arc::clone(s)
            }
            None => {
                return error_json(
                    StatusCode::NOT_FOUND,
                    format!("Session '{}' not found", req.session_id),
                );
            }
        }
    };

    let mut channel = match session.handle.channel_open_session().await {
        Ok(ch) => ch,
        Err(e) => {
            error!(error = %e, "failed to open channel");
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open SSH channel: {e}"),
            );
        }
    };

    if let Err(e) = channel.exec(true, req.command.as_bytes()).await {
        error!(error = %e, "failed to execute command");
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to execute command: {e}"),
        );
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: Option<u32> = None;
    let mut truncated = false;

    let timeout = Duration::from_secs(req.timeout_secs);
    let timed_out = tokio::time::timeout(timeout, async {
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    let remaining = MAX_OUTPUT_BYTES.saturating_sub(stdout.len());
                    if remaining > 0 {
                        let take = data.len().min(remaining);
                        stdout.extend_from_slice(&data[..take]);
                        if take < data.len() {
                            truncated = true;
                        }
                    }
                }
                ChannelMsg::ExtendedData { data, ext } => {
                    if ext == 1 {
                        let remaining = MAX_OUTPUT_BYTES.saturating_sub(stderr.len());
                        if remaining > 0 {
                            let take = data.len().min(remaining);
                            stderr.extend_from_slice(&data[..take]);
                            if take < data.len() {
                                truncated = true;
                            }
                        }
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => {
                    exit_code = Some(exit_status);
                }
                ChannelMsg::Close => {
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .is_err();

    if timed_out {
        stderr.extend_from_slice(b"\n[timeout: command exceeded time limit]");
    }
    if truncated {
        stderr.extend_from_slice(b"\n[warning: output truncated at 10MB limit]");
    }

    let stdout_str = String::from_utf8_lossy(&stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&stderr).to_string();

    debug!(
        session_id = %req.session_id,
        command = %req.command,
        exit_code = ?exit_code,
        "command executed"
    );

    (
        StatusCode::OK,
        Json(
            serde_json::to_value(ExecuteResponse {
                stdout: stdout_str,
                stderr: stderr_str,
                exit_code,
            })
            .expect("serialize response"),
        ),
    )
}

async fn handle_disconnect(
    State(state): State<AppState>,
    Json(req): Json<DisconnectRequest>,
) -> impl IntoResponse {
    if req.session_id.is_empty() || req.session_id.len() > MAX_INPUT_LENGTH {
        return error_json(StatusCode::BAD_REQUEST, "Invalid session_id length".into());
    }
    let session = {
        let mut sessions = state.sessions.write().await;
        sessions.remove(&req.session_id)
    };
    match session {
        Some(session) => {
            let _ = session
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            info!(session_id = %req.session_id, "SSH session disconnected");
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(MessageResponse {
                        message: format!("Session '{}' disconnected", req.session_id),
                    })
                    .expect("serialize response"),
                ),
            )
        }
        None => error_json(
            StatusCode::NOT_FOUND,
            format!("Session '{}' not found", req.session_id),
        ),
    }
}

async fn handle_list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    let list: Vec<SessionInfo> = sessions
        .iter()
        .map(|(id, s)| SessionInfo {
            session_id: id.clone(),
            host: s.host.clone(),
            port: s.port,
            username: s.username.clone(),
            age_secs: s.created_at.elapsed().as_secs(),
        })
        .collect();

    Json(serde_json::to_value(list).expect("serialize sessions"))
}

async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let bearer_token = std::env::var("SSH_GATEWAY_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    if bearer_token.is_some() {
        info!("bearer token authentication enabled");
    } else {
        warn!("no SSH_GATEWAY_TOKEN set — gateway is unauthenticated, only bind to localhost");
    }

    let state = AppState {
        sessions: Arc::new(RwLock::new(HashMap::new())),
        bearer_token,
        max_sessions: cli.max_sessions,
        session_ttl: Duration::from_secs(cli.session_ttl_secs),
    };

    let protected = Router::new()
        .route("/sessions", get(handle_list_sessions))
        .route("/connect", post(handle_connect))
        .route("/execute", post(handle_execute))
        .route("/disconnect", delete(handle_disconnect))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    let app = Router::new()
        .route("/health", get(handle_health))
        .merge(protected)
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
    info!(addr = %addr, "remote-shell-gateway starting");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_auth_method() {
        let auth = AuthMethod::default();
        match auth {
            AuthMethod::Password { password } => assert!(password.is_empty()),
            _ => panic!("default should be Password"),
        }
    }

    #[test]
    fn test_connect_request_deserialization() {
        let json = r#"{
            "host": "example.com",
            "username": "deploy",
            "auth": { "type": "password", "password": "secret123" },
            "insecure_ignore_host_key": true
        }"#;
        let req: ConnectRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.host, "example.com");
        assert_eq!(req.username, "deploy");
        assert!(req.port.is_none());
        assert!(req.session_id.is_none());
        assert!(req.insecure_ignore_host_key);
        assert!(req.host_key_fingerprint.is_none());
    }

    #[test]
    fn test_connect_request_with_fingerprint() {
        let json = r#"{
            "host": "example.com",
            "username": "deploy",
            "auth": { "type": "password", "password": "secret123" },
            "host_key_fingerprint": "SHA256:abcdef123456"
        }"#;
        let req: ConnectRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.host_key_fingerprint, Some("SHA256:abcdef123456".into()));
        assert!(!req.insecure_ignore_host_key);
    }

    #[test]
    fn test_connect_request_with_key_auth() {
        let json = r#"{
            "host": "server.internal",
            "port": 2222,
            "username": "admin",
            "session_id": "my-session",
            "auth": { "type": "private_key", "key_pem": "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n-----END OPENSSH PRIVATE KEY-----" },
            "insecure_ignore_host_key": true
        }"#;
        let req: ConnectRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.port, Some(2222));
        assert_eq!(req.session_id, Some("my-session".into()));
        match &req.auth {
            AuthMethod::PrivateKey {
                key_pem,
                passphrase,
            } => {
                assert!(key_pem.contains("PRIVATE KEY"));
                assert!(passphrase.is_none());
            }
            _ => panic!("expected PrivateKey auth"),
        }
    }

    #[test]
    fn test_execute_request_default_timeout() {
        let json = r#"{"session_id": "abc", "command": "ls -la"}"#;
        let req: ExecuteRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.timeout_secs, 30);
    }

    #[test]
    fn test_execute_request_custom_timeout() {
        let json = r#"{"session_id": "abc", "command": "long-task", "timeout_secs": 120}"#;
        let req: ExecuteRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.timeout_secs, 120);
    }

    #[test]
    fn test_session_info_serialization() {
        let info = SessionInfo {
            session_id: "test-id".into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            age_secs: 42,
        };
        let json = serde_json::to_string(&info).expect("should serialize");
        assert!(json.contains("test-id"));
        assert!(json.contains("10.0.0.1"));
        assert!(json.contains("\"age_secs\":42"));
    }

    #[test]
    fn test_connect_request_defaults() {
        let json = r#"{
            "host": "example.com",
            "username": "user"
        }"#;
        let req: ConnectRequest = serde_json::from_str(json).expect("should deserialize");
        assert!(!req.insecure_ignore_host_key);
        assert!(req.host_key_fingerprint.is_none());
    }
}
