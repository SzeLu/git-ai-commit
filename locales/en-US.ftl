# English (en-US) — the base rung of the fallback chain in src/i18n.rs.
#
# Every key of zh-CN.ftl must exist here too: a user of any other locale
# falls through to this file, and a missing key shows up as the raw key
# name. Keep the two files' key sets identical.
#
# A value starting with {"..."} carries the leading spaces of the original
# prompt line, and a trailing {" "} is the original trailing space. Fluent
# strips leading/trailing whitespace from a value, so it is written as a
# string literal to keep the terminal output byte-identical.
#
# 注意：`hello` 是 src/i18n.rs 的测试固定键，重写本文件时必须保留。
hello = Hello, { $name }!

# main.rs — startup, config, progress and pre-commit prompts
config_model_required = ❌ No usable model in the config; please enter the model details manually
model_input_prompt = Enter the model name (e.g., deepseek-chat):
model_name_validation = ❌ Model name cannot be empty. Aborting.
base_url_input_prompt = Enter the model API URL:
base_url_validation = ❌ Model API URL cannot be empty. Aborting.
api_token_input_prompt = Enter the API token:
api_token_validation = ❌ API token cannot be empty. Aborting.
save_config_error = ❌ Failed to save config: { $error }
no_active_model_error = No usable model in the config
current_model_label = 📌 Using model: { $model }
not_git_repo = ❌ The current directory is not a Git repository
analyzing_changes = 📊 Analyzing code changes...
no_changes_detected = ❌ No changes detected
staged_changes_required = ❌ No staged changes detected; stage files with 'git add' first
use_all_flag_hint = {"   "}or pass --all to include all changes
generating_message = 🤖 Analyzing changes...
dry_run_label = 📝 Generated Commit Message (Dry Run):
format_validation_passed = ✅ Format validation passed
format_validation_failed = ⚠️  Format validation failed, please review
config_auto_commit = ⚙️  auto_commit=true in config.json, skipping confirmation
strict_format_blocked = ❌ Generated commit message failed format validation (strict_format = true); commit blocked
strict_format_blocked_hint = {"   "}Preview it with --dry-run, or set strict_format=false in config.json
format_warning_strict = ⚠️  strict_format = true, but this run is interactive; the decision to commit is yours
format_warning_nonstrict = ⚠️  Format validation failed (strict_format = false, commit still allowed)
degraded_confirmation_required = ⚠️  This message is degraded (the model did not return normally); confirmation is forced
short_diff_generating = ⚡ Diff is a manageable size; streaming the commit message...
long_diff_warning = ⚠️  The diff is large; using the "summarize chunks -> merge -> generate" pipeline...
chunk_summary = Chunk { $index }/{ $total } summary
chunk_progress = {"   "}[chunk { $index }/{ $total }] generating summary...
chunk_complete = {" "}done
chunk_degraded = {" "}⚠️ Fell back to a local structured summary
degradation_warning = ⚠️  { $count }/{ $total } chunks used local fallback summaries; the final message may be lower quality
block_details = {"   "}Degraded chunks: { $blocks }
debug_hint = {"   "}Hint: add --debug to see the full raw response, or raise max_tokens in config.json
final_message = ✍️  Generating the final commit message...
final_generation_failed = ⚠️  Final generation failed; falling back to a local message: { $error }
# Keep the `chore:` prefix untranslated: commit.rs validates the type.
fallback_message = chore: update { $count } file(s)

    { $body }(this message came from the local fallback: the model returned no content)
# The original literal also carried a layout newline at its edge;
# it stays at the call site and is not part of this value.
fallback_more_files = - …({ $count } more files omitted)
unknown_error = Unknown error

# commit.rs — validation problems, confirmation banner, commit result
empty_message = The commit message is empty
invalid_header_format = Invalid header format: { $header } (expected <type>(<scope>): <subject>)
subject_too_long = Subject exceeds 50 characters: { $length }
missing_blank_line = A blank line is required between the header and the body
validation_issues = ⚠️  { $issue }
commit_confirmation = 🚀 Committing automatically...
commit_success = ✅ Commit succeeded!
commit_message_banner = 📝 Generated Commit Message (Conventional + Body):
# The original literal also carried a layout newline at its edge;
# it stays at the call site and is not part of this value.
edit_message_prompt = Commit with this message? (y/n/e to edit):{" "}
commit_cancelled = Commit cancelled
editor_exit_error = The editor exited abnormally

# git.rs — failed git commands
git_diff_failed = Failed to get the Git diff
git_status_failed = Failed to get the Git status
git_diff_stats_failed = Failed to get the Git diff stats
git_commit_failed = Git commit failed
git_commit_failed_code = Git commit failed, exit code: { $code }

# config.rs — config file path
home_dir_error = Could not determine the home directory

# ai.rs — model call failures, stream diagnostics and the stats report
diagnosis_reasoning_truncated = The model spent every token on the reasoning channel; max_tokens={ $limit } ran out before any content was emitted. Raise max_tokens (or summary_max_tokens) in config.json
diagnosis_output_truncated = Output was truncated at max_tokens={ $limit }; the content may be incomplete
diagnosis_reasoning_channel = The gateway is delivering content on the reasoning_content / reasoning channel; the model only reasoned and never answered. The reasoning text may also have used up the output budget
diagnosis_error_frame = The gateway returned an error object inside the stream
diagnosis_parse_failed = Every frame failed to parse; the gateway may not be returning SSE
diagnosis_closed_early = The connection closed before [DONE]; the response may be truncated
report_endpoint = {"  "}Endpoint: { $endpoint }   Model: { $model }
report_request = {"  "}Request: max_tokens={ $max_tokens } temperature={ $temperature }   HTTP { $status }
report_frames = {"  "}Frames: { $total } total (data { $data } / other { $heartbeat })  parse failures { $parse_failures }
report_content_frames = {"  "}Frames with content { $content_frames } ({ $content_chars } chars)   frames with reasoning { $reasoning_frames } ({ $reasoning_chars } chars)
report_finish_reason = {"  "}finish_reason: { $reason }   [DONE]: { $done }
not_provided = not provided
received = received
not_received = not received
report_stream_error = {"  "}In-stream error: { $error }
report_diagnosis = {"  "}Diagnosis: { $diagnosis }
report_raw_frame = {"  "}Raw frame { $index }: { $frame }
error_no_content = { $what } failed: the model returned no content
    { $body }
no_content_error = { $body }
    {"  "}Hint: add --debug to see the full raw response, or adjust max_tokens in config.json
truncated_output_warning = ⚠️  { $what } was truncated at max_tokens={ $limit }; the content may be incomplete
http_client_error = Failed to build the HTTP client
api_call_failed = Failed to call the model API
api_request_failed = API request failed (HTTP { $status }): { $body }
stream_read_failed = Failed to read the response stream
no_active_model_context = No active model configured. Please run with --model or set it in config.json
what_commit_message = Commit message generation
