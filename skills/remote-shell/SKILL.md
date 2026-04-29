---
name: remote-shell
version: 0.1.0
description: Best practices for the remote_shell tool — SSH into remote machines via the local gateway, run commands, and manage persistent sessions safely.
activation:
  keywords:
    - "ssh"
    - "scp"
    - "remote"
    - "remote shell"
    - "remote command"
    - "remote server"
    - "remote host"
    - "remote machine"
    - "production server"
    - "staging server"
    - "deploy"
    - "ansible"
    - "host key"
    - "fingerprint"
    - "session"
  exclude_keywords:
    - "github actions"
    - "browser"
    - "chrome"
  patterns:
    - "(?i)\\bssh\\s+(into|to|-i|-p|root@|deploy@|user@)"
    - "(?i)(connect|log\\s*in|login)\\s+(to|into)\\s+(server|host|prod|staging|remote|\\S+@\\S+)"
    - "(?i)run\\s+.+\\s+on\\s+(server|host|remote|prod|staging|the\\s+box|the\\s+vm)"
    - "(?i)(ssh-keyscan|ssh-keygen|known_hosts|authorized_keys)"
    - "(?i)remote[_-]shell"
  tags:
    - "ops"
    - "ssh"
    - "remote"
  max_context_tokens: 1500
---

# remote-shell Best Practices

Use this skill whenever the user wants to operate on a **remote** machine over
SSH. The `remote_shell` tool talks to a local gateway service
(`remote-shell-gateway`) which holds the actual SSH connections; the tool
itself is sandboxed and only speaks HTTP to `127.0.0.1`.

## Sandbox HTTP restriction (read this first)

IronClaw's WASM sandbox **blocks all HTTP requests to `127.0.0.1`** (both
scheme and loopback-IP layers). The `remote_shell` tool talks to the local
gateway over `http://127.0.0.1:9022`, so every action will fail with a
`Gateway request failed: the IronClaw sandbox blocks HTTP requests to
localhost` error — even when `lsof` confirms the gateway is listening.

**This is a known IronClaw sandbox limitation, not a gateway bug.**

### Workaround via the `shell` tool

Use IronClaw's built-in `shell` tool to send the HTTP request directly. The
`shell` tool runs on the host without WASM restrictions and can reach
localhost.

**Health check:**
```bash
curl -s 'http://127.0.0.1:9022/health'
```

**List active sessions:**
```bash
curl -s 'http://127.0.0.1:9022/sessions'
```

**Connect (password auth):**
```bash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"host":"server.example.com","port":22,"username":"deploy",
       "auth":{"type":"password","password":"<PASSWORD>"},
       "host_key_fingerprint":"SHA256:...","insecure_ignore_host_key":false}' \
  'http://127.0.0.1:9022/connect'
```

**Connect (private key — write key to temp file to avoid shell-history exposure):**
```bash
cat > /tmp/ssh_key_$$ << 'KEYEOF'
-----BEGIN OPENSSH PRIVATE KEY-----
<paste key here>
-----END OPENSSH PRIVATE KEY-----
KEYEOF
chmod 600 /tmp/ssh_key_$$
curl -s -X POST -H 'Content-Type: application/json' \
  -d "{\"host\":\"server.example.com\",\"port\":22,\"username\":\"deploy\",
       \"auth\":{\"type\":\"private_key\",\"key_pem\":\"$(sed 's/$/\\n/g' /tmp/ssh_key_$$ | tr -d '\n')\"},
       \"host_key_fingerprint\":\"SHA256:...\",\"insecure_ignore_host_key\":false}" \
  'http://127.0.0.1:9022/connect'
rm /tmp/ssh_key_$$
```

**Execute a command:**
```bash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"session_id":"prod","command":"uptime","timeout_secs":30}' \
  'http://127.0.0.1:9022/execute'
```

**Disconnect:**
```bash
curl -s -X DELETE -H 'Content-Type: application/json' \
  -d '{"session_id":"prod"}' \
  'http://127.0.0.1:9022/disconnect'
```

All gateway responses are JSON. On success the HTTP status is 2xx. On error
check the `"error"` key in the response body.

> **Bearer token**: if the gateway was started with `SSH_GATEWAY_TOKEN`, add
> `-H 'Authorization: Bearer <token>'` to every curl command.

---

## Tool selection

- The IronClaw built-in `shell` tool runs commands on the **local** machine
  (where IronClaw is running). Use it for the user's own repo, builds, tests.
- `remote_shell` runs commands on **another** machine over SSH. Use it only
  when the host is genuinely remote.
- Never SCP or rsync files using `remote_shell.execute "cat … | base64"` if a
  proper file-transfer tool is available — but for small text files that
  pattern is acceptable.

