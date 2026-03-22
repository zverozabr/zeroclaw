#!/usr/bin/env bash
# Restart zeroclaw daemon with all required env vars.
# Usage: ./dev/restart-daemon.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="$SCRIPT_DIR/.env"
BINARY="$SCRIPT_DIR/target/release/zeroclaw"
LOG="/tmp/zeroclaw_daemon.log"

if [[ ! -f "$ENV_FILE" ]]; then
    echo "ERROR: $ENV_FILE not found" >&2
    exit 1
fi

if [[ ! -x "$BINARY" ]]; then
    echo "ERROR: $BINARY not found or not executable. Run: cargo build --release" >&2
    exit 1
fi

# Source all env vars
set -a
source "$ENV_FILE"
set +a

# Kill existing daemon
OLD_PID=$(pgrep -f "zeroclaw daemon" 2>/dev/null | head -1 || true)
if [[ -n "$OLD_PID" ]]; then
    echo "Stopping old daemon (PID $OLD_PID)..."
    kill "$OLD_PID" 2>/dev/null || true
    sleep 2
    # Force kill if still alive
    kill -0 "$OLD_PID" 2>/dev/null && kill -9 "$OLD_PID" 2>/dev/null || true
fi

# Verify critical env vars
MISSING=""
for VAR in GOOGLE_API_KEY TELEGRAM_BOT_TOKEN TELEGRAM_OPERATOR_CHAT_ID TELEGRAM_API_ID TELEGRAM_API_HASH; do
    if [[ -z "${!VAR:-}" ]]; then
        MISSING="$MISSING $VAR"
    fi
done

if [[ -n "$MISSING" ]]; then
    echo "ERROR: Missing required env vars:$MISSING" >&2
    echo "Check $ENV_FILE" >&2
    exit 1
fi

# Start daemon
cd "$SCRIPT_DIR"
nohup "$BINARY" daemon >> "$LOG" 2>&1 &
NEW_PID=$!
sleep 2

if kill -0 "$NEW_PID" 2>/dev/null; then
    echo "Daemon started (PID $NEW_PID)"
    echo "Log: $LOG"
    # Quick health check
    ERRORS=$(tail -20 "$LOG" | grep -c "ERROR" || true)
    WARNS=$(tail -20 "$LOG" | grep -c "WARN" || true)
    LISTENING=$(tail -20 "$LOG" | grep -c "listening" || true)
    echo "Health: ${LISTENING} channels listening, ${WARNS} warnings, ${ERRORS} errors"
else
    echo "ERROR: Daemon failed to start. Check $LOG" >&2
    tail -20 "$LOG" >&2
    exit 1
fi
