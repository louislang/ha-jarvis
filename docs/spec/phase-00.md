# Phase 0: Repository and Delivery

This phase makes the repository, the gates, the PR workflow, the architecture diagram, the Quint gate, the GPU runner, the release workflow and the merge gate.

The coverage gate runs in the pre-commit hook and in CI from the first commit. Every later gate is proven by a gate test: `xtask/src/gates.rs` writes a small fixture (a crate, a Python file, a Quint model) into a temporary folder, runs the real tool, and checks the exit code and the message. Workflow and hook files are checked by structure tests that parse them. All of these are normal tests in `rust-test`.

The first commit (sub-phase 0.1) goes directly to `main`, because no PR process exists yet. From sub-phase 0.2, each phase is one stack of PRs (`02-rules.md` section 7). Until `just merge` exists, the bootstrap rule (`02-rules.md` section 7.1, rule 10) applies.

---

### 0.1 Repository, Coverage Gate, Hook and CI

**Goal:** Create the repository, the Cargo workspace (an empty `jarvis` library and binary, and an empty `xtask` tool crate), and the coverage gate in both the pre-commit hook and CI. No code exists without a gated coverage check.

- **Implements:** G10, G17, G22. **Depends on:** none.
- **Files:** create `.gitignore`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `justfile`, `.pre-commit-config.yaml`, `.github/workflows/ci.yml`, `src/lib.rs`, `src/main.rs`, `xtask/Cargo.toml`, `xtask/src/main.rs`, `xtask/src/gates.rs`. Add the existing `CLAUDE.md` and `docs/spec/`.

**Model.**

- `Cargo.toml` is a workspace with the members `jarvis` (root) and `xtask`.
- `rust-toolchain.toml` names one fixed stable version with `clippy`, `rustfmt` and `llvm-tools-preview`.
- `src/lib.rs` and `xtask/src/main.rs` have an empty `main`. `src/main.rs` has a `main` that only calls `jarvis::main`. Each of the three files has one test that calls its `main`.
- `justfile`: `test` (`cargo test --workspace --release`), `cov` (`cargo llvm-cov --workspace --release --fail-under-lines 80.01`), `workflow-lint` (`actionlint`), `check` (runs `workflow-lint`, then `cov`), `setup` (runs every `install-` recipe, then `pre-commit install --hook-type pre-commit`).
- Tool versions (R23): the `justfile` has one variable for each tool version and one `install-<tool>` recipe for each tool. This sub-phase has `cargo-llvm-cov`, `actionlint` and `pre-commit`. Each later sub-phase that adds a tool adds its variable and its recipe. A CI job installs a tool only with its `install-` recipe. A `setup-node`, `setup-java` or `setup-python` step uses the version of its `justfile` variable.
- `.pre-commit-config.yaml`: one `local` hook, `entry: just check`, `language: system`, `stages: [pre-commit]`, `pass_filenames: false`, `always_run: true`.
- `ci.yml`: triggers `push`, `pull_request`, `workflow_call`. Jobs `rust-test` (`just cov`) and `workflow-lint` (`just workflow-lint`), on `ubuntu-latest`, with the cargo cache.
- `xtask/src/gates.rs` is `#[cfg(test)]`. It has the helpers `fixture_crate(source)` (a temporary crate with the workspace lint settings) and `run(command) -> Output`, and the gate and structure tests of Phase 0.
- Structure tests read `just --dump --dump-format json`, `.pre-commit-config.yaml` and the workflow files with `serde_yaml`. The file list test reads the table of `01-architecture.md` section 8 and `git ls-files`.

**Acceptance criteria.**

- AC-0.1-1: When you run `cargo build --workspace`, it exits with code 0.
- AC-0.1-2: When `just test` runs, the tests `ac_0_1_2_main_returns` in `src/lib.rs`, `src/main.rs` and `xtask/src/main.rs` pass.
- AC-0.1-3: When a fixture crate has 10 lines of which 5 are covered, `cargo llvm-cov --fail-under-lines 80.01` exits with a non-zero code. When all lines are covered, it exits with code 0.
- AC-0.1-4: When the `justfile` is dumped, `check` runs `workflow-lint` and `cov`, and `cov` has `--fail-under-lines 80.01`.
- AC-0.1-5: When `.pre-commit-config.yaml` is parsed, it has exactly one hook, with `entry: just check`, `stages: [pre-commit]`, `always_run: true` and `pass_filenames: false`.
- AC-0.1-6: When `ci.yml` is parsed, it has the triggers `push`, `pull_request` and `workflow_call`, a job `rust-test` that runs `just cov`, and a job `workflow-lint` that runs `just workflow-lint`.
- AC-0.1-7: When `git check-ignore` runs for `target/`, `models/`, `.venv/`, `jarvis.local.toml`, `habits.json`, `voices.json`, `jarvis.db`, `generated.json`, `training.json`, `pools.json` and `learned.toml`, it lists each path.
- AC-0.1-8: When `rust-toolchain.toml` is parsed, its channel is a fixed version `x.y.z`, and its components are `clippy`, `rustfmt` and `llvm-tools-preview`.
- AC-0.1-9: When the `justfile` and `ci.yml` are parsed, each tool has a version variable and an `install-` recipe, `setup` runs every `install-` recipe, and each CI step that installs a tool runs its `install-` recipe or uses the version of its variable.
- AC-0.1-10: When `git ls-files` is compared with the table of `01-architecture.md` section 8, each tracked file is in the table. A file in `docs/spec/` matches the row `docs/spec/*.md`.

**Proofs.**

- P-0.1-2. Ensures: each `main` returns. Argument: each `main` has an empty body or only calls `jarvis::main`, which has an empty body.
- P-0.1-1, P-0.1-3 to P-0.1-10. Evidence: test.

**Field log.** Run `gh repo create jarvis --private --source . --push`. Run `just setup`. Record that the first CI run passes.

---

### 0.2 Lint Gates

**Goal:** Turn on the format and lint gates, including doc comments on every item, in the hook and in CI. Code that breaks a Rust rule fails before it can merge.

