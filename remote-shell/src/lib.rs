wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../wit/tool.wit",
});

use serde::{Deserialize, Serialize};

const MAX_TEXT_LENGTH: usize = 65536;
const DEFAULT_GATEWAY_PORT: u16 = 9022;

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
        Some(60000),
    )
    .map_err(|e| format!("Gateway request failed: {e}"))?;

    let body_str =
        String::from_utf8(response.body).map_err(|e| format!("Invalid UTF-8 response: {e}"))?;

    if response.status >= 200 && response.status < 300 {
        Ok(body_str)
    } else {
        Err(format!(
            "Gateway error (HTTP {}): {}",
            response.status, body_str
        ))
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
        "Connect to remote machines via SSH and execute commands. \
         Manages persistent SSH sessions through a local gateway service. \
         Supports password and private key authentication. \
         Start the gateway first: remote-shell-gateway"
            .to_string()
    }
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
            gateway_port,
        } => {
            validate_hostname(&host)?;
            validate_input_length(&username, "username")?;

            let gw_req = GatewayConnectRequest {
                session_id,
                host,
                port,
                username,
                auth: auth.into(),
            };
            let body = serde_json::to_string(&gw_req)
                .map_err(|e| format!("Failed to serialize request: {e}"))?;
            gateway_request("POST", "/connect", Some(body), gateway_port)
        }

        RemoteShellAction::Execute {
            session_id,
            command,
            timeout_secs,
            gateway_port,
        } => {
            validate_input_length(&session_id, "session_id")?;
            validate_command(&command)?;

            let gw_req = GatewayExecuteRequest {
                session_id,
                command,
                timeout_secs: timeout_secs.unwrap_or(30),
            };
            let body = serde_json::to_string(&gw_req)
                .map_err(|e| format!("Failed to serialize request: {e}"))?;
            gateway_request("POST", "/execute", Some(body), gateway_port)
        }

        RemoteShellAction::Disconnect {
            session_id,
            gateway_port,
        } => {
            validate_input_length(&session_id, "session_id")?;

            let gw_req = GatewayDisconnectRequest { session_id };
            let body = serde_json::to_string(&gw_req)
                .map_err(|e| format!("Failed to serialize request: {e}"))?;
            gateway_request("DELETE", "/disconnect", Some(body), gateway_port)
        }

        RemoteShellAction::ListSessions { gateway_port } => {
            gateway_request("GET", "/sessions", None, gateway_port)
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
                            "required": ["type", "password"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": { "const": "private_key" },
                                "key_pem": { "type": "string", "description": "PEM-encoded private key" },
                                "passphrase": { "type": "string", "description": "Key passphrase (if encrypted)" }
                            },
                            "required": ["type", "key_pem"]
                        }
                    ],
                    "description": "Authentication method"
                },
                "session_id": { "type": "string", "description": "Optional session identifier (auto-generated if omitted)" },
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
            "auth": { "type": "password", "password": "s3cret" }
        }"#;
        let action: RemoteShellAction = serde_json::from_str(json).expect("should deserialize");
        match action {
            RemoteShellAction::Connect { host, username, .. } => {
                assert_eq!(host, "server.example.com");
                assert_eq!(username, "deploy");
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
            }
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
    fn test_invalid_action_parse_error() {
        let json = r#"{"action": "nonexistent"}"#;
        let result: Result<RemoteShellAction, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
