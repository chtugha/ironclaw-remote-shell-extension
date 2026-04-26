# Investigation — Codebase Audit (`ironclaw-remote-shell-extension`)

## 1. Bug summary

The user requested a thorough audit of the complete codebase for **bugs, stubs,
security issues, magic numbers and dead code**, plus an evaluation of whether
"our shell execution can really be used by the agent" and whether any IronClaw
**skills need to be overridden / enhanced**.

The codebase is small (two crates, ~1.6k LOC) but contains a non-trivial number
of correctness / security / UX issues that should be fixed before declaring the
extension "excellent software". The most impactful problems:

| # | Severity | Area | Issue |
|---|---|---|---|
| B1 | High (correctness) | gateway | `channel.exec(want_reply=true, …)` followed by no `eof()` causes any command that reads stdin (e.g. `cat`, `read`, interactive scripts) to hang until the timeout. The agent has no way to feed input. |
| B2 | High (UX/correctness) | wasm tool | `AuthMethod` is `#[serde(default)]` to an empty-password Password — when an LLM omits `auth`, connect silently sends `password=""` and the user sees a confusing "credentials rejected" rather than a validation error. The JSON-Schema declares `auth` required, so the two layers disagree. |
| B3 | High (security/DoS) | gateway | `axum::Json` body has no size limit. A malicious client (or a runaway LLM) can OOM the gateway by POSTing a giant `key_pem` or `command`. |
| B4 | Medium (correctness) | gateway | Expired sessions are only reaped on `/connect`. `GET /sessions` happily returns expired entries with stale `age_secs`, and `DELETE /disconnect` returns 200 for an expired session that is still in the map. |
| B5 | Medium (security) | gateway | `tracing` debug log includes the full `command` string (line ~552), which can contain secrets (`env VAR=secret …`, `mysql -p…`). |
| B6 | Medium (security) | wasm tool | Host-key requirement is only enforced by the gateway. The WASM tool happily forwards a connect request that has neither `host_key_fingerprint` nor `insecure_ignore_host_key`; the user only learns at gateway round-trip. The schema says one of the two is required but does not encode a `oneOf`/`anyOf` constraint. |
| B7 | Medium (correctness) | wasm tool | `((timeout_secs + HTTP_EXECUTE_TIMEOUT_BUFFER_SECS) * 1000)` uses regular addition / multiplication. If an LLM sends `timeout_secs = u64::MAX` it overflows and panics in the WASM module before any validation. WASM tool does not enforce `MAX_TIMEOUT_SECS`. |
| B8 | Medium (UX) | wasm tool | `validate_hostname` rejects spaces / CR / LF, but not the much more important embedded NUL byte and tab; also accepts an all-whitespace string after the empty-check (e.g. `"   "`). |
| B9 | Low (correctness) | gateway | `channel.exec(true, …)` — `want_reply=true` means we wait for a reply that some servers never send promptly, racing with the read loop. Conventional usage is `false`. |
| B10 | Low (correctness) | gateway | After `exec` we never call `channel.eof()`, so the remote shell does not get EOF on stdin and `read`/`cat` block. |
| B11 | Low (correctness) | gateway | When the read loop times out the channel is left open (only dropped); we never call `channel.close()` to free the remote PTY/exec slot. |
| B12 | Low (UX) | wasm tool | Description ("The remote-shell-gateway service must be running locally before use.") gives the agent no way to **detect** that the gateway is down. There is no `health` / `ping` action that maps to `GET /health`. |
| B13 | Low (UX) | gateway | `--host` accepts `0.0.0.0` silently. Combined with no token, this exposes anonymous SSH-as-a-service. We should refuse `0.0.0.0` unless `SSH_GATEWAY_TOKEN` is set, or at minimum log a louder warning. |
| B14 | Low (style) | both | Magic numbers `253`, `10 * 1024 * 1024`, `65536`, `30`, `9022`, `22` are partially constants in the gateway and partially literal in the WASM crate. The WASM crate hard-codes `253` inside `validate_hostname` instead of a named constant. |
| B15 | Low (correctness) | gateway | `PrivateKeyWithHashAlg::new(Arc::new(key_pair), None)` passes `None` for the hash algorithm, which forces SSH-RSA's deprecated SHA-1 signature for RSA keys. Many modern SSH servers (OpenSSH ≥ 8.8) reject this by default. ED25519 / ECDSA work. We should pass `Some(HashAlg::Sha512)` for RSA keys, or at least try Sha512 → Sha256 → None on failure. |
| B16 | Low (UX) | wasm tool | The format of `format_execute_response` always shows `--- stdout ---` and `--- stderr ---`; for noisy commands the agent's context window is wasted on `(empty)` placeholders. Truncate to a single line when both empty. |
| B17 | Low (correctness) | gateway | `auth_middleware` uses `constant_time_eq` correctly, but the middleware short-circuits on `headers.get("authorization").to_str()` failing (non-UTF8) by attempting auth-fail — fine, but the early `_ =>` arm also matches `Some("")` after the prefix strip, which is OK. Worth a regression test. |
| B18 | Low (dead code) | wasm tool | `AuthMethod::default()` is **only** used by `#[serde(default)]`. Once we drop that attribute (B2), the `Default` impl is dead code and should be removed in both crates. |
| B19 | Low (style) | wasm tool | `validate_input_length` is invoked only inside `validate_command`. Other paths (username, session_id, key_pem, password) duplicate the length logic with literal `MAX_TEXT_LENGTH`. Use the helper everywhere or remove the helper. |
| B20 | Low (test gap) | both | No integration tests covering: timeout enforcement, auth-failure path, expired-session reaping, body-size limits, MITM detection on fingerprint mismatch. The unit tests only exercise serde shapes. |

