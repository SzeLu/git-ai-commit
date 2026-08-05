#!/bin/bash
# test.sh - 测试 Rust 版本

set -e

print_color() {
    echo -e "\033[0;34m${1}\033[0m"
}

print_color "🧪 测试 Git AI Commit"

# 检查二进制文件
if [ ! -f "target/release/git-ai-commit" ]; then
    print_color "❌ 未找到二进制文件，请先运行 build.sh"
    exit 1
fi

# 测试版本
print_color "测试版本..."
./target/release/git-ai-commit --version

# 测试 Dry Run（如果在 Git 仓库中）
if git rev-parse --git-dir > /dev/null 2>&1; then
    print_color "测试 Dry Run..."
    ./target/release/git-ai-commit --dry-run || true
else
    print_color "⚠️  当前目录不是 Git 仓库，跳过功能测试"
fi

print_color "✅ 测试完成"