- **Implements:** G10, G22. **Depends on:** 0.1.
- **Files:** create `clippy.toml`. Modify `Cargo.toml`, `xtask/Cargo.toml`, `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.**

- `[workspace.lints.rust]`: `warnings = "deny"`, `dead_code = "deny"`, `missing_docs = "deny"`, `unsafe_code = "forbid"`.
- `[workspace.lints.clippy]`: `pedantic = "deny"`, `unwrap_used = "deny"`, `string_add = "deny"`, `missing_docs_in_private_items = "deny"`, `undocumented_unsafe_blocks = "deny"`.
- `jarvis` and `xtask` have `[lints] workspace = true`. Thus the lint levels apply to both crates.
- `clippy.toml`: `too-many-lines-threshold = 50`, `allow-unwrap-in-tests = true`.
- `format` runs `cargo fmt --all --check`. `lint` runs `cargo clippy --workspace --all-targets -- -D clippy::pedantic -D warnings`. `check` runs `workflow-lint`, `format`, `lint`, then `cov`. CI jobs `rust-format` and `rust-lint`.

**Acceptance criteria.**

- AC-0.2-1: When a fixture function has 51 lines, `lint` fails with `too_many_lines`. At 50 lines, it passes.
- AC-0.2-2: When a fixture function is not used, `lint` fails with `dead_code`.
- AC-0.2-3: When fixture code outside a test calls `unwrap`, `lint` fails with `unwrap_used`. Inside a test, it passes.
- AC-0.2-4: When a fixture private function or private struct has no doc comment, `lint` fails with `missing_docs_in_private_items`.
- AC-0.2-5: When a fixture has an `unsafe` block, the build fails with `unsafe_code`.
- AC-0.2-6: When a fixture file is not formatted, `format` fails.
- AC-0.2-7: When the `justfile` is dumped, `check` runs `workflow-lint`, `format`, `lint` and `cov` in that order. When `ci.yml` is parsed, the jobs `rust-format` and `rust-lint` run `just format` and `just lint`.

**Proofs.**

- P-0.2-1 to P-0.2-7. Evidence: test.

---

### 0.3 Complexity Gate

**Goal:** Add the `xtask complexity` command and its gate. A Rust or Python function above the cyclomatic or Halstead limits, or a Python function above 50 code lines, fails.

- **Implements:** G10, G22. **Depends on:** 0.2.
- **Files:** create `xtask/src/complexity.rs`. Modify `xtask/src/main.rs`, `xtask/Cargo.toml`, `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.**

- `xtask complexity <paths>` runs `rust-code-analysis-cli -m -O json -p <path>` and reads the units.
- `violations(unit, limits) -> Vec<Violation>` walks the space tree. A space of kind `function` is a violation when `cyclomatic.sum > 10`, `halstead.volume > 1000`, `halstead.difficulty > 30`, or (Python and `loc.ploc > 50`).
- Output: one line `<file>:<line> <function> <metric> <value> > <limit>` for each violation. Exit code 1 when there is one or more.
- `complexity` recipe: `cargo run -p xtask -- complexity src xtask`. `check` runs it after `lint`. CI job `complexity`.

**Acceptance criteria.**

- AC-0.3-1: When a function has `cyclomatic.sum = 11`, `violations` returns one violation with its file, line, name and metric.
- AC-0.3-2: When a function has `cyclomatic.sum = 10`, `violations` returns none.
- AC-0.3-3: When a function has `halstead.volume = 1000.1`, `violations` returns one violation. At `1000.0`, none.
- AC-0.3-4: When a function has `halstead.difficulty = 30.1`, `violations` returns one violation. At `30.0`, none.
- AC-0.3-5: When a Python function has `loc.ploc = 51`, `violations` returns one violation. A Rust function with `loc.ploc = 51` gives none.
- AC-0.3-6: When a function is inside an `impl` inside a unit, `violations` checks it.
- AC-0.3-7: When a space is not a function, `violations` does not check its metrics.
- AC-0.3-8: When a fixture Rust file has a function with 11 `if` branches, `xtask complexity` exits with code 1 and prints the function name.
- AC-0.3-9: When a fixture file with let chains is read, `rust-code-analysis-cli` gives metrics for each function.
- AC-0.3-10: When the `justfile` is dumped, `check` runs `complexity` after `lint`. When `ci.yml` is parsed, the job `complexity` runs `just complexity`.

**Proofs.**

- P-0.3-1 to P-0.3-5. Requires: a finite space tree and the limits `(10, 1000, 30, 50)`. Ensures: a function space is in the result if and only if one of its metrics is strictly greater than its limit. Argument: the check is a disjunction of strict `>` comparisons, so a value equal to a limit passes. The `ploc` comparison is guarded by "the file is Python".
- P-0.3-6. Ensures: every function space of the tree is checked once. Argument: the walk recurses on every child. The tree is finite, so it ends, and it visits each node once.
- P-0.3-7. Ensures: a space that is not a function gives no violation. Argument: the check is guarded by `kind == "function"`.
- P-0.3-8 to P-0.3-10. Evidence: test.

---

### 0.4 Duplication Gate

**Goal:** Add the duplication gate with `jscpd` in the hook and in CI. A change that brings duplicated code above 3% fails.

- **Implements:** G10, G22. **Depends on:** 0.3.
- **Files:** modify `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.** `duplication` runs `jscpd --threshold 3 --min-tokens 50 --format rust,python --ignore "**/target/**" src xtask`. `check` runs it after `complexity`. CI job `duplication`. Sub-phase 3.5 adds `train`, because `jscpd` fails on a path that does not exist.

**Acceptance criteria.**

- AC-0.4-1: When a fixture folder has two copies of a 40-line function and nothing else, `jscpd --threshold 3` exits with a non-zero code and names both locations.
- AC-0.4-2: When the fixture folder has one copy, it exits with code 0.
- AC-0.4-3: When the `justfile` is dumped, `check` runs `duplication` after `complexity`. When `ci.yml` is parsed, the job `duplication` runs `just duplication`.

**Proofs.**

- P-0.4-1 to P-0.4-3. Evidence: test.

---

### 0.5 PR Template, Stacks and Review

**Goal:** Add the PR template, the stack tooling and the Codex review jobs. Every PR states what it did, confirms each required check, and sits in the phase stack, and each push to a PR gets an AI code review.

- **Implements:** G21. **Depends on:** 0.4.
- **Files:** create `.github/pull_request_template.md`. Modify `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.**

