# Phase 0: Repository and Delivery

This phase makes the repository, the gates, the PR workflow, the architecture diagram, the Quint gate, the GPU runner, the release workflow and the merge gate.

The coverage gate runs in the pre-commit hook and in CI from the first commit. Every later gate is proven by a gate test: `xtask/src/gates.rs` writes a small fixture (a crate, a Python file, a Quint model) into a temporary folder, runs the real tool, and checks the exit code and the message. Workflow and hook files are checked by structure tests that parse them. All of these are normal tests in `rust-test`.

The first commit (sub-phase 0.1) goes directly to `main`, because no PR process exists yet. From sub-phase 0.2, each phase is one stack of PRs (`02-rules.md` section 7).

---

### 0.1 Repository, Coverage Gate, Hook and CI

**Goal:** Create the repository, the Cargo workspace with an empty `jarvis` binary and an empty `xtask` tool crate, and the coverage gate in both the pre-commit hook and CI. No code exists without a gated coverage check.

- **Implements:** G10, G17, G22. **Depends on:** none.
- **Files:** create `.gitignore`, `Cargo.toml`, `Cargo.lock`, `justfile`, `.pre-commit-config.yaml`, `.github/workflows/ci.yml`, `src/main.rs`, `xtask/Cargo.toml`, `xtask/src/main.rs`, `xtask/src/gates.rs`. Add the existing `CLAUDE.md` and `docs/spec/`.

**Model.**

- `Cargo.toml` is a workspace with the members `jarvis` (root) and `xtask`.
- `src/main.rs` and `xtask/src/main.rs` have an empty `main` and one test that calls it.
- `justfile`: `test` (`cargo test --workspace --release`), `cov` (`cargo llvm-cov --workspace --release --fail-under-lines 80.01`), `check` (runs `cov`), `setup` (`pre-commit install --hook-type pre-commit --hook-type pre-push`).
- `.pre-commit-config.yaml`: one `local` hook, `entry: just check`, `language: system`, `pass_filenames: false`, `always_run: true`.
- `ci.yml`: triggers `push`, `pull_request`, `workflow_call`. Jobs `rust-test` (`just cov`) and `workflow-lint` (`actionlint`), on `ubuntu-latest`, with the cargo cache.
- `xtask/src/gates.rs` is `#[cfg(test)]`. It has the helpers `fixture_crate(source)` (a temporary crate with the workspace lint settings) and `run(command) -> Output`, and the gate and structure tests of Phase 0.
- Structure tests read `just --dump --dump-format json`, `.pre-commit-config.yaml` and the workflow files with `serde_yaml`.

**Acceptance criteria.**

- AC-0.1-1: When you run `cargo build --workspace`, it exits with code 0.
- AC-0.1-2: When `just test` runs, the tests `ac_0_1_2_main_returns` in both crates pass.
- AC-0.1-3: When a fixture crate has 10 lines of which 5 are covered, `cargo llvm-cov --fail-under-lines 80.01` exits with a non-zero code. When all lines are covered, it exits with code 0.
- AC-0.1-4: When the `justfile` is dumped, `check` runs `cov`, and `cov` has `--fail-under-lines 80.01`.
- AC-0.1-5: When `.pre-commit-config.yaml` is parsed, it has exactly one hook, with `entry: just check`, `always_run: true` and `pass_filenames: false`.
- AC-0.1-6: When `ci.yml` is parsed, it has the triggers `push`, `pull_request` and `workflow_call`, a job `rust-test` that runs `just cov`, and a job `workflow-lint` that runs `actionlint`.
- AC-0.1-7: When `git check-ignore` runs for `target/`, `models/`, `.venv/`, `habits.json`, `voices.json`, `jarvis.db`, `generated.json` and `training.json`, it lists each path.

**Proofs.**

- P-0.1-2. Requires: nothing. Ensures: `main` returns. Argument: `main` has an empty body.
- P-0.1-1, P-0.1-3 to P-0.1-7. Evidence: test.

