wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../wit/tool.wit",
});

use serde::{Deserialize, Serialize};

const MAX_TEXT_LENGTH: usize = 65536;
const DEFAULT_GATEWAY_PORT: u16 = 9022;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const HTTP_TIMEOUT_MS: u32 = 60_000;
const HTTP_EXECUTE_TIMEOUT_BUFFER_SECS: u64 = 30;

fn validate_input_length(s: &str, field_name: &str) -> Result<(), String> {
    if s.len() > MAX_TEXT_LENGTH {
        return Err(format!(
            "Input '{}' exceeds maximum length of {} characters",
            field_name, MAX_TEXT_LENGTH
        ));
    }
    Ok(())
}

fn validate_hostname(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("Hostname cannot be empty".into());
    }
    if host.len() > 253 {
        return Err("Hostname too long (max 253 characters)".into());
    }
    if host.contains(' ') || host.contains('\n') || host.contains('\r') {
        return Err("Hostname contains invalid characters".into());
    }
    Ok(())
}

fn validate_command(command: &str) -> Result<(), String> {
    if command.is_empty() {
        return Err("Command cannot be empty".into());
    }
    validate_input_length(command, "command")
}

struct RemoteShellTool;

#[derive(Debug, Deserialize)]
#[serde(tag = "action")]
enum RemoteShellAction {
    #[serde(rename = "connect")]
    Connect {
        host: String,
        port: Option<u16>,
        username: String,
        #[serde(default)]
        auth: AuthMethod,
        session_id: Option<String>,
        host_key_fingerprint: Option<String>,
        #[serde(default)]
        insecure_ignore_host_key: bool,
        gateway_port: Option<u16>,
    },

    #[serde(rename = "execute")]
    Execute {
        session_id: String,
        command: String,
        timeout_secs: Option<u64>,
        gateway_port: Option<u16>,
    },

    #[serde(rename = "disconnect")]
    Disconnect {
        session_id: String,
        gateway_port: Option<u16>,
    },

