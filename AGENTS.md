# AGENTS.md

## Common Commands

| Purpose | Command |
|---------|---------|
| Build the Rust binary (release) | `./build.sh` or `cargo build --release` |
| Run unit tests (all) | `cargo test` |
| Run a single test by name | `cargo test --test <test_name>` |
| Lint & format checks | `cargo clippy --all-targets` and `cargo fmt --check` |
| Install the CLI binary to `$HOME/bin` | `./install.sh` |
| Uninstall the CLI binary | `./uninstall.sh` |

## High-Level Architecture

The project is a Rust-based CLI tool that uses LLMs to generate conventional commit messages.

- `src/main.rs`: CLI entry point and orchestration.
- `src/git.rs`: Git command wrapper (diff, status, commit).
- `src/ai.rs`: LLM API interaction.
- `src/commit.rs`: Validation and execution of `git commit`.
- `src/config.rs`: Configuration management (`$HOME/.git-ai-commit/config.json`).

## Model Resolution (`main.rs`)

On startup the active model is resolved via `Config::active_model()` (`models[selected_model]`, `None` when `selected_model` is empty or misses):

- `active_model()` is `None` **and** `models` is empty → interactive prompts collect model name / base URL / API token; the entry is inserted, selected, and saved.
- `active_model()` is `None` **and** `models` is non-empty (empty or dangling `selected_model`) → an `inquire::Select` picker lists the configured models; the choice replaces `selected_model` and is saved.
- After resolution, `ai::check_availability` probes the model. On failure: if `models.len() > 1` the picker is offered again to switch; otherwise the run aborts with `model_unavailable`.

## Key Developer Notes

- **Configuration**: User config is stored in `$HOME/.git-ai-commit/config.json`.
- **Testing**: Use `cargo test` to run the test suite.
- **Linter**: Always run `cargo clippy --all-targets` and `cargo fmt --check` before completing tasks.
- **Binary**: The tool is intended to be installed via `./install.sh` to `~/.local/bin`.