- The template has three sections: `## Summary` (two or three sentences, written after the code), `## Changes` (short bullet points) and `## Checks`.
- `## Checks` has one box for each required CI job (`02-rules.md` section 7.3), then the boxes "Started in a fresh context", "Codex review threads resolved" and "`[GPU host]` criteria pass in `gpu-test`".
- `setup` also runs `gh extension install github/gh-stack`.
- The `justfile` has the variable `codex_version` and the recipe `install-codex` (`npm install --global @openai/codex@<codex_version>`).
- `ci.yml` has two jobs that run only on `pull_request`, so each push to a PR gets a review:
  - `codex-review` has only the permission `contents: read`. It checks out the PR head and its base branch with `persist-credentials: false`. Then it runs `openai/codex-action` at a fixed release tag with `openai-api-key: ${{ secrets.OPENAI_API_KEY }}`, `permission-profile: ":workspace"`, the version `codex_version` and an output schema. The prompt asks Codex to review only the changes of the PR, against `CLAUDE.md` and the spec sections that the PR implements. The schema is a list of findings, each with `path`, `line` and `body`. The job output is the final message.
  - `codex-post` needs `codex-review` and has the permission `pull-requests: write`. With `actions/github-script` at a fixed release tag, it posts one PR review on the head commit. Each finding is one review comment, so each finding is one review thread. A finding on a line outside the diff becomes a file comment. With no finding, the review body says that Codex found no problem.
- The merge gate (sub-phases 0.15 and 0.16) blocks the merge until both jobs passed on the head commit and every review thread is resolved.
- A later sub-phase that adds a CI job also adds its check box to the template.

**Acceptance criteria.**

- AC-0.5-1: When the template is parsed, it has the sections `Summary`, `Changes` and `Checks`, in that order.
- AC-0.5-2: When the template is compared with `ci.yml`, it has exactly one check box for each required CI job name, plus the three other boxes.
- AC-0.5-3: When the `justfile` is dumped, `setup` installs `github/gh-stack`.
- AC-0.5-4: When `ci.yml` is parsed, the jobs `codex-review` and `codex-post` run only on `pull_request`. `codex-review` has only the permission `contents: read` and uses `openai/codex-action` at a fixed release tag, with the secret `OPENAI_API_KEY`, the permission profile `:workspace` and an output schema. `codex-post` needs `codex-review` and has the permission `pull-requests: write`.

**Proofs.**

- P-0.5-2. Argument: the test builds the set of job names from `ci.yml` and the set of box labels from the template, and compares the sets.
- P-0.5-1, P-0.5-3, P-0.5-4. Evidence: test.

**Field log.** Add the repository secret `OPENAI_API_KEY`. Record that the PR of this sub-phase gets a Codex review: one review thread for each finding, or a review that says that Codex found no problem.

---

### 0.6 PR Size Gate

**Goal:** Add the `pr-size` command and CI job. A PR that changes more than 400 counted lines against the layer below it fails.

- **Implements:** G21. **Depends on:** 0.5.
- **Files:** create `xtask/src/pr_size.rs`. Modify `xtask/src/main.rs`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`, `.github/pull_request_template.md`.

**Model.**

- `xtask pr-size --base <ref> --limit <n>` runs `git diff --numstat <ref>...HEAD`.
- `counted_lines(numstat, excluded) -> Result<usize>`: each line is `<added>\t<deleted>\t<path>`. A binary file (`-\t-`) counts 0. An excluded path counts 0. Other lines count `added + deleted`. Another format is an error.
- `excluded = ["Cargo.lock", "docs/architecture.md", "src/testing/house.json"]`.
- The CI job runs only for `pull_request` with `--base origin/${{ github.base_ref }}`. In a stack, the base is the branch of the layer below, so each PR is measured alone. The job checks out with `fetch-depth: 0`, so the base exists.

**Acceptance criteria.**

- AC-0.6-1: When the numstat is `300\t0\ta.rs` and `40\t10\tb.rs`, `counted_lines` returns 350.
- AC-0.6-2: When the count is 400 and the limit is 400, the check passes. At 401, it fails and prints `401 > 400`.
- AC-0.6-3: When `Cargo.lock` has 5000 changed lines, they do not count.
- AC-0.6-4: When a binary file changes, it counts 0.
- AC-0.6-5: When a numstat line has two fields, `counted_lines` returns an error that contains the line.
- AC-0.6-6: When `ci.yml` is parsed, the job `pr-size` runs only on `pull_request`, checks out with `fetch-depth: 0` and passes `origin/${{ github.base_ref }}` as the base.

**Proofs.**

- P-0.6-1, P-0.6-3, P-0.6-4. Requires: every line parses. Ensures: the result is the sum of `added + deleted` over lines that are not excluded and not binary. Argument: a fold with a running sum, where excluded and binary lines add 0.
- P-0.6-2. Ensures: the check fails if and only if the count is more than the limit. Argument: the check fails on `>`, so equality passes.
- P-0.6-5. Ensures: a line that does not split into three fields gives `Err` with that line. Argument: a line must split into exactly three fields, else `Err` with the line, and the fold stops at the first `Err`.
- P-0.6-6. Evidence: test.

---

### 0.7 Module Graph

**Goal:** Add the module graph builder to `xtask`. It reads the module tree and the `use` items with `syn` and gives the modules, their layers and their `use` dependencies, which the rules and the diagram need.

- **Implements:** G10, G20. **Depends on:** 0.6.
- **Files:** create `xtask/src/graph.rs`. Modify `xtask/src/main.rs`, `xtask/Cargo.toml`.

**Model.**

- A node is a module: its path (`crate::domain::intent`), its file, its layer and a hash of its file content.
- The layer is the first path segment: `domain`, `ports`, `app`, `adapters`, `testing`, or `root` for `crate`.
- The module tree comes from the `mod` items in `src/lib.rs`. An inline `pub mod domain { pub mod intent; }` gives the file `src/domain/intent.rs`.
- An edge is `(from, to, test)`. Each leaf of a `use` tree gives one edge. `to` is the longest prefix that is a module. `test` is true inside a `#[cfg(test)]` item, and in every file of a module that `src/lib.rs` declares under `#[cfg(test)]` (for example `testing`).
- A path that starts with `self` or `super` resolves against the current module. Each `use` leaf binds its last segment, or its `as` name, to its full path in the alias table of the file. Sub-phase 0.7b resolves other paths through this table. `use super::*` gives no edge.

**Acceptance criteria.**

