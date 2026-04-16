use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::Router;
use clap::Parser;
use russh::client;
use russh_keys::key::PrivateKeyWithHashAlg;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info};

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
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<RwLock<HashMap<String, SshSession>>>,
}

struct SshSession {
    handle: client::Handle<SshHandler>,
    host: String,
    port: u16,
    username: String,
}

struct SshHandler;

#[async_trait::async_trait]
impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
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

fn default_timeout() -> u64 {
    30
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
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
struct MessageResponse {
    message: String,
}

async fn handle_connect(
    State(state): State<AppState>,
    Json(req): Json<ConnectRequest>,
) -> impl IntoResponse {
    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let port = req.port.unwrap_or(22);

    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(300)),
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    });

    let addr = format!("{}:{}", req.host, port);
    debug!(addr = %addr, "connecting to SSH server");

    let handler = SshHandler;
    let mut handle = match client::connect(config, &addr, handler).await {
        Ok(h) => h,
        Err(e) => {
            error!(error = %e, "SSH connection failed");
            return (
                StatusCode::BAD_GATEWAY,
                Json(
                    serde_json::to_value(ErrorResponse {
                        error: format!("SSH connection failed: {e}"),
                    })
                    .expect("serialize error"),
                ),
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
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(
                            serde_json::to_value(ErrorResponse {
                                error: format!("Invalid private key: {e}"),
                            })
                            .expect("serialize error"),
                        ),
                    );
                }
            };
            let key_with_hash = match PrivateKeyWithHashAlg::new(Arc::new(key_pair), None) {
                Ok(k) => k,
                Err(e) => {
                    error!(error = %e, "failed to prepare private key");
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(
                            serde_json::to_value(ErrorResponse {
                                error: format!("Failed to prepare private key: {e}"),
                            })
                            .expect("serialize error"),
                        ),
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
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    serde_json::to_value(ErrorResponse {
                        error: "Authentication failed: credentials rejected".into(),
                    })
                    .expect("serialize error"),
                ),
            );
        }
        Err(e) => {
            error!(error = %e, "SSH authentication error");
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    serde_json::to_value(ErrorResponse {
                        error: format!("Authentication error: {e}"),
                    })
                    .expect("serialize error"),
                ),
            );
        }
    }

    let session = SshSession {
        handle,
        host: req.host.clone(),
        port,
        username: req.username.clone(),
    };

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), session);

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
    let sessions = state.sessions.read().await;
    let session = match sessions.get(&req.session_id) {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(
                    serde_json::to_value(ErrorResponse {
                        error: format!("Session '{}' not found", req.session_id),
                    })
                    .expect("serialize error"),
                ),
            );
        }
    };

    let channel = match session.handle.channel_open_session().await {
        Ok(ch) => ch,
        Err(e) => {
            error!(error = %e, "failed to open channel");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    serde_json::to_value(ErrorResponse {
                        error: format!("Failed to open SSH channel: {e}"),
                    })
                    .expect("serialize error"),
                ),
            );
        }
    };

    if let Err(e) = channel.exec(true, req.command.as_bytes()).await {
        error!(error = %e, "failed to execute command");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                serde_json::to_value(ErrorResponse {
                    error: format!("Failed to execute command: {e}"),
                })
                .expect("serialize error"),
            ),
        );
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit_code: Option<u32> = None;

    let timeout = Duration::from_secs(req.timeout_secs);
    let result = tokio::time::timeout(timeout, async {
        let mut stream = channel.into_stream();
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 8192];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => stdout.extend_from_slice(&buf[..n]),
                Err(e) => {
                    stderr.extend_from_slice(format!("Read error: {e}").as_bytes());
                    break;
                }
            }
        }
    })
    .await;

    if result.is_err() {
        stderr.extend_from_slice(b"\n[timeout: command exceeded time limit]");
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
    let mut sessions = state.sessions.write().await;
    match sessions.remove(&req.session_id) {
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
        None => (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::to_value(ErrorResponse {
                    error: format!("Session '{}' not found", req.session_id),
                })
                .expect("serialize error"),
            ),
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

    let state = AppState {
        sessions: Arc::new(RwLock::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/sessions", get(handle_list_sessions))
        .route("/connect", post(handle_connect))
        .route("/execute", post(handle_execute))
        .route("/disconnect", delete(handle_disconnect))
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
            "auth": { "type": "password", "password": "secret123" }
        }"#;
        let req: ConnectRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.host, "example.com");
        assert_eq!(req.username, "deploy");
        assert!(req.port.is_none());
        assert!(req.session_id.is_none());
    }

    #[test]
    fn test_connect_request_with_key_auth() {
        let json = r#"{
            "host": "server.internal",
            "port": 2222,
            "username": "admin",
            "session_id": "my-session",
            "auth": { "type": "private_key", "key_pem": "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n-----END OPENSSH PRIVATE KEY-----" }
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
        };
        let json = serde_json::to_string(&info).expect("should serialize");
        assert!(json.contains("test-id"));
        assert!(json.contains("10.0.0.1"));
    }
}