**Field log.** Run `gh repo create jarvis --private --source . --push`. Run `just setup`. Record that the first CI run passes.

---

### 0.2 Lint Gates

**Goal:** Turn on the format and lint gates, including doc comments on every item, in the hook and in CI. Code that breaks a Rust rule fails before it can merge.

- **Implements:** G10, G22. **Depends on:** 0.1.
- **Files:** create `clippy.toml`. Modify `Cargo.toml`, `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.**

- `[workspace.lints.rust]`: `warnings = "deny"`, `dead_code = "deny"`, `missing_docs = "deny"`, `unsafe_code = "forbid"`.
- `[workspace.lints.clippy]`: `pedantic = "deny"`, `unwrap_used = "deny"`, `string_add = "deny"`, `missing_docs_in_private_items = "deny"`, `undocumented_unsafe_blocks = "deny"`.
- `clippy.toml`: `too-many-lines-threshold = 50`, `allow-unwrap-in-tests = true`.
- `format` runs `cargo fmt --all --check`. `lint` runs `cargo clippy --workspace --all-targets -- -D clippy::pedantic -D warnings`. `check` runs `format`, `lint`, then `cov`. CI jobs `rust-format` and `rust-lint`.

**Acceptance criteria.**

- AC-0.2-1: When a fixture function has 51 lines, `lint` fails with `too_many_lines`. At 50 lines, it passes.
- AC-0.2-2: When a fixture function is not used, `lint` fails with `dead_code`.
- AC-0.2-3: When fixture code outside a test calls `unwrap`, `lint` fails with `unwrap_used`. Inside a test, it passes.
- AC-0.2-4: When a fixture private function or private struct has no doc comment, `lint` fails with `missing_docs_in_private_items`.
- AC-0.2-5: When a fixture has an `unsafe` block, the build fails with `unsafe_code`.
- AC-0.2-6: When a fixture file is not formatted, `format` fails.
- AC-0.2-7: When the `justfile` is dumped, `check` runs `format`, `lint` and `cov` in that order. When `ci.yml` is parsed, the jobs `rust-format` and `rust-lint` run `just format` and `just lint`.

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
- P-0.3-6. Argument: the walk recurses on every child. The tree is finite, so it ends, and it visits each node once.
- P-0.3-7. Argument: the check is guarded by `kind == "function"`.
- P-0.3-8 to P-0.3-10. Evidence: test.

---

### 0.4 Duplication Gate

**Goal:** Add the duplication gate with `jscpd` in the hook and in CI. A change that brings duplicated code above 3% fails.

- **Implements:** G10, G22. **Depends on:** 0.3.
- **Files:** modify `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.** `duplication` runs `jscpd --threshold 3 --min-tokens 50 --format rust,python --ignore "**/target/**" src xtask train`. `check` runs it after `complexity`. CI job `duplication`.

**Acceptance criteria.**

- AC-0.4-1: When a fixture folder has two copies of a 40-line function and nothing else, `jscpd --threshold 3` exits with a non-zero code and names both locations.
- AC-0.4-2: When the fixture folder has one copy, it exits with code 0.
- AC-0.4-3: When the `justfile` is dumped, `check` runs `duplication` after `complexity`. When `ci.yml` is parsed, the job `duplication` runs `just duplication`.

**Proofs.**

- P-0.4-1 to P-0.4-3. Evidence: test.

---

### 0.5 PR Template, Stacks and Review

**Goal:** Add the PR template and the stack tooling. Every PR states what it did, confirms each required check, and sits in the phase stack, and each PR gets an AI code review.

- **Implements:** G21. **Depends on:** 0.4.
- **Files:** create `.github/pull_request_template.md`. Modify `justfile`, `xtask/src/gates.rs`.

**Model.**

