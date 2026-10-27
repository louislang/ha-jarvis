# Jarvis

Voice-controlled home automation for Home Assistant, written in Rust. `docs/spec/` is the source of truth: goals and phase index (`00-overview.md`), architecture and allowed files (`01-architecture.md`), rules (`02-rules.md`), and one file for each phase.

## Principles

- The best code is the code that is not written. The least amount of code that meets the acceptance criteria is the ideal.
- Do not write code you were not asked to write. Build only what the spec specifies or what the user explicitly asks for. No extra features, options, helpers, abstractions or "just in case" code. If an addition seems useful, ask first.
- Do not add files that `01-architecture.md` section 8 does not list. To add a file, change the spec first, with the user's approval.
- Keep the spec in ASD-STE100 Simplified Technical English: short sentences, active voice, present tense.

## Workflow

1. One sub-phase is one PR with one concern. Work in the order of the phase index. A spec PR (`Spec: <Title>`) changes only `docs/spec/` and `CLAUDE.md`. A fix PR (`Fix: <Title>`) fixes a defect or updates a dependency on `main`. Both have the base `main` (`02-rules.md` section 7.1).
2. Start each sub-phase in a fresh context: a new session that reads only this file, `00-overview.md`, `01-architecture.md`, `02-rules.md`, that sub-phase's section, and the sections that its proofs, criteria and model cite.
3. Each phase is one stack of GitHub native stacked PRs (`gh stack`), trunk `main`, one layer per sub-phase. After a change to a lower layer, run `gh stack rebase` and `gh stack push`.
4. Before any code, write a test for every acceptance criterion: automated test (untagged), `[Bench]` instruction budget, `[GPU host]` `#[ignore]` test, or `[Quint]` model test or invariant. Name each test after its criterion ID (`ac_2_3_1_...`). Run them and make sure they fail.
5. Write the least code that makes them pass.
6. The sub-phase is complete only when all of its acceptance tests pass in the required checks. A phase is complete only when all of its sub-phases' acceptance tests pass on `main`.
7. Every test is deterministic: paused tokio clock and `FakeClock`, fixed seeds, local servers only, temporary folders, deterministic Torch, Quint `--seed=1`. No wall-clock sleeps or timing asserts outside `[GPU host]` tests. Under a paused clock, no timer runs while code waits for a real socket: an adapter with timers talks to its fake server over `tokio::io::duplex` (`02-rules.md` section 6.2). Setup recipes (`models`, `fixture-model`) download model files before the tests. Tests read `models/` and do not download.
8. Coverage is more than 80% for all code written, enforced by the hook and CI from the first commit.
9. A stateful change updates the Rust code, its Quint model and its conformance driver in the same PR.
10. The PR changes at most 400 lines against the layer below (not counting `Cargo.lock`, `docs/architecture.md`, `src/testing/house.json`). If it cannot fit, split the sub-phase in a spec PR first: the first part keeps its number, and each later part adds a letter (`4.5b`).
11. The PR description follows `.github/pull_request_template.md`: a summary of two or three sentences that states what the PR actually did and which problem it solves or which feature it adds, short bullet points, and every check box. Write it after the code, from the real change. Do not copy the sub-phase goal.
12. The Codex GitHub app reviews each PR. Resolve every thread.
13. Merge only with `just merge <pr>`: it checks every required check, every review thread, every check box, the title, the branch and the summary, then squash merges the PR (the bottom layer of a stack, a spec PR or a fix PR) at the inspected head commit. The commit message is the PR title, a blank line and the PR summary. Never add "Co-Authored-By" or any AI attribution. Phase 0 layers before `just merge` exists follow the bootstrap rule in `02-rules.md` section 7.1.
14. Never skip the hooks (`--no-verify`). The pre-push hook refuses pushes to `main`.

## Required Checks

`just check` runs these in the pre-commit hook. CI runs each as a separate job. `bench` runs in the hook on Linux only. `gpu-test` runs in the hook on the GPU host only. `pr-size` and `diagram` run in CI only, because they need the PR base.

| Check | What fails it |
|---|---|
| `rust-format` | `cargo fmt --check` finds a change. |
| `rust-lint` | Any clippy pedantic finding or warning. A function over 50 lines. Dead code. `unwrap` outside tests. `unsafe`. A missing doc comment on any item. |
| `complexity` | A Rust or Python function with cyclomatic complexity over 10, Halstead volume over 1000 or Halstead difficulty over 30. A Python function over 50 code lines. |
| `duplication` | More than 3% duplicated code (`jscpd`, Rust and Python). |
| `architecture` | A layer rule is broken, or code outside `src/lib.rs` names a concrete adapter. |
| `rust-test` | A failed test (unit, gate, structure or conformance), or line coverage of 80% or less. |
| `bench` | A benchmark over its instruction budget. |
| `python` | `ruff format`, `ruff check` (all rules), `mypy --strict`, `vulture`, `interrogate` (100% docstrings) or `pytest` (coverage of 80% or less) fails. |
| `quint` | A Quint model fails type check, its tests, simulation or model checking. |
| `workflow-lint` | `actionlint` finds a problem in a workflow. |
| `gpu-test` | A `[GPU host]` test fails on the GPU host. |
| `pr-size` | The PR changes more than 400 counted lines. |
| `diagram` | The PR diagram cannot be generated. |

