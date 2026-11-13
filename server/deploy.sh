#!/usr/bin/env bash
# Deploy the cross-compiled usb-mcp binary to the Pi and (re)start the service.
# Usage: ./deploy.sh [pi ssh target]   (default: pi-mcp.lan via the repo's .ssh/config)
set -euo pipefail
cd "$(dirname "$0")"

SSH_TARGET=${1:-pi-mcp.lan}
SSH_CONFIG=${SSH_CONFIG:-../.ssh/config}
SSH="ssh -F $SSH_CONFIG $SSH_TARGET"
SCP="scp -F $SSH_CONFIG"
BIN=target/arm-unknown-linux-musleabi/release/usb-mcp

[ -f "$BIN" ] || { echo "run ./build.sh first ($BIN missing)" >&2; exit 1; }

$SSH 'mkdir -p ~/usb-mcp'
$SCP "$BIN" "$SSH_TARGET":~/usb-mcp/usb-mcp.new
$SSH 'chmod +x ~/usb-mcp/usb-mcp.new'

# token directory for MCP_AUTH_TOKEN; the env file itself is never written by
# this script (an existing token must survive redeploys)
$SSH 'sudo install -d -m 700 /etc/usb-mcp'

# stop any stray nohup instance, then install the systemd unit.
# NOTE: `restart` (not `enable --now`): the Restart=always auto-restart after
# pkill can win the race against the mv below and relaunch the OLD inode —
# `--now` is a no-op on an already-active unit and would keep serving it.
$SSH 'pkill -x usb-mcp 2>/dev/null; true'
$SSH 'sudo tee /etc/systemd/system/usb-mcp.service > /dev/null' < deploy/usb-mcp.service
$SSH 'mv ~/usb-mcp/usb-mcp.new ~/usb-mcp/usb-mcp
sudo systemctl daemon-reload
sudo systemctl enable usb-mcp
sudo systemctl restart usb-mcp
sleep 1
systemctl is-active usb-mcp && ss -ltn | grep -E ":8765\b"
journalctl -u usb-mcp -n 3 --no-pager'
echo "deployed to $SSH_TARGET"