- The template has three sections: `## Summary` (two or three sentences, written after the code), `## Changes` (short bullet points) and `## Checks`.
- `## Checks` has one box for each required CI job (`02-rules.md` section 7.3), then the boxes "Started in a fresh context", "Codex review threads resolved" and "`[GPU host]` criteria pass in `gpu-test`".
- `setup` also runs `gh extension install github/gh-stack`.
- The Codex GitHub app reviews each PR. Sub-phase 0.15 blocks the merge until its threads are resolved.

**Acceptance criteria.**

- AC-0.5-1: When the template is parsed, it has the sections `Summary`, `Changes` and `Checks`, in that order.
- AC-0.5-2: When the template is compared with `ci.yml`, it has exactly one check box for each required CI job name, plus the three other boxes.
- AC-0.5-3: When the `justfile` is dumped, `setup` installs `github/gh-stack`.

**Proofs.**

- P-0.5-2. Argument: the test builds the set of job names from `ci.yml` and the set of box labels from the template, and compares the sets.
- P-0.5-1, P-0.5-3. Evidence: test.

**Field log.** Install the Codex GitHub app on the repository. Record that a test PR receives a Codex review.

---

### 0.6 PR Size Gate

**Goal:** Add the `pr-size` command and CI job. A PR that changes more than 400 counted lines against the layer below it fails.

- **Implements:** G21. **Depends on:** 0.5.
- **Files:** create `xtask/src/pr_size.rs`. Modify `xtask/src/main.rs`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.**

- `xtask pr-size --base <ref> --limit <n>` runs `git diff --numstat <ref>...HEAD`.
- `counted_lines(numstat, excluded) -> Result<usize>`: each line is `<added>\t<deleted>\t<path>`. A binary file (`-\t-`) counts 0. An excluded path counts 0. Other lines count `added + deleted`. Another format is an error.
- `excluded = ["Cargo.lock", "docs/architecture.md", "src/testing/house.json"]`.
- The CI job runs only for `pull_request` with `--base origin/${{ github.base_ref }}`. In a stack, the base is the branch of the layer below, so each PR is measured alone.

**Acceptance criteria.**

- AC-0.6-1: When the numstat is `300\t0\ta.rs` and `40\t10\tb.rs`, `counted_lines` returns 350.
- AC-0.6-2: When the count is 400 and the limit is 400, the check passes. At 401, it fails and prints `401 > 400`.
- AC-0.6-3: When `Cargo.lock` has 5000 changed lines, they do not count.
- AC-0.6-4: When a binary file changes, it counts 0.
- AC-0.6-5: When a numstat line has two fields, `counted_lines` returns an error that contains the line.
- AC-0.6-6: When `ci.yml` is parsed, the job `pr-size` runs only on `pull_request` and passes `origin/${{ github.base_ref }}` as the base.

**Proofs.**

- P-0.6-1, P-0.6-3, P-0.6-4. Requires: every line parses. Ensures: the result is the sum of `added + deleted` over lines that are not excluded and not binary. Argument: a fold with a running sum, where excluded and binary lines add 0.
- P-0.6-2. Argument: the check fails on `>`, so equality passes.
- P-0.6-5. Argument: a line must split into exactly three fields, else `Err` with the line, and the fold stops at the first `Err`.
- P-0.6-6. Evidence: test.

---

### 0.7 Module Graph

**Goal:** Add the module graph builder to `xtask`. It reads the Rust source with `syn` and gives the modules, their layers and their dependencies, which the rules and the diagram need.

- **Implements:** G10, G20. **Depends on:** 0.6.
- **Files:** create `xtask/src/graph.rs`. Modify `xtask/src/main.rs`, `xtask/Cargo.toml`.

**Model.**