    #[serde(rename = "list_sessions")]
    ListSessions { gateway_port: Option<u16> },
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

#[derive(Debug, Serialize)]
struct GatewayConnectRequest {
    session_id: Option<String>,
    host: String,
    port: Option<u16>,
    username: String,
    auth: GatewayAuthMethod,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_key_fingerprint: Option<String>,
    insecure_ignore_host_key: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum GatewayAuthMethod {
    Password {
        password: String,
    },
    PrivateKey {
        key_pem: String,
        passphrase: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct GatewayExecuteRequest {
    session_id: String,
    command: String,
    timeout_secs: u64,
}

#[derive(Debug, Serialize)]
struct GatewayDisconnectRequest {
    session_id: String,
}

impl From<AuthMethod> for GatewayAuthMethod {
    fn from(auth: AuthMethod) -> Self {
        match auth {
            AuthMethod::Password { password } => GatewayAuthMethod::Password { password },
            AuthMethod::PrivateKey {
                key_pem,
                passphrase,
            } => GatewayAuthMethod::PrivateKey {
                key_pem,
                passphrase,
            },
        }
    }
}

fn gateway_url(port: Option<u16>) -> String {
    let port = port.unwrap_or(DEFAULT_GATEWAY_PORT);
    format!("http://127.0.0.1:{}", port)
}

fn gateway_request(
    method: &str,
    path: &str,
    body: Option<String>,
    gateway_port: Option<u16>,
    http_timeout_ms: u32,
) -> Result<String, String> {
    let base = gateway_url(gateway_port);
    let url = format!("{}{}", base, path);

    let headers = serde_json::json!({
        "Content-Type": "application/json",
        "Accept": "application/json"
    });

    let body_bytes = body.map(|b| b.into_bytes());

    let response = near::agent::host::http_request(
        method,
        &url,
        &headers.to_string(),
        body_bytes.as_deref(),
        Some(http_timeout_ms),
    )
    .map_err(|e| format!("Gateway request failed: {e}"))?;

    let body_str =
        String::from_utf8(response.body).map_err(|e| format!("Invalid UTF-8 response: {e}"))?;

    if response.status >= 200 && response.status < 300 {
        Ok(body_str)
    } else {
        let msg = serde_json::from_str::<serde_json::Value>(&body_str)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or(body_str);
        Err(format!("Gateway error (HTTP {}): {}", response.status, msg))
    }
}

impl exports::near::agent::tool::Guest for RemoteShellTool {
    fn execute(req: exports::near::agent::tool::Request) -> exports::near::agent::tool::Response {
        match execute_inner(&req.params) {
            Ok(result) => exports::near::agent::tool::Response {
                output: Some(result),
                error: None,
            },
            Err(e) => exports::near::agent::tool::Response {
                output: None,
                error: Some(e),
            },
        }
    }

    fn schema() -> String {
        SCHEMA.to_string()
    }

    fn description() -> String {
        "SSH remote shell: connect to servers and run commands. \
         Actions: \
         'connect' — open an SSH session to a host (returns a session_id); \
         'execute' — run a shell command on an open session (returns stdout, stderr, exit code); \
         'disconnect' — close an SSH session; \
         'list_sessions' — list all currently open sessions. \
         The remote-shell-gateway service must be running locally before use."
            .to_string()
    }
}

#[derive(Deserialize)]
struct ConnectGatewayResponse {
    session_id: String,
    message: String,
}

#[derive(Deserialize)]
struct ExecuteGatewayResponse {
    stdout: String,
    stderr: String,
    exit_code: Option<u32>,
}

#[derive(Deserialize)]
struct SessionInfoItem {
    session_id: String,
    host: String,
    port: u16,
    username: String,
    age_secs: u64,
}

fn format_connect_response(raw: &str) -> Result<String, String> {
    let resp: ConnectGatewayResponse =
        serde_json::from_str(raw).map_err(|e| format!("Failed to parse gateway response: {e}"))?;
    Ok(format!(
        "Connected successfully.\n\
         Session ID: {}\n\
         {}\n\n\
         Use this session_id for 'execute' and 'disconnect' calls.",
        resp.session_id, resp.message
    ))
}

fn format_execute_response(raw: &str) -> Result<String, String> {
    let resp: ExecuteGatewayResponse =
        serde_json::from_str(raw).map_err(|e| format!("Failed to parse gateway response: {e}"))?;
    let exit_str = resp
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown (command may have timed out)".to_string());
    let stdout_section = if resp.stdout.is_empty() {
        "(empty)".to_string()
    } else {
        resp.stdout
    };
    let stderr_section = if resp.stderr.is_empty() {
        "(empty)".to_string()
    } else {
        resp.stderr
    };
    Ok(format!(
        "Exit code: {exit_str}\n\n--- stdout ---\n{stdout_section}\n--- stderr ---\n{stderr_section}"
    ))
}

fn format_sessions_response(raw: &str) -> Result<String, String> {
    let sessions: Vec<SessionInfoItem> =
        serde_json::from_str(raw).map_err(|e| format!("Failed to parse gateway response: {e}"))?;
    if sessions.is_empty() {
        return Ok("No active sessions.".to_string());
    }
    let mut out = format!("Active sessions ({}):\n", sessions.len());
    for s in &sessions {
        out.push_str(&format!(
            "\n  Session ID : {}\n  Host       : {}:{}\n  Username   : {}\n  Age        : {} seconds\n",
            s.session_id, s.host, s.port, s.username, s.age_secs
        ));
    }
    Ok(out)
}

fn execute_inner(params: &str) -> Result<String, String> {
    let action: RemoteShellAction =
        serde_json::from_str(params).map_err(|e| format!("Invalid parameters: {e}"))?;

    match action {
        RemoteShellAction::Connect {
            host,
            port,
            username,
            auth,
            session_id,
            host_key_fingerprint,
            insecure_ignore_host_key,
            gateway_port,
        } => {
            validate_hostname(&host)?;
            if username.is_empty() || username.len() > MAX_TEXT_LENGTH {
                return Err(format!("username must be 1-{MAX_TEXT_LENGTH} characters"));
            }

            let gw_req = GatewayConnectRequest {
                session_id,
                host,
                port,
                username,
                auth: auth.into(),
                host_key_fingerprint,
                insecure_ignore_host_key,
            };
            let body = serde_json::to_string(&gw_req)
                .map_err(|e| format!("Failed to serialize request: {e}"))?;
            let raw = gateway_request(
                "POST",
                "/connect",
                Some(body),
                gateway_port,
                HTTP_TIMEOUT_MS,
            )?;
            format_connect_response(&raw)
        }

        RemoteShellAction::Execute {
            session_id,
            command,
            timeout_secs,
            gateway_port,
        } => {
            if session_id.is_empty() || session_id.len() > MAX_TEXT_LENGTH {
                return Err(format!("session_id must be 1-{MAX_TEXT_LENGTH} characters"));
            }
            validate_command(&command)?;

            let timeout_secs = timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
            let http_timeout_ms = ((timeout_secs + HTTP_EXECUTE_TIMEOUT_BUFFER_SECS) * 1000)
                .min(u64::from(u32::MAX)) as u32;

            let gw_req = GatewayExecuteRequest {
                session_id,
                command,
                timeout_secs,
            };
            let body = serde_json::to_string(&gw_req)
                .map_err(|e| format!("Failed to serialize request: {e}"))?;
            let raw = gateway_request(
                "POST",
                "/execute",
                Some(body),
                gateway_port,
                http_timeout_ms,
            )?;
            format_execute_response(&raw)
        }

        RemoteShellAction::Disconnect {
            session_id,
            gateway_port,
        } => {
            if session_id.is_empty() || session_id.len() > MAX_TEXT_LENGTH {
                return Err(format!("session_id must be 1-{MAX_TEXT_LENGTH} characters"));
            }
            let session_id_display = session_id.clone();
            let gw_req = GatewayDisconnectRequest { session_id };
            let body = serde_json::to_string(&gw_req)
                .map_err(|e| format!("Failed to serialize request: {e}"))?;
            gateway_request(
                "DELETE",
                "/disconnect",
                Some(body),
                gateway_port,
                HTTP_TIMEOUT_MS,
            )?;
            Ok(format!(
                "Session '{session_id_display}' disconnected successfully."
            ))
        }

        RemoteShellAction::ListSessions { gateway_port } => {
            let raw = gateway_request("GET", "/sessions", None, gateway_port, HTTP_TIMEOUT_MS)?;
            format_sessions_response(&raw)
        }
    }
}

const SCHEMA: &str = r#"{
    "type": "object",
    "required": ["action"],
    "oneOf": [
        {
            "properties": {
                "action": { "const": "connect" },
                "host": { "type": "string", "description": "SSH server hostname or IP address" },
                "port": { "type": "integer", "description": "SSH port (default: 22)", "default": 22 },
                "username": { "type": "string", "description": "SSH username" },
                "auth": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "type": { "const": "password" },
                                "password": { "type": "string", "description": "SSH password" }
                            },
                            "required": ["type", "password"],
                            "description": "Password auth — example: {\"type\": \"password\", \"password\": \"mypass\"}"
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": { "const": "private_key" },
                                "key_pem": { "type": "string", "description": "PEM-encoded private key content (full -----BEGIN ... END----- block)" },
                                "passphrase": { "type": "string", "description": "Key passphrase (omit if key is not encrypted)" }
                            },
                            "required": ["type", "key_pem"],
                            "description": "Private key auth — example: {\"type\": \"private_key\", \"key_pem\": \"-----BEGIN OPENSSH PRIVATE KEY-----\\n...\\n-----END OPENSSH PRIVATE KEY-----\"}"
                        }
                    ],
                    "description": "Authentication credentials. Choose 'password' for password auth or 'private_key' for key-based auth."
                },
                "session_id": { "type": "string", "description": "Optional name for this session (auto-generated UUID if omitted). Use a memorable name like 'prod' or 'staging' for easy reference." },
                "host_key_fingerprint": { "type": "string", "description": "SHA256 fingerprint of the server's host key for MITM protection. Obtain with: ssh-keyscan <host> | ssh-keygen -lf -. Either this or insecure_ignore_host_key must be provided." },
                "insecure_ignore_host_key": { "type": "boolean", "description": "Set true to skip host key verification. INSECURE — vulnerable to MITM attacks. Only use on fully trusted private networks when you cannot get the fingerprint.", "default": false },
                "gateway_port": { "type": "integer", "description": "SSH gateway port (default: 9022)" }
            },
            "required": ["action", "host", "username", "auth"]
        },
        {
            "properties": {
                "action": { "const": "execute" },
                "session_id": { "type": "string", "description": "Session ID from a previous connect call" },
                "command": { "type": "string", "description": "Shell command to execute on the remote machine" },
                "timeout_secs": { "type": "integer", "description": "Command timeout in seconds (default: 30)", "default": 30 },
                "gateway_port": { "type": "integer", "description": "SSH gateway port (default: 9022)" }
            },
            "required": ["action", "session_id", "command"]
        },
        {
            "properties": {
                "action": { "const": "disconnect" },
                "session_id": { "type": "string", "description": "Session ID to disconnect" },
                "gateway_port": { "type": "integer", "description": "SSH gateway port (default: 9022)" }
            },
            "required": ["action", "session_id"]
        },
        {
            "properties": {
                "action": { "const": "list_sessions" },
                "gateway_port": { "type": "integer", "description": "SSH gateway port (default: 9022)" }
            },
            "required": ["action"]
        }
    ]
}"#;

