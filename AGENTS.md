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

## Key Developer Notes

- **Configuration**: User config is stored in `$HOME/.git-ai-commit/config.json`.
- **Testing**: Use `cargo test` to run the test suite.
- **Linter**: Always run `cargo clippy --all-targets` and `cargo fmt --check` before completing tasks.
- **Binary**: The tool is intended to be installed via `./install.sh` to `~/.local/bin`.
