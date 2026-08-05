#!/bin/bash
# build.sh - 构建 Rust 版本

set -e

print_color() {
    echo -e "\033[0;34m${1}\033[0m"
}

print_color "🚀 构建 Git AI Commit (Rust 版本)"

# 检查 Rust
if ! command -v cargo &> /dev/null; then
    print_color "❌ 未找到 Rust，请先安装: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

# 清理旧的构建
print_color "🧹 清理旧的构建..."
cargo clean

# 构建
print_color "📦 编译 (Release 模式)..."
cargo build --release

# 检查构建结果
if [ -f "target/release/git-ai-commit" ]; then
    print_color "✅ 构建成功！"
    print_color "📁 二进制文件: target/release/git-ai-commit"
    
    # 显示文件大小
    if [[ "$OSTYPE" == "darwin"* ]]; then
        SIZE=$(stat -f%z target/release/git-ai-commit)
    else
        SIZE=$(stat -c%s target/release/git-ai-commit)
    fi
    SIZE_MB=$((SIZE / 1024 / 1024))
    print_color "📊 文件大小: ${SIZE_MB}MB"
else
    print_color "❌ 构建失败"
    exit 1
fi