## Code Rules

- Rust stable, edition 2024. The other languages are Python, only in `train/`, and the inline JavaScript of `src/adapters/review.html`.
- Do not repeat code. Move shared code into one function.
- Use a crate or a tool when one gives the function. Do not write your own version.
- As few files as possible. A file has 500 lines or fewer where possible.
- Each struct and each function has one responsibility.
- Use pure functions where possible. All domain functions are pure.
- Each port has exactly one production adapter. The only exception is `Trainer` (local process and RunPod fallback). Port fakes are in `src/testing/fakes.rs` only. A fake server that only one adapter's tests use (for example `FakeHaServer`) is in the test block of that adapter.
- The same code runs on the GPU and the CPU. `device` in `jarvis.toml` selects it. No separate GPU and CPU code paths.

## Comments and Text

These rules apply to code, comments, doc comments, commit messages, PR descriptions and docs.

- Every function, method, struct, enum, trait, field, variant, constant and module has a doc comment. Python: every module, class and function has a docstring.
- A doc comment says what the item is or does, in one line where possible. It does not restate the name.
- A normal comment says only what the code cannot: a reason, a constraint or a source. Most functions have none.
- Short, terse, to the point. High signal-to-noise ratio.
- No emojis, banners or ASCII art.
- No AI-style text: no "Here we", "Let's", "Now we", "Note that", "Simply", "This function", "comprehensive", "robust", "seamlessly", "leverage", no hedging, no apologies, no summaries of what was just written.
- No commented-out code. No `TODO` or `FIXME` without an issue number.

## Python

The training script in `train/` is the only Python code (Python 3.12). Every rule in this file that can apply to Python applies to it. `02-rules.md` section 3 maps each Rust rule to its Python rule.

- `ruff` with every rule on. Ignore only the formatter conflicts (`COM812`, `ISC001`) and one side of each conflicting docstring pair (`D203`, `D213`). Test files can use `assert`.
- Every function has type annotations. `mypy --strict` passes.
- Do not turn off a rule with `# noqa` or `# type: ignore` to get past a check. Change the code instead.
- The Google Rust Style Guide rules below apply where Python has the same concept:
  - No wildcard imports.
  - Use an `Enum` instead of an unclear `bool` parameter.
  - Use f-strings instead of `+` or `%` for formatting.
  - Do not abbreviate names.
  - Raise exceptions only for real failures. Never catch a bare or blind `Exception`.
  - Keep comprehensions and generator chains short.
  - Prefer pattern matching (`match`) over check-then-index.

## Architecture

Hexagonal (ports and adapters). See `01-architecture.md` section 1.

- `src/domain/`: pure rules. No `crate::ports`, `crate::app`, `crate::adapters`, `tokio`, `std::fs`, `std::net`, `std::process`, `std::thread`, `std::env`, `std::time::SystemTime`, `std::time::Instant`, `std::collections::HashMap`, `std::collections::HashSet`, `jiff::Timestamp::now`, `jiff::Zoned::now`, `jiff::tz::TimeZone::system`, `rand::thread_rng`, `rand::rng`, `rand::random`, `rand::rngs::OsRng`, `rand::rngs::ThreadRng`. Use `BTreeMap` and `BTreeSet`. Time, randomness and the environment come in as parameters. Return typed errors, not `anyhow`.
- CPU and GPU work (STT, TTS, `predict`, voice embedding, SQLite) runs in `spawn_blocking`, never on the async workers.
- `src/ports.rs`: all port traits and their data types. A port gets a method only in the sub-phase that uses it.
- `src/app/`: use cases. No `crate::adapters`.
- `src/adapters/`: one technology each. No `crate::app`.
- `src/lib.rs`: the composition root. The only code that names concrete adapters. It declares the folders as inline `pub` modules, so there are no `mod.rs` files. `src/main.rs` only calls `jarvis::main`, so `benches/` can import the library.
- `xtask/`: developer tools (complexity, PR size, module graph, layer rules, diagram, merge gate) and the gate tests. Jarvis does not depend on it.
- `quint/`: one Quint model for each stateful component, plus the system model.

## Tests

- Unit tests go in a `#[cfg(test)] mod tests` block at the end of each file. Shared test code goes in `src/testing/` only.
- Domain tests use no fakes. Application tests use port fakes, with no network and no models. Adapter tests use local servers (`wiremock`, the fake HA server), in-memory object stores or the real models in `models/`.
- Conformance drivers (`quint-connect`) go in the test block of the module they check. The system driver is in `src/testing/e2e.rs`.
- CPU performance is an instruction budget (`[Bench]`, Gungraun). Wall-clock latency is measured only in `[GPU host]` tests.
- Tool behavior and file structure are tested in `xtask/src/gates.rs`, never left to a manual step.
- GPU host tests have `#[ignore]` and run with `just gpu-test`. CI and the MacBook run everything else on the CPU.

## Google Rust Style Guide

