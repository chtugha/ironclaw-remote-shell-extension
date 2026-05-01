# remote-shell — IronClaw SSH Extension

Connect to remote machines over SSH and run commands directly from the
IronClaw AI chat console. The extension manages **persistent, named SSH
sessions** — open a connection once and reuse it for as many commands as
you like, without re-authenticating every time.

---

## Table of Contents

1. [How it works](#1-how-it-works)
2. [Critical limitation — read this first](#2-critical-limitation--read-this-first)
3. [Prerequisites](#3-prerequisites)
4. [Quick Install (recommended)](#4-quick-install-recommended)
5. [Manual Installation (step by step)](#5-manual-installation-step-by-step)
6. [Starting the gateway](#6-starting-the-gateway)
7. [Enabling the shell tool (ALLOW_LOCAL_TOOLS)](#7-enabling-the-shell-tool-allow_local_tools)
8. [Installing the companion skill](#8-installing-the-companion-skill)
9. [Configuring the bearer token in IronClaw](#9-configuring-the-bearer-token-in-ironclaw)
10. [Using the extension from IronClaw chat](#10-using-the-extension-from-ironclaw-chat)
11. [Complete curl API reference](#11-complete-curl-api-reference)
12. [Gateway configuration reference](#12-gateway-configuration-reference)
13. [Security guide](#13-security-guide)
14. [Limits](#14-limits)
15. [Troubleshooting](#15-troubleshooting)
16. [Changelog](#16-changelog)

---

## 1. How it works

IronClaw AI runs tools inside a **WASM sandbox** that cannot open raw TCP
connections or make HTTP calls to `localhost`. This extension uses a
two-component architecture to work around that:

```
You (chat)
    │
    ▼
IronClaw AI (LLM)
    │  decides to run a shell command via the shell tool
    ▼
shell tool  ─── runs on the host, unrestricted ──▶  curl http://127.0.0.1:9022/...
                                                              │
                                                              ▼
                                               remote-shell-gateway  (local HTTP→SSH bridge)
                                                              │  SSH (port 22)
                                                              ▼
                                                     Remote server
```

**Component 1 — `remote-shell-gateway`** (native binary): a small HTTP
server that runs on your local machine. It manages a pool of SSH connections
and exposes a simple REST API (`/connect`, `/execute`, `/disconnect`, etc.).
This is the component that actually speaks SSH.

**Component 2 — `remote-shell` WASM tool**: a tool loaded into IronClaw that
provides the schema (action names, parameters, validation) and tells the agent
what to do. Due to the sandbox limitation described in section 2, the agent
does **not** call this tool directly — it uses the `shell` tool with `curl` to
reach the gateway instead.

**Companion skill (`SKILL.md`)**: a knowledge file installed into IronClaw
that teaches the agent exactly how to use curl to talk to the gateway,
including ready-to-paste commands for every action.

---

## 2. Critical limitation — read this first

> **IronClaw's WASM sandbox blocks all HTTP requests to `127.0.0.1`.**

There are two independent blocking layers in IronClaw:

1. **HTTPS-only check** — the sandbox rejects `http://` URLs (`InsecureScheme`
   error). The gateway uses plain HTTP on localhost.
2. **Loopback SSRF guard** — even if HTTPS were used, the sandbox rejects any
   connection to `127.0.0.1` / loopback addresses.

This means calling `remote_shell` actions directly from the IronClaw chat
will **always fail** — even when the gateway is running and `lsof` shows it
listening on port 9022. This is a known IronClaw sandbox limitation, not a
bug in this extension.

### The workaround: `shell` tool + `curl`

IronClaw's built-in **`shell` tool** runs commands on the host OS outside the
WASM sandbox. It has full access to `localhost`. The agent uses it to run
`curl` commands against the gateway REST API.

**This requires `ALLOW_LOCAL_TOOLS=true`** — see [section 7](#7-enabling-the-shell-tool-allow_local_tools).

If `ALLOW_LOCAL_TOOLS` is not set, you will get `Tool error: Tool shell not
found` and the extension cannot function at all.

---

## 3. Prerequisites

You need all of the following before installing:

| Requirement | How to check | How to install |
|---|---|---|
| Linux (Debian/Ubuntu recommended) or macOS | `uname -s` | — |
| Rust toolchain | `rustc --version` | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| `rustup` | `rustup --version` | Installed with Rust above |
| `cargo` | `cargo --version` | Installed with Rust above |
| `ironclaw` CLI | `ironclaw --version` | Follow [IronClaw installation docs](https://github.com/nearai/ironclaw) |
| `sudo` (Linux) | `sudo -v` | Pre-installed on most systems |
| `curl` | `curl --version` | `sudo apt install curl` |
| `git` | `git --version` | `sudo apt install git` |
| `openssl` (for token generation) | `openssl version` | `sudo apt install openssl` |

On Debian/Ubuntu, install all build dependencies at once:

```bash
sudo apt update
sudo apt install -y curl build-essential pkg-config libssl-dev git openssl
```

---

## 4. Quick Install (recommended)

If you already have Rust, the `ironclaw` CLI, and `sudo`, the install script
handles everything automatically.

### Step 1 — Clone the repository

```bash
git clone https://github.com/chtugha/ironclaw-remote-shell-extension
cd ironclaw-remote-shell-extension
```

### Step 2 — Run the install script

```bash
./install.sh
```

The script will:

1. Check that `cargo`, `rustup`, and `ironclaw` are installed
2. Add the `wasm32-wasip2` build target if it's missing
3. Stop any running gateway (systemd service or standalone process)
4. Build the gateway binary from source
5. Install the gateway to `/usr/local/bin/remote-shell-gateway` (requires `sudo`)
6. Build the WASM tool from source
7. Install (or update) the WASM tool into IronClaw via `ironclaw tool install --force`
8. Copy the companion skill to `~/.ironclaw/skills/remote-shell/SKILL.md`
9. Set `ALLOW_LOCAL_TOOLS=true` in `~/.ironclaw/.env`
10. Restart the gateway service if a systemd unit was already configured

### Step 3 — Start the gateway (if not already running as a service)

If no systemd service exists yet, start the gateway manually:

```bash
remote-shell-gateway
```

Or with a bearer token (recommended — see [section 6](#6-starting-the-gateway)):

```bash
export SSH_GATEWAY_TOKEN="$(openssl rand -hex 32)"
echo "Your token: $SSH_GATEWAY_TOKEN"   # save this — you'll paste it into IronClaw
remote-shell-gateway
```

### Step 4 — Restart IronClaw

**You must restart IronClaw** after install so it picks up:
- The new WASM tool
- The companion skill
- The `ALLOW_LOCAL_TOOLS=true` setting

### Step 5 — Configure the bearer token in IronClaw (if used)

If you set `SSH_GATEWAY_TOKEN` in step 3, open IronClaw Settings →
Secrets and add the token under the key **`ssh_gateway_token`**.
See [section 9](#9-configuring-the-bearer-token-in-ironclaw) for details.

---

## 5. Manual Installation (step by step)

Follow this section if you prefer to install each component individually, or
if the quick-install script fails.

### Step 1 — Install build dependencies (Debian/Ubuntu)

```bash
sudo apt update
sudo apt install -y curl build-essential pkg-config libssl-dev git openssl
```

On macOS:

```bash
xcode-select --install
brew install openssl pkg-config
```

### Step 2 — Install Rust

Skip this step if `rustc --version` already works.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the on-screen prompts. When it finishes, activate Rust in your current
shell:

```bash
source "$HOME/.cargo/env"
```

Verify:

```bash
rustc --version    # should print e.g. rustc 1.78.0
cargo --version    # should print e.g. cargo 1.78.0
```

### Step 3 — Add the WASM build target

```bash
rustup target add wasm32-wasip2
```

Verify:

```bash
rustup target list --installed | grep wasm32-wasip2
```

You should see `wasm32-wasip2` in the output.

### Step 4 — Clone the repository

```bash
git clone https://github.com/chtugha/ironclaw-remote-shell-extension
cd ironclaw-remote-shell-extension
```

### Step 5 — Build the SSH gateway

```bash
cargo build --release -p remote-shell-gateway
```

This produces: `target/release/remote-shell-gateway`

This step takes 1–5 minutes on a first build (compiling all dependencies).
Subsequent builds are much faster.

### Step 6 — Install the gateway binary system-wide

```bash
sudo cp target/release/remote-shell-gateway /usr/local/bin/
sudo chmod +x /usr/local/bin/remote-shell-gateway
```

Verify:

```bash
remote-shell-gateway --help
```

You should see the gateway's help text listing `--host`, `--port`,
`--max-sessions`, and `--session-ttl-secs`.

### Step 7 — Build the WASM tool

```bash
cargo build --release --target wasm32-wasip2 -p remote-shell
```

This produces: `target/wasm32-wasip2/release/remote_shell.wasm`

### Step 8 — Install the WASM tool into IronClaw

```bash
ironclaw tool install \
  --name remote-shell \
  --force \
  target/wasm32-wasip2/release/remote_shell.wasm \
  --capabilities remote-shell/remote-shell.capabilities.json
```

- `--name remote-shell` — the name IronClaw uses for this tool
- `--force` — overwrites the tool if it was already installed (safe to run on updates)
- `--capabilities` — points to the JSON file that describes what the tool can do

### Step 9 — Enable the shell tool (required)

The companion skill directs the agent to use IronClaw's built-in `shell`
tool with `curl` to reach the gateway. The `shell` tool is only available
when `ALLOW_LOCAL_TOOLS=true` is set in IronClaw's environment.

```bash
mkdir -p ~/.ironclaw
echo "ALLOW_LOCAL_TOOLS=true" >> ~/.ironclaw/.env
```

If the file already contains `ALLOW_LOCAL_TOOLS` with a different value,
edit it instead:

```bash
# Check current value:
grep ALLOW_LOCAL_TOOLS ~/.ironclaw/.env

# Update it:
sed -i 's/^ALLOW_LOCAL_TOOLS=.*/ALLOW_LOCAL_TOOLS=true/' ~/.ironclaw/.env
```

### Step 10 — Install the companion skill

```bash
mkdir -p ~/.ironclaw/skills/remote-shell
cp skills/remote-shell/SKILL.md ~/.ironclaw/skills/remote-shell/SKILL.md
```

The skill file is a Markdown document with a YAML front-matter header that
IronClaw reads when it starts. It tells the agent:
- The extension can only be used via `shell` + `curl` (never direct WASM calls)
- Ready-to-paste curl commands for every action
- Security rules (host-key verification, credential handling)
- Recovery steps for every known error

**To update the skill** after a version upgrade: simply overwrite the file
with `cp` as shown above — no uninstall step is needed.

### Step 11 — Start the gateway

See [section 6](#6-starting-the-gateway) for full details.

Quick start for testing:

```bash
remote-shell-gateway
```

### Step 12 — Restart IronClaw

IronClaw reads tools, skills, and the `.env` file at startup. Restart it
now so all the changes take effect.

### Step 13 — Configure the bearer token (if used)

See [section 9](#9-configuring-the-bearer-token-in-ironclaw).

---

## 6. Starting the gateway

The gateway is a small HTTP server that must be running on your local machine
whenever you want to use the extension. It manages all SSH connections.

### Option A — Run in the terminal (simplest, for testing)

Open a terminal and run:

```bash
remote-shell-gateway
```

You will see log output like:

```
INFO remote_shell_gateway: Listening on 127.0.0.1:9022
```

Leave this terminal open. The gateway stops when you press `Ctrl+C` or close
the terminal.

### Option B — Run with a bearer token (recommended for security)

Generate a random token and start the gateway with it:

```bash
export SSH_GATEWAY_TOKEN="$(openssl rand -hex 32)"
echo "Your token is: $SSH_GATEWAY_TOKEN"
remote-shell-gateway
```

Copy the token — you will need to add it to IronClaw secrets (see section 9).

When a token is set, every HTTP request to the gateway must include:

```
Authorization: Bearer <your-token>
```

The agent adds this header automatically once you've added the secret to
IronClaw.

### Option C — Run as a systemd user service (recommended for always-on use)

This keeps the gateway running automatically in the background, even after
reboots.

**Step 1** — Choose a token:

```bash
export SSH_GATEWAY_TOKEN="$(openssl rand -hex 32)"
echo "Your token: $SSH_GATEWAY_TOKEN"   # write this down
```

**Step 2** — Create the service file:

```bash
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/remote-shell-gateway.service << EOF
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
EOF
```

Replace `REPLACE_WITH_YOUR_TOKEN` with the token you generated.

**Step 3** — Enable and start the service:

```bash
systemctl --user daemon-reload
systemctl --user enable --now remote-shell-gateway
```

**Step 4** — Verify it is running:

```bash
systemctl --user status remote-shell-gateway
```

You should see `Active: active (running)`.

**Step 5** — View logs:

```bash
journalctl --user -u remote-shell-gateway -f
```

**Updating the gateway binary**: stop the service, copy the new binary, then
start the service again. The `install.sh` script does this automatically.

```bash
systemctl --user stop remote-shell-gateway
sudo cp target/release/remote-shell-gateway /usr/local/bin/
systemctl --user start remote-shell-gateway
```

### Gateway CLI flags

| Flag | Default | Description |
|---|---|---|
| `--host` | `127.0.0.1` | IP address to listen on. Non-loopback addresses (e.g. `0.0.0.0`) require `SSH_GATEWAY_TOKEN` — the gateway refuses to start without it. |
| `--port` | `9022` | TCP port to listen on. |
| `--max-sessions` | `64` | Maximum number of concurrent SSH sessions. |
| `--session-ttl-secs` | `3600` | How long an idle session lives before it is automatically closed (seconds). |

### Gateway environment variables

| Variable | Description |
|---|---|
| `SSH_GATEWAY_TOKEN` | Bearer token. When set, every request must carry `Authorization: Bearer <token>`. Mandatory when `--host` is a non-loopback address. |
| `RUST_LOG` | Log verbosity. Example: `RUST_LOG=remote_shell_gateway=debug` shows body sizes and 64-character command previews. Full commands and credentials are **never** logged. |

---

## 7. Enabling the shell tool (ALLOW_LOCAL_TOOLS)

IronClaw's `shell` tool lets the agent run commands on the host machine
(outside the WASM sandbox). This tool is the only way the agent can reach
the gateway, because the WASM sandbox blocks all HTTP to localhost.

The `shell` tool is **disabled by default**. You must explicitly enable it.

### What happens without it

If `ALLOW_LOCAL_TOOLS=true` is not set, the agent will report:

```
Tool error: Tool shell not found
```

This means the agent has no way to reach the gateway. The extension is
completely non-functional without this setting.

### How to enable it

Add `ALLOW_LOCAL_TOOLS=true` to IronClaw's environment file:

```bash
mkdir -p ~/.ironclaw
echo "ALLOW_LOCAL_TOOLS=true" >> ~/.ironclaw/.env
```

If the file already has `ALLOW_LOCAL_TOOLS` set to something else:

```bash
sed -i 's/^ALLOW_LOCAL_TOOLS=.*/ALLOW_LOCAL_TOOLS=true/' ~/.ironclaw/.env
```

Verify the setting is in place:

```bash
grep ALLOW_LOCAL_TOOLS ~/.ironclaw/.env
```

Expected output: `ALLOW_LOCAL_TOOLS=true`

**Restart IronClaw** after making this change. The setting is read at startup.

The `install.sh` script applies this setting automatically.

---

## 8. Installing the companion skill

The companion skill (`SKILL.md`) is a knowledge file that IronClaw loads when
it detects SSH-related topics in the conversation. It teaches the agent:

- That the WASM tool cannot be called directly (sandbox limitation)
- Exactly which `curl` commands to run for each action
- The security rules to follow (host-key verification, credential handling)
- How to recover from every known error

Without the skill, the agent has to guess how to use the extension and will
likely fail. **Install the skill.**

### Install

```bash
mkdir -p ~/.ironclaw/skills/remote-shell
cp skills/remote-shell/SKILL.md ~/.ironclaw/skills/remote-shell/SKILL.md
```

### Update after an upgrade

Overwrite the file — no uninstall step is needed:

```bash
cp skills/remote-shell/SKILL.md ~/.ironclaw/skills/remote-shell/SKILL.md
```

### When the skill activates

The skill loads automatically when the conversation contains keywords like:
`ssh`, `remote`, `deploy`, `production server`, `staging server`, `connect to
host`, `run on the server`, `session`, `fingerprint`, `ansible`.

---

## 9. Configuring the bearer token in IronClaw

If you started the gateway with `SSH_GATEWAY_TOKEN` set, you need to tell
IronClaw about it so the agent can include the correct `Authorization` header
in its requests.

### Step 1 — Open IronClaw settings

In the IronClaw UI, go to **Settings** → **Secrets** (or the equivalent in
your version of the CLI).

### Step 2 — Add the secret

Add a new secret with:

- **Key**: `ssh_gateway_token`
- **Value**: the exact token you used when starting the gateway (the hex string)

IronClaw will inject this value as `Authorization: Bearer <token>` on every
request to the gateway.

### Step 3 — Verify (optional)

Ask IronClaw:

> Is the SSH gateway running?

The agent will run:

```bash
curl -s -H 'Authorization: Bearer <your-token>' 'http://127.0.0.1:9022/health'
```

A successful response looks like:

```json
{"status":"ok"}
```

### If you do not use a bearer token

Skip this section entirely. The gateway runs without authentication if
`SSH_GATEWAY_TOKEN` is not set. This is safe only on a single-user machine
where nothing else can reach `localhost:9022`.

---

## 10. Using the extension from IronClaw chat

All interactions happen through natural-language chat. You describe what you
want to do; IronClaw figures out the right `curl` command to run via the
`shell` tool.

You can also give IronClaw explicit JSON if you want precise control over
individual parameters.

### The recommended workflow

```
1. Check the gateway is running  →  health
2. Check for an existing session  →  list sessions
3. Open a new session (if needed)  →  connect
4. Run as many commands as needed  →  execute (reuse the same session_id)
5. Close the session when done  →  disconnect
```

**Do not reconnect for every command.** Sessions are persistent — reuse the
`session_id` across all commands within a conversation.

---

### Getting a host key fingerprint (before you connect)

Before connecting, you must verify the SSH server's host key to protect
against man-in-the-middle attacks. Run this on your local machine:

```bash
ssh-keyscan -p 22 server.example.com | ssh-keygen -lf -
```

Example output:

```
256 SHA256:oBJ5MHd/vRDDe7jDGTrVEV5lN3S8J8Kpb2Hq7EXAMPLE server.example.com (ED25519)
```

Use the `SHA256:…` part as `host_key_fingerprint` in your connect call.

---

### Check if the gateway is running

**Natural language:**
> Is the SSH gateway running?

The agent runs:

```bash
curl -s 'http://127.0.0.1:9022/health'
```

Success response:

```json
{"status":"ok"}
```

If this fails, start the gateway (see section 6) before doing anything else.

---

### Connect to a server (password authentication)

**Natural language:**
> Connect to server.example.com as the user `deploy` using password `mypassword`.
> The host key fingerprint is SHA256:oBJ5MHd… Name the session `prod`.

The agent runs:

```bash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{
    "host": "server.example.com",
    "port": 22,
    "username": "deploy",
    "auth": {"type": "password", "password": "mypassword"},
    "host_key_fingerprint": "SHA256:oBJ5MHd/vRDDe7jDGTrVEV5lN3S8J8Kpb2Hq7EXAMPLE",
    "session_id": "prod"
  }' \
  'http://127.0.0.1:9022/connect'
```

Success response:

```json
{"session_id":"prod","message":"Connected to server.example.com"}
```

---

### Connect with a private key

**Natural language:**
> Connect to server.example.com as `ubuntu` using my private key. Fingerprint is
> SHA256:oBJ5MHd… Call the session `prod`.

The agent uses a temp file to avoid embedding the key in process listings:

```bash
cat > /tmp/ssh_key_$$ << 'KEYEOF'
-----BEGIN OPENSSH PRIVATE KEY-----
<key content here>
-----END OPENSSH PRIVATE KEY-----
KEYEOF
chmod 600 /tmp/ssh_key_$$
python3 -c "
import json
key = open('/tmp/ssh_key_$$').read()
print(json.dumps({
  'host': 'server.example.com',
  'port': 22,
  'username': 'ubuntu',
  'auth': {'type': 'private_key', 'key_pem': key},
  'host_key_fingerprint': 'SHA256:oBJ5MHd/vRDDe7jDGTrVEV5lN3S8J8Kpb2Hq7EXAMPLE',
  'session_id': 'prod'
}))
" > /tmp/ssh_body_$$
curl -s -X POST -H 'Content-Type: application/json' \
  -d @/tmp/ssh_body_$$ 'http://127.0.0.1:9022/connect'
rm /tmp/ssh_key_$$ /tmp/ssh_body_$$
```

---

### Run a command

**Natural language:**
> Run `df -h` on the prod session.

The agent runs:

```bash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"session_id":"prod","command":"df -h","timeout_secs":30}' \
  'http://127.0.0.1:9022/execute'
```

Success response:

```json
{
  "exit_code": 0,
  "stdout": "Filesystem      Size  Used Avail Use% Mounted on\n...",
  "stderr": ""
}
```

**With a longer timeout** (for slow commands like backups, builds, etc.):

> Run `tar czf /tmp/backup.tar.gz /var/www` on prod. Give it 5 minutes.

```bash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"session_id":"prod","command":"tar czf /tmp/backup.tar.gz /var/www","timeout_secs":300}' \
  'http://127.0.0.1:9022/execute'
```

Default timeout is 30 seconds. Range: 1–3600 seconds.

---

### List all open sessions

**Natural language:**
> What SSH sessions are currently open?

```bash
curl -s 'http://127.0.0.1:9022/sessions'
```

Success response:

```json
[
  {"session_id":"prod","host":"server.example.com","port":22,"username":"deploy","age_secs":142},
  {"session_id":"staging","host":"staging.example.com","port":22,"username":"ci","age_secs":30}
]
```

---

### Disconnect a session

**Natural language:**
> Disconnect the prod session.

```bash
curl -s -X DELETE -H 'Content-Type: application/json' \
  -d '{"session_id":"prod"}' \
  'http://127.0.0.1:9022/disconnect'
```

Success response:

```json
{"message":"Session 'prod' disconnected"}
```

---

### Full example conversation

```
You:  Is the gateway running?

AI:   ✓ Gateway is healthy (status: ok).

You:  Connect to build.internal as ci. Password is "ci-pass".
      Fingerprint is SHA256:xyz… Call the session "build".

AI:   Connected. Session "build" is open to build.internal.

You:  Run `git pull && make test` on build. Give it 2 minutes.

AI:   Exit code 0.
      stdout:
        Already up to date.
        All 47 tests passed.

You:  Run `df -h` to check disk space.

AI:   Exit code 0.
      stdout:
        Filesystem  Size  Used Avail Use%  Mounted on
        /dev/sda1   100G   42G   58G  42%  /

You:  Disconnect the build session.

AI:   Session "build" disconnected.
```

---

## 11. Complete curl API reference

All endpoints are on `http://127.0.0.1:9022` by default. If you use a
bearer token, add `-H 'Authorization: Bearer <your-token>'` to every command.

### GET /health

Confirms the gateway is running.

```bash
curl -s 'http://127.0.0.1:9022/health'
```

Response:

```json
{"status":"ok"}
```

---

### GET /sessions

Lists all currently open SSH sessions.

```bash
curl -s 'http://127.0.0.1:9022/sessions'
```

Response (array, may be empty `[]`):

```json
[
  {
    "session_id": "prod",
    "host": "server.example.com",
    "port": 22,
    "username": "deploy",
    "age_secs": 142
  }
]
```

---

### POST /connect

Opens a new SSH session.

**Request body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `host` | string | yes | Hostname or IP of the remote server |
| `port` | integer | no | SSH port (default: 22) |
| `username` | string | yes | SSH username |
| `auth` | object | yes | Authentication (see below) |
| `host_key_fingerprint` | string | see note | SHA256 fingerprint of the server's host key (e.g. `SHA256:abc…`) |
| `insecure_ignore_host_key` | boolean | see note | Set to `true` to skip host-key verification. **Insecure — only for trusted private networks.** |
| `session_id` | string | no | A name for this session. Auto-generated UUID if omitted. |

> Either `host_key_fingerprint` **or** `insecure_ignore_host_key: true` is
> required. Omitting both is rejected by the WASM tool before any network
> call is made.

**`auth` object (password):**

```json
{"type": "password", "password": "your-password"}
```

**`auth` object (private key):**

```json
{"type": "private_key", "key_pem": "-----BEGIN OPENSSH PRIVATE KEY-----\n...\n-----END OPENSSH PRIVATE KEY-----"}
```

**Full example (password auth):**

```bash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{
    "host": "server.example.com",
    "port": 22,
    "username": "deploy",
    "auth": {"type": "password", "password": "mypassword"},
    "host_key_fingerprint": "SHA256:oBJ5MHd/vRDDe7jDGTrVEV5lN3S8J8Kpb2Hq7EXAMPLE",
    "session_id": "prod"
  }' \
  'http://127.0.0.1:9022/connect'
```

**Success response (HTTP 200):**

```json
{"session_id":"prod","message":"Connected to server.example.com"}
```

---

### POST /execute

Runs a command on an open SSH session.

**Request body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `session_id` | string | yes | The session to run the command on |
| `command` | string | yes | The shell command to execute |
| `timeout_secs` | integer | no | Timeout in seconds (default: 30, range: 1–3600) |

```bash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"session_id":"prod","command":"uptime","timeout_secs":30}' \
  'http://127.0.0.1:9022/execute'
```

**Success response (HTTP 200):**

```json
{
  "exit_code": 0,
  "stdout": " 14:32:01 up 42 days,  3:17,  1 user,  load average: 0.01, 0.05, 0.07\n",
  "stderr": ""
}
```

If a command times out, `exit_code` will be `null` and both `stdout`/`stderr`
will contain whatever was collected before the timeout.

If stdout or stderr exceeds 10 MB, output is truncated and a warning is
appended to `stderr`.

---

### DELETE /disconnect

Closes an SSH session and frees its resources on the remote server.

**Request body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `session_id` | string | yes | The session to close |

```bash
curl -s -X DELETE -H 'Content-Type: application/json' \
  -d '{"session_id":"prod"}' \
  'http://127.0.0.1:9022/disconnect'
```

**Success response (HTTP 200):**

```json
{"message":"Session 'prod' disconnected"}
```

---

### Error responses

All error responses have this shape:

```json
{"error": "description of what went wrong"}
```

Common HTTP status codes:

| Code | Meaning |
|---|---|
| 400 | Bad request — missing or invalid field in the request body |
| 401 | Unauthorized — bearer token missing or incorrect |
| 404 | Session not found — session was never created, has expired, or was disconnected |
| 413 | Request body too large — `key_pem` or `command` exceeds the size limit |
| 500 | Internal server error — SSH-level error (authentication failure, host-key mismatch, etc.) |

---

### Using a gateway on a non-default port

If the gateway was started with `--port 9100`, pass `gateway_port` in the
WASM tool's JSON (for schema-aware calls) or simply change the URL in curl:

```bash
curl -s 'http://127.0.0.1:9100/health'
```

---

## 12. Gateway configuration reference

### Session lifecycle

- Sessions are created by `/connect` and destroyed by `/disconnect`.
- Sessions are also automatically **reaped** at the start of every request
  (`connect`, `execute`, `disconnect`, `sessions`) when their TTL has elapsed.
- A request that references a reaped or non-existent `session_id` returns
  `HTTP 404`. The correct recovery is to reconnect using the same `session_id`.
- All sessions are held **in memory**. They are lost if the gateway is
  restarted. After restarting the gateway, reconnect all sessions.

### SSH key authentication

- **RSA keys** are tried with `rsa-sha2-512` first, then `rsa-sha2-256`,
  then the legacy `ssh-rsa` (SHA-1). This covers both modern OpenSSH (≥ 8.8,
  which rejects SHA-1) and older servers.
- **ED25519 and ECDSA keys** authenticate in a single attempt.
- **Passphrase-protected keys** are not currently supported. Decrypt the
  key before use (`ssh-keygen -p -f <keyfile>`).

### SSH keepalive

The gateway sends an SSH keepalive every 30 seconds. If 3 consecutive
keepalives go unanswered, the session is marked dead and cleaned up. This
detects silently dropped connections (e.g. NAT timeouts).

---

## 13. Security guide

### Bind address and bearer token

The gateway binds to `127.0.0.1` by default, which means only processes on
the same machine can reach it. If you change `--host` to `0.0.0.0` or a
public IP, the gateway **refuses to start** unless `SSH_GATEWAY_TOKEN` is
set. This prevents accidentally running an unauthenticated SSH-relay service
on a public interface.

**Always use a bearer token on shared or multi-user machines.**

Token comparison uses constant-time equality to prevent timing attacks.

### Host-key verification

Always supply `host_key_fingerprint`. Without it, you cannot detect a
man-in-the-middle attack where someone intercepts your SSH connection and
presents a fake server.

How to get the fingerprint:

```bash
ssh-keyscan -p 22 server.example.com | ssh-keygen -lf -
```

Only use `insecure_ignore_host_key: true` on networks you control completely
(e.g. a local VM you just created).

### Credentials in transit

SSH passwords and private keys are sent over HTTP between the agent (via
the `shell` tool running `curl`) and the gateway. Because the gateway binds
to `127.0.0.1` by default, this traffic never leaves the machine. However:

- Do not run the gateway as a different user from IronClaw.
- Do not expose the gateway port to the network without a bearer token.
- Private keys are handled via temp files (never embedded in curl arguments)
  to keep them out of process listings.

### Logging

- Full command strings are **never** logged, even at `debug` level.
- At `debug` level, a 64-character preview of the command is logged with the
  remainder truncated.
- Passwords, key material, and passphrases are never logged at any level.
- Body sizes are logged at `debug` level for diagnostics.

### Request body limits

| Endpoint | Body size limit |
|---|---|
| `/connect` | 512 KiB |
| All others | 1 MiB |

Oversized requests are rejected with `HTTP 413` before any processing occurs.

---

## 14. Limits

| Parameter | Limit |
|---|---|
| Max concurrent SSH sessions | 64 (configurable via `--max-sessions`) |
| Session TTL | 3600 seconds = 1 hour (configurable via `--session-ttl-secs`) |
| Min command timeout | 1 second |
| Max command timeout | 3600 seconds = 1 hour |
| Default command timeout | 30 seconds |
| Max stdout per command | 10 MB (output truncated, warning appended) |
| Max stderr per command | 10 MB (output truncated, warning appended) |
| Max hostname length | 253 characters |
| Max command length | 65 536 characters |
| Max username length | 65 536 characters |
| Max private-key PEM length | 256 KiB (within the 512 KiB connect body limit) |
| SSH keepalive interval | 30 seconds |
| SSH keepalive max missed | 3 (session killed after 3 missed keepalives) |
| Logged command preview | 64 characters (remainder truncated; never the full command) |

---

## 15. Troubleshooting

### Quick self-check

Before digging into error messages, run this self-check:

```bash
# 1. Is the gateway binary installed?
which remote-shell-gateway

# 2. Is it listening?
lsof -i :9022 | grep LISTEN

# 3. Does it respond?
curl -s 'http://127.0.0.1:9022/health'

# 4. Is ALLOW_LOCAL_TOOLS set?
grep ALLOW_LOCAL_TOOLS ~/.ironclaw/.env

# 5. Is the WASM tool installed?
ironclaw tool list | grep remote-shell

# 6. Is the skill installed?
ls ~/.ironclaw/skills/remote-shell/SKILL.md
```

---

### Error reference

| Symptom (what you see in IronClaw) | Cause | Fix |
|---|---|---|
| `Tool error: Tool shell not found` | `ALLOW_LOCAL_TOOLS=true` is not set. The `shell` tool is never registered by IronClaw without it. | Add `ALLOW_LOCAL_TOOLS=true` to `~/.ironclaw/.env` and restart IronClaw. See [section 7](#7-enabling-the-shell-tool-allow_local_tools). |
| `sandbox blocks HTTP requests to localhost` | IronClaw WASM sandbox blocks HTTP to 127.0.0.1 at two independent layers. | This error should not appear if the companion skill is installed and the agent is using curl via the `shell` tool. Reinstall the skill (section 8) and restart IronClaw. |
| `Gateway is NOT reachable` / `Gateway request failed` | The local gateway is not running, or is on a different port than expected. | Check with `lsof -i :9022`. Start the gateway (section 6). |
| `Gateway error (HTTP 401): Unauthorized` | The bearer token the agent sent does not match the token the gateway expects. | Re-add the `ssh_gateway_token` secret in IronClaw with the correct value. Or restart the gateway without `SSH_GATEWAY_TOKEN` if you do not want authentication. |
| `Gateway error (HTTP 404): Session 'X' not found` | The session was never created, expired (TTL elapsed), or the gateway was restarted (clearing all in-memory sessions). | Reconnect using the same `session_id`. |
| `Gateway error (HTTP 413): Request Entity Too Large` | The request body exceeds the size limit (1 MiB for most endpoints, 512 KiB for `/connect`). The most common cause is a very large private key or an extremely long command. | For long commands, split the work. For keys, confirm the key is not accidentally double-encoded. |
| `Authentication failed: credentials rejected` | Wrong username, wrong password, wrong private key, or the server requires keyboard-interactive authentication (not supported). | Test manually with `ssh username@host`. Do not brute-force. |
| `host key fingerprint mismatch` | The server presented a different host key than `host_key_fingerprint`. Either the server's host key legitimately changed (e.g. after a rebuild), or you are being MITM'd. | **Stop and verify the new fingerprint out-of-band** (e.g. via the cloud provider console) before retrying. Never disable host-key verification to resolve this. |
| `Exit code: unknown (command may have timed out)` | The command ran longer than `timeout_secs`. | Increase `timeout_secs` (max 3600). For very long jobs, run them in the background on the server: `nohup <command> > /tmp/out.log 2>&1 &` and then check the log file in a later command. |
| `Refusing to bind to non-loopback address '…' without SSH_GATEWAY_TOKEN` | The gateway was started with `--host 0.0.0.0` (or similar) but `SSH_GATEWAY_TOKEN` is not set. | Either change `--host` back to `127.0.0.1`, or set `SSH_GATEWAY_TOKEN` before starting the gateway. |
| Build fails: `error: linker 'cc' not found` | Build dependencies are missing. | Run `sudo apt install build-essential` (Debian/Ubuntu) or `xcode-select --install` (macOS). |
| Build fails: `error[E0463]: can't find crate for 'std'` on WASM build | The `wasm32-wasip2` target is not installed. | Run `rustup target add wasm32-wasip2`. |
| `ironclaw: command not found` | IronClaw CLI is not installed or not in `PATH`. | Install IronClaw following the [official instructions](https://github.com/nearai/ironclaw). |

---

### Enabling debug logging

For deep troubleshooting, start the gateway with verbose logging:

```bash
RUST_LOG=remote_shell_gateway=debug remote-shell-gateway
```

This shows:
- Each incoming HTTP request (method, path, body size)
- The 64-character command preview (never the full command)
- SSH connection events (connect, disconnect, keepalive)
- Session reaping events

---

## 16. Changelog

### 0.1.4 — Enable local tools prerequisite (current)

- Root-caused `Tool shell not found`: IronClaw's `shell` tool requires
  `ALLOW_LOCAL_TOOLS=true` in `~/.ironclaw/.env`. Without it, the `shell`
  tool is never registered and the extension cannot reach the gateway via any
  path (WASM sandbox blocks localhost HTTP; `http` builtin also blocks loopback
  IPs).
- `install.sh`: added `ensure_allow_local_tools()` — idempotent function that
  handles both unquoted (`true`) and IronClaw-written quoted (`"true"`) formats.
- `SKILL.md`: new Prerequisites section; `ALLOW_LOCAL_TOOLS` references in
  tool-selection and workaround sections; `Tool shell not found` failure-mode
  entry.
- `capabilities.json`: WARNING note and first conditional requirement updated
  with the prerequisite and recovery guidance.
- `README.md`: full rewrite with step-by-step instructions for every component.

### 0.1.3 — Install script

- Added `install.sh` — automates the full build-and-install workflow:
  detects and stops a running gateway (service or process), builds both
  components from source, installs the gateway binary, the WASM tool, and the
  companion skill, then restarts the gateway. Handles fresh installs and
  updates. `--force` flag used on `ironclaw tool install` for idempotency.

### 0.1.2 — Sandbox diagnostics

- WASM tool detects when the IronClaw sandbox blocks the HTTP call to
  localhost and returns an actionable error message with the equivalent `curl`
  command for the agent to run via the `shell` tool.
- `SKILL.md` updated with sandbox workaround guide: complete curl commands for
  every action, private-key temp-file pattern, bearer-token instructions.
- `README.md` documents the sandbox HTTP restriction as a known limitation with
  root-cause analysis and upstream fix reference.
- 7 new unit tests covering `is_sandbox_restriction` and
  `sandbox_gateway_error`.

### 0.1.1 — Hardening pass

**Correctness**
- Stdin EOF (`channel.eof()`) sent after every `execute` — commands that read
  stdin (`cat`, `read`, etc.) no longer hang indefinitely.
- `channel.exec(want_reply=false, …)` removes a round-trip that caused hangs
  on servers that do not reply promptly.
- `channel.close()` called on command timeout to free the remote exec slot.
- HTTP execute timeout uses saturating arithmetic — `u64::MAX` `timeout_secs`
  no longer panics the WASM module.
- RSA keys try `SHA-512 → SHA-256 → SHA-1`, fixing authentication failures
  against OpenSSH ≥ 8.8. ED25519/ECDSA use a single-iteration path.
- Expired sessions reaped on every endpoint (not just `/connect`).

**Security / DoS**
- `DefaultBodyLimit` enforced: 1 MiB on general endpoints, 512 KiB on
  `/connect`.
- Gateway refuses to bind to a non-loopback address without
  `SSH_GATEWAY_TOKEN`.
- Loopback detection is case-insensitive (`localhost`, `LOCALHOST`,
  `127.0.0.0/8`, `::1`).
- Command-string logging replaced with a 64-char truncated preview; passwords
  and key material are never logged.

**UX**
- New `health` action maps to `GET /health`, giving the agent a one-call
  probe for gateway availability.
- WASM tool fails fast for missing `auth`, missing host-key verification,
  out-of-range `timeout_secs`, and oversized inputs — before any network
  round-trip.
- Empty stdout/stderr collapse to `(no output)` in the response.

**Skill**
- Ships `skills/remote-shell/SKILL.md` so IronClaw's skill loader picks up
  the lifecycle, security, and recovery guidance automatically.

**Tests**
- 39 WASM-side and 10 gateway-side tests covering: serde shapes, validation
  bounds, timeout overflow, hostname rules, body-size limits, host-key
  enforcement, auth-required, and case-insensitive loopback detection.
