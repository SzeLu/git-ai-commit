#!/bin/bash
# Fix src/ai.rs

# 1. Add the model_config line before the prompt
# We'll use a temporary file to avoid issues.
sed -i '517i         let model_config = config.active_model().context("No active model configured. Please run with --model or set it in config.json")?;' src/ai.rs

# 2. Fix the stream_chat_completion call
# I'll use a more robust way by targeting the specific lines.
# Replace line 573-576 with correct version
# Actually, it's easier to use a single sed if I'm careful.
# But I'll just use the original file and fix it.
