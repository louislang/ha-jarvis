# Tool versions (R23). Each tool has one `install-` recipe.
just_version := "1.59.0"
cargo_llvm_cov_version := "0.9.1"
actionlint_version := "1.7.12"
pre_commit_version := "4.7.0"
rust_code_analysis_cli_version := "0.0.25"
jscpd_version := "5.4.2"
quint_version := "0.33.0"
quint_evaluator_version := "v0.7.0"
verus_version := "0.2026.10.26.d17fbd0"
python_version := "3.12"
# The apt package of `ubuntu-latest`.
valgrind_version := "1:3.26.0-0ubuntu1"
# The version of the `gungraun` library.
gungraun_runner_version := "0.20.0"
# The tools of the `python` recipe.
venv := justfile_directory() / ".venv/bin"

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
    cargo run -p xtask -- complexity src xtask train

# Check the duplication limit.
duplication:
    jscpd --threshold 3 --min-tokens 50 --format rust,python --ignore "**/target/**" src xtask train

# Check the layer and modularity rules.
architecture:
    cargo run -p xtask -- rules

# Check each Quint model.
quint:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob
    for model in quint/*.qnt; do
        quint typecheck "$model"
        quint test --seed=1 --match='^ac_' "$model"
        quint run --seed=1 --invariant=allInvariants --max-samples=10000 --max-steps=30 "$model"
        quint verify --invariant=allInvariants --max-steps=10 "$model"
    done

# Check the Python code with the tools of `02-rules.md` section 3.
[working-directory("train")]
python:
    {{ venv }}/ruff format --check
    {{ venv }}/ruff check
    {{ venv }}/mypy --strict .
    {{ venv }}/vulture --min-confidence 60 --ignore-names forward .
    {{ venv }}/interrogate --fail-under 100 .
    {{ venv }}/pytest --cov --cov-fail-under=80.01

# Prove the Verus proofs of the domain.
verus:
    PATH="$HOME/.verus/{{ verus_version }}:$PATH" cargo verus verify -p jarvis

# Check the instruction budgets of the benchmarks.
bench:
    cargo bench --bench domain

# Run the `[GPU host]` tests on CUDA.
gpu-test:
    JARVIS_DEVICE=cuda cargo test --workspace --release -- --ignored

# Write the architecture diagram.
diagram:
    cargo run -p xtask -- diagram docs/architecture.md

# Squash merge a PR when it meets the merge rules.
merge pr:
    cargo run -p xtask -- merge {{ pr }}

# Lint the workflows.
workflow-lint:
    actionlint

# Train the fixture model on the CPU when its inputs change.
fixture-model:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p target/fixture
    cargo run --release --quiet -- dataset jarvis.toml target/fixture/train.json --house src/testing/house.json
    stamp=models/system-one/fixture/inputs.sha256
    if hash=$({{ just_executable() }} fixture-stale "$stamp"); then
        {{ venv }}/python train/train.py --data target/fixture/train.json --out models/system-one/fixture --device cpu
        echo "$hash" > "$stamp"
    fi

# Print the hash of the training set file and the tracked files of `train/`. Exit with code 0 when it differs from the file `stamp` or a model file next to `stamp` is missing.
fixture-stale stamp:
    #!/usr/bin/env bash
    set -euo pipefail
    hash=$(cat target/fixture/train.json $(git ls-files train) | shasum -a 256 | cut -d ' ' -f 1)
    echo "$hash"
    folder=$(dirname "{{ stamp }}")
    for file in model.onnx tokenizer.json labels.json; do
        [ -f "$folder/$file" ] || exit 0
    done
    [ "$hash" != "$(cat "{{ stamp }}" 2> /dev/null)" ]

# Run the checks of the pre-commit hook.
check: workflow-lint format lint complexity duplication architecture quint python verus fixture-model cov
    {{ if os() == "linux" { "just bench" } else { "" } }}
    {{ if env("JARVIS_DEVICE", "") == "cuda" { "just gpu-test" } else { "" } }}

# Install the tools and the pre-commit hook.
setup: pre-push-hook install-just install-cargo-llvm-cov install-actionlint install-pre-commit install-rust-code-analysis-cli install-jscpd install-quint install-quint-evaluator install-verus install-valgrind install-gungraun-runner install-python
    pre-commit install --hook-type pre-commit
    gh extension install github/gh-stack

# Write the pre-push guard as a direct Git hook: `pre-commit` does not pass the pushed refs to its hooks.
pre-push-hook:
    printf '#!/bin/sh\nexec cargo run --quiet --manifest-path "%s" -p xtask -- guard-push\n' "{{ justfile_directory() / "Cargo.toml" }}" > .git/hooks/pre-push
    chmod +x .git/hooks/pre-push

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

install-quint:
    npm install --global @informalsystems/quint@{{ quint_version }}

# Quint fetches the evaluator through the GitHub API, whose rate limit fails a runner without a token.
install-quint-evaluator:
    mkdir -p ~/.quint/rust-evaluator-{{ quint_evaluator_version }}
    curl -sSfL https://github.com/quint-co/quint/releases/download/evaluator/{{ quint_evaluator_version }}/quint_evaluator-$(rustc -vV | sed -n 's/host: //p').tar.gz | tar -xz -C ~/.quint/rust-evaluator-{{ quint_evaluator_version }}

install-verus:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -sm)" in
        "Linux x86_64") platform=x86-linux ;;
        "Darwin x86_64") platform=x86-macos ;;
        "Darwin arm64") platform=arm64-macos ;;
        *) echo "Verus has no release for $(uname -sm)." >&2; exit 1 ;;
    esac
    folder=~/.verus/{{ verus_version }}
    rm -rf "$folder" "$folder.zip" ~/.verus/verus-$platform
    curl -sSfL --create-dirs -o "$folder.zip" https://github.com/verus-lang/verus/releases/download/release%2F{{ verus_version }}/verus-{{ verus_version }}-$platform.zip
    unzip -q "$folder.zip" -d ~/.verus
    mv ~/.verus/verus-$platform "$folder"
    rm "$folder.zip"
    rustup toolchain install "$("$folder/verus" --version | sed -n 's/^ *Toolchain: \([^ ]*\).*/\1/p')"

# Valgrind does not run on macOS.
install-valgrind:
    {{ if os() == "linux" { "sudo apt-get update && sudo apt-get install --yes valgrind=" + valgrind_version } else { "" } }}

install-gungraun-runner:
    cargo install gungraun-runner --version {{ gungraun_runner_version }} --locked

# Make `.venv` with the training script and its pinned packages.
install-python:
    python{{ python_version }} -m venv .venv
    {{ venv }}/pip install './train[dev]'
