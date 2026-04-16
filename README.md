# remote-shell — IronClaw SSH Extension

Connect to remote machines via SSH and run commands directly from the IronClaw chat console. Manages persistent, named SSH sessions so you can open a connection once and reuse it across multiple commands.

## Architecture

```
IronClaw LLM
     │ tool call (JSON)
     ▼
remote-shell.wasm          ← WASM tool (sandboxed, HTTP-only)
     │ HTTP POST/GET/DELETE
     ▼
remote-shell-gateway       ← local HTTP→SSH bridge (native binary)
     │ SSH (russh)
     ▼
Remote server (port 22)
```

The WASM sandbox cannot open raw TCP connections, so the extension uses a two-component design: a WASM tool that runs inside IronClaw, and a small gateway process that runs locally and handles the actual SSH connections.

---

## Installation on Debian

### 1 — Install build dependencies

```bash
sudo apt update
sudo apt install -y curl build-essential pkg-config libssl-dev git
```

### 2 — Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### 3 — Add the WASM target

```bash
rustup target add wasm32-wasip2
```

### 4 — Clone the repository

```bash
git clone https://github.com/YOUR_ORG/ironclaw-remote-shell-extension.git
cd ironclaw-remote-shell-extension
```

### 5 — Build the SSH gateway

```bash
cargo build --release -p remote-shell-gateway
```

The binary is written to `target/release/remote-shell-gateway`.

### 6 — Install the gateway system-wide

```bash
sudo cp target/release/remote-shell-gateway /usr/local/bin/
sudo chmod +x /usr/local/bin/remote-shell-gateway
```

### 7 — Build the WASM tool

```bash
cargo build --release --target wasm32-wasip2 -p remote-shell
```

The component is written to `target/wasm32-wasip2/release/remote_shell.wasm`.

### 8 — Install the IronClaw tool

Copy the WASM component and capabilities file to IronClaw's tools directory:

```bash
mkdir -p ~/.ironclaw/tools/remote-shell
cp target/wasm32-wasip2/release/remote_shell.wasm \
   ~/.ironclaw/tools/remote-shell/remote_shell.wasm
cp remote-shell/remote-shell.capabilities.json \
   ~/.ironclaw/tools/remote-shell/remote-shell.capabilities.json
```

Restart IronClaw so it picks up the new tool.

### 9 — Start the gateway

The gateway must be running before you use the tool. Start it in a terminal or as a systemd service.

**Terminal (foreground):**

```bash
remote-shell-gateway
```

**With a bearer token (recommended):**

```bash
export SSH_GATEWAY_TOKEN="$(openssl rand -hex 32)"
echo "Token: $SSH_GATEWAY_TOKEN"   # copy this — you'll paste it into IronClaw
remote-shell-gateway
```

**As a systemd user service:**

Create `~/.config/systemd/user/remote-shell-gateway.service`:

