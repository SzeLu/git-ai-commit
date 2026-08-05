#!/bin/bash
# quick_install.sh - 快速安装

set -e

print_color() {
    echo -e "\033[0;34m${1}\033[0m"
}

print_color "🚀 快速安装 Git AI Commit (Rust 版本)"

# 构建
./build.sh

# 安装到 ~/.local/bin
mkdir -p ~/.local/bin
cp target/release/git-ai-commit ~/.local/bin/
chmod +x ~/.local/bin/git-ai-commit

# 配置 Git 别名
git config --global alias.aic '!git-ai-commit'
git config --global alias.aica '!git-ai-commit --auto'
git config --global alias.aicd '!git-ai-commit --dry-run'
git config --global alias.aicv '!git-ai-commit --version'

print_color "✅ 安装完成！"
print_color ""
print_color "使用方法："
print_color "  git aic   - 生成 commit 消息"
print_color "  git aica  - 自动提交"
print_color "  git aicd  - 预览消息"
print_color "  git aicv  - 查看版本"
