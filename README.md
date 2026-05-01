# remote-shell — IronClaw SSH Extension

Connect to remote machines via SSH and run commands directly from the IronClaw chat console. Manages persistent, named SSH sessions so you can open a connection once and reuse it across multiple commands.

**Status:** hardened — all known correctness, security and DoS issues from the
0.1.0 audit have been addressed (see `CHANGELOG` section at the bottom).
Ships with a companion **`remote-shell` skill** so the agent learns the
correct connect → execute → disconnect lifecycle automatically.

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

## Quick Install

If you have Rust (with `rustup`), the `ironclaw` CLI, and `sudo` access, run:

```bash
git clone https://github.com/chtugha/ironclaw-remote-shell-extension
cd ironclaw-remote-shell-extension
./install.sh
```

The script automatically:

- Detects and stops a running gateway (systemd service or standalone process)
- Builds the gateway binary and the WASM tool from source
- Installs the gateway to `/usr/local/bin/remote-shell-gateway` (requires `sudo`)
- Installs (or updates) the WASM tool via `ironclaw tool install`
- Copies (or overwrites) the companion skill to `~/.ironclaw/skills/remote-shell/`
- Sets `ALLOW_LOCAL_TOOLS=true` in `~/.ironclaw/.env` (required — see below)
- Restarts the gateway service if a systemd unit exists

After the script finishes, **restart IronClaw** so it picks up the new tool,
skill, and `ALLOW_LOCAL_TOOLS` setting. Then configure the bearer token (if
used) — see steps 9 and 10 in the manual installation section below.

---

## Manual Installation on Debian

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
git clone https://github.com/chtugha/ironclaw-remote-shell-extension
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


```bash
ironclaw tool install \
  --name remote-shell \
  target/wasm32-wasip2/release/remote_shell.wasm \
  --capabilities remote-shell/remote-shell.capabilities.json \
  --skip-build
```

Restart IronClaw so it picks up the new tool.

### 8a — Enable local tools (required)

The extension communicates with the gateway via the `shell` tool and `curl`.
The `shell` tool is only available when `ALLOW_LOCAL_TOOLS=true`:

```bash
echo "ALLOW_LOCAL_TOOLS=true" >> ~/.ironclaw/.env
```

If the file already exists with a different value, update it instead.

### 8b — Install the companion skill (recommended)

The repository ships with a `remote-shell` skill that teaches the agent how
to use this tool safely (lifecycle, security rules, failure-mode recovery).
Copy it into IronClaw's skills directory:

```bash
mkdir -p ~/.ironclaw/skills/remote-shell
cp skills/remote-shell/SKILL.md ~/.ironclaw/skills/remote-shell/SKILL.md
```

Restart IronClaw so the skill is registered. The skill activates on
keywords like *ssh*, *remote*, *deploy*, *production server* and on common
phrases like "connect to host …" / "run X on the server".

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
| `--host` | `127.0.0.1` | Bind address. Non-loopback values (e.g. `0.0.0.0`) require `SSH_GATEWAY_TOKEN`; the gateway will refuse to start otherwise. |
| `--port` | `9022` | HTTP listen port |
| `--max-sessions` | `64` | Maximum concurrent SSH sessions |
| `--session-ttl-secs` | `3600` | Idle session lifetime in seconds |

**Environment variables:**

| Variable | Description |
|---|---|
| `SSH_GATEWAY_TOKEN` | Optional bearer token. When set, every gateway request must carry `Authorization: Bearer $SSH_GATEWAY_TOKEN`. Mandatory when binding to a non-loopback address. |
| `RUST_LOG` | Standard `tracing` filter. `RUST_LOG=remote_shell_gateway=debug` reveals body sizes and command previews (truncated to 64 chars; full commands and credentials are never logged). |

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

The tool exposes five actions: `health`, `connect`, `execute`,
`list_sessions`, `disconnect`. The companion skill teaches the agent the
recommended order: probe with `health` if unsure, reuse a session via
`list_sessions`, otherwise `connect`, run many `execute` calls on the same
`session_id`, then `disconnect` when done.

### Probe the gateway

Before doing anything else (and especially after IronClaw restarts), confirm
the local gateway is up:

**Natural language:**
> Is the SSH gateway running?

**Explicit JSON:**
```json
{ "action": "health" }
```

A successful response confirms the local gateway service is reachable. If
this fails, start `remote-shell-gateway` (see step 9) — do **not** retry
`connect` blindly.

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
| Max private-key PEM length | 256 KiB |
| Max request body — `/connect` | 512 KiB |
| Max request body — other endpoints | 1 MiB |
| Logged command preview | 64 chars (truncated; full command never logged) |