From <https://google.github.io/styleguide/rustguide.html>. Jarvis is application code, so this list leaves out the library-only, Chromium-only and Fuchsia-only rules.

### Organization and Imports

- Name a file or leaf module after its main item (`foo.rs` for `Foo`).
- No glob imports, except `use super::*` in a test module and `use Enum::*` inside code that is about that enum.
- One module per `use` statement. Do not nest paths inside braces (`use a::{b::C}`). rustfmt cannot enforce this on stable, so check it by hand.
- Import a trait inside the function body that uses it, unless the whole file uses its methods.
- Call free functions through their module (`mem::take`, not `take`). Use types and traits unqualified, except ambiguous names such as `io::Error`.

### Formatting

- rustfmt with the default settings.
- `#[rustfmt::skip]` only for deliberate manual alignment. Never for personal style.

### Ownership and Borrowing

- Prefer unique ownership. `Arc` is fine for ports and shared state that tasks use concurrently. Use `Rc` or `Arc` elsewhere only when unique ownership is unreasonably complex.
- Use `.clone()` for real copies, not to avoid working out a borrow. Cloning an `Arc` is fine.

### Function Inputs and Outputs

- Take the most flexible borrowed type: `&[T]` not `&Vec<T>`, `&str` not `&String`, `&T` not `&Box<T>`, `Option<&T>` not `&Option<T>`.
- Returning `Vec` is fine when it is simpler. Slices are fine as parameters.
- Almost never use `impl Into<T>` parameters.
- Use an `enum` instead of a `bool` parameter when the meaning is not clear at the call site. If you keep the `bool`, name it in a comment at the call site.
- Errors: `Option` for simple, obvious absence. `Result` for recoverable failures the caller must respond to. `panic!` only for bugs and broken invariants, never for runtime conditions.

### Types and Traits

- Use a newtype instead of a type alias when the value has its own meaning or needs type safety.
- Prefer named fields over tuples and tuple structs. Single-field newtypes and very local tuples are fine.
- Define a trait only for generic code, for `impl Trait` or `dyn Trait`, or for an extension trait. Ports are traits for this reason. Do not use traits only to organize code.
- Name extension traits `<Type>Ext`.
- If a method does what a standard trait does (`FromStr`, `Default`, `Display`), implement the trait.
- Use `#[derive]` instead of a manual implementation that derive can express.
- Implement `Clone` for most types, except types that hold external resources. Implement `Copy` only when the type will clearly stay `Copy`.
- Implement `Default` for types with `new()` or an obvious default.
- Prefer `PartialEq` and `PartialOrd`. Use `Eq` and `Ord` only when you need them, for example for map keys (`Hash` + `Eq`).
- Pair an important `From` impl with an explicit constructor (`new`, `from_foo`).

### Concurrency

- Never access the contents of a poisoned mutex.

### Macros

- Prefer, in order: functions, types and traits, then `macro_rules!`, then code generation, then procedural macros.

### Naming

- Follow the standard library conventions for `as_`, `to_` and `into_`, with the matching `self` form.
- Constructors: `new()`, `new_with_foo()`, `try_new()` for a constructor that returns `Result`, `from_foo()` with a `From` impl.
- Getters and setters: `foo()` returns `&T` (or `T` if `Copy`), `foo_mut()` returns `&mut T`, `set_foo(T)`, `take_foo()`, `replace_foo(T)`.
- Descriptive lifetime names. `'_` where it works. Single letters only for simple input and output relationships.
- Short generic names that say what the type is. `T` and `U` are fine when there are no bounds.
- Do not abbreviate words or drop letters, except for well-known abbreviations.
- If a type has `len()`, it also has `is_empty()`.

### Documentation and Comments

- Do not explain basic language features.
- `///` for items. `//!` for modules, at the top of the file. `//` for implementation notes.
- Correct capitalization and punctuation. Markdown in doc comments.
- Document the panic conditions of a function that can panic on bad input.

### Other Language Features

- Use only stable features.
- Do not run code before `main` (no `ctor`).
- Overload operators only for their mathematical or logical meaning.
- Add `#[must_use]` when ignoring the return value is a bug, or when the value carries an error or important information. Skip it when the return type already has it. Clippy pedantic `must_use_candidate` reports it.
- Add `#[must_use]` to error types and to lazily evaluated types.
- Mark a function `const` only if it will stay `const`.
- Never call `panic::catch_unwind`.

### Implementation

- Do not overuse type inference. Write the type on the `let` for `.into()`, `.parse()`, `collect()` and complex iterator results. Prefer a typed `let` over a turbofish.
- Prefer pattern matching over check-then-unwrap.
- Where a panic cannot be avoided, use `expect("reason")`, not `unwrap()`.
- To match a `#[non_exhaustive]` enum, list every variant you care about and use one wildcard arm for the rest. Use `if let` or `let ... else` when only one variant matters.
- `assert!` messages explain the expectation and show the values. They do not repeat the condition.
- Keep iterator chains short. Do not put a chain in a `for` header; bind it to a variable first. Avoid `Iterator::for_each`.
- Use `format!` or `write!` to join more than two strings, and to substitute values instead of `str::replace`.
