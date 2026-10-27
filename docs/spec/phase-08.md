# Phase 8: Response Backfill

This phase adds new responses to a pool when the pool has too few unused responses.

---

### 8.1 Backfill Policy

**Goal:** Add the pure backfill rules: when a pool needs new responses, which candidates are valid, and how they merge. Pools can now grow without duplicates or broken templates.

- **Implements:** G5. **Depends on:** 7.5.
- **Files:** modify `src/domain/responses.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- `needs_backfill(pool, min_fresh, max_pool) -> bool`: fewer than `min_fresh` responses have `uses = 0`, and the pool has fewer than `max_pool` responses. A full pool rotates by `uses` and does not grow. Only a pool whose templates use no variable, or only `area`, gets a backfill. Thus every new response can be pre-rendered (sub-phase 6.3).
- `validate(candidate, pool, variables, max_chars) -> Result<()>`: not empty, at most `max_chars`, not equal to an existing response after `normalize`, uses only allowed variables, renders without error with `area = "kitchen"`.
- `merge(pool, candidates, max_pool) -> Pool`: appends the valid candidates with `uses = 0`, up to `max_pool` responses.
- This sub-phase adds `[backfill]` (`min_fresh`, `batch`, `max_chars`, `max_pool`).

**Acceptance criteria.**

- AC-8.1-1: When a pool has fewer than `min_fresh` responses with `uses = 0`, `needs_backfill` returns true. With exactly `min_fresh`, it returns false.
- AC-8.1-2: When a candidate equals an existing response after `normalize`, `validate` rejects it.
- AC-8.1-3: When a candidate uses a variable that the intent does not have, `validate` rejects it.
- AC-8.1-4: When a candidate is empty or longer than `max_chars`, `validate` rejects it.
- AC-8.1-5: When `merge` adds candidates, the result has the old responses unchanged, then the candidates with `uses = 0`.
- AC-8.1-6: When a pool has `max_pool` responses, `needs_backfill` returns false, and `merge` adds no candidate.
- AC-8.1-7: When a pool has a template with a variable other than `area`, `needs_backfill` returns false.

**Proofs.**

- P-8.1-1, P-8.1-6, P-8.1-7. Ensures: `needs_backfill = only_area(pool) and count(uses == 0) < min_fresh and len < max_pool`, and `merge` gives at most `max_pool` responses. Argument: `needs_backfill` is this conjunction. `merge` takes at most `max_pool - len` candidates.
- P-8.1-2 to P-8.1-4. Ensures: `validate` returns `Ok` if and only if every listed check passes. Argument: `validate` is the conjunction of the listed checks and returns the first failure.
- P-8.1-5. Ensures: the result is the old responses unchanged, then the candidates with `uses = 0`. Argument: `merge` is `pool ++ map(candidates, uses = 0)`. Concatenation keeps the prefix.

---

### 8.2 Response Generation

**Goal:** Add `generate` to the `Reasoner` port with structured output. System Two can now write new responses for a pool.

- **Implements:** G5. **Depends on:** 8.1.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/testing/fakes.rs`.

**Model.** `generate(pool name, existing, variables, n) -> Result<Vec<String>>`. The request uses a JSON schema `{ "responses": [string] }` and `bulk_max_output_tokens`. The prompt lists all existing responses and the allowed variable names.

**Acceptance criteria.**

- AC-8.2-1: When the prompt builder runs, the prompt contains all existing responses and the allowed variable names.
- AC-8.2-2: When the mock returns `{"responses": ["A", "B"]}`, `generate` returns "A" and "B".
- AC-8.2-3: When the mock returns JSON that does not match the schema, `generate` returns an error.

**Proofs.**

- P-8.2-1. Argument: the prompt builder iterates the existing list and the variable list.
- P-8.2-2, P-8.2-3. Evidence: test.

---

### 8.3 Backfill Quint Model

**Goal:** Add `quint/backfill.qnt`, the model of backfills that run beside response use. It proves that one pool never has two backfills at once and that a failed backfill never changes a pool.

