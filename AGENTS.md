# AGENTS.md

## Branch layout (read this first)

- `main` is a documentation stub — only `README.md`, no code. Never look for sources there.
- All code lives in one of two independent implementations of the same tool (not a monorepo):
  - `base-on-rust` — **recommended** (per README). Rust CLI: `src/{main,git,ai,commit,config}.rs`, `build.sh` / `test.sh` / `install.sh`.
  - `base-on-python` — single-file `git_ai_commit.py` plus `setup.py` / `requirements.txt` / `build.sh` / `install.sh` / `uninstall.sh`.
- Do all work on the relevant implementation branch. The working tree may contain ignored build artifacts (`target/`, `.venv`) left over from other branches — they are untracked noise, not source.

## What the tool does

CLI that reads the git diff, sends it to the DeepSeek API, and generates a Chinese Conventional Commits message (`<type>(<scope>): <subject>` + body). It then commits, after interactive confirmation unless `--auto`. Single binary `git-ai-commit`; there are no library consumers and no unit tests anywhere in the repo.

## Rust branch commands

- Iterate with `cargo build` (debug). Do **not** use `./build.sh` for quick loops: it runs `cargo clean` first, and the release profile uses `lto = true, codegen-units = 1` (slow full rebuild).
- `./build.sh` → release binary at `target/release/git-ai-commit`.
- `./test.sh` is a smoke test only: `--version` plus `--dry-run ... || true`. The `|| true` means it passes even without `DEEPSEEK_API_KEY` — it gives false confidence; it is not real test coverage.
- Real functional check: `DEEPSEEK_API_KEY=... cargo run -- --dry-run` in a git repo with staged changes.
- `./install.sh` mutates global state (copies to `~/.local/bin`, sets global git aliases `aic` / `aica` / `aicd` / `aicv`). Never run it without an explicit user request.
- No linter/formatter/CI is configured; `cargo build` is the verification bar.

## Rust branch gotchas

- `src/config.rs` is dead code: `Config::load()` is never called. `~/.git-ai-commit/config.json` is not read at runtime; behavior is driven solely by CLI flags + the `DEEPSEEK_API_KEY` env var (default model `deepseek-chat`).
- The DeepSeek endpoint is hardcoded to `https://api.deepseek.com/chat/completions` in `src/ai.rs`.
- `--all` does not mean "everything": without it, the code runs `git diff --cached` (staged only); with it, plain `git diff` (unstaged only). Neither captures staged + unstaged together (`git diff HEAD` would).
- In `src/commit.rs`, the "subject ≤ 50 chars" validation uses byte length (`str::len`) — that is ~16 CJK characters, while the prompt asks for 50 Chinese chars, so the warning fires often. Mind this if you "fix" it.
- The interactive confirm prompt (`y`/`n`/`e`) reads stdin; without a TTY it gets EOF and silently cancels the commit. Use `--dry-run` or `--auto` in any non-interactive context. `e` opens `$EDITOR` (fallback `vim`) on a temp file.
- All user-facing strings, comments, and generated commit messages are Chinese — match that when adding UI text.
- `Cargo.lock` is gitignored by repo convention — do not commit it, even though this is a binary crate.

## Python branch notes

- `build.sh` only packages a tarball (`git-ai-commit-1.0.0.tar.gz`); it does not "build" anything executable.
- `install.sh` is interactive: prompts for the API key, appends to the shell rc, sets global git aliases, writes `~/.git-ai-commit/config.json`. Never run it unattended. (The config.json it writes is also never read back by `git_ai_commit.py`.)
- `git_ai_commit.py` uses the `openai` SDK with `base_url="https://api.deepseek.com"`; same key/model/env-var conventions as the Rust version.
- No tests exist on this branch either.