- AC-0.7-1: When `src/lib.rs` has `pub mod domain { pub mod intent; }` and the file exists, the graph has the nodes `crate` and `crate::domain::intent` with layer `domain`.
- AC-0.7-2: When `src/app/x.rs` has `use crate::ports::Home;`, the graph has the edge `crate::app::x -> crate::ports`.
- AC-0.7-3: When a file has `use crate::{ports::A, domain::b::C};`, the graph has two edges.
- AC-0.7-4: When a `use crate::testing::fakes::FakeHome;` is inside `#[cfg(test)] mod tests`, its edge has `test = true`.
- AC-0.7-5: When the content of a file changes, its node hash changes.
- AC-0.7-6: When `src/app/x.rs` has `use super::super::ports::Home;`, the graph has the edge `crate::app::x -> crate::ports`.
- AC-0.7-7: When `src/lib.rs` has `#[cfg(test)] pub mod testing { pub mod fakes; }`, every edge from `crate::testing::fakes` has `test = true`.
- AC-0.7-8: When a file has `use crate::ports::Home as H;` and `use tokio::sync::Mutex;`, the alias table maps `H` to `crate::ports::Home` and `Mutex` to `tokio::sync::Mutex`.

**Proofs.**

- P-0.7-1. Ensures: the node set equals the set of declared modules. Argument: the builder adds one node for each `mod` item and from no other source.
- P-0.7-2, P-0.7-3. Ensures: one edge for each leaf of each `use` tree. Argument: a `use` tree is finite. The visitor recurses on groups and emits only at leaves.
- P-0.7-4, P-0.7-7. Ensures: `test` is true exactly for an edge inside a `#[cfg(test)]` item or in a module that `src/lib.rs` declares under `#[cfg(test)]`. Argument: the visitor keeps a depth counter of enclosing `#[cfg(test)]` items. It starts the counter at 1 in the files of such a module, and `test` is true when the counter is more than 0.
- P-0.7-6. Ensures: a path with `k` leading `super` segments resolves to the module `k` levels above the current module. Argument: the resolver removes one segment of the current module path for each `super`, then appends the rest of the path.
- P-0.7-8. Ensures: the table maps each bound name to the full path of its `use` leaf. Argument: the visitor inserts one entry at each leaf, with the `as` name when it exists, else the last segment.
- P-0.7-5. Evidence: test (the hash function).

---

### 0.7b Module Graph Paths

**Goal:** Add the edges of the paths outside `use` items, with names resolved through the alias table, and the external edges. A rule now sees every crate path that code names, also inside a macro.

- **Implements:** G10, G20. **Depends on:** 0.7.
- **Files:** modify `xtask/src/graph.rs`, `xtask/Cargo.toml`.

**Model.**

- Each path in another position (expression, type, pattern, trait bound, macro path) gives one edge after resolution.
- Resolution of the first segment: `crate`, `self` and `super` resolve as in sub-phase 0.7. A name in the alias table resolves to its full path. A dependency name of `Cargo.toml` (with `-` replaced by `_`), `std`, `core` and `alloc` start an external path. Any other first segment (a local item, a variable, a generic parameter, or a prelude name such as `Vec` or `Ok`) gives no edge.
- A resolved path to an outside crate gives an `external` edge with the full path (for example `std::fs::read`). Rules match external paths by prefix.
- A macro body is a token stream. The builder parses it as expressions separated by commas. When that fails, it takes each run of `name :: name ...` tokens as a path. Thus `format!("{}", std::fs::read_to_string(p)?)` gives an edge.

**Acceptance criteria.**

- AC-0.7b-1: When `src/lib.rs` calls `crate::adapters::ha::HaHome::connect()`, the graph has the edge `crate -> crate::adapters::ha`.
- AC-0.7b-2: When a domain file has `use tokio::sync::Mutex;`, the graph has an external edge to `tokio::sync::Mutex`. When it calls `std::fs::read`, the graph has an external edge to `std::fs::read`.
- AC-0.7b-3: When a file has `use crate::ports as p;` and a function parameter of type `p::Home`, the graph has an edge to `crate::ports` for the parameter type.
- AC-0.7b-4: When a domain file has `use tokio::sync::Mutex;` and calls `Mutex::new()`, the graph has an external edge to `tokio::sync::Mutex::new`.
- AC-0.7b-5: When a domain file calls `Vec::new()`, `Ok(1)` and `Intent::parse()` for a local type `Intent`, the graph has no external edge.
- AC-0.7b-6: When a domain file has `format!("{}", std::fs::read_to_string(p)?)`, the graph has an external edge to `std::fs::read_to_string`.

**Proofs.**

- P-0.7b-1. Ensures: a `crate` path gives an edge to its longest module prefix. Argument: the expression visitor sees every path. The longest module prefix exists because `crate` is a module.
- P-0.7b-2, P-0.7b-4. Ensures: a path whose first segment is a dependency name, or an alias of such a path, gives an external edge with the full path. Argument: resolution first replaces an alias with its full path, then tests the first segment against the dependency names (P-0.7-8).
- P-0.7b-3. Ensures: a path that starts with an alias of a module gives an edge to that module. Argument: resolution replaces `p` with `crate::ports`, and the longest module prefix of `crate::ports::Home` is `crate::ports`.
- P-0.7b-5. Ensures: a path whose first segment is not `crate`, `self`, `super`, an alias, a dependency name, `std`, `core` or `alloc` gives no edge. Argument: resolution has one arm for each of these cases and a last arm that gives no edge. `self` and `super` paths become `crate` paths first, so they cannot avoid a rule.
- P-0.7b-6. Ensures: a path in a macro body gives the same edge as the same path in an expression. Argument: the body becomes expressions or token paths, and each path goes through the same resolution.

---

### 0.8 Layer and Modularity Rules

**Goal:** Add the `rules` command and the `architecture` gate in the hook and in CI. A broken layer rule, or code outside `src/lib.rs` that names a concrete adapter, fails.

- **Implements:** G10, G22. **Depends on:** 0.7b.
- **Files:** create `xtask/src/rules.rs`. Modify `xtask/src/main.rs`, `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`, `.github/pull_request_template.md`.

**Model.** `violations(graph) -> Vec<RuleViolation>` checks each edge with `test = false`:

1. `domain -> ports | app | adapters`.
2. `domain -> external`, unless the crate is in the domain list of `01-architecture.md` section 7 or is `std`, and the path does not start with a banned path of `01-architecture.md` section 1, rule 1.
3. `app -> adapters`.
4. `adapters -> app`.
5. `X -> adapters::*`, unless `X` is `root` or the same adapter module.
6. `ports -> app | adapters`.