## Action lifecycle (memorise this order)

> **Sandbox note**: all actions below fail with a sandbox HTTP restriction
> error inside IronClaw. Use the `shell` tool + `curl` workaround from the
> section above instead of calling `remote_shell` actions directly.

1. **`health`** — call once at the start of a session if you don't already
   know the gateway is up. On failure, ask the user to start
   `remote-shell-gateway` (see project README) instead of retrying blindly.
2. **`list_sessions`** — check whether a session you can reuse is already
   open. Reusing a session avoids re-authentication and keeps the SSH
   keepalive alive.
3. **`connect`** — only when no usable session exists. Pick a memorable
   `session_id` (`prod`, `staging`, `db-1`) so you can refer to it later.
4. **`execute`** — call repeatedly on the same `session_id`. Default timeout
   is 30 s (allowed range: 1–3600). Bump `timeout_secs` for long jobs.
5. **`disconnect`** — when finished. The gateway will reap idle sessions
   after the TTL anyway, but explicit disconnect is cleaner.

## Security rules (non-negotiable)

- **Always pass `host_key_fingerprint`** unless the user explicitly opts out.
  Get it via `ssh-keyscan <host> | ssh-keygen -lf -` and use the
  `SHA256:…` portion. Without it the agent is vulnerable to MITM.
- Use `insecure_ignore_host_key: true` only for known-trusted lab networks
  the user explicitly identified.
- Prefer `auth.type: "private_key"` over passwords. If a password is
  unavoidable, never echo it back to the user in summaries.
- Never log full credentials, key material, or `auth.passphrase`.
- The `gateway_port` defaults to 9022. Don't change it unless the user said
  so.

## Failure modes & recovery

| Symptom (tool error string contains) | Likely cause | Recovery |
|---|---|---|
| `sandbox blocks HTTP requests to localhost` | IronClaw WASM sandbox blocks HTTP to 127.0.0.1. | Use the `shell` tool with `curl` instead (see **Sandbox HTTP restriction** above). |
| `Gateway request failed` / `Gateway is NOT reachable` | The local gateway isn't running. | Tell the user to start `remote-shell-gateway`, then `health`. Do not retry connect blindly. |
| `Gateway error (HTTP 401)` | Bearer token mismatch. | Ask the user to re-add the `ssh_gateway_token` secret. |
| `Gateway error (HTTP 413)` | Body too large (oversized `key_pem` or `command`). | Shorten the input; for large transfers prefer multiple smaller commands. |
| `Gateway error (HTTP 404): Session 'X' not found` | Session was reaped (TTL elapsed) or never existed. | Reconnect using the same `session_id`. |
| `Authentication failed: credentials rejected` | Wrong password/key, wrong username, or server requires keyboard-interactive. | Ask the user to confirm credentials; do not brute-force. |
| `host key fingerprint mismatch` | Either the host changed keys legitimately, or you are being MITM'd. | **Stop.** Ask the user to verify the fingerprint out-of-band before retrying. Never auto-disable verification. |
| `Exit code: unknown (command may have timed out)` | Command exceeded `timeout_secs`. | Increase `timeout_secs`, or run the work in a `nohup … &` style background. |

## Output handling

- Each `execute` returns `Exit code`, plus `--- stdout ---` and
  `--- stderr ---` blocks. Empty results render as `(no output)`.
- Stdout / stderr are capped at 10 MB each. If the response contains
  `[warning: output truncated at 10MB limit]`, you missed data — narrow the
  command (`head`, `tail`, `grep`).
- Don't blindly summarise large outputs into the user's chat — quote the
  relevant lines only.

## Quick examples

Probe and reuse a session:

```json
{ "action": "health" }
{ "action": "list_sessions" }
{ "action": "connect", "host": "build.internal", "username": "ci",
  "auth": { "type": "private_key", "key_pem": "-----BEGIN ...-----\n..." },
  "host_key_fingerprint": "SHA256:abc…",
  "session_id": "build" }
{ "action": "execute", "session_id": "build",
  "command": "git pull && make test", "timeout_secs": 300 }
{ "action": "disconnect", "session_id": "build" }
```

## Don'ts

- Don't reconnect for every command. Reuse the `session_id`.
- Don't log credentials.
- Don't pipe enormous files through `cat`; use `tail -c` / `head -c` and
  iterate.
- Don't set `insecure_ignore_host_key: true` to "make it work" — fix the
  fingerprint instead.
- Don't run destructive commands (`rm -rf`, `dd`, `shutdown`, `:(){:|:&};:`)
  without an explicit confirmation from the user that names the target host.
