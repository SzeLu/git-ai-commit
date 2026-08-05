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

# 构建
print_color "📦 编译..."
cargo build --release

# 测试
print_color "🧪 测试..."
cargo test

# 打包
print_color "📦 打包..."
mkdir -p dist
cp target/release/git-ai-commit dist/
cp README.md dist/
cp install.sh dist/

print_color "✅ 构建完成！"
print_color "二进制文件: dist/git-ai-commit"