`architecture` runs `xtask rules`. `check` runs it after `duplication`. CI job `architecture`.

**Acceptance criteria.**

- AC-0.8-1: When a domain file uses `crate::ports`, `violations` returns rule 1 with the file and the edge.
- AC-0.8-2: When a domain file uses `tokio`, `violations` returns rule 2. With `minijinja`, none.
- AC-0.8-3: When a domain file uses `std::fs::read`, `std::env::var` or `std::collections::HashMap`, `violations` returns rule 2. With `std::collections::BTreeMap`, none.
- AC-0.8-4: When an app file uses `crate::adapters::ha`, `violations` returns rule 3.
- AC-0.8-5: When an adapter file uses `crate::app`, `violations` returns rule 4.
- AC-0.8-6: When `crate::adapters::web` uses `crate::adapters::sqlite`, `violations` returns rule 5.
- AC-0.8-7: When an edge has `test = true`, `violations` ignores it.
- AC-0.8-8: When the `justfile` is dumped, `check` runs `architecture` after `duplication`. When `ci.yml` is parsed, the job `architecture` runs `just architecture`.
- AC-0.8-9: When `src/ports.rs` uses `crate::app`, `violations` returns rule 6.
- AC-0.8-10: When a domain file uses `jiff::Zoned::now` or `rand::random`, `violations` returns rule 2. With `jiff::civil::Date`, none.

**Proofs.**

- P-0.8-1 to P-0.8-6, P-0.8-9, P-0.8-10. Ensures: an edge with `test = false` is in the result if and only if one of rules 1 to 6 matches. Argument: `violations` is a filter with the disjunction of the six rules as its predicate. Rule 2 checks the banned paths also for a crate of the domain list.
- P-0.8-7. Ensures: an edge with `test = true` is never in the result. Argument: the filter first requires `test = false`.
- P-0.8-8. Evidence: test.

---

### 0.9 Architecture Diagram

**Goal:** Add the `diagram` command, which draws the module graph as a Mermaid flowchart grouped by layer, and write the first `docs/architecture.md`.

- **Implements:** G20. **Depends on:** 0.8.
- **Files:** create `xtask/src/mermaid.rs`, `docs/architecture.md`. Modify `xtask/src/main.rs`, `justfile`.

**Model.** `render(graph) -> String`: `flowchart LR`, one `subgraph` per layer, one node per module. The node ID is the path with `::` replaced by `_`. The label is the last segment. One arrow for each distinct `(from, to)` with `test = false`. Nodes and arrows are sorted. `docs/architecture.md` has a title, one line that says CI generates it, and the Mermaid block. `just diagram` writes it.

**Acceptance criteria.**

- AC-0.9-1: When `render` runs two times on the same graph, the outputs are equal byte for byte.
- AC-0.9-2: When two edges have the same `from` and `to`, the output has one arrow.
- AC-0.9-3: When an edge has `test = true`, the output has no arrow for it.
- AC-0.9-4: When the graph has modules in four layers, the output has four subgraphs.
- AC-0.9-5: When a module path is `crate::domain::intent`, its node ID is `crate_domain_intent`.
- AC-0.9-6: When `just diagram` runs on the repository, it writes `docs/architecture.md` with the title, the CI line and one Mermaid block. A PR does not update this file. CI writes it on `main` (sub-phase 0.11).

**Proofs.**

- P-0.9-1. Ensures: `render` is a function of the graph only. Argument: nodes and arrows are in `BTreeSet`s, and `render` reads no clock, environment or random source.
- P-0.9-2. Ensures: the output has one arrow for each distinct `(from, to)`. Argument: arrows are a set keyed by `(from, to)`.
- P-0.9-3. Ensures: no arrow comes from an edge with `test = true`. Argument: only edges with `test = false` are inserted.
- P-0.9-4. Ensures: the number of subgraphs equals the number of distinct layers. Argument: one subgraph for each distinct layer in the node set.
- P-0.9-5. Ensures: the node ID is the path with each `::` replaced by `_`. Argument: path segments are Rust identifiers, and `::` is replaced with `_`.
- P-0.9-6. Evidence: test.

---

### 0.10 PR Diagram with Change Colors

**Goal:** Add `diagram --base <ref>`, which colors new modules green, updated modules orange and deleted modules red, and the `diagram` CI job that posts it on each PR.

- **Implements:** G20. **Depends on:** 0.9.
- **Files:** modify `xtask/src/mermaid.rs`, `xtask/src/graph.rs`, `xtask/src/main.rs`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`, `.github/pull_request_template.md`.

**Model.**

- The base graph is built from the files at `<ref>` (`git show <ref>:<path>`). The head graph is built from the working tree.
- `status(module)` over both module sets: `Added` (head only, class `added`, fill `#2da44e`), `Updated` (both, different hashes, class `updated`, fill `#fb8500`), `Deleted` (base only, class `deleted`, fill `#cf222e`, dashed arrows), `Unchanged` (no class).
- The `diagram` job runs only for `pull_request`, with `--base origin/${{ github.base_ref }}` (the layer below in a stack). It checks out with `fetch-depth: 0` and has the permission `pull-requests: write`. It writes the job summary and creates or updates one PR comment that starts with `<!-- jarvis-architecture -->`.

**Acceptance criteria.**

- AC-0.10-1: When a module is only in head, its node has class `added`.
- AC-0.10-2: When a module is in both with different content, its node has class `updated`.
- AC-0.10-3: When a module is only in base, the output has its node with class `deleted` and its arrows dashed.
- AC-0.10-4: When a module is in both with the same content, its node has no class.
- AC-0.10-5: When base and head are the same, the output has no class assignments.
- AC-0.10-6: When the output defines the classes, the fills are `#2da44e`, `#fb8500` and `#cf222e`.
- AC-0.10-7: When `ci.yml` is parsed, the job `diagram` runs only on `pull_request`, checks out with `fetch-depth: 0`, has `pull-requests: write`, passes `origin/${{ github.base_ref }}`, and updates a comment with the marker `<!-- jarvis-architecture -->`.

**Proofs.**

