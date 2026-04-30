#!/usr/bin/env bash
set -euo pipefail

GATEWAY_BIN_NAME="remote-shell-gateway"
GATEWAY_INSTALL_PATH="/usr/local/bin/${GATEWAY_BIN_NAME}"
TOOL_NAME="remote-shell"
WASM_TARGET="wasm32-wasip2"
SKILL_DIR="${HOME}/.ironclaw/skills/remote-shell"
SERVICE_NAME="remote-shell-gateway"
SERVICE_FILE="${HOME}/.config/systemd/user/${SERVICE_NAME}.service"

info()  { printf '\033[1;34m[INFO]\033[0m  %s\n' "$*"; }
warn()  { printf '\033[1;33m[WARN]\033[0m  %s\n' "$*"; }
err()   { printf '\033[1;31m[ERROR]\033[0m %s\n' "$*" >&2; }
ok()    { printf '\033[1;32m[OK]\033[0m    %s\n' "$*"; }

die() { err "$@"; exit 1; }

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "Required command not found: $1"
}

repo_root() {
    cd "$(dirname "$0")" && pwd
}

gateway_service_active() {
    systemctl --user is-active --quiet "${SERVICE_NAME}" 2>/dev/null
}

gateway_process_running() {
    pgrep -x "${GATEWAY_BIN_NAME}" >/dev/null 2>&1
}

stop_gateway() {
    if gateway_service_active; then
        info "Stopping systemd user service ${SERVICE_NAME}..."
        systemctl --user stop "${SERVICE_NAME}"
        ok "Service stopped."
        return 0
    fi

    if gateway_process_running; then
        info "Stopping running ${GATEWAY_BIN_NAME} process..."
        pkill -x "${GATEWAY_BIN_NAME}" || true
        sleep 1
        if gateway_process_running; then
            warn "Process did not stop gracefully, sending SIGKILL..."
            pkill -9 -x "${GATEWAY_BIN_NAME}" || true
            sleep 1
        fi
        ok "Process stopped."
        return 0
    fi

    info "No running gateway found."
}

start_gateway() {
    if gateway_service_active; then
        ok "Gateway service is already running."
        return 0
    fi

    if [ -f "${SERVICE_FILE}" ]; then
        info "Starting systemd user service ${SERVICE_NAME}..."
        systemctl --user daemon-reload
        systemctl --user start "${SERVICE_NAME}"
        ok "Service started."
    else
        info "No systemd service file found at ${SERVICE_FILE}."
        info "Start the gateway manually: ${GATEWAY_BIN_NAME}"
    fi
}

build_gateway() {
    info "Building gateway (release)..."
    cargo build --release -p "${GATEWAY_BIN_NAME}"
    ok "Gateway built: target/release/${GATEWAY_BIN_NAME}"
}

install_gateway() {
    local src="target/release/${GATEWAY_BIN_NAME}"
    [ -f "${src}" ] || die "Gateway binary not found at ${src}. Build first."

    if [ -f "${GATEWAY_INSTALL_PATH}" ]; then
        info "Updating gateway binary at ${GATEWAY_INSTALL_PATH}..."
    else
        info "Installing gateway binary to ${GATEWAY_INSTALL_PATH}..."
    fi

    sudo cp "${src}" "${GATEWAY_INSTALL_PATH}"
    sudo chmod +x "${GATEWAY_INSTALL_PATH}"
    ok "Gateway binary installed."
}

build_wasm_tool() {
    info "Building WASM tool (release, target ${WASM_TARGET})..."
    cargo build --release --target "${WASM_TARGET}" -p remote-shell
    ok "WASM tool built: target/${WASM_TARGET}/release/remote_shell.wasm"
}

install_wasm_tool() {
    local wasm="target/${WASM_TARGET}/release/remote_shell.wasm"
    local caps="remote-shell/remote-shell.capabilities.json"

    [ -f "${wasm}" ] || die "WASM binary not found at ${wasm}. Build first."
    [ -f "${caps}" ] || die "Capabilities file not found at ${caps}."

    info "Installing WASM tool '${TOOL_NAME}' into IronClaw..."
    ironclaw tool install \
        --name "${TOOL_NAME}" \
        --force \
        "${wasm}" \
        --capabilities "${caps}"
    ok "WASM tool installed."
}

install_skill() {
    info "Installing companion skill..."
    mkdir -p "${SKILL_DIR}"
    cp skills/remote-shell/SKILL.md "${SKILL_DIR}/SKILL.md"
    ok "Skill installed to ${SKILL_DIR}/SKILL.md"
}

print_summary() {
    echo ""
    echo "============================================"
    ok "Installation complete!"
    echo "============================================"
    echo ""
    echo "  Gateway binary : ${GATEWAY_INSTALL_PATH}"
    echo "  WASM tool      : ${TOOL_NAME} (ironclaw tool list)"
    echo "  Skill          : ${SKILL_DIR}/SKILL.md"
    echo ""
    if [ -f "${SERVICE_FILE}" ]; then
        echo "  Service status : systemctl --user status ${SERVICE_NAME}"
    else
        echo "  Start gateway  : ${GATEWAY_BIN_NAME}"
    fi
    echo ""
    echo "  Configure auth : ironclaw tool auth ${TOOL_NAME}"
    echo ""
}

main() {
    local root
    root="$(repo_root)"
    cd "${root}"

    info "Installing ironclaw-remote-shell-extension from ${root}"
    echo ""

    need_cmd cargo
    need_cmd rustup
    need_cmd ironclaw

    if ! rustup target list --installed | grep -q "${WASM_TARGET}"; then
        info "Adding WASM target ${WASM_TARGET}..."
        rustup target add "${WASM_TARGET}"
    fi

    stop_gateway

    build_gateway
    install_gateway

    build_wasm_tool
    install_wasm_tool

    install_skill

    start_gateway

    print_summary
}

main "$@"
