wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../wit/tool.wit",
});

use serde::{Deserialize, Serialize};

const MAX_TEXT_LENGTH: usize = 65536;
const MAX_HOSTNAME_LENGTH: usize = 253;
const MAX_KEY_PEM_LENGTH: usize = 256 * 1024;
const DEFAULT_GATEWAY_PORT: u16 = 9022;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MIN_TIMEOUT_SECS: u64 = 1;
const MAX_TIMEOUT_SECS: u64 = 3600;
const HTTP_TIMEOUT_MS: u32 = 60_000;
const HTTP_EXECUTE_TIMEOUT_BUFFER_SECS: u64 = 30;

fn validate_input_length(s: &str, field_name: &str, max_len: usize) -> Result<(), String> {
    if s.len() > max_len {
        return Err(format!(
            "Input '{}' exceeds maximum length of {} characters",
            field_name, max_len
        ));
    }
    Ok(())
}

fn validate_non_empty(s: &str, field_name: &str, max_len: usize) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("'{}' cannot be empty", field_name));
    }
    validate_input_length(s, field_name, max_len)
}

fn validate_hostname(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("Hostname cannot be empty".into());
    }
    if host.trim().is_empty() {
        return Err("Hostname cannot be whitespace only".into());
    }
    if host.len() > MAX_HOSTNAME_LENGTH {
        return Err(format!(
            "Hostname too long (max {} characters)",
            MAX_HOSTNAME_LENGTH
        ));
    }
    if host
        .chars()
        .any(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\0')
    {
        return Err("Hostname contains invalid characters".into());
    }
    Ok(())
}

fn validate_command(command: &str) -> Result<(), String> {
    validate_non_empty(command, "command", MAX_TEXT_LENGTH)
}

fn compute_http_execute_timeout_ms(timeout_secs: u64) -> u32 {
    timeout_secs
        .saturating_add(HTTP_EXECUTE_TIMEOUT_BUFFER_SECS)
        .saturating_mul(1000)
        .min(u64::from(u32::MAX)) as u32
}

fn validate_timeout_secs(t: u64) -> Result<u64, String> {
    if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&t) {
        return Err(format!(
            "timeout_secs must be {}-{} (got {})",
            MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS, t
        ));
    }
    Ok(t)
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

    #[serde(rename = "health")]
    Health { gateway_port: Option<u16> },
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

fn is_sandbox_restriction(err: &str) -> bool {
    err.contains("HTTP not allowed")
        || err.contains("InsecureScheme")
        || err.contains("private/internal IP")
        || err.contains("HTTP request to private")
        || err.contains("DNS rebinding detected")
}