- P-0.10-1 to P-0.10-4. Requires: two finite module sets `B` and `H`. Ensures: each module in `B ∪ H` gets exactly one status. Argument: `H \ B`, `B \ H`, and `B ∩ H` split by hash equality are disjoint and cover `B ∪ H`.
- P-0.10-5. Ensures: with `B = H` and equal hashes, no module has a class. Argument: the first two sets are empty and every module is `Unchanged`.
- P-0.10-6. Ensures: the three fills are `#2da44e`, `#fb8500` and `#cf222e`. Argument: the class definitions are constants.
- P-0.10-7. Evidence: test.

---

### 0.11 Diagram on `main`

**Goal:** Add the workflow that writes the full diagram to `docs/architecture.md` on each commit to `main`.

- **Implements:** G20. **Depends on:** 0.10.
- **Files:** create `.github/workflows/architecture.yml`. Modify `xtask/src/gates.rs`.

**Model.** Trigger: `push` to `main`. Permission: `contents: write`. The workflow has `concurrency: architecture` with `cancel-in-progress: false`, so two runs never push at the same time. The job runs `just diagram`. If `docs/architecture.md` changed, it commits only that file with the message `Update the architecture diagram` and the body `CI regenerated docs/architecture.md from main.`, and pushes with `GITHUB_TOKEN`. A push made with `GITHUB_TOKEN` does not start new workflow runs.

**Acceptance criteria.**

- AC-0.11-1: When `architecture.yml` is parsed, it runs on `push` to `main` only, has `contents: write` and the concurrency group `architecture`, runs `just diagram`, and commits only `docs/architecture.md`.
- AC-0.11-2: When the workflow step script runs on a clean working tree, it makes no commit.

**Proofs.**

- P-0.11-2. Requires: `render` depends only on `src/` (P-0.9-1). Ensures: the workflow commits at most once for each push to `main`. Argument: the commit step is guarded by `git diff --quiet docs/architecture.md`. The CI commit does not start a new run, a rerun on the same source renders the same file, and the concurrency group runs one workflow at a time.
- P-0.11-1. Evidence: test.

---

### 0.12 Quint Gate

**Goal:** Add the `quint` gate in the hook and in CI. It type checks, tests, simulates and model checks every model in `quint/` with fixed seeds.

- **Implements:** G19, G22. **Depends on:** 0.11.
- **Files:** modify `justfile`, `.github/workflows/ci.yml`, `.gitignore`, `xtask/src/gates.rs`, `.github/pull_request_template.md`.

**Model.** Each model defines `val allInvariants`. `just quint` runs for each `quint/*.qnt`: `quint typecheck`, `quint test --seed=1`, `quint run --seed=1 --invariant=allInvariants --max-samples=10000 --max-steps=30` and `quint verify --invariant=allInvariants --max-steps=10`. With no model, it exits with code 0. `check` runs `quint` after `architecture`. CI job `quint` with Node and Java 17. The `rust-test` job also installs Node and the Quint CLI, because `quint-connect` runs Quint. `.gitignore` adds `_apalache-out/`.

**Acceptance criteria.**

- AC-0.12-1: When `quint/` has no model, `just quint` exits with code 0.
- AC-0.12-2: When a fixture model has an invariant that a reachable state breaks, the `quint` commands exit with a non-zero code and print a trace.
- AC-0.12-3: When a fixture model has a true invariant, the `quint` commands exit with code 0.
- AC-0.12-4: When `just quint` runs two times on the same model, the exit codes and the pass or fail result of each test and invariant are the same. Timings in the output can differ.
- AC-0.12-5: When the `justfile` is dumped, `check` runs `quint` after `architecture`. When `ci.yml` is parsed, the job `quint` runs `just quint`, and the job `rust-test` installs the Quint CLI.

**Proofs.**

- P-0.12-4. Argument: `quint test` and `quint run` use the fixed seed `1`, and `quint verify` is exhaustive to the step bound, so no command depends on a random source.
- P-0.12-1 to P-0.12-3, P-0.12-5. Evidence: test.

---

### 0.13 GPU Runner

**Goal:** Add the `gpu-test` CI job on the GPU host as a self-hosted runner. Every `[GPU host]` criterion is gated on each PR, on the real hardware.

- **Implements:** G18, G22. **Depends on:** 0.12.
- **Files:** modify `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`, `.github/pull_request_template.md`.

**Model.** `gpu-test` runs `cargo test --workspace --release -- --ignored` with `JARVIS_DEVICE=cuda`. The GPU host also runs the live Jarvis as the systemd unit `jarvis` (`01-architecture.md` section 10). Thus the `gpu-test` job stops the `jarvis` service before the tests and starts it again after them, also when they fail, so the tests get the whole GPU and the latency is not noisy. Live Jarvis is down while the job runs: a push of a stack with N layers stops it N times. The CI job `gpu-test` runs on `[self-hosted, linux, gpu]` for `push` and `pull_request`. On the GPU host, `check` also runs `gpu-test` when `JARVIS_DEVICE=cuda` is set, so the hook gates it there.

**Acceptance criteria.**

- AC-0.13-1: When `ci.yml` is parsed, the job `gpu-test` runs on `[self-hosted, linux, gpu]` and runs `just gpu-test`.
- AC-0.13-2: When the `justfile` is dumped, `gpu-test` sets `JARVIS_DEVICE=cuda` and passes `--ignored`, and `check` runs `gpu-test` only when `JARVIS_DEVICE=cuda` is set.
- AC-0.13-3: When there are no ignored tests, `just gpu-test` exits with code 0.
- AC-0.13-4: When `ci.yml` is parsed, the job `gpu-test` stops the `jarvis` service before `just gpu-test` and has a step with `if: always()` that starts it again.

**Proofs.**

- P-0.13-1 to P-0.13-4. Evidence: test.

**Field log.** On the GPU host, install the NVIDIA driver, CUDA 12 and cuDNN 9. Register the GitHub Actions runner with the labels `self-hosted`, `linux` and `gpu`. Create the systemd unit `jarvis` and its environment file (`01-architecture.md` section 10). Until sub-phase 5.4, `jarvis` prints its usage and exits, so `stop` and `start` succeed. Let the runner user run `systemctl stop jarvis` and `systemctl start jarvis` without a password. Record that the `gpu-test` job runs on the runner.

---

### 0.14 Release Workflow

**Goal:** Add the release workflow for `v*` tags. It runs CI, builds Jarvis for Linux x86-64 and Intel macOS, verifies each archive, and publishes a GitHub Release.

- **Implements:** G17. **Depends on:** 0.13.
- **Files:** create `.github/workflows/release.yml`. Modify `xtask/src/gates.rs`.