Sessions expire automatically after the TTL and are reaped on **every**
request (`connect`, `execute`, `disconnect`, `list_sessions`). Expired
sessions are disconnected gracefully on the remote server. A request that
references a reaped or unknown `session_id` returns `HTTP 404` —
reconnecting with the same `session_id` is the recommended recovery.

### Authentication and host-key behaviour

- **Host-key verification** is enforced on **both** sides: the WASM tool
  refuses any `connect` that lacks both `host_key_fingerprint` and
  `insecure_ignore_host_key: true`, and the gateway re-checks server keys
  against the supplied fingerprint at SSH handshake time.
- **RSA keys** are tried with `rsa-sha2-512`, then `rsa-sha2-256`, then the
  legacy `ssh-rsa` (SHA-1) signature scheme. This works with both modern
  OpenSSH (≥ 8.8, which rejects SHA-1) and older servers.
- **ED25519 / ECDSA keys** authenticate in a single attempt — no fallback
  loop is run for them.
- **Auth is a required field**: omitting `auth` returns a clean validation
  error from the WASM tool rather than an opaque "credentials rejected"
  from the SSH server.

---

## Security notes

- The gateway binds to `127.0.0.1` by default. **Non-loopback bind addresses
  (`0.0.0.0`, public IPs, hostnames other than `localhost`/`127.0.0.0/8`,
  `::1`) are refused at startup unless `SSH_GATEWAY_TOKEN` is set.** This
  prevents accidentally exposing an anonymous SSH-as-a-service.
- Always set `SSH_GATEWAY_TOKEN` and add it to IronClaw secrets on any
  shared or multi-user system. Token comparison uses constant-time equality
  to defeat timing attacks.
- Always supply `host_key_fingerprint` in production. Only use
  `insecure_ignore_host_key: true` on fully trusted private networks.
- Request bodies are size-capped (1 MiB normal, 512 KiB on `/connect`) to
  prevent memory-exhaustion attacks via oversized `key_pem` or `command`
  fields. Oversized requests return `HTTP 413`.
- Secrets in transit: SSH passwords and private keys are sent over HTTP to
  the local gateway. Because the gateway binds to localhost by default,
  traffic never leaves the machine — but avoid running the gateway as a
  different user from IronClaw, and never expose the port off-host without
  a bearer token.
- Logging: full command strings are **never** logged. At `debug` level the
  gateway emits a 64-char preview only; passphrases and key material are
  redacted entirely.

---

## Known Limitation — IronClaw Sandbox HTTP Restriction

IronClaw's WASM sandbox **blocks all HTTP requests to `127.0.0.1`** at two
independent layers (HTTPS-only scheme check and loopback-IP SSRF guard). This
means every `remote_shell` action will fail with:

```
Gateway request failed: the IronClaw sandbox blocks HTTP requests to localhost (127.0.0.1).
The gateway may be running but cannot be reached from within the WASM sandbox.
Workaround — use the shell tool to run:
  curl -s 'http://127.0.0.1:9022/health'
```

**This is a structural sandbox constraint, not a bug in the gateway or tool.**

### Workaround

Use IronClaw's built-in `shell` tool to send `curl` commands directly to the
gateway. The `shell` tool runs on the host without WASM restrictions. See
`skills/remote-shell/SKILL.md` for the complete set of ready-to-paste `curl`
commands for each action (`connect`, `execute`, `disconnect`, `list_sessions`,
`health`).

**The `shell` tool requires `ALLOW_LOCAL_TOOLS=true`** in IronClaw's
environment. Without this setting, the `shell` tool is not registered and
the extension has no viable path to reach the gateway. Add the following
to `~/.ironclaw/.env`:

```
ALLOW_LOCAL_TOOLS=true
```

Then restart IronClaw. The `install.sh` script sets this automatically.

> This limitation requires an upstream IronClaw change to expose an
> `allow_loopback_http` capabilities flag (the `AllowlistValidator::allow_http()`
> method already exists in IronClaw but is not wired to any capabilities JSON
> field). A PR to the IronClaw repository would be the long-term fix.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `Tool shell not found` | `ALLOW_LOCAL_TOOLS=true` is not set in IronClaw's environment. | Add `ALLOW_LOCAL_TOOLS=true` to `~/.ironclaw/.env` and restart IronClaw. |
| `sandbox blocks HTTP requests to localhost` | IronClaw WASM sandbox blocks HTTP to 127.0.0.1 — structural limitation. | Use the `shell` tool with `curl` instead; see **Known Limitation** above and `SKILL.md`. |
| `Gateway request failed` | The local gateway isn't running, or is on a different port. | Run `health`. If it still fails, start `remote-shell-gateway`. |
| `Gateway error (HTTP 401)` | Bearer-token mismatch between IronClaw secret and gateway env var. | Re-paste `ssh_gateway_token` in IronClaw, or restart the gateway with the matching token. |
| `Gateway error (HTTP 404): Session 'X' not found` | Session was reaped (TTL elapsed) or never existed. | Reconnect using the same `session_id`. |
| `Gateway error (HTTP 413)` | Body too large (oversized `key_pem` or `command`). | Shorten the input or split the work into smaller commands. |
| `Authentication failed: credentials rejected` | Wrong username/password/key, or server requires keyboard-interactive. | Verify credentials manually with `ssh`. |
| `host key fingerprint mismatch` | Server changed keys, or you are being MITM'd. | **Stop.** Verify the new fingerprint out-of-band before retrying. |
| `Exit code: unknown (command may have timed out)` | Command exceeded `timeout_secs`. | Bump `timeout_secs` (max 3600), or run the work in the background. |
| Gateway exits: `Refusing to bind to non-loopback address '…' without SSH_GATEWAY_TOKEN` | Non-loopback bind address requested without a token. | Either bind to `127.0.0.1`, or export `SSH_GATEWAY_TOKEN` first. |

