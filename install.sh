#!/bin/bash
# install.sh - 安装 Rust 版本

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

print_color() {
    echo -e "${2}${1}${NC}"
}

print_color "🚀 安装 Git AI Commit (Rust 版本)" "$BLUE"

# 检查二进制文件
if [ ! -f "./git-ai-commit" ] && [ ! -f "./target/release/git-ai-commit" ]; then
    print_color "❌ 未找到二进制文件，请先运行 build.sh" "$RED"
    exit 1
fi

# 复制二进制文件
mkdir -p ~/.local/bin
if [ -f "./git-ai-commit" ]; then
    cp ./git-ai-commit ~/.local/bin/
else
    cp ./target/release/git-ai-commit ~/.local/bin/
fi
chmod +x ~/.local/bin/git-ai-commit

# 配置 PATH
if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    SHELL_CONFIG="$HOME/.zshrc"
    if [ ! -f "$SHELL_CONFIG" ]; then
        SHELL_CONFIG="$HOME/.bashrc"
    fi
    echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$SHELL_CONFIG"
    print_color "✅ PATH 已添加到 $SHELL_CONFIG" "$GREEN"
fi

export PATH="$HOME/.local/bin:$PATH"

# 配置 Git 别名
print_color "⚙️  配置 Git 别名..." "$YELLOW"
git config --global alias.aic '!git-ai-commit'
git config --global alias.aica '!git-ai-commit --auto'
git config --global alias.aicd '!git-ai-commit --dry-run'
git config --global alias.aicv '!git-ai-commit --version'

print_color "✅ Git 别名配置完成" "$GREEN"

# 设置 API Key
echo ""
print_color "🔑 设置 DeepSeek API Key" "$YELLOW"
print_color "获取 API Key: https://platform.deepseek.com/api_keys" "$BLUE"
echo ""

read -p "请输入 API Key (直接回车跳过): " api_key

if [ -n "$api_key" ]; then
    SHELL_CONFIG="$HOME/.zshrc"
    if [ ! -f "$SHELL_CONFIG" ]; then
        SHELL_CONFIG="$HOME/.bashrc"
    fi
    echo "export DEEPSEEK_API_KEY='$api_key'" >> "$SHELL_CONFIG"
    export DEEPSEEK_API_KEY="$api_key"
    print_color "✅ API Key 已设置" "$GREEN"
fi

print_color "🎉 安装完成！" "$GREEN"
echo ""
print_color "使用方法：" "$BLUE"
echo "  git aic   - 生成 Conventional + Body 格式的 commit 消息"
echo "  git aica  - 自动生成并提交"
echo "  git aicd  - 预览消息"
echo "  git aicv  - 查看版本"
