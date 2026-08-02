#!/bin/bash
set -euo pipefail

echo "Starting web-agent..."
tmux new-session -d -s web-agent 'npx -y @mjswensen/web-agent --host 0.0.0.0 --port 6500'