- A node is a module: its path (`crate::domain::intent`), its file, its layer and a hash of its file content.
- The layer is the first path segment: `domain`, `ports`, `app`, `adapters`, `testing`, or `root` for `crate`.
- The module tree comes from the `mod` items in `src/main.rs`. An inline `mod domain { pub mod intent; }` gives the file `src/domain/intent.rs`.
- An edge is `(from, to, test)`. Each leaf of a `use crate::...` tree gives one edge. Each path expression that starts with `crate::` gives one edge. `to` is the longest prefix that is a module. `test` is true inside a `#[cfg(test)]` item.
- A path to an outside crate gives an `external` edge with the crate name. `use super::*` gives no edge.

**Acceptance criteria.**

- AC-0.7-1: When `src/main.rs` has `mod domain { pub mod intent; }` and the file exists, the graph has the nodes `crate` and `crate::domain::intent` with layer `domain`.
- AC-0.7-2: When `src/app/x.rs` has `use crate::ports::Home;`, the graph has the edge `crate::app::x -> crate::ports`.
- AC-0.7-3: When a file has `use crate::{ports::A, domain::b::C};`, the graph has two edges.
- AC-0.7-4: When `src/main.rs` calls `crate::adapters::ha::HaHome::connect()`, the graph has the edge `crate -> crate::adapters::ha`.
- AC-0.7-5: When a `use crate::testing::fakes::FakeHome;` is inside `#[cfg(test)] mod tests`, its edge has `test = true`.
- AC-0.7-6: When a domain file has `use tokio::sync::Mutex;`, the graph has an external edge to `tokio`.
- AC-0.7-7: When the content of a file changes, its node hash changes.

**Proofs.**

- P-0.7-1. Ensures: the node set equals the set of declared modules. Argument: the builder adds one node for each `mod` item and from no other source.
- P-0.7-2, P-0.7-3. Ensures: one edge for each leaf of each `use` tree. Argument: a `use` tree is finite. The visitor recurses on groups and emits only at leaves.
- P-0.7-4. Argument: the expression visitor sees every path. For a `crate` path, the longest module prefix exists because `crate` is a module.
- P-0.7-5. Argument: the visitor keeps a depth counter of enclosing `#[cfg(test)]` items, and `test` is true when it is more than 0.
- P-0.7-6. Argument: a path whose first segment is not `crate`, `self` or `super` is external.
- P-0.7-7. Evidence: test.

---

### 0.8 Layer and Modularity Rules

**Goal:** Add the `rules` command and the `architecture` gate in the hook and in CI. A broken layer rule, or code outside `src/main.rs` that names a concrete adapter, fails.

