#!/usr/bin/env bash
set -euo pipefail

# ── TeleICU Gateway Demo Launcher ──────────────────────────────────
# Starts the gateway on plain HTTP and Caddy as a self-signed HTTPS
# reverse proxy in front of it. One command, zero config.
#
# Usage:
#   ./dev/demo.sh                    # HTTPS on :8443
#   DEMO_PORT=9443 ./dev/demo.sh     # HTTPS on :9443
#   GATEWAY_DEVICE_ID=ward-1 ./dev/demo.sh
# ────────────────────────────────────────────────────────────────────

DEMO_PORT="${DEMO_PORT:-8443}"
GATEWAY_PORT="${BIND_PORT:-8090}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORK_DIR="$PROJECT_DIR/.demo"

cleanup() {
    echo ""
    echo "── Shutting down ──"
    # Kill child processes
    if [[ -n "${CADDY_PID:-}" ]] && kill -0 "$CADDY_PID" 2>/dev/null; then
        echo "Stopping Caddy (PID $CADDY_PID)"
        kill "$CADDY_PID" 2>/dev/null || true
    fi
    if [[ -n "${GATEWAY_PID:-}" ]] && kill -0 "$GATEWAY_PID" 2>/dev/null; then
        echo "Stopping gateway (PID $GATEWAY_PID)"
        kill "$GATEWAY_PID" 2>/dev/null || true
    fi
    wait 2>/dev/null || true
    echo "Done."
}
trap cleanup EXIT INT TERM

# ── Preflight checks ──────────────────────────────────────────────

if ! command -v caddy &>/dev/null; then
    echo "Error: 'caddy' not found in PATH."
    echo "Run this via:  nix develop .#demo -c ./dev/demo.sh"
    echo "   or:         nix run .#demo"
    exit 1
fi

# Build gateway if needed
GATEWAY_BIN="$PROJECT_DIR/target/release/teleicu-gateway"
if [[ ! -x "$GATEWAY_BIN" ]]; then
    echo "── Building gateway (release) ──"
    (cd "$PROJECT_DIR" && cargo build --release)
fi

# ── Prepare working directory ─────────────────────────────────────

mkdir -p "$WORK_DIR/caddy_data" "$WORK_DIR/caddy_config"

# Generate Caddyfile from template
CADDYFILE="$WORK_DIR/Caddyfile"
sed -e "s/{{DEMO_PORT}}/$DEMO_PORT/g" -e "s/{{GATEWAY_PORT}}/$GATEWAY_PORT/g" "$SCRIPT_DIR/Caddyfile" > "$CADDYFILE"

# Ensure a .env exists (gateway needs at least GATEWAY_DEVICE_ID)
if [[ ! -f "$PROJECT_DIR/.env" ]] && [[ -z "${GATEWAY_DEVICE_ID:-}" ]]; then
    echo "Warning: No .env file and GATEWAY_DEVICE_ID not set."
    echo "         Using default: GATEWAY_DEVICE_ID=demo-gateway"
    export GATEWAY_DEVICE_ID="demo-gateway"
fi

# ── Start gateway ─────────────────────────────────────────────────

echo "── Starting gateway on http://localhost:$GATEWAY_PORT ──"

export BIND_HOST="${BIND_HOST:-127.0.0.1}"
export BIND_PORT="$GATEWAY_PORT"

(cd "$PROJECT_DIR" && exec "$GATEWAY_BIN") &
GATEWAY_PID=$!

# Wait for gateway to be ready
echo -n "Waiting for gateway"
for i in $(seq 1 30); do
    if curl -sf "http://127.0.0.1:$GATEWAY_PORT/health/ping" >/dev/null 2>&1; then
        echo " ready!"
        break
    fi
    if ! kill -0 "$GATEWAY_PID" 2>/dev/null; then
        echo ""
        echo "Error: gateway exited unexpectedly."
        exit 1
    fi
    echo -n "."
    sleep 0.5
done

if ! curl -sf "http://127.0.0.1:$GATEWAY_PORT/health/ping" >/dev/null 2>&1; then
    echo ""
    echo "Error: gateway did not become ready in time."
    exit 1
fi

# ── Start Caddy ───────────────────────────────────────────────────

echo "── Starting Caddy HTTPS proxy on https://localhost:$DEMO_PORT ──"

XDG_DATA_HOME="$WORK_DIR/caddy_data" \
XDG_CONFIG_HOME="$WORK_DIR/caddy_config" \
    caddy run --config "$CADDYFILE" --adapter caddyfile &
CADDY_PID=$!

sleep 1

if ! kill -0 "$CADDY_PID" 2>/dev/null; then
    echo "Error: Caddy failed to start."
    exit 1
fi

# ── Ready ─────────────────────────────────────────────────────────

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  TeleICU Gateway Demo                                      ║"
echo "╠══════════════════════════════════════════════════════════════╣"
echo "║                                                            ║"
echo "║  HTTPS : https://localhost:$DEMO_PORT                         ║"
echo "║  HTTP  : http://localhost:$GATEWAY_PORT  (direct, no TLS)       ║"
echo "║                                                            ║"
echo "║  Health: https://localhost:$DEMO_PORT/health/status            ║"
echo "║  Ping  : https://localhost:$DEMO_PORT/health/ping              ║"
echo "║                                                            ║"
echo "║  ⚠  Self-signed cert — browser will warn, accept to proceed║"
echo "║  Press Ctrl+C to stop                                      ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Wait for either process to exit
wait -n "$GATEWAY_PID" "$CADDY_PID" 2>/dev/null || true
