# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common Commands

| Purpose | Command |
|---------|---------|
| Build the Rust binary (release) | `./build.sh` or `cargo build --release` |
| Run unit tests (all) | `cargo test` |
| Run a single test by name | `cargo test --test <test_name>` (replace `<test_name>` with the test function or module name) |
| Lint & format checks | `cargo clippy --all-targets` and `cargo fmt --check` |
| Install the CLI binary to `$HOME/bin` | `./install.sh` (creates a symlink in `~/.local/bin`) |
| Uninstall the CLI binary | `./uninstall.sh` |

## High‑Level Architecture

```
├─ src/
│  ├─ main.rs          # CLI entry point – parses args, loads config, orchestrates flow
│  ├─ git.rs           # thin wrapper around Git commands (diff, status, commit)
│  ├─ ai.rs            # talks to LLM API to generate a Conventional‑Commit message
│  ├─ commit.rs        # validates format and performs the actual `git commit`
│  └─ config.rs        # loads/serialises user configuration from `$HOME/.git-ai-commit/config.json`
├─ build.sh            # builds the Rust binary for distribution
├─ install.sh          # symlink installer for `git‑ai-commit`
└─ uninstall.sh        # removes the symlink
```

1. **CLI (`main.rs`)** – Uses `clap` to expose flags such as `--api-key`, `--model`, `--auto`, `--all`, and `--dry-run`. After parsing, it:
   * Loads the configuration file (multiple model support).
   * Verifies we are inside a Git repo.
   * Gathers the diff, status and stats via `git.rs`.
   * Calls `ai::generate_commit_message` to ask LLM for a commit message.
   * If `--dry-run`, prints the generated message and runs `commit::validate_commit_message`.
   * Otherwise calls `commit_with_confirmation` to prompt the user and commit.

2. **Git helper (`git.rs`)** – Executes Git commands using `std::process::Command`. It returns the raw output strings used by the AI prompt.

3. **AI module (`ai.rs`)** – Sends a JSON payload to the LLM endpoint (configured via `config::ModelConfig`). It passes the diff, status, stats and repository info. The response is a plain commit message.

4. **Commit module (`commit.rs`)** – Validates the format against Conventional Commit rules and commits using a temporary file to avoid argument length limits.

5. **Configuration (`config.rs`)** – Stores a map of named models, the currently selected model, token limits and temperature. Configuration is read from `$HOME/.git-ai-commit/config.json`.

## Usage Tips

* The tool can automatically commit (`--auto`) after generating the message.
* `--dry-run` is useful to preview the message before committing.
* Use `--all` if you want un‑staged changes included in the diff sent to the AI.
* The configuration file supports multiple models; you can change `selected_model` or add new entries.

## Extending the Tool

* Add more Git metrics (e.g., `git diff --name-status`) if you want richer prompts.
* Integrate additional LLM providers by adding a new entry in `config.json` and extending `ai::generate_commit_message`.
* Hook into CI by invoking the binary in a pre‑commit or commit‑msg hook.

---

**Note:** The repository contains no cursor rules, Copilot instructions, or external AI configs. If you have a `~/.codex/config.toml` or similar, you can import it later via `/import`.