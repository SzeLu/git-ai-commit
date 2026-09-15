# 简体中文（zh-CN）—— git-ai-commit 的界面文案。
#
# 每个键对应 src/ 里的一处硬编码字面量；值就是那句话本身，
# 因此**不要**顺手改动标点、全角/半角或空格——终端输出要和以前一致。
#
# 值以 {"..."} 开头的地方，是原文里的前导空格（提示行的缩进），
# 必须留着：Fluent 会把消息值行首的空白吃掉，只能用字符串字面量写出来。
# 同理，值末尾的 {" "} 是原文结尾的那个空格。

# main.rs —— 启动、配置、进度与提交前的提示
config_model_required = ❌ 配置中没有可用的模型，需手动输入模型信息
model_input_prompt = 请输入模型名称 (e.g., deepseek-chat):
model_name_validation = ❌ 模型名称不能为空。终止操作。
base_url_input_prompt = 请输入模型 API URL:
base_url_validation = ❌ 模型 API URL 不能为空。终止操作。
api_token_input_prompt = 请输入 API Token:
api_token_validation = ❌ API Token 不能为空。终止操作。
save_config_error = ❌ 保存配置失败: { $error }
no_active_model_error = 配置中没有可用的模型
current_model_label = 📌 当前使用的大模型: { $model }
not_git_repo = ❌ 当前目录不是 Git 仓库
analyzing_changes = 📊 分析代码变更...
no_changes_detected = ❌ 没有检测到任何变更
staged_changes_required = ❌ 没有检测到暂存的变更，请先使用 'git add' 添加文件
use_all_flag_hint = {"   "}或使用 --all 参数包含所有变更
generating_message = 🤖 正在分析变更...
dry_run_label = 📝 生成的 Commit 消息 (Dry Run)：
format_validation_passed = ✅ 格式验证通过
format_validation_failed = ⚠️  格式验证失败，请检查
config_auto_commit = ⚙️  config.json 中 auto_commit=true，跳过确认
strict_format_blocked = ❌ 生成的 commit 消息未通过格式校验（strict_format = true），已阻止提交
strict_format_blocked_hint = {"   "}可先用 --dry-run 预览，或在 config.json 中设置 strict_format=false
format_warning_strict = ⚠️  strict_format = true，但当前是交互模式，是否提交由你决定
format_warning_nonstrict = ⚠️  格式校验未通过（strict_format = false，仍可提交）
degraded_confirmation_required = ⚠️  本次为降级消息（模型未正常返回），强制走确认流程
short_diff_generating = ⚡ 变更规模适中，正在实时生成提交消息...
long_diff_warning = ⚠️  变更内容较长，正在采用“分块总结 -> 合并 -> 生成”机制进行处理...
chunk_summary = 第 { $index }/{ $total } 块摘要
chunk_progress = {"   "}[块 { $index }/{ $total }] 正在生成摘要...
chunk_complete = {" "}完成
chunk_degraded = {" "}⚠️ 已降级为本地结构化摘要
degradation_warning = ⚠️  有 { $count }/{ $total } 块使用了本地降级摘要，最终消息质量可能下降
block_details = {"   "}降级的块: { $blocks }
debug_hint = {"   "}提示: 加 --debug 查看完整原始响应，或在 config.json 中调大 max_tokens
final_message = ✍️  正在生成最终提交消息...
final_generation_failed = ⚠️  最终生成失败，改用本地兜底消息：{ $error }
# `chore:` 这个前缀不能翻译：commit.rs 的校验只认小写 type。
fallback_message = chore: 更新 { $count } 个文件

    { $body }（本条消息由本地降级逻辑生成：模型未返回内容）
# 原文这里还有一个用于排版的首/尾换行，不放进键值，仍留在调用处。
fallback_more_files = - …（其余 { $count } 个文件略）
unknown_error = 未知错误

# commit.rs —— 校验问题、确认横幅与提交结果
empty_message = 消息为空
invalid_header_format = 标题格式不正确: { $header } （期望 <type>(<scope>): <subject>）
subject_too_long = Subject 超过50字符: { $length }
missing_blank_line = 标题和 body 之间需要有空行
validation_issues = ⚠️  { $issue }
commit_confirmation = 🚀 自动提交...
commit_success = ✅ 提交成功！
commit_message_banner = 📝 生成的 Commit 消息 (Conventional + Body)：
# 原文这里还有一个用于排版的首/尾换行，不放进键值，仍留在调用处。
edit_message_prompt = 是否使用此消息提交？(y/n/e 编辑):{" "}
commit_cancelled = 已取消提交
editor_exit_error = 编辑器退出异常

# git.rs —— git 命令失败
git_diff_failed = 获取 Git diff 失败
git_status_failed = 获取 Git status 失败
git_diff_stats_failed = 获取 Git diff stats 失败
git_commit_failed = Git commit 失败
git_commit_failed_code = Git commit 失败，退出码: { $code }

# config.rs —— 配置文件路径
home_dir_error = 无法获取 home 目录

# ai.rs —— 模型调用失败、流诊断与统计报告
diagnosis_reasoning_truncated = 模型把全部 token 用在推理通道，max_tokens={ $limit } 在输出正文前就用尽了；请调大 config.json 的 max_tokens（或 summary_max_tokens）
diagnosis_output_truncated = 输出在 max_tokens={ $limit } 处被截断，内容可能不完整
diagnosis_reasoning_channel = 网关把内容放在 reasoning_content / reasoning 通道，模型只思考未作答；也可能是推理内容占满了输出预算
diagnosis_error_frame = 网关在流中返回了 error 对象
diagnosis_parse_failed = 所有帧都解析失败，网关返回的可能不是 SSE 格式
diagnosis_closed_early = 连接在 [DONE] 之前就断开了，响应可能被截断
report_endpoint = {"  "}端点: { $endpoint }   模型: { $model }
report_request = {"  "}请求: max_tokens={ $max_tokens } temperature={ $temperature }   HTTP { $status }
report_frames = {"  "}帧: 共 { $total }（data { $data } / 其他 { $heartbeat }）  解析失败 { $parse_failures }
report_content_frames = {"  "}含 content 的帧 { $content_frames }（{ $content_chars } 字）   含推理内容的帧 { $reasoning_frames }（{ $reasoning_chars } 字）
report_finish_reason = {"  "}finish_reason: { $reason }   [DONE]: { $done }
not_provided = 未提供
received = 已收到
not_received = 未收到
report_stream_error = {"  "}流内错误: { $error }
report_diagnosis = {"  "}诊断: { $diagnosis }
report_raw_frame = {"  "}原始片段 { $index }: { $frame }
error_no_content = { $what }失败：模型没有返回任何内容
    { $body }
no_content_error = { $body }
    {"  "}提示: 加 --debug 查看完整原始响应；或在 config.json 中调整 max_tokens
truncated_output_warning = ⚠️  { $what }的模型输出在 max_tokens={ $limit } 处被截断，内容可能不完整
http_client_error = 构建 HTTP 客户端失败
api_call_failed = 调用 大模型 API 失败
api_request_failed = API 请求失败 (HTTP { $status }): { $body }
stream_read_failed = 读取响应流失败
no_active_model_context = No active model configured. Please run with --model or set it in config.json
what_commit_message = 生成 commit 消息
