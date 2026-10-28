# Tool versions (R23). Each tool has one `install-` recipe.
just_version := "1.59.0"
cargo_llvm_cov_version := "0.9.1"
actionlint_version := "1.7.12"
pre_commit_version := "4.7.0"

# Run the tests.
test:
    cargo test --workspace --release

# Run the tests with the coverage gate.
cov:
    cargo llvm-cov --workspace --release --fail-under-lines 80.01

# Lint the workflows.
workflow-lint:
    actionlint

# Run the checks of the pre-commit hook.
check: workflow-lint cov

# Install the tools and the pre-commit hook.
setup: install-just install-cargo-llvm-cov install-actionlint install-pre-commit
    pre-commit install --hook-type pre-commit

install-just:
    cargo install just --version {{ just_version }} --locked

install-cargo-llvm-cov:
    cargo install cargo-llvm-cov --version {{ cargo_llvm_cov_version }} --locked

install-actionlint:
    curl -sSfL https://raw.githubusercontent.com/rhysd/actionlint/v{{ actionlint_version }}/scripts/download-actionlint.bash | bash -s -- {{ actionlint_version }} "${CARGO_HOME:-$HOME/.cargo}/bin"

install-pre-commit:
    pipx install --force pre-commit=={{ pre_commit_version }}