---

## Changelog

### 0.1.4 — ALLOW_LOCAL_TOOLS prerequisite

- Root-caused `Tool shell not found` error: IronClaw's `shell` tool requires
  `ALLOW_LOCAL_TOOLS=true` (env var, defaults to `false`). Without it the
  `shell` tool is never registered and the extension has no viable path to
  the gateway (WASM sandbox blocks localhost HTTP, `http` builtin also blocks
  loopback IPs).
- `install.sh` now sets `ALLOW_LOCAL_TOOLS=true` in `~/.ironclaw/.env`
  automatically.
- `SKILL.md` updated with a Prerequisites section, failure-mode entry for
  `Tool shell not found`, and `ALLOW_LOCAL_TOOLS` references throughout.
- `capabilities.json` discovery WARNING updated with the prerequisite.
- `README.md` updated: Quick Install mentions the setting, manual install
  adds step 8a, troubleshooting table includes the new error, Known
  Limitation section documents the `ALLOW_LOCAL_TOOLS` requirement.

### 0.1.3 — Install script

- Added `install.sh` that automates the full build-and-install workflow:
  detects and stops a running gateway, builds both components from source,
  installs the gateway binary, the WASM tool, and the companion skill, then
  restarts the gateway. Handles both fresh installs and updates.
- README updated with a Quick Install section.

### 0.1.2 — Sandbox diagnostics

**Diagnostics**
- WASM tool now detects when the IronClaw sandbox blocks the HTTP call to
  localhost and returns an actionable error with the equivalent `curl` command
  to run via the `shell` tool.
- `SKILL.md` updated with sandbox workaround guide (complete curl commands for
  every action, private-key temp-file pattern).
- `README.md` documents the IronClaw sandbox HTTP restriction as a known
  limitation with root-cause analysis and upstream fix reference.
- 7 new tests covering `is_sandbox_restriction` and `sandbox_gateway_error`.

### 0.1.1 — Hardening pass

**Correctness**
- Stdin EOF (`channel.eof()`) is now sent after every `execute`, so
  commands that read stdin (`cat`, `read`, etc.) no longer hang.
- `channel.exec(want_reply=false, …)` removes a round-trip / hang on
  servers that do not reply promptly.
- `channel.close()` is invoked on timeout to free the remote PTY/exec slot.
- HTTP-execute timeout uses saturating arithmetic — a `u64::MAX`
  `timeout_secs` no longer panics the WASM module.
- RSA keys try `SHA-512 → SHA-256 → SHA-1`, fixing failures against
  OpenSSH ≥ 8.8 servers. ED25519/ECDSA keep their single-iteration path.
- Expired sessions are reaped on every endpoint, not just `connect`.

**Security / DoS**
- `DefaultBodyLimit` enforced (1 MiB / 512 KiB on `/connect`).
- Gateway refuses to bind to a non-loopback host without
  `SSH_GATEWAY_TOKEN`.
- Loopback detection is case-insensitive (`localhost`, `LOCALHOST`,
  `127.0.0.0/8`, `::1`).
- Command-string logging is replaced with a 64-char truncated preview;
  credentials are never logged.

**UX**
- New `health` action maps to `GET /health`, giving the agent a one-call
  probe for "is the gateway up?".
- WASM tool fails-fast for missing `auth`, missing host-key, out-of-range
  timeout, and oversized inputs — before any HTTP round-trip.
- Empty `(stdout)` + `(stderr)` collapse to `(no output)`.

**Skill**
- Ships `skills/remote-shell/SKILL.md` so IronClaw's skill loader picks up
  the lifecycle, security and recovery guidance automatically.

**Tests**
- 31 WASM-side and 10 gateway-side tests cover: serde shapes, validation
  bounds, timeout overflow, hostname rules, body-size limits, host-key
  enforcement, auth-required, and case-insensitive loopback detection.