## 2. Root cause analysis

### 2.1 Connect / execute UX (B1, B2, B6, B12)

The WASM tool was intentionally designed as a thin pass-through: it serialises
the LLM-supplied JSON, ships it to the gateway, and stringifies the answer.
That design is sound, but two layers must agree on **validation**:

* The WASM tool should fail-fast for anything the gateway will reject. Today it
  validates length and hostname punctuation, then forwards everything else. It
  trusts the gateway to enforce host-key policy, timeout bounds and auth
  presence.
* The JSON-Schema published by `schema()` and the Rust deserialiser disagree on
  whether `auth` is required (`required: ["…","auth"]` vs
  `#[serde(default)] auth: AuthMethod`).

Because the WASM tool is the *only* contract the agent sees, every gateway
constraint (timeout 1-3600, host-key-or-insecure, no body > N MB, gateway must
be reachable) needs to be representable in the schema **or** detectable via a
dedicated probe. The `health` action is the simplest fix for "is the gateway
running".

### 2.2 SSH execution semantics (B1, B9, B10, B11, B15)

`russh` exposes a low-level channel API. The current implementation is a
straight read loop on `channel.wait()`. Three things bite:

1. `exec(true, cmd)` requests a reply (`channel-success` / `channel-failure`).
   We don't actually use it; setting `false` removes one round-trip and a
   subtle hang on broken servers.
2. We never call `channel.eof()` after `exec`, so the remote `cat -` (and
   anything else that reads stdin) waits forever and we report "timed out"
   even though the command is well-behaved.
3. Hash algorithm `None` for `PrivateKeyWithHashAlg` forces ssh-rsa-sha1, which
   modern servers reject. The default fingerprint check passed; auth then
   fails with a misleading "credentials rejected".

### 2.3 Resource & DoS (B3, B4, B13)

* No request-body limit lets a single malicious POST fill the gateway's
  memory. axum 0.8 ships `DefaultBodyLimit::max(N)` for exactly this case.
* `reap_expired_sessions` runs only at the head of `handle_connect`. A long-
  running tenant that never reconnects accumulates dead sessions, exhausting
  the `max_sessions` budget.
