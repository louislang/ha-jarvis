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
# The release downloads of sherpa-onnx.
sherpa_onnx := "https://github.com/k2-fsa/sherpa-onnx/releases/download"
# The library folder of the sherpa-onnx 2.0.1 CPU shared libraries for the host system.
cpu_libraries := justfile_directory() / "models" / ("sherpa-onnx-v2.0.1-" + (if os() == "macos" { "osx" } else { "linux" }) + "-x64-shared-lib") / "lib"
# The library folder of the sherpa-onnx 2.0.1 CUDA 12 shared libraries.
cuda_libraries := justfile_directory() / "models/sherpa-onnx-v2.0.1-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu/lib"
# The SHA-256 of each file of `models`, one list for each archive.
cpu_sums := if os() == "macos" { "be9042971c5d1b14da76a960534d2a8cdb6f0e5e02f5e1dc8bddff8576359bdd  sherpa-onnx-v2.0.1-osx-x64-shared-lib.tar.bz2" } else { "532c45bbd41036d83cd70606797529aa8086c2053395a58967736c727625b120  sherpa-onnx-v2.0.1-linux-x64-shared-lib.tar.bz2" }
supertonic_sums := """
    82fa96f91c4ef8abaae3a14a3f4153facf88bed821d1f7331cec2700f432c427  sherpa-onnx-supertonic-3-tts-int8-2026-05-11.tar.bz2
    c3eb91414d5ff8a7a239b7fe9e34e7e2bf8a8140d8375ffb14718b1c639325db  sherpa-onnx-supertonic-3-tts-int8-2026-05-11/duration_predictor.int8.onnx
    c7befd5ea8c3119769e8a6c1486c4edc6a3bc8365c67621c881bbb774b9902ff  sherpa-onnx-supertonic-3-tts-int8-2026-05-11/text_encoder.int8.onnx
    20cd86fa5c6effedfda0e7cffe5b0569ca401c440a0c3a1d72bf39286c0db3fd  sherpa-onnx-supertonic-3-tts-int8-2026-05-11/vector_estimator.int8.onnx
    e923d60f53f95eb1ce235f1dc33ec56d9c057823c96fa6f8acf98f32b0da6152  sherpa-onnx-supertonic-3-tts-int8-2026-05-11/vocoder.int8.onnx"""
# The fp32 Supertonic-3 graphs of Hugging Face `Supertone/supertonic-3`, one file for each list.
supertonic_fp32 := "https://huggingface.co/Supertone/supertonic-3/resolve/3cadd1ee6394adea1bd021217a0e650ede09a323/onnx"
vector_estimator_sums := "883ac868ea0275ef0e991524dc64f16b3c0376efd7c320af6b53f5b780d7c61c  sherpa-onnx-supertonic-3-tts-int8-2026-05-11/vector_estimator.onnx"
vocoder_sums := "085de76dd8e8d5836d6ca66826601f615939218f90e519f70ee8a36ed2a4c4ba  sherpa-onnx-supertonic-3-tts-int8-2026-05-11/vocoder.onnx"
nemotron_sums := """
    0ae73a41cd51599dc7cac9ac083d9d35de53d762ca45923505fde47a3751814b  sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25.tar.bz2
    71111f61b18e1e65e01e369434a5c0434868d2f44892742ae54240600c681209  sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25/encoder.int8.onnx
    0be9702c2f427a2b6bb241d298e0d3836a558de1f5b9fd3018f1cce6e2b3fa98  sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25/decoder.int8.onnx
    a35eac38a22ebceb04d230ed7afe0d68f446ba6914a036b97f14fece95967e23  sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25/joiner.int8.onnx
    dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff  sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25/tokens.txt"""
silero_sums := "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6  silero_vad.onnx"
# The SHA-256 of each file of `gpu-models`, one list for each archive.
cuda_sums := "2aa541a65c1e7d921a053283419b70b30c1f6ae0a0f52f64b947ebe88c5f2a77  sherpa-onnx-v2.0.1-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu.tar.bz2"
parakeet_sums := """
    99f63605b3a85a54c250c0869670a687b7d6598a47bf2421515e1f839a76e150  sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2
    6716910b7a0833997fec7a410494c995d70124001a0e9b66d6370d6aced577e0  sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming/encoder.int8.onnx
    a5e223392c90e75f8144cdb5eb95af7625db389e39edef2bd1a9c872b3298fe6  sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming/decoder.int8.onnx
    869f43f7d24595c55581ad3bf249a935fb8a71389fbdaa7504b9f46f93140f8a  sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming/joiner.int8.onnx
    dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff  sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming/tokens.txt"""
# The folder of the fp16 STT files, which are not in a download (`01-architecture.md` section 10).
fp16_models := "/opt/jarvis-models/fp16"
# The SHA-256 of each fp16 file of each pass in `fp16_models`.
nemotron_fp16_sums := """
    f2ff4ff73af5961cd9680e79868d4ed8f36544455d92115a71012f1a5bb0e55a  nemotron-160ms-encoder.fp16.onnx
    b4bc4b9579327604ea77d252a50c8bb99b4259324c26e64aa39b756011979f85  nemotron-160ms-encoder.fp16.weights
    0aad1db9eeaacf2c92ecf7e280281636b82d225c63834e51429cca4f3e7c5f48  nemotron-160ms-decoder.fp16.onnx
    fb270026bfcdd0fc9cb4c253b5180fd495fefbba8072e65cabe44b95e4e7e53e  nemotron-160ms-joiner.fp16.onnx"""
