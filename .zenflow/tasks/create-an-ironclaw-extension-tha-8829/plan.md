# IronClaw Remote Shell Extension

## Architecture

Two-component extension following ironclaw's WASM tool pattern:

1. **remote-shell** (WASM tool) — LLM-facing interface implementing ironclaw's WIT `sandboxed-tool` world. Provides actions: `connect`, `execute`, `disconnect`, `list_sessions`, `upload`, `download`. Communicates with the gateway over HTTP.

2. **remote-shell-gateway** — Lightweight HTTP service managing SSH sessions via `russh` crate. Runs locally, exposes REST API on a configurable port. Manages SSH connection lifecycle, command execution, and file transfers.

## Key Decisions
- **WASM + HTTP gateway pattern**: WASM sandbox only supports HTTP; SSH requires TCP. Gateway bridges the gap (same pattern as all ironclaw WASM tools).
- **`russh` for SSH**: Pure-Rust async SSH implementation. No external `ssh` binary dependency.
- **Session-based API**: Gateway maintains named sessions to allow connection reuse across tool invocations.
- **Credentials via ironclaw secrets**: SSH keys/passwords stored in ironclaw secret store, injected by host at HTTP boundary.

## Files
- `Cargo.toml` — workspace root
- `wit/tool.wit` — WIT interface (from ironclaw)
- `remote-shell/Cargo.toml`, `remote-shell/src/lib.rs` — WASM tool
- `remote-shell/remote-shell.capabilities.json` — capabilities declaration
- `remote-shell-gateway/Cargo.toml`, `remote-shell-gateway/src/main.rs` — SSH gateway service
- `.gitignore`

### [x] Step: Investigation
- Read ironclaw documentation, understand WASM tool architecture, WIT interface, capabilities.json format, and extension patterns.

### [x] Step: Project scaffolding
- Create workspace Cargo.toml, .gitignore, copy wit/tool.wit, set up crate structure.

### [x] Step: Implement SSH gateway service
- Create `remote-shell-gateway` crate with REST API endpoints for SSH session management and command execution using `russh`.

### [x] Step: Implement WASM tool
- Create `remote-shell` WASM tool crate implementing the `sandboxed-tool` WIT interface with action-based API matching ironclaw patterns. Include capabilities.json.

### [x] Step: Build verification and tests
- Verify both crates compile, run tests, check clippy/fmt.
- Gateway builds with russh 0.49 + russh-keys 0.49
- WASM tool builds targeting wasm32-wasip2
- 20 tests pass (14 WASM tool + 6 gateway)
- cargo clippy clean (no warnings)
- cargo fmt applied

### [x] Step: Security hardening
- Host key verification: require `host_key_fingerprint` or explicit `insecure_ignore_host_key: true` (issue #1)
- Bearer token auth middleware on all protected endpoints via `SSH_GATEWAY_TOKEN` env var (issue #2)
- Exit code capture via `channel.wait()` + `ChannelMsg::ExitStatus` instead of `into_stream()` (issue #3)
- stderr capture via `ChannelMsg::ExtendedData` (issue #4)
- Lock not held during command execution: sessions wrapped in `Arc`, cloned out before I/O (issue #5)
- Session collision rejected with HTTP 409 Conflict (issue #6)
- Removed redundant pre-flight health checks from WASM tool (issue #7)
- Session TTL + max session count enforced (issue #8)
- 22 tests pass (14 WASM tool + 8 gateway), clippy clean, fmt clean

### [x] Step: Codebase audit and fixes
- BUG: Fixed Eof breaking channel read loop before ExitStatus arrives (SSH sends Eof→ExitStatus→Close)
- BUG: Fixed reap_expired_sessions dropping sessions without calling disconnect() — leaked remote SSH connections
- BUG: Fixed handle_disconnect holding write lock during async disconnect I/O — blocked all concurrent operations
- SECURITY: Added 10MB output buffer cap on stdout/stderr to prevent OOM from large command output
- SECURITY: Added gateway-side input validation (host, username, session_id, command lengths; timeout bounds 1-3600s)
- Removed unused dependencies: base64, thiserror
- Extracted all magic numbers to named constants (SSH config, HTTP timeout, buffer limits, port defaults)
- Removed unnecessary .clone() on expected_fingerprint
- 24 tests pass (16 WASM tool + 8 gateway), clippy clean, fmt clean
