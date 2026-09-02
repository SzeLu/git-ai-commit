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
  # Prompt user whether to delete configuration directory
  read -r -p "是否删除配置文件？[y/N] " choice
enum=0
  case "$choice" in
    [yY][eE][sS]|[yY])
      rm -rf "$CONFIG_DIR"
      echo "✓ Removed $CONFIG_DIR"
      ;;
    *)
      echo "⚠️  保留配置目录 $CONFIG_DIR"
      ;;
  esac
else
  echo "⚠️ Config directory not found at $CONFIG_DIR"
fi

echo "Uninstall complete."