fn sandbox_gateway_error(method: &str, url: &str) -> String {
    let cmd = if method == "GET" {
        format!("curl -s '{url}'")
    } else {
        format!(
            "curl -s -X {method} -H 'Content-Type: application/json' -d '<json-body>' '{url}'"
        )
    };
    format!(
        "Gateway request failed: the IronClaw sandbox blocks HTTP requests to \
         localhost (127.0.0.1). The gateway may be running but cannot be reached \
         from within the WASM sandbox.\n\
         Workaround — use the shell tool to run:\n  {cmd}\n\
         For 'connect': construct the JSON body without embedding credentials in shell \
         history (write the key to a temp file or pipe via stdin). \
         See SKILL.md for the full workaround guide."
    )
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
    .map_err(|e| {
        if is_sandbox_restriction(&e) {
            sandbox_gateway_error(method, &url)
        } else {
            format!("Gateway request failed: {e}")
        }
    })?;

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
         'list_sessions' — list all currently open sessions; \
         'health' — verify the local gateway service is reachable. \
         The remote-shell-gateway service must be running locally before use; call 'health' first if unsure."
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
    if resp.stdout.is_empty() && resp.stderr.is_empty() {
        return Ok(format!("Exit code: {exit_str}\n(no output)"));
    }
    let mut out = format!("Exit code: {exit_str}\n");
    if !resp.stdout.is_empty() {
        out.push_str("\n--- stdout ---\n");
        out.push_str(&resp.stdout);
    }
    if !resp.stderr.is_empty() {
        out.push_str("\n--- stderr ---\n");
        out.push_str(&resp.stderr);
    }
    Ok(out)
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
            validate_non_empty(&username, "username", MAX_TEXT_LENGTH)?;
            if let Some(ref sid) = session_id {
                validate_non_empty(sid, "session_id", MAX_TEXT_LENGTH)?;
            }
            match &auth {
                AuthMethod::Password { password } => {
                    validate_input_length(password, "auth.password", MAX_TEXT_LENGTH)?;
                }
                AuthMethod::PrivateKey {
                    key_pem,
                    passphrase,
                } => {
                    validate_non_empty(key_pem, "auth.key_pem", MAX_KEY_PEM_LENGTH)?;
                    if let Some(p) = passphrase {
                        validate_input_length(p, "auth.passphrase", MAX_TEXT_LENGTH)?;
                    }
                }
            }
            if !insecure_ignore_host_key && host_key_fingerprint.is_none() {
                return Err(
                    "Either 'host_key_fingerprint' (recommended) or 'insecure_ignore_host_key': true must be provided. \
                     Get the fingerprint with: ssh-keyscan <host> | ssh-keygen -lf -"
                        .to_string(),
                );
            }
            if let Some(ref fp) = host_key_fingerprint {
                validate_non_empty(fp, "host_key_fingerprint", MAX_TEXT_LENGTH)?;
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
            validate_non_empty(&session_id, "session_id", MAX_TEXT_LENGTH)?;
            validate_command(&command)?;

            let timeout_secs = validate_timeout_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS))?;
            let http_timeout_ms = compute_http_execute_timeout_ms(timeout_secs);

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
            validate_non_empty(&session_id, "session_id", MAX_TEXT_LENGTH)?;
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

        RemoteShellAction::Health { gateway_port } => {
            match gateway_request("GET", "/health", None, gateway_port, HTTP_TIMEOUT_MS) {
                Ok(_) => Ok(format!(
                    "Gateway is reachable at {}.",
                    gateway_url(gateway_port)
                )),
                Err(e) => Err(format!(
                    "Gateway is NOT reachable at {}. Start it with `remote-shell-gateway` (see README). Detail: {e}",
                    gateway_url(gateway_port)
                )),
            }
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
                "timeout_secs": { "type": "integer", "description": "Command timeout in seconds (1-3600, default: 30)", "default": 30, "minimum": 1, "maximum": 3600 },
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
        },
        {
            "properties": {
                "action": { "const": "health" },
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
        assert!(actions.contains("health"));
        assert_eq!(actions.len(), 5);
    }

    #[test]
    fn test_validate_hostname() {
        assert!(validate_hostname("example.com").is_ok());
        assert!(validate_hostname("192.168.1.1").is_ok());
        assert!(validate_hostname("my-server.internal").is_ok());
        assert!(validate_hostname("").is_err());
        assert!(validate_hostname("   ").is_err());
        assert!(validate_hostname("host name").is_err());
        assert!(validate_hostname("host\tname").is_err());
        assert!(validate_hostname("host\nname").is_err());
        assert!(validate_hostname("host\0name").is_err());
        let long = "a".repeat(MAX_HOSTNAME_LENGTH + 1);
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
        assert!(validate_input_length("short", "test", MAX_TEXT_LENGTH).is_ok());
        let long = "a".repeat(MAX_TEXT_LENGTH + 1);
        assert!(validate_input_length(&long, "test", MAX_TEXT_LENGTH).is_err());
    }

    #[test]
    fn test_validate_timeout_secs() {
        assert!(validate_timeout_secs(1).is_ok());
        assert!(validate_timeout_secs(30).is_ok());
        assert!(validate_timeout_secs(3600).is_ok());
        assert!(validate_timeout_secs(0).is_err());
        assert!(validate_timeout_secs(3601).is_err());
        assert!(validate_timeout_secs(u64::MAX).is_err());
    }

    #[test]
    fn test_connect_requires_host_key_or_insecure() {
        let params = r#"{
            "action": "connect",
            "host": "example.com",
            "username": "deploy",
            "auth": { "type": "password", "password": "x" }
        }"#;
        let err = execute_inner(params).unwrap_err();
        assert!(err.contains("host_key_fingerprint"));
        assert!(err.contains("insecure_ignore_host_key"));
    }

    #[test]
    fn test_connect_requires_auth() {
        let params = r#"{
            "action": "connect",
            "host": "example.com",
            "username": "deploy",
            "insecure_ignore_host_key": true
        }"#;
        let err = execute_inner(params).unwrap_err();
        assert!(err.to_lowercase().contains("invalid parameters"));
    }

    #[test]
    fn test_execute_rejects_out_of_range_timeout() {
        let params = r#"{
            "action": "execute",
            "session_id": "s",
            "command": "ls",
            "timeout_secs": 10000
        }"#;
        let err = execute_inner(params).unwrap_err();
        assert!(err.contains("timeout_secs"));
    }

    #[test]
    fn test_execute_rejects_zero_timeout() {
        let params = r#"{
            "action": "execute",
            "session_id": "s",
            "command": "ls",
            "timeout_secs": 0
        }"#;
        assert!(execute_inner(params).is_err());
    }

    #[test]
    fn test_format_execute_response_collapses_empty() {
        let raw = r#"{"stdout":"","stderr":"","exit_code":0}"#;
        let result = format_execute_response(raw).expect("should format");
        assert!(result.contains("(no output)"));
        assert!(!result.contains("--- stdout ---"));
        assert!(!result.contains("--- stderr ---"));
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
        assert!(!result.contains("--- stderr ---"));
        assert!(!result.contains("(empty)"));
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
        let http_timeout_ms = compute_http_execute_timeout_ms(120);
        assert_eq!(http_timeout_ms, 150_000);
        assert!(http_timeout_ms > HTTP_TIMEOUT_MS);
    }

    #[test]
    fn test_http_execute_timeout_saturates_on_overflow() {
        let http_timeout_ms = compute_http_execute_timeout_ms(u64::MAX - 1);
        assert_eq!(http_timeout_ms, u32::MAX);
    }

    #[test]
    fn test_http_execute_timeout_saturates_at_max_timeout_secs() {
        let http_timeout_ms = compute_http_execute_timeout_ms(MAX_TIMEOUT_SECS);
        let expected = (MAX_TIMEOUT_SECS + HTTP_EXECUTE_TIMEOUT_BUFFER_SECS) * 1000;
        assert_eq!(u64::from(http_timeout_ms), expected);
    }

    #[test]
    fn test_description_lists_all_actions() {
        use crate::exports::near::agent::tool::Guest;
        let desc = RemoteShellTool::description();
        assert!(desc.contains("connect"));
        assert!(desc.contains("execute"));
        assert!(desc.contains("disconnect"));
        assert!(desc.contains("list_sessions"));
        assert!(desc.contains("health"));
    }

    #[test]
    fn test_is_sandbox_restriction_detects_https_scheme_error() {
        assert!(is_sandbox_restriction(
            "HTTP not allowed: HTTP request not allowed: Denied(InsecureScheme(\"http\"))"
        ));
    }

    #[test]
    fn test_is_sandbox_restriction_detects_private_ip_error() {
        assert!(is_sandbox_restriction(
            "HTTP request to private/internal IP 127.0.0.1 is not allowed"
        ));
    }

    #[test]
    fn test_is_sandbox_restriction_detects_dns_rebinding() {
        assert!(is_sandbox_restriction(
            "DNS rebinding detected: localhost resolved to private IP 127.0.0.1"
        ));
    }

    #[test]
    fn test_is_sandbox_restriction_does_not_match_normal_errors() {
        assert!(!is_sandbox_restriction("Gateway request failed: connection refused"));
        assert!(!is_sandbox_restriction("Gateway error (HTTP 404): Session not found"));
        assert!(!is_sandbox_restriction("Rate limit exceeded"));
        assert!(!is_sandbox_restriction("Failed to parse URL: invalid URL"));
    }

    #[test]
    fn test_sandbox_gateway_error_get_produces_curl_without_x_flag() {
        let msg = sandbox_gateway_error("GET", "http://127.0.0.1:9022/health");
        assert!(msg.contains("sandbox blocks HTTP"));
        assert!(msg.contains("curl -s 'http://127.0.0.1:9022/health'"));
        assert!(!msg.contains("-X GET"));
    }

    #[test]
    fn test_sandbox_gateway_error_post_includes_method_and_placeholder() {
        let msg = sandbox_gateway_error("POST", "http://127.0.0.1:9022/execute");
        assert!(msg.contains("-X POST"));
        assert!(msg.contains("<json-body>"));
        assert!(msg.contains("http://127.0.0.1:9022/execute"));
    }

    #[test]
    fn test_sandbox_gateway_error_delete_includes_method() {
        let msg = sandbox_gateway_error("DELETE", "http://127.0.0.1:9022/disconnect");
        assert!(msg.contains("-X DELETE"));
        assert!(msg.contains("http://127.0.0.1:9022/disconnect"));
    }
}