- **Implements:** G10, G22. **Depends on:** 0.7.
- **Files:** create `xtask/src/rules.rs`. Modify `xtask/src/main.rs`, `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.** `violations(graph) -> Vec<RuleViolation>` checks each edge with `test = false`:

1. `domain -> ports | app | adapters`.
2. `domain -> external`, unless the crate is in the domain list of `01-architecture.md` section 7, or it is `std` except `std::fs`, `std::net`, `std::process` and `std::thread`.
3. `app -> adapters`.
4. `adapters -> app`.
5. `X -> adapters::*`, unless `X` is `root` or the same adapter module.

`architecture` runs `xtask rules`. `check` runs it after `duplication`. CI job `architecture`.

**Acceptance criteria.**

- AC-0.8-1: When a domain file uses `crate::ports`, `violations` returns rule 1 with the file and the edge.
- AC-0.8-2: When a domain file uses `tokio`, `violations` returns rule 2. With `minijinja`, none.
- AC-0.8-3: When a domain file uses `std::fs`, `violations` returns rule 2.
- AC-0.8-4: When an app file uses `crate::adapters::ha`, `violations` returns rule 3.
- AC-0.8-5: When an adapter file uses `crate::app`, `violations` returns rule 4.
- AC-0.8-6: When `crate::adapters::web` uses `crate::adapters::sqlite`, `violations` returns rule 5.
- AC-0.8-7: When an edge has `test = true`, `violations` ignores it.
- AC-0.8-8: When the `justfile` is dumped, `check` runs `architecture` after `duplication`. When `ci.yml` is parsed, the job `architecture` runs `just architecture`.

**Proofs.**

- P-0.8-1 to P-0.8-6. Ensures: an edge with `test = false` is in the result if and only if one of rules 1 to 5 matches. Argument: `violations` is a filter with the disjunction of the five rules as its predicate.
- P-0.8-7. Argument: the filter first requires `test = false`.
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
- AC-0.9-6: When `just diagram` runs on the repository, `docs/architecture.md` equals the committed file.

**Proofs.**

- P-0.9-1. Ensures: `render` is a function of the graph only. Argument: nodes and arrows are in `BTreeSet`s, and `render` reads no clock, environment or random source.
- P-0.9-2. Argument: arrows are a set keyed by `(from, to)`.
- P-0.9-3. Argument: only edges with `test = false` are inserted.
- P-0.9-4. Argument: one subgraph for each distinct layer in the node set.
- P-0.9-5. Argument: path segments are Rust identifiers, and `::` is replaced with `_`.
- P-0.9-6. Evidence: test.

---

### 0.10 PR Diagram with Change Colors

**Goal:** Add `diagram --base <ref>`, which colors new modules green, updated modules orange and deleted modules red, and the `diagram` CI job that posts it on each PR.

- **Implements:** G20. **Depends on:** 0.9.
- **Files:** modify `xtask/src/mermaid.rs`, `xtask/src/graph.rs`, `xtask/src/main.rs`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.**

- The base graph is built from the files at `<ref>` (`git show <ref>:<path>`). The head graph is built from the working tree.
- `status(module)` over both module sets: `Added` (head only, class `added`, fill `#2da44e`), `Updated` (both, different hashes, class `updated`, fill `#fb8500`), `Deleted` (base only, class `deleted`, fill `#cf222e`, dashed arrows), `Unchanged` (no class).
- The `diagram` job runs only for `pull_request`, with `--base origin/${{ github.base_ref }}` (the layer below in a stack). It writes the job summary and creates or updates one PR comment that starts with `<!-- jarvis-architecture -->`.

**Acceptance criteria.**

- AC-0.10-1: When a module is only in head, its node has class `added`.
- AC-0.10-2: When a module is in both with different content, its node has class `updated`.
- AC-0.10-3: When a module is only in base, the output has its node with class `deleted` and its arrows dashed.
- AC-0.10-4: When a module is in both with the same content, its node has no class.
- AC-0.10-5: When base and head are the same, the output has no class assignments.
- AC-0.10-6: When the output defines the classes, the fills are `#2da44e`, `#fb8500` and `#cf222e`.
- AC-0.10-7: When `ci.yml` is parsed, the job `diagram` runs only on `pull_request`, passes `origin/${{ github.base_ref }}`, and updates a comment with the marker `<!-- jarvis-architecture -->`.

**Proofs.**

- P-0.10-1 to P-0.10-4. Requires: two finite module sets `B` and `H`. Ensures: each module in `B ∪ H` gets exactly one status. Argument: `H \ B`, `B \ H`, and `B ∩ H` split by hash equality are disjoint and cover `B ∪ H`.
- P-0.10-5. Argument: with `B = H` and equal hashes, the first two sets are empty and every module is `Unchanged`.
- P-0.10-6. Argument: the class definitions are constants.
- P-0.10-7. Evidence: test.

---

### 0.11 Diagram on `main`

**Goal:** Add the workflow that writes the full diagram to `docs/architecture.md` on each commit to `main`.

- **Implements:** G20. **Depends on:** 0.10.
- **Files:** create `.github/workflows/architecture.yml`. Modify `xtask/src/gates.rs`.

**Model.** Trigger: `push` to `main`. Permission: `contents: write`. The job runs `just diagram`. If `docs/architecture.md` changed, it commits only that file with the message `Update the architecture diagram` and the body `CI regenerated docs/architecture.md from main.`, and pushes with `GITHUB_TOKEN`. A push made with `GITHUB_TOKEN` does not start new workflow runs.

