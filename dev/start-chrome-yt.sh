#!/usr/bin/env bash
# Start Chrome with YouTube session for yt-dlp cookies.
# Keeps Chrome running with Xvfb for --cookies-from-browser to work.
#
# Usage: ./dev/start-chrome-yt.sh
# Check:  curl -s http://localhost:9222/json/version
# Login:  ssh -L 9222:localhost:9222 spex@server, then open http://localhost:9222

set -euo pipefail

CHROME_PROFILE="/home/spex/.config/google-chrome-yt"
DISPLAY_NUM=42
NODE_PATH="/home/spex/.nvm/versions/node/v22.22.0/bin/node"

# Check if already running
if curl -s http://localhost:9222/json/version &>/dev/null; then
    echo "Chrome already running on :9222"
    exit 0
fi

# Kill stale processes
pkill -f "Xvfb :${DISPLAY_NUM}" 2>/dev/null || true
pkill -f "chrome.*${CHROME_PROFILE}" 2>/dev/null || true
sleep 1

# Start virtual display
Xvfb :${DISPLAY_NUM} -screen 0 1280x1024x24 -ac &
sleep 1

if ! pgrep -f "Xvfb :${DISPLAY_NUM}" &>/dev/null; then
    echo "ERROR: Xvfb failed to start" >&2
    exit 1
fi

# Start Chrome
DISPLAY=:${DISPLAY_NUM} google-chrome \
    --no-sandbox --disable-gpu --no-first-run --no-default-browser-check \
    --remote-debugging-port=9222 \
    --user-data-dir="${CHROME_PROFILE}" \
    "https://www.youtube.com" &>/tmp/chrome-yt.log &

sleep 4

if curl -s http://localhost:9222/json/version &>/dev/null; then
    echo "Chrome started on :9222 (profile: ${CHROME_PROFILE})"
    echo "To re-login: ssh -L 9222:localhost:9222 spex@server"
else
    echo "ERROR: Chrome failed to start. Check /tmp/chrome-yt.log" >&2
    tail -5 /tmp/chrome-yt.log >&2
    exit 1
fi