* Listening on `0.0.0.0` with no token turns the gateway into an open SSH
  proxy. We have a warning but no policy.

### 2.4 Skills integration (Are skills sufficient?)

The native IronClaw skills surveyed (`coding`, `local-test`,
`code-review`, etc.) talk about the **local** `shell` tool. None of them know
about `remote_shell`. The gap is **not** in the extension — the extension's
schema and capabilities are well-formed — but the agent has no skill telling
it:

* When to prefer `remote_shell.execute` over a local `shell` tool that doesn't
  know about the remote.
* The connect → execute → disconnect lifecycle (don't reconnect every call;
  reuse `session_id`; call `list_sessions` first).
* Security guidance (always pass `host_key_fingerprint`, never use
  `insecure_ignore_host_key` outside lab networks, prefer `private_key` over
  `password`).
* Recovery guidance ("if you get `Gateway request failed`, run a `health`
  probe; if `Session 'X' has expired`, call connect again").

So the answer to *"can our shell execution really be used by the agent?"* is
**yes, but not reliably without skill guidance and the bug fixes above**.
The execute path itself works for typical commands, but B1 (stdin EOF) and
B15 (RSA SHA-1) make it unusable for two very common cases (`cat … | sh`
patterns, and modern OpenSSH RSA keys).

## 3. Affected components

| File | Issues |
|---|---|
| `./remote-shell/src/lib.rs` | B2, B6, B7, B8, B12, B14, B16, B18, B19 |
| `./remote-shell-gateway/src/main.rs` | B1, B3, B4, B5, B9, B10, B11, B13, B14, B15, B17 |
| `./remote-shell/remote-shell.capabilities.json` | B6, B12 (advertise the new `health` action; add `setup` doc to instruct skill installation) |
| Tests in both crates | B20 |
| New (skill guidance) | A `remote-shell` skill markdown to ship alongside the extension so IronClaw can load it (override / augment `coding`). |

## 4. Proposed solution

### 4.1 Code fixes (binding for the Implementation step)

1. **Drop `#[serde(default)]` on `AuthMethod`** in both crates and remove the
   `Default for AuthMethod` impls. Make `auth` required at the deserialisation
   layer too, returning a clean validation error instead of "auth failed". (B2,
   B18)
2. **WASM-side host-key gate**: in `RemoteShellAction::Connect`, return
   `Err("Either host_key_fingerprint or insecure_ignore_host_key must be set")`
   before calling the gateway. (B6)
3. **WASM-side timeout bounds**: enforce `MIN_TIMEOUT_SECS = 1`,
   `MAX_TIMEOUT_SECS = 3600` and use `saturating_add` / `saturating_mul` for
   the HTTP timeout calculation. (B7)
4. **WASM-side hostname rules**: extract `MAX_HOSTNAME_LENGTH = 253` constant,
   reject NUL/tab, require at least one non-whitespace byte. (B8, B14)
5. **Add a `health` action** (`GET /health`) to the WASM tool, surfaced in
   schema and description, so the agent has a one-call probe. Treat
   non-200 as "gateway unavailable, ask the user to start the service". (B12)
6. **Use `validate_input_length` consistently** for username, session_id, and
   key_pem (the latter via a higher cap, e.g. 256 KiB, since real OpenSSH
   keys are around 2 KiB but encrypted ones can be larger). (B19)
7. **Gateway: body size limit** —
   `Router::new().layer(DefaultBodyLimit::max(2 * 1024 * 1024))` for normal
   routes, with a higher cap (or per-route override) on `/connect` for
   private keys (e.g. 256 KiB). (B3)
8. **Gateway: stdin EOF** — after `channel.exec(false, cmd.as_bytes()).await`,
   call `channel.eof().await`. Switch `want_reply` to `false`. (B1, B9, B10)
9. **Gateway: close channel on timeout** — `let _ = channel.close().await;`
   in the timeout/truncated branches. (B11)
10. **Gateway: hash-alg fallback** — try `HashAlg::Sha512`, then `Sha256`,
    then `None` (ssh-rsa). Use the first that authenticates successfully.
    Apply only to RSA keys; ED25519/ECDSA pass `None`. (B15)
11. **Gateway: reap on every action** — call `reap_expired_sessions(&state)`
    at the top of `handle_list_sessions`, `handle_execute`, and
    `handle_disconnect`. (B4)
12. **Gateway: redact command in logs** — log `command_len = req.command.len()`
    instead of the raw command at debug, or guard behind a
    `RUST_LOG=ironclaw_gateway=trace` opt-in. (B5)
13. **Gateway: refuse `0.0.0.0` without token** — exit with a fatal error in
    `main()` if `cli.host` is non-loopback and `bearer_token.is_none()`.
    (B13)
14. **Pretty-print empty execute output** — collapse the
    "--- stdout ---\n(empty)\n--- stderr ---\n(empty)" block into
    `"(no output)"`. (B16)
15. **Constants cleanup** — introduce `MAX_HOSTNAME_LENGTH`,
    `MIN_TIMEOUT_SECS`, `MAX_TIMEOUT_SECS`, `MAX_OUTPUT_BYTES` etc. in the
    WASM crate so the two crates use the same names. (B14)

### 4.2 Tests to add (B20)

* `gateway::tests::execute_rejects_invalid_timeout` — exercise the bounds.
* `gateway::tests::body_limit_rejects_oversized_connect` — uses
  `tower::ServiceExt::oneshot` against the router with a giant body.
* `gateway::tests::expired_session_is_reaped_on_list` — fast-forward via a
  configurable clock or short TTL.
* `gateway::tests::host_key_fingerprint_required` — connect without either
  field returns 400.
* `wasm::tests::connect_without_host_key_or_insecure_fails_locally` — make
  sure the WASM tool errors before any HTTP call.
* `wasm::tests::execute_clamps_timeout` — invalid timeout returns the WASM
  validation error.

### 4.3 Skill enhancements

Ship a new file `./skills/remote-shell/SKILL.md` (and document its install
location in the README) that the IronClaw skills loader can pick up. Suggested
front-matter:

```yaml
---
name: remote-shell
version: 0.1.0
description: Best practices for the remote_shell tool (SSH via local gateway).
activation:
  keywords: ["ssh", "remote", "server", "deploy", "production", "remote shell", "scp"]
  patterns:
    - "(?i)(ssh|connect)\\s+(to|into)\\s+\\S+"
    - "(?i)run\\s+.+\\s+on\\s+(server|host|remote|prod|staging)"
  tags: ["ops", "remote"]
  max_context_tokens: 1500
---
```

Body must cover:

* **Lifecycle**: `list_sessions` → reuse existing → otherwise `connect` →
  many `execute` → `disconnect` when finished.
* **Security**: prefer `private_key`; always pass `host_key_fingerprint`;
  never log the credentials; never use `insecure_ignore_host_key` in prod.
* **Failure recovery**: on `Gateway request failed`, call the new `health`
  action; on `Session not found / expired`, reconnect.
* **Long commands**: bump `timeout_secs`; remember stdout / stderr are
  capped at 10 MB.
* **Local vs remote**: when the user is in their local repo, prefer the
  IronClaw built-in `shell`. Only use `remote_shell` when the host is *not*
  the local machine.

This complements (does not replace) the native `coding` skill — `coding`
keeps its rules; `remote-shell` adds the SSH-specific overlay. No native
skill needs to be wholesale overridden; only `coding` could optionally
reference `remote_shell` as the "remote" counterpart of `shell`.

### 4.4 Out of scope (intentionally not changing)

* Adding a persistent session store. Sessions are intentionally in-memory.
* Adding interactive PTY / streaming output. The current request/response
  model is sufficient for the agent's command-and-result loop.
* Replacing `russh` with an `ssh2`/`libssh2` based implementation.

## 5. Edge cases & side effects

* Switching `auth` from optional to required is a **breaking change** for any
  caller that today omits the field; given the only caller is the LLM tool
  description (which always supplies `auth`), the impact is acceptable. The
  WASM tool will now return a clear error rather than reaching the gateway.
* Adding `channel.eof()` is safe for commands that don't read stdin
  (`uname`, `df`, etc.). They simply ignore the EOF.
* `DefaultBodyLimit::max` returns `413 Payload Too Large` automatically; the
  WASM tool already surfaces non-2xx status as a `Gateway error (HTTP 413): …`
  string, so no extra wiring needed.
* RSA SHA-2 fallback: trying Sha512 first then Sha256 doubles the auth round
  trips for OpenSSH < 7.2 servers, which is acceptable (single-digit ms).
* Refusing `0.0.0.0` without a token is a behavioural change: anyone
  intentionally exposing the gateway must now set `SSH_GATEWAY_TOKEN`. This
  is the intended outcome.
* Reaping on every read path slightly increases `RwLock` write contention,
  but only when sessions are *expired* (write path is taken only then).
  Hot read path is unaffected.

## 6. Implementation notes

All 20 findings (B1–B20) and the skill-enhancement recommendation were
implemented. Summary of changes:

### 6.1 WASM tool (`./remote-shell/src/lib.rs`)

- Removed `#[serde(default)]` on `auth` and the `Default for AuthMethod`
  impl (B2, B18). Auth is now required at the deserialiser level.
- Introduced constants `MAX_HOSTNAME_LENGTH = 253`,
  `MAX_KEY_PEM_LENGTH = 256 KiB`, `MIN_TIMEOUT_SECS = 1`,
  `MAX_TIMEOUT_SECS = 3600` (B14).
- Added `validate_non_empty`, `validate_timeout_secs`; tightened
  `validate_hostname` to reject NUL, tab, whitespace-only inputs (B8).
- Added a WASM-side host-key gate that returns a clean error before any
  HTTP call (B6).
- Used `saturating_add`/`saturating_mul` and clamped to `u32::MAX` when
  computing the HTTP execute timeout (B7).
- Added a new `Health` action mapped to `GET /health` (B12), surfaced in
  the JSON-Schema, description, and capabilities discovery summary.
- Used `validate_input_length` consistently for username, session_id,
  passphrase, and key_pem (B19).
- Collapsed empty stdout+stderr into a single `(no output)` line (B16).
- Schema now declares `minimum`/`maximum` on `timeout_secs` and lists the
  `health` action.

### 6.2 Gateway (`./remote-shell-gateway/src/main.rs`)

- Removed `Default for AuthMethod` (B2/B18).
- Introduced `MAX_BODY_BYTES = 1 MiB` for normal routes and
  `MAX_CONNECT_BODY_BYTES = 512 KiB` for `/connect`, applied via
  `DefaultBodyLimit::max(...)` per route (B3).
- Switched `channel.exec(true, …)` to `false`, called `channel.eof()`
  after exec, and `channel.close()` on timeout / exec error (B1, B9, B10,
  B11).
- Added `authenticate_with_hash_fallback` that tries `HashAlg::Sha512 →
  Sha256 → None` for RSA private keys (B15). Resolved a tricky
  `russh::Error` vs `anyhow::Error` arm-type mismatch by annotating
  `Result<bool, anyhow::Error>` and `.map_err(anyhow::Error::from)` on
  the password branch.
- Called `reap_expired_sessions` at the head of `handle_execute`,
  `handle_disconnect`, and `handle_list_sessions` (B4).
- Replaced the raw debug log of `command` with
  `command_preview = truncate_for_log(.., 64)` and `command_len`
  fields (B5).
- Added `is_loopback_host` and refused to bind on a non-loopback host
  without a `SSH_GATEWAY_TOKEN` in `main()` (B13).
- Validated `--max-sessions > 0` and `--session-ttl-secs > 0`.

### 6.3 Capabilities & skill

- Updated `./remote-shell/remote-shell.capabilities.json` to advertise
  the `health` action and document session reuse and the 10 MB output
  cap.
- Added `./skills/remote-shell/SKILL.md` with full activation
  front-matter (keywords, exclude_keywords, regex patterns, tags,
  `max_context_tokens`), tool-selection guidance, the
  health → list_sessions → connect → execute → disconnect lifecycle, a
  failure-mode recovery table, security non-negotiables, and don'ts.
  No native skill was wholly overridden; the new skill complements
  `coding` for the SSH overlay.

### 6.4 Tests added (B20)

WASM tool (`cargo test -p remote-shell`, **29 passed, 0 failed**):
- `test_connect_without_host_key_or_insecure_fails_locally`
- `test_connect_requires_auth_field`
- `test_execute_rejects_invalid_timeout`
- `test_format_execute_response_collapses_empty`
- `test_validate_hostname` (extended with NUL/tab/whitespace-only cases)
- `test_validate_timeout_secs`
- `test_http_execute_timeout_scales_with_command_timeout`
- `test_schema_has_all_actions` (now includes `health`)

Gateway (`cargo test -p remote-shell-gateway`, **10 passed, 0 failed**):
- `test_connect_request_requires_auth` (regression for B2)
- `test_is_loopback_host` (regression for B13)
- `test_truncate_for_log` (regression for B5)
- Existing serde-shape tests retained.

Removed obsolete tests `test_default_auth_method` and the
`(empty)`-block assertion inside `test_format_execute_response_with_output`.

### 6.5 Verification

- `cargo test -p remote-shell` — 29 passed, 0 failed.
- `cargo test -p remote-shell-gateway` — 10 passed, 0 failed.
- `cargo build --release --target wasm32-wasip2 -p remote-shell` —
  succeeds.
- `cargo build --release -p remote-shell-gateway` — succeeds.
- `cargo clippy --workspace` — only one pre-existing
  `collapsible_match` warning in untouched output-loop code; no new lint
  regressions.

### 6.6 Deviations from plan

- The capabilities JSON's `setup` block was kept; no separate skill
  install instructions were added there because the skill ships under
  `./skills/remote-shell/SKILL.md` and is loaded by the IronClaw skills
  loader directly.
- For RSA hash fallback, kept the order `Sha512 → Sha256 → None`. ED25519
  / ECDSA paths skip the fallback (the loop terminates on `Ok(true)` at
  the first iteration).
- Body limits are applied via `DefaultBodyLimit` per route rather than
  globally to keep `/connect`'s 512 KiB cap distinct from the 1 MiB
  cap for `/execute`.

### 6.7 Post-review refinements

Cross-review surfaced four issues; all addressed:

- **SKILL.md HTTP 410 row removed.** The gateway no longer emits 410
  (reaping happens at the head of every handler, so expired sessions
  surface as `404`). Merged the two rows into a single `404` entry with
  TTL-elapsed wording.
- **Timeout test now exercises real production code.** Extracted
  `compute_http_execute_timeout_ms` as a free function in
  `./remote-shell/src/lib.rs`; the production call site and the tests
  both call it. Added
  `test_http_execute_timeout_saturates_on_overflow`
  (`u64::MAX - 1`) and `test_http_execute_timeout_saturates_at_max_timeout_secs`
  to actually exercise the `saturating_*` path.
- **`authenticate_with_hash_fallback` short-circuits for non-RSA keys.**
  Now branches on `key_pair.algorithm().is_rsa()`; ED25519 / ECDSA take
  the single-iteration `&[None]` path, RSA gets the
  `Sha512 → Sha256 → None` fallback. Removes spurious debug noise.
- **`is_loopback_host` is case-insensitive.** Switched
  `matches!(host, "localhost")` to
  `host.eq_ignore_ascii_case("localhost")`; tests cover `LOCALHOST` /
  `Localhost`.

Final test results: `cargo test -p remote-shell` 31 passed,
`cargo test -p remote-shell-gateway` 10 passed, release builds for both
crates succeed.