**Model.** The workflow calls `ci.yml` first. A build matrix runs `cargo build --release`:

| Target | Runner |
|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` |
| `x86_64-apple-darwin` | `macos-15-intel` |

Each build makes `jarvis-<tag>-<target>.tar.gz` with the binary and its shared libraries. A verification step fails the job unless `file jarvis` reports an x86-64 ELF executable (Linux) or an x86_64 Mach-O executable (macOS), and `./jarvis` exits with code 0. A last job runs `gh release create` with both archives. The GPU host runs the Linux binary in a checkout of the repository at the same tag. The checkout has `train/`, `intents.toml`, `responses.toml`, `jarvis.toml` and the `.venv` of `just setup`. Sub-phase 3.10 adds the training image. Sub-phase 6.2 adds the CUDA libraries to the Linux archive. When GitHub stops the x86-64 macOS runners (fall 2027), the macOS job builds on arm64 with `--target x86_64-apple-darwin`.

**Acceptance criteria.**

- AC-0.14-1: When `release.yml` is parsed, it runs on tags `v*`, calls `ci.yml` first, and has the two targets on the two runners.
- AC-0.14-2: When `release.yml` is parsed, each build job has the verification step before the upload, and the release job needs both builds.

**Proofs.**

- P-0.14-1, P-0.14-2. Evidence: test.

**Field log.** Push the tag `v0.0.1`. On the Intel MacBook Pro, extract the macOS archive, run `xattr -d com.apple.quarantine jarvis`, and record that `jarvis` runs.

---

### 0.15 Merge Rules

**Goal:** Add `merge_allowed`, the pure rule that decides whether a PR can merge. A free private repository cannot block merges on GitHub, so this rule is the gate: every required check passed, every review thread is resolved, and the PR follows the PR rules.

- **Implements:** G17, G21, G22. **Depends on:** 0.14.
- **Files:** create `xtask/src/merge.rs`. Modify `xtask/src/main.rs`.

**Model.** `merge_allowed(pr, required) -> Result<(), Vec<Reason>>`. `pr` has the title, the branch, the base, the changed files, the description, the check results and the review threads. The conditions:

- Each required check (the job names in `ci.yml` that run on `pull_request`) has conclusion `success`.
- Every review thread is resolved. The required checks include `codex-review` and `codex-post`, so the head commit has a Codex review (sub-phase 0.5).
- Every check box in the description is checked.
- The base is `main`.
- The title and the branch follow one kind of PR (`02-rules.md` section 7.1): a sub-phase (`<phase>.<n> <Title>` and `<phase>.<n>-<short-title>`, where `<n>` is a number with an optional letter), a spec PR (`Spec: <Title>` and `spec-<short-title>`) or a fix PR (`Fix: <Title>` and `fix-<short-title>`).
- A spec PR changes only files in `docs/spec/` and `CLAUDE.md`.
- The `Summary` section has two or three sentences.
- The description has no `Co-Authored-By` line and no `Generated with` line.

**Acceptance criteria.**

- AC-0.15-1: When one required check has conclusion `failure`, `merge_allowed` returns that check as a reason.
- AC-0.15-2: When a required check is missing, `merge_allowed` returns it as a reason.
- AC-0.15-3: When one review thread is not resolved, `merge_allowed` returns a reason.
- AC-0.15-4: When one check box is not checked, `merge_allowed` returns a reason.
- AC-0.15-5: When the base is not `main`, `merge_allowed` returns a reason.
- AC-0.15-6: When all conditions hold, `merge_allowed` returns `Ok`.
- AC-0.15-7: When the title follows no kind of PR, or the branch does not follow the kind of the title, `merge_allowed` returns a reason.
- AC-0.15-8: When the `Summary` section has one sentence or four sentences, `merge_allowed` returns a reason.
- AC-0.15-9: When the description has a `Co-Authored-By` line, `merge_allowed` returns a reason.
- AC-0.15-10: When the title and the branch are `4.5b Title` and `4.5b-title`, `Spec: Title` and `spec-title`, or `Fix: Title` and `fix-title`, `merge_allowed` returns no reason for them.
- AC-0.15-11: When a spec PR changes a file outside `docs/spec/` and `CLAUDE.md`, `merge_allowed` returns a reason that names the file.

**Proofs.**

- P-0.15-1 to P-0.15-11. Requires: the PR data and the required set. Ensures: `Ok` if and only if all conditions hold, and otherwise the result lists each failed condition. Argument: `merge_allowed` evaluates each condition, collects a reason for each failure, and returns `Ok` only when the list is empty. The title rule is one match over the three kinds of PR.

---

### 0.16 Merge Command

**Goal:** Add `xtask merge` and `just merge`, which read the PR from GitHub, apply the merge rules and squash merge at the inspected head commit. A PR that changes after the check cannot merge.

- **Implements:** G17, G21, G22. **Depends on:** 0.15.
- **Files:** modify `xtask/src/merge.rs`, `xtask/src/main.rs`, `justfile`, `xtask/src/gates.rs`.

**Model.**

- `xtask merge <pr>` reads the PR with `gh pr view --json` (with `headRefOid` and the changed files). It reads all pages of the review threads with `gh api graphql`. Then it calls `merge_allowed`. A failed `gh` call stops the command with code 1 and the error.
- When `merge_allowed` returns `Ok`, the command runs `gh pr merge <pr> --squash --match-head-commit <headRefOid> --subject <title> --body <summary>`. GitHub refuses the merge when the head changed after the read. Else the command prints each reason and exits with code 1.
- The recipe `merge` runs `cargo run -p xtask -- merge <pr>`.
- Tests put a fake `gh` script first in `PATH`. It prints fixture JSON and records its arguments.

**Acceptance criteria.**

- AC-0.16-1: When `merge_allowed` returns `Ok`, the merge command has `--match-head-commit` with the `headRefOid` that was read, `--subject` with the title and `--body` with the summary.
- AC-0.16-2: When the review threads span two GraphQL pages and one thread on the second page is not resolved, `xtask merge` refuses the merge.
- AC-0.16-3: When a `gh` call fails, `xtask merge` exits with code 1, prints the error and runs no merge.
- AC-0.16-4: When the `justfile` is dumped, `merge` runs `xtask merge`.

**Proofs.**

- P-0.16-1. Argument: the command builds the merge arguments from the values that it read and gave to `merge_allowed`. It runs the merge only in the `Ok` arm.
- P-0.16-3. Argument: each `gh` call returns a `Result`, and the command returns at the first `Err`, before the merge call.
- P-0.16-2, P-0.16-4. Evidence: test.

---

### 0.17 Pre-Push Guard

**Goal:** Add the pre-push guard, which refuses a push to `main` from a local repository. Only `just merge` and the CI diagram commit change `main`.

- **Implements:** G17, G22. **Depends on:** 0.16.
- **Files:** modify `xtask/src/merge.rs`, `xtask/src/main.rs`, `justfile`, `xtask/src/gates.rs`.

**Model.** `xtask guard-push` reads the pre-push lines from standard input. It exits with code 1 when any line has the remote ref `refs/heads/main`, also for a deletion. `pre-commit` does not pass these lines to its hooks, so the guard is a direct Git hook: `setup` writes `.git/hooks/pre-push`, which runs `xtask guard-push`.

**Acceptance criteria.**

- AC-0.17-1: When one line of the pre-push input has the remote ref `refs/heads/main`, `guard-push` exits with code 1, also when it is the second line or a deletion. When every line has the remote ref `refs/heads/2.3-ha-requests`, it exits with code 0.
- AC-0.17-2: When the `justfile` is dumped, `setup` writes `.git/hooks/pre-push` that runs `xtask guard-push`.
- AC-0.17-3: When a temporary repository with the hook pushes a branch and `main` in one `git push` to a temporary bare repository, the push fails and the bare repository has no `main`.

**Proofs.**

- P-0.17-1. Ensures: the exit code is 1 if and only if a line has the remote ref `refs/heads/main`. Argument: `guard-push` is `any(line.remote_ref == "refs/heads/main")` over all lines, mapped to exit code 1. It does not skip deletions.
- P-0.17-2, P-0.17-3. Evidence: test.

---

### 0.18 Verus Gate

**Goal:** Add the `verus` gate in the hook and in CI. From sub-phase 2.1 on, each domain sub-phase writes Verus proofs, and this gate proves them before the merge.

- **Implements:** G22, G23. **Depends on:** 0.17.
- **Files:** create `src/domain/contracts.rs`. Modify `Cargo.toml`, `rust-toolchain.toml`, `src/lib.rs`, `justfile`, `.github/workflows/ci.yml`, `.github/pull_request_template.md`, `xtask/src/gates.rs`.

**Model.**

- `Cargo.toml`: the `jarvis` package depends on `vstd` and has `[package.metadata.verus] verify = true`. The workspace lints declare the `verus_only` cfg (`unexpected_cfgs`), so a normal build gives no warning.
- `rust-toolchain.toml`: the channel is the Rust version that the pinned Verus release requires.
- `src/domain/contracts.rs` has its module doc comment and an empty `verus!` block. `src/lib.rs` declares the module `domain::contracts`.
- `justfile`: the variable `verus_version` and the recipe `install-verus`. The recipe downloads the release `verus_version` for the platform (`x86-linux`, `x86-macos` or `arm64-macos`) from the Verus GitHub releases, unpacks it to `~/.verus/<verus_version>/`, and installs the Rust toolchain that the release requires. The recipe `verus` runs `cargo verus verify -p jarvis` with that folder on the `PATH`. `check` runs `verus` after `quint`.
- CI job `verus` on `ubuntu-latest`: `just install-verus`, then `just verus`. The job `rust-test` also runs `just install-verus`, for the gate tests.
- `.github/pull_request_template.md`: a check box for `verus`.
- `verus_findings(files) -> Vec<Finding>` in `xtask/src/gates.rs` reads Rust files with `syn`. It does not enter a `#[cfg(test)]` module. It gives a finding for:
  1. Each function outside a `verus!` block without `#[verus_spec]` or `#[verus_verify]` on the function or on its `impl` block.
  2. Each trusted item in a file other than `src/domain/contracts.rs`: an `assume_specification`, a call of `assume` or `admit`, or an attribute whose name starts with `external`. This check also reads the tokens inside `verus!` blocks.