**Acceptance criteria.**

- AC-0.11-1: When `architecture.yml` is parsed, it runs on `push` to `main` only, has `contents: write`, runs `just diagram`, and commits only `docs/architecture.md`.
- AC-0.11-2: When the workflow step script runs on a clean working tree, it makes no commit.

**Proofs.**

- P-0.11-2. Requires: `render` depends only on `src/` (P-0.9-1). Ensures: the workflow commits at most once for each push to `main`. Argument: the commit step is guarded by `git diff --quiet docs/architecture.md`. The CI commit does not start a new run, and a rerun on the same source renders the same file.
- P-0.11-1. Evidence: test.

---

### 0.12 Quint Gate

**Goal:** Add the `quint` gate in the hook and in CI. It type checks, tests, simulates and model checks every model in `quint/` with fixed seeds.

- **Implements:** G19, G22. **Depends on:** 0.11.
- **Files:** modify `justfile`, `.github/workflows/ci.yml`, `.gitignore`, `xtask/src/gates.rs`.

**Model.** Each model defines `val allInvariants`. `just quint` runs for each `quint/*.qnt`: `quint typecheck`, `quint test --seed=1`, `quint run --seed=1 --invariant=allInvariants --max-samples=10000 --max-steps=30` and `quint verify --invariant=allInvariants --max-steps=10`. With no model, it exits with code 0. `check` runs `quint` after `architecture`. CI job `quint` with Node and Java 17. `.gitignore` adds `_apalache-out/`.

**Acceptance criteria.**

- AC-0.12-1: When `quint/` has no model, `just quint` exits with code 0.
- AC-0.12-2: When a fixture model has an invariant that a reachable state breaks, the `quint` commands exit with a non-zero code and print a trace.
- AC-0.12-3: When a fixture model has a true invariant, the `quint` commands exit with code 0.
- AC-0.12-4: When `just quint` runs two times on the same model, the output is the same.
- AC-0.12-5: When the `justfile` is dumped, `check` runs `quint` after `architecture`. When `ci.yml` is parsed, the job `quint` runs `just quint`.

**Proofs.**

- P-0.12-4. Argument: `quint test` and `quint run` use the fixed seed `1`, and `quint verify` is exhaustive to the step bound, so no command depends on a random source.
- P-0.12-1 to P-0.12-3, P-0.12-5. Evidence: test.

---

### 0.13 GPU Runner

**Goal:** Add the `gpu-test` CI job on the GPU host as a self-hosted runner. Every `[GPU host]` criterion is gated on each PR, on the real hardware.