```ini
[Unit]
Description=IronClaw SSH Gateway
After=network.target

[Service]
ExecStart=/usr/local/bin/remote-shell-gateway
Environment=SSH_GATEWAY_TOKEN=REPLACE_WITH_YOUR_TOKEN
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Then enable and start it:

```bash
systemctl --user daemon-reload
systemctl --user enable --now remote-shell-gateway
systemctl --user status remote-shell-gateway
```

**Gateway CLI options:**

| Flag | Default | Description |
|---|---|---|
| `--host` | `127.0.0.1` | Bind address (keep on localhost) |
| `--port` | `9022` | HTTP listen port |
| `--max-sessions` | `64` | Maximum concurrent SSH sessions |
| `--session-ttl-secs` | `3600` | Idle session lifetime in seconds |

### 10 — Configure the bearer token in IronClaw

If you set `SSH_GATEWAY_TOKEN`, open IronClaw settings → Secrets and add the token under the key **`ssh_gateway_token`**. IronClaw injects it automatically as an `Authorization: Bearer …` header on every gateway request.

If you leave `SSH_GATEWAY_TOKEN` empty, skip this step — the gateway runs unauthenticated (only safe on a single-user machine where localhost is trusted).

---

## Getting a host key fingerprint

Before connecting, obtain the fingerprint of the remote server's host key:

```bash
ssh-keyscan -p 22 server.example.com | ssh-keygen -lf -
```

Example output:

```
256 SHA256:oBJ5MHd/vRDDe7jDGTrVEV5lN3S8J8Kpb2Hq7EXAMPLE server.example.com (ED25519)
```

Use the `SHA256:…` part as the `host_key_fingerprint` value in your connect call. This protects against man-in-the-middle attacks.

---

## Using from the IronClaw chat console

All interactions go through the **remote_shell** tool. Describe what you want to do in plain English — IronClaw will call the right action. You can also give IronClaw the exact JSON if you prefer precise control.

### Connect to a server

**Natural language:**
> Connect to server.example.com as deploy using password "mypassword". The host key fingerprint is SHA256:oBJ5MHd…

**Explicit JSON:**
```json
{
  "action": "connect",
  "host": "server.example.com",
  "port": 22,
  "username": "deploy",
  "auth": {
    "type": "password",
    "password": "mypassword"
  },
  "host_key_fingerprint": "SHA256:oBJ5MHd/vRDDe7jDGTrVEV5lN3S8J8Kpb2Hq7EXAMPLE"
}
```

**With a private key:**
```json
{
  "action": "connect",
  "host": "server.example.com",
  "username": "ubuntu",
  "auth": {
    "type": "private_key",
    "key_pem": "-----BEGIN OPENSSH PRIVATE KEY-----\n...\n-----END OPENSSH PRIVATE KEY-----"
  },
  "host_key_fingerprint": "SHA256:oBJ5MHd/vRDDe7jDGTrVEV5lN3S8J8Kpb2Hq7EXAMPLE",
  "session_id": "prod"
}
```

A successful connect returns a `session_id` (auto-generated UUID if you did not supply one). Use it in every subsequent call.

**Skip host key verification (trusted networks only — insecure):**
```json
{
  "action": "connect",
  "host": "192.168.1.10",
  "username": "admin",
  "auth": { "type": "password", "password": "admin" },
  "insecure_ignore_host_key": true,
  "session_id": "local-dev"
}
```

### Run a command

**Natural language:**
> Run `df -h` on the prod session.

**Explicit JSON:**
```json
{
  "action": "execute",
  "session_id": "prod",
  "command": "df -h"
}
```

The response includes `stdout`, `stderr`, and `exit_code`.

**With a custom timeout (default is 30 seconds):**
```json
{
  "action": "execute",
  "session_id": "prod",
  "command": "tar czf /tmp/backup.tar.gz /var/www",
  "timeout_secs": 300
}
```

### List active sessions

**Natural language:**
> What SSH sessions are currently open?

**Explicit JSON:**
```json
{
  "action": "list_sessions"
}
```

Returns a list of sessions with `session_id`, `host`, `port`, `username`, and `age_secs`.

### Disconnect

**Natural language:**
> Disconnect the prod session.

**Explicit JSON:**
```json
{
  "action": "disconnect",
  "session_id": "prod"
}
```

### Use a non-default gateway port

If you started the gateway with `--port 9100`, pass `gateway_port` in any action:

```json
{
  "action": "list_sessions",
  "gateway_port": 9100
}
```

---

## Example chat session

```
You:  Connect to build.internal as ci. The key fingerprint is SHA256:xyz…
      Use password "ci-pass" and name the session "build".

AI:   Connected. Session "build" is open to build.internal.

You:  Run `git pull && make test` on the build session with a 120-second timeout.

AI:   Exit code 0.
      stdout:
        Already up to date.
        All tests passed.

You:  Disconnect the build session.

AI:   Session "build" disconnected.
```

---

## Limits and behaviour

| Parameter | Limit |
|---|---|
| Max stdout / stderr per command | 10 MB each (output truncated, warning appended to stderr) |
| Min command timeout | 1 second |
| Max command timeout | 3600 seconds (1 hour) |
| Max concurrent sessions | 64 (configurable via `--max-sessions`) |
| Session TTL | 3600 seconds (configurable via `--session-ttl-secs`) |
| SSH keepalive interval | 30 seconds (3 missed = disconnect) |
| Max hostname length | 253 characters |
| Max command / username length | 65 536 characters |

Sessions expire automatically after the TTL and are reaped on the next connect request. Expired sessions are disconnected gracefully on the remote server.

---

## Security notes

- The gateway binds to `127.0.0.1` by default. Do not expose it on a public interface.
- Always set `SSH_GATEWAY_TOKEN` and add it to IronClaw secrets on any shared or multi-user system.
- Always supply `host_key_fingerprint` in production. Only use `insecure_ignore_host_key: true` on fully trusted private networks.
- SSH passwords and private keys are sent over HTTP to the gateway. Since the gateway binds to localhost only, traffic never leaves the machine, but avoid running the gateway as a different user from IronClaw.