- **Implements:** G5, G19. **Depends on:** 8.2.
- **Files:** create `quint/backfill.qnt`.
- **Quint:** `backfill.qnt`.

**Model.**

- Constants: `POOLS = Set("a", "b")`.
- State: `fresh: POOL -> int`, `running: POOL -> int` (backfill jobs of the pool), `version: POOL -> int`, `prevVersion: POOL -> int` and `lastAction` (history of the last step).
- Actions: `use(p)` (`fresh[p] -= 1` if positive, then `start(p)` if `fresh[p] < MIN` and `running[p] = 0`: `running[p] += 1`), `succeed(p)` (when `running[p] > 0`: `fresh[p] += BATCH`, `version[p] += 1`, `running[p] -= 1`), `fail(p)` (when `running[p] > 0`: `running[p] -= 1`). Each action first sets `prevVersion := version` and `lastAction`.
- Invariants:
  - `OneBackfillPerPool`: `running[p] <= 1` for each pool.
  - `PoolsChangeOnlyOnSuccess`: `version[p] != prevVersion[p]` implies `lastAction = succeed(p)`.

**Acceptance criteria.**

- AC-8.3-1 [Quint]: `quint typecheck` passes.
- AC-8.3-2 [Quint]: `OneBackfillPerPool` holds in simulation and to 10 steps in `quint verify`.
- AC-8.3-3 [Quint]: `PoolsChangeOnlyOnSuccess` holds in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-8.3-2. Argument: `start(p)` is the only increment of `running[p]`, and it is guarded by `running[p] = 0`. So `running[p]` goes from 0 to 1 and back, never to 2.
- P-8.3-3. Argument: each action sets `prevVersion := version` first, and only `succeed(p)` then writes `version[p]`.
- P-8.3-1. Evidence: model check.

---

### 8.4 Backfill Use Case

**Goal:** Start a backfill in the background after a response use, save the merged pool and synthesize the new responses. Pools grow without delaying any reply.

- **Implements:** G4, G5, G19. **Depends on:** 8.3.
- **Files:** modify `src/app/background.rs`, `src/app/conversation.rs`, `src/app/speech.rs`.
- **Quint:** `backfill.qnt` (conformance).

**Model.** After `record_use`, if `needs_backfill`, the use case inserts the pool name into an in-flight set. If the insert succeeds, it spawns `generate`, validates, pre-renders the new responses, sends the merge to the pools task (sub-phase 4.8, which saves it) and removes the name. New responses have `uses = 0`, so `select` prefers them, and the pre-render comes first. On error, it removes the name and changes nothing.

**Acceptance criteria.**

- AC-8.4-1: When a use makes `needs_backfill` true, `FakeReasoner` records one `generate` call.
- AC-8.4-2: When a backfill for a pool is in progress, a second use of that pool starts no second backfill.
- AC-8.4-3: When `generate` completes, the response store has the new responses and the audio cache has their audio.
- AC-8.4-4: When `generate` returns an error, the pools do not change.
- AC-8.4-5: When `quint-connect` replays 100 traces of `backfill.qnt`, the in-flight set and the pool versions match the model after each step.
- AC-8.4-6: When the backfill task is blocked inside `generate`, `select` on the same pool returns before virtual time advances.
- AC-8.4-7: When the merge reaches the pools task, the audio cache already has the audio of each new response.

**Proofs.**

- P-8.4-2. Requires: `OneBackfillPerPool` (P-8.3-2). Argument: `HashSet::insert` returns false when the name is present, and the spawn runs only when it returns true. This is the `start` guard of the model.
- P-8.4-4. Requires: `PoolsChangeOnlyOnSuccess` (P-8.3-3). Argument: the save runs only in the `Ok` branch.
- P-8.4-6. Argument: `select` reads the pools from an `ArcSwap` and holds no lock that the backfill task holds.
- P-8.4-7. Argument: the task awaits the pre-render before it sends the merge.
- P-8.4-1, P-8.4-3, P-8.4-5. Evidence: test.