- **Implements:** G18, G22. **Depends on:** 0.12.
- **Files:** modify `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.** `gpu-test` runs `cargo test --workspace --release -- --ignored` with `JARVIS_DEVICE=cuda`. The CI job `gpu-test` runs on `[self-hosted, linux, gpu]` for `push` and `pull_request`. On the GPU host, `check` also runs `gpu-test` when `JARVIS_DEVICE=cuda` is set, so the hook gates it there.

**Acceptance criteria.**

- AC-0.13-1: When `ci.yml` is parsed, the job `gpu-test` runs on `[self-hosted, linux, gpu]` and runs `just gpu-test`.
- AC-0.13-2: When the `justfile` is dumped, `gpu-test` sets `JARVIS_DEVICE=cuda` and passes `--ignored`, and `check` runs `gpu-test` only when `JARVIS_DEVICE=cuda` is set.
- AC-0.13-3: When there are no ignored tests, `just gpu-test` exits with code 0.

**Proofs.**

- P-0.13-1 to P-0.13-3. Evidence: test.

**Field log.** On the GPU host, install the NVIDIA driver, CUDA 12 and cuDNN 9. Register the GitHub Actions runner with the labels `self-hosted`, `linux` and `gpu`. Record that the `gpu-test` job runs on it.

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

Each build makes `jarvis-<tag>-<target>.tar.gz` with the binary and its shared libraries. A verification step fails the job unless `file jarvis` reports an x86-64 ELF executable (Linux) or an x86_64 Mach-O executable (macOS), and `./jarvis` exits with code 0. A last job runs `gh release create` with both archives. Sub-phase 3.10 adds the training image. Sub-phase 6.2 adds the CUDA libraries to the Linux archive. When GitHub stops the x86-64 macOS runners (fall 2027), the macOS job builds on arm64 with `--target x86_64-apple-darwin`.

**Acceptance criteria.**

- AC-0.14-1: When `release.yml` is parsed, it runs on tags `v*`, calls `ci.yml` first, and has the two targets on the two runners.
- AC-0.14-2: When `release.yml` is parsed, each build job has the verification step before the upload, and the release job needs both builds.

**Proofs.**

- P-0.14-1, P-0.14-2. Evidence: test.

**Field log.** Push the tag `v0.0.1`. On the Intel MacBook Pro, extract the macOS archive, run `xattr -d com.apple.quarantine jarvis`, and record that `jarvis` runs.

---

### 0.15 Merge Gate

**Goal:** Add `just merge` and the pre-push guard. A free private repository cannot block merges on GitHub, so this gate refuses to merge a PR unless every required check passed and every review thread is resolved, and it refuses direct pushes to `main`.

- **Implements:** G17, G21, G22. **Depends on:** 0.14.
- **Files:** create `xtask/src/merge.rs`. Modify `xtask/src/main.rs`, `justfile`, `.pre-commit-config.yaml`, `xtask/src/gates.rs`.

**Model.**

- `xtask merge <pr>` reads the PR with `gh pr view --json` and its review threads with `gh api graphql`, then calls `merge_allowed(pr, required) -> Result<(), Vec<Reason>>`:
  - Each required check (the job names in `ci.yml` that run on `pull_request`) has conclusion `success`.
  - A Codex review exists, and every review thread is resolved.
  - Every check box in the description is checked.
  - The PR is the bottom of its stack: its base is `main`.
- When `merge_allowed` returns `Ok`, the command runs `gh pr merge --squash`. Else it prints each reason and exits with code 1.
- `xtask guard-push` reads the pre-push lines from standard input. It exits with code 1 when a line pushes to `refs/heads/main`. `.pre-commit-config.yaml` adds it as a `pre-push` hook.

**Acceptance criteria.**

- AC-0.15-1: When one required check has conclusion `failure`, `merge_allowed` returns that check as a reason.
- AC-0.15-2: When a required check is missing, `merge_allowed` returns it as a reason.
- AC-0.15-3: When one review thread is not resolved, `merge_allowed` returns a reason.
- AC-0.15-4: When there is no Codex review, `merge_allowed` returns a reason.
- AC-0.15-5: When one check box is not checked, `merge_allowed` returns a reason.
- AC-0.15-6: When the base is not `main`, `merge_allowed` returns a reason.
- AC-0.15-7: When all conditions hold, `merge_allowed` returns `Ok`.
- AC-0.15-8: When the pre-push input pushes to `refs/heads/main`, `guard-push` exits with code 1. When it pushes to `refs/heads/2.3-ha-requests`, it exits with code 0.
- AC-0.15-9: When `.pre-commit-config.yaml` is parsed, it has a `pre-push` hook that runs `xtask guard-push`.

**Proofs.**

- P-0.15-1 to P-0.15-7. Requires: the PR data and the required set. Ensures: `Ok` if and only if all five conditions hold, and otherwise the result lists each failed condition. Argument: `merge_allowed` evaluates each condition, collects a reason for each failure, and returns `Ok` only when the list is empty.
- P-0.15-8. Argument: `guard-push` is `any(line.remote_ref == "refs/heads/main")` mapped to exit code 1.
- P-0.15-9. Evidence: test.
