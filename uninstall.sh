#!/usr/bin/env bash
set -euo pipefail

# Path to the installed binary (default used in install.sh)
BIN_PATH="/usr/local/bin/git-ai-commit"
# Configuration directory created by install.sh
CONFIG_DIR="$HOME/.git-ai-commit"

echo "Removing git‑ai‑commit binary..."
if [ -f "$BIN_PATH" ]; then
  rm -f "$BIN_PATH"
  echo "✓ Removed $BIN_PATH"
else
  echo "⚠️ Binary not found at $BIN_PATH"
fi

echo "Removing configuration directory..."
if [ -d "$CONFIG_DIR" ]; then
  rm -rf "$CONFIG_DIR"
  echo "✓ Removed $CONFIG_DIR"
else
  echo "⚠️ Config directory not found at $CONFIG_DIR"
fi

echo "Uninstall complete."