parakeet_fp16_sums := """
    cf899d8960088d0afe80f17713b35a185a85a3a6b24c31dacb40d8d934ec8957  parakeet-unified-encoder.fp16.onnx
    0f13a87a83a3a77939f7a57aadea91e7fab963bda6f4ecdd0eb8d210cdb91bbc  parakeet-unified-encoder.fp16.weights
    c31c063cffc978d19a0564eb31c63d131f1352ddedbd2b9c4559635b225cdcc1  parakeet-unified-decoder.fp16.onnx
    1e09b15a71aeede79442ace27092d0690af323e50c210d609dc41bc11b60be97  parakeet-unified-joiner.fp16.onnx"""
# Each build links the sherpa-onnx libraries of this folder, so no build downloads them.
export SHERPA_ONNX_LIB_DIR := cpu_libraries
# The environment that loads the ONNX Runtime of `cpu_libraries`.
cpu_runtime := "ORT_DYLIB_PATH=" + quote(cpu_libraries / "libonnxruntime." + if os() == "macos" { "dylib" } else { "so" }) + (if os() == "macos" { " DYLD_LIBRARY_PATH=" } else { " LD_LIBRARY_PATH=" }) + quote(cpu_libraries)

# Run the tests.
test:
    {{ cpu_runtime }} cargo test --workspace --release

# Run the tests with the coverage gate.
cov:
    {{ cpu_runtime }} cargo llvm-cov --workspace --release --fail-under-lines 80.01

# Download the CPU libraries and models.
models: (fetch sherpa_onnx + "/v2.0.1" cpu_sums) (fetch sherpa_onnx + "/asr-models" nemotron_sums) (fetch sherpa_onnx + "/tts-models" supertonic_sums) (fetch supertonic_fp32 vector_estimator_sums) (fetch supertonic_fp32 vocoder_sums) (fetch sherpa_onnx + "/asr-models" silero_sums)

# Download the GPU models and the CUDA libraries, and copy the fp16 files next to the int8 files of the same model.
gpu-models: cuda-libraries (fetch sherpa_onnx + "/asr-models" parakeet_sums) (fetch sherpa_onnx + "/asr-models" nemotron_sums) (copy-fp16 nemotron_fp16_sums "nemotron-160ms-" "sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25") (copy-fp16 parakeet_fp16_sums "parakeet-unified-" "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming")

# Download the CUDA libraries.
cuda-libraries: (fetch sherpa_onnx + "/v2.0.1" cuda_sums)

# Check the SHA-256 of each file of `sums` in `fp16_models`, then copy each file without `prefix` to the folder `folder` of `models/`. The encoder names its weights file, so the copied names must not change.
copy-fp16 sums prefix folder:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ quote(fp16_models) }}
    shasum -a 256 --check --quiet <<< "{{ sums }}"
    while read -r _ file; do
        cp "$file" {{ quote(justfile_directory() / "models" / folder) }}/"${file#{{ prefix }}}"
    done <<< "{{ sums }}"

# Download the file of the first line of `sums` from `release` to `models/` and extract it when it is an archive, unless its folder or file exists and each file of `sums` has its SHA-256.
fetch release sums:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p models
    cd models
    line=$(head -n 1 <<< "{{ sums }}")
    archive=${line#*  }
    if [ -e "${archive%.tar.bz2}" ] && shasum -a 256 --check --status <<< "{{ sums }}" 2> /dev/null; then exit 0; fi
    if ! shasum -a 256 --check --status <<< "$line" 2> /dev/null; then
        curl -sSfL -o "$archive" "{{ release }}/${archive##*/}"
        shasum -a 256 --check --quiet <<< "$line"
    fi
    [[ "$archive" != *.tar.bz2 ]] || tar -xjf "$archive"
    shasum -a 256 --check --quiet <<< "{{ sums }}"

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
gpu-test: models gpu-models
    JARVIS_DEVICE=cuda SHERPA_ONNX_LIB_DIR={{ quote(cuda_libraries) }} ORT_DYLIB_PATH={{ quote(cuda_libraries / "libonnxruntime.so") }} LD_LIBRARY_PATH={{ quote(cuda_libraries) }} cargo test --workspace --release -- --ignored --test-threads=1

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
    {{ cpu_runtime }} cargo run --release --quiet -- dataset jarvis.toml target/fixture/train.json --house src/testing/house.json
    stamp=models/system-one/fixture/inputs.sha256
    if hash=$({{ just_executable() }} fixture-stale "$stamp"); then
        {{ venv }}/python train/train.py --data target/fixture/train.json --out models/system-one/fixture --device cpu --epochs 80
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
check: models workflow-lint format lint complexity duplication architecture diagram quint python verus fixture-model cov
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