export!(RemoteShellTool);

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_actions() -> std::collections::HashSet<String> {
        let schema: serde_json::Value =
            serde_json::from_str(SCHEMA).expect("schema should be valid JSON");
        schema["oneOf"]
            .as_array()
            .expect("schema.oneOf should be an array")
            .iter()
            .filter_map(|variant| {
                variant
                    .pointer("/properties/action/const")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect()
    }

    #[test]
    fn test_schema_is_valid_json() {
        let _: serde_json::Value =
            serde_json::from_str(SCHEMA).expect("schema should be valid JSON");
    }

    #[test]
    fn test_schema_has_all_actions() {
        let actions = schema_actions();
        assert!(actions.contains("connect"));
        assert!(actions.contains("execute"));
        assert!(actions.contains("disconnect"));
        assert!(actions.contains("list_sessions"));
        assert_eq!(actions.len(), 4);
    }

    #[test]
    fn test_validate_hostname() {
        assert!(validate_hostname("example.com").is_ok());
        assert!(validate_hostname("192.168.1.1").is_ok());
        assert!(validate_hostname("my-server.internal").is_ok());
        assert!(validate_hostname("").is_err());
        assert!(validate_hostname("host name").is_err());
        assert!(validate_hostname("host\nname").is_err());
        let long = "a".repeat(254);
        assert!(validate_hostname(&long).is_err());
    }

    #[test]
    fn test_validate_command() {
        assert!(validate_command("ls -la").is_ok());
        assert!(validate_command("echo hello && cat /etc/hostname").is_ok());
        assert!(validate_command("").is_err());
        let long = "a".repeat(MAX_TEXT_LENGTH + 1);
        assert!(validate_command(&long).is_err());
    }

    #[test]
    fn test_validate_input_length() {
        assert!(validate_input_length("short", "test").is_ok());
        let long = "a".repeat(MAX_TEXT_LENGTH + 1);
        assert!(validate_input_length(&long, "test").is_err());
    }

    #[test]
    fn test_connect_action_deserialization() {
        let json = r#"{
            "action": "connect",
            "host": "server.example.com",
            "username": "deploy",
            "auth": { "type": "password", "password": "s3cret" },
            "insecure_ignore_host_key": true
        }"#;
        let action: RemoteShellAction = serde_json::from_str(json).expect("should deserialize");
        match action {
            RemoteShellAction::Connect {
                host,
                username,
                insecure_ignore_host_key,
                host_key_fingerprint,
                ..
            } => {
                assert_eq!(host, "server.example.com");
                assert_eq!(username, "deploy");
                assert!(insecure_ignore_host_key);
                assert!(host_key_fingerprint.is_none());
            }
            _ => panic!("expected Connect action"),
        }
    }

    #[test]
    fn test_connect_with_fingerprint() {
        let json = r#"{
            "action": "connect",
            "host": "server.example.com",
            "username": "deploy",
            "auth": { "type": "password", "password": "s3cret" },
            "host_key_fingerprint": "SHA256:abc123"
        }"#;
        let action: RemoteShellAction = serde_json::from_str(json).expect("should deserialize");
        match action {
            RemoteShellAction::Connect {
                host_key_fingerprint,
                insecure_ignore_host_key,
                ..
            } => {
                assert_eq!(host_key_fingerprint, Some("SHA256:abc123".into()));
                assert!(!insecure_ignore_host_key);
            }
            _ => panic!("expected Connect action"),
        }
    }

    #[test]
    fn test_connect_with_key_auth() {
        let json = r#"{
            "action": "connect",
            "host": "10.0.0.5",
            "port": 2222,
            "username": "admin",
            "session_id": "prod-server",
            "auth": {
                "type": "private_key",
                "key_pem": "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n-----END OPENSSH PRIVATE KEY-----"
            },
            "insecure_ignore_host_key": true
        }"#;
        let action: RemoteShellAction = serde_json::from_str(json).expect("should deserialize");
        match action {
            RemoteShellAction::Connect {
                host,
                port,
                session_id,
                ..
            } => {
                assert_eq!(host, "10.0.0.5");
                assert_eq!(port, Some(2222));
                assert_eq!(session_id, Some("prod-server".into()));
            }
            _ => panic!("expected Connect action"),
        }
    }

    #[test]
    fn test_execute_action_deserialization() {
        let json = r#"{
            "action": "execute",
            "session_id": "my-session",
            "command": "uptime"
        }"#;
        let action: RemoteShellAction = serde_json::from_str(json).expect("should deserialize");
        match action {
            RemoteShellAction::Execute {
                session_id,
                command,
                timeout_secs,
                ..
            } => {
                assert_eq!(session_id, "my-session");
                assert_eq!(command, "uptime");
                assert!(timeout_secs.is_none());
            }
            _ => panic!("expected Execute action"),
        }
    }

    #[test]
    fn test_disconnect_action_deserialization() {
        let json = r#"{"action": "disconnect", "session_id": "abc-123"}"#;
        let action: RemoteShellAction = serde_json::from_str(json).expect("should deserialize");
        match action {
            RemoteShellAction::Disconnect { session_id, .. } => {
                assert_eq!(session_id, "abc-123");
            }
            _ => panic!("expected Disconnect action"),
        }
    }

    #[test]
    fn test_list_sessions_deserialization() {
        let json = r#"{"action": "list_sessions"}"#;
        let action: RemoteShellAction = serde_json::from_str(json).expect("should deserialize");
        assert!(matches!(action, RemoteShellAction::ListSessions { .. }));
    }

    #[test]
    fn test_gateway_url_default() {
        assert_eq!(gateway_url(None), "http://127.0.0.1:9022");
    }

    #[test]
    fn test_gateway_url_custom() {
        assert_eq!(gateway_url(Some(8888)), "http://127.0.0.1:8888");
    }

    #[test]
    fn test_auth_conversion() {
        let auth = AuthMethod::Password {
            password: "test".into(),
        };
        let gw: GatewayAuthMethod = auth.into();
        match gw {
            GatewayAuthMethod::Password { password } => assert_eq!(password, "test"),
            _ => panic!("expected Password"),
        }

        let auth = AuthMethod::PrivateKey {
            key_pem: "key-data".into(),
            passphrase: Some("pass".into()),
        };
        let gw: GatewayAuthMethod = auth.into();
        match gw {
            GatewayAuthMethod::PrivateKey {
                key_pem,
                passphrase,
            } => {
                assert_eq!(key_pem, "key-data");
                assert_eq!(passphrase, Some("pass".into()));
            }
            _ => panic!("expected PrivateKey"),
        }
    }

    #[test]
    fn test_gateway_connect_request_includes_host_key_fields() {
        let req = GatewayConnectRequest {
            session_id: None,
            host: "example.com".into(),
            port: None,
            username: "user".into(),
            auth: GatewayAuthMethod::Password {
                password: "pass".into(),
            },
            host_key_fingerprint: Some("SHA256:xyz".into()),
            insecure_ignore_host_key: false,
        };
        let json = serde_json::to_string(&req).expect("should serialize");
        assert!(json.contains("host_key_fingerprint"));
        assert!(json.contains("SHA256:xyz"));
        assert!(json.contains("insecure_ignore_host_key"));

        let req2 = GatewayConnectRequest {
            session_id: None,
            host: "example.com".into(),
            port: None,
            username: "user".into(),
            auth: GatewayAuthMethod::Password {
                password: "pass".into(),
            },
            host_key_fingerprint: None,
            insecure_ignore_host_key: true,
        };
        let json2 = serde_json::to_string(&req2).expect("should serialize");
        assert!(!json2.contains("host_key_fingerprint"));
        assert!(json2.contains("\"insecure_ignore_host_key\":true"));
    }

    #[test]
    fn test_invalid_action_parse_error() {
        let json = r#"{"action": "nonexistent"}"#;
        let result: Result<RemoteShellAction, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_format_connect_response() {
        let raw =
            r#"{"session_id":"abc-123","message":"Connected to server.example.com:22 as deploy"}"#;
        let result = format_connect_response(raw).expect("should format");
        assert!(result.contains("abc-123"));
        assert!(result.contains("Session ID:"));
        assert!(!result.contains("list_sessions"));
    }

    #[test]
    fn test_format_execute_response_with_output() {
        let raw = r#"{"stdout":"hello world\n","stderr":"","exit_code":0}"#;
        let result = format_execute_response(raw).expect("should format");
        assert!(result.contains("Exit code: 0"));
        assert!(result.contains("hello world"));
        assert!(result.contains("--- stdout ---"));
        assert!(result.contains("--- stderr ---"));
        assert!(result.contains("(empty)"));
    }

    #[test]
    fn test_format_execute_response_no_exit_code() {
        let raw = r#"{"stdout":"","stderr":"timeout\n","exit_code":null}"#;
        let result = format_execute_response(raw).expect("should format");
        assert!(result.contains("unknown (command may have timed out)"));
        assert!(result.contains("timeout"));
    }

    #[test]
    fn test_format_sessions_response_empty() {
        let result = format_sessions_response("[]").expect("should format");
        assert_eq!(result, "No active sessions.");
    }

    #[test]
    fn test_format_sessions_response_with_sessions() {
        let raw = r#"[{"session_id":"prod","host":"server.example.com","port":22,"username":"deploy","age_secs":120}]"#;
        let result = format_sessions_response(raw).expect("should format");
        assert!(result.contains("Active sessions (1)"));
        assert!(result.contains("prod"));
        assert!(result.contains("server.example.com:22"));
        assert!(result.contains("120 seconds"));
    }

    #[test]
    fn test_http_execute_timeout_scales_with_command_timeout() {
        let timeout_secs: u64 = 120;
        let http_timeout_ms = ((timeout_secs + HTTP_EXECUTE_TIMEOUT_BUFFER_SECS) * 1000)
            .min(u64::from(u32::MAX)) as u32;
        assert_eq!(http_timeout_ms, 150_000);
        assert!(http_timeout_ms > HTTP_TIMEOUT_MS);
    }

    #[test]
    fn test_description_lists_all_actions() {
        use crate::exports::near::agent::tool::Guest;
        let desc = RemoteShellTool::description();
        assert!(desc.contains("connect"));
        assert!(desc.contains("execute"));
        assert!(desc.contains("disconnect"));
        assert!(desc.contains("list_sessions"));
    }
}