- A structure test runs `verus_findings` on `src/domain/`.

**Acceptance criteria.**

- AC-0.18-1: When a fixture crate has a function whose `ensures` clause does not hold, `cargo verus verify` exits with a non-zero code and names the function. When the clause holds, it exits with code 0.
- AC-0.18-2: When a fixture function returns the sum of two `u32` parameters and has no `requires` clause, `cargo verus verify` exits with a non-zero code. With `requires a < 1000, b < 1000`, it exits with code 0.
- AC-0.18-3: When `just verus` runs two times on the same code, the exit codes and the result of each function are the same.
- AC-0.18-4: When a fixture domain file has a function without `#[verus_spec]` or `#[verus_verify]`, `verus_findings` returns that function. A method in an `impl` block with `#[verus_verify]`, a function in a `#[cfg(test)]` module and a spec function in a `verus!` block give no finding.
- AC-0.18-5: When a fixture domain file has an `assume_specification`, a call of `admit` in a `verus!` block and a `#[verus_verify(external_body)]` attribute, `verus_findings` returns each of them with the file. The same file at the path `src/domain/contracts.rs` gives no finding.
- AC-0.18-6: When `verus_findings` runs on `src/domain/`, it returns no finding.
- AC-0.18-7: When `verus --version` runs, the version in its `Toolchain` line equals the channel of `rust-toolchain.toml`.
- AC-0.18-8: When the `justfile` is dumped, `check` runs `verus` after `quint`, and `verus` runs `cargo verus verify -p jarvis`. When `ci.yml` is parsed, the job `verus` runs `just install-verus` and `just verus`, and the job `rust-test` runs `just install-verus`. The PR template has a check box for `verus`.

**Proofs.**

- P-0.18-4, P-0.18-5. Ensures: a function outside the test modules and the `verus!` blocks is in the result if and only if neither it nor its `impl` block has `#[verus_spec]` or `#[verus_verify]`. A trusted item is in the result if and only if its file is not `src/domain/contracts.rs`. Argument: the walk visits each item of each file once and does not enter a `#[cfg(test)]` module. The attribute check is a disjunction over the function and its `impl` block. The trusted-item check reads every token of the file, also inside `verus!` blocks, and then filters by the file path.
- P-0.18-1 to P-0.18-3, P-0.18-6 to P-0.18-8. Evidence: test.
