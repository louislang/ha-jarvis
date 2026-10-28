# Tool versions (R23). Each tool has one `install-` recipe.
just_version := "1.59.0"
cargo_llvm_cov_version := "0.9.1"
actionlint_version := "1.7.12"
pre_commit_version := "4.7.0"
rust_code_analysis_cli_version := "0.0.25"
jscpd_version := "5.4.2"
codex_version := "0.162.0"

# Run the tests.
test:
    cargo test --workspace --release

# Run the tests with the coverage gate.
cov:
    cargo llvm-cov --workspace --release --fail-under-lines 80.01

# Check the format of the Rust code.
format:
    cargo fmt --all --check

# Lint the Rust code.
lint:
    cargo clippy --workspace --all-targets -- -D clippy::pedantic -D warnings

# Check the complexity limits of each function.
complexity:
    cargo run -p xtask -- complexity src xtask

# Check the duplication limit.
duplication:
    jscpd --threshold 3 --min-tokens 50 --format rust,python --ignore "**/target/**" src xtask

# Lint the workflows.
workflow-lint:
    actionlint

# Run the checks of the pre-commit hook.
check: workflow-lint format lint complexity duplication cov

# Install the tools and the pre-commit hook.
setup: install-just install-cargo-llvm-cov install-actionlint install-pre-commit install-rust-code-analysis-cli install-jscpd install-codex
    pre-commit install --hook-type pre-commit
    gh extension install github/gh-stack

install-just:
    cargo install just --version {{ just_version }} --locked

install-cargo-llvm-cov:
    cargo install cargo-llvm-cov --version {{ cargo_llvm_cov_version }} --locked

install-actionlint:
    curl -sSfL https://raw.githubusercontent.com/rhysd/actionlint/v{{ actionlint_version }}/scripts/download-actionlint.bash | bash -s -- {{ actionlint_version }} "${CARGO_HOME:-$HOME/.cargo}/bin"

install-pre-commit:
    pipx install --force pre-commit=={{ pre_commit_version }}

install-rust-code-analysis-cli:
    cargo install rust-code-analysis-cli --version {{ rust_code_analysis_cli_version }} --locked

install-jscpd:
    npm install --global jscpd@{{ jscpd_version }}

install-codex:
    npm install --global @openai/codex@{{ codex_version }}
