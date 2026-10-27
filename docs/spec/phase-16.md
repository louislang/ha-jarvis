# Phase 16: System Model

This phase composes the component models into one Quint model of the whole request lifecycle and checks the Rust application against it.

---

### 16.1 System Quint Model

**Goal:** Add `quint/jarvis.qnt`, which composes the component models into the full request lifecycle. Prove the properties that span components: one answer per request, reply before action, filler before deferred answers, and atomic model swaps.

- **Implements:** G19. **Depends on:** 15.7.
- **Files:** create `quint/jarvis.qnt`.
- **Quint:** `jarvis.qnt`, which imports one instance of `commit` and one of `announce` for each request, and one instance of `dialog`, `feedback`, `training`, `learned` and `prefetch`, with small constants. `cache_sync`, `backfill` and `speaker` are not in the composition: the request reads the cache, a backfill changes only pools, and the speaker is a slot value. Their own models cover them.

**Model.**

- Constants: two satellites, three requests, two model versions.
- State: for each request: `phase: Listening | Committed | Handling | Replied | Deferred | Done`, `handled: int` (number of `Handled` messages), `replyTime`, `callTime`, `fillerEnd`, `announceTimes`, `modelVersion`. Global: `activeModel`, plus the state of the imported models.
- Actions: `audio(r)`, `commit(r)` (from the `commit` instance of `r`), `handle(r)` (reads `activeModel` once and stores it as `modelVersion`), `reply(r)`, `fail(r)` (an error: `not-handled`, `Handling` to `Done`, `handled += 1`), `call(r)` (enabled in `Replied` or `Deferred`, because System Two and fetch jobs call after the filler), `defer(r)`, `announce(r)` (from `announce`), `ask(r)` and `answer(r)` (from `dialog`), `mark` (from `feedback`), `swap` (from `training`: `activeModel := v`).
- Invariants:
  - `OneHandledPerRequest`: `handled <= 1` for each request, and `handled = 1` when `phase` is `Replied`, `Deferred` or `Done`.
  - `ReplyBeforeCall`: when a request has a call, `callTime >= replyTime`.
  - `FillerBeforeAnswer`: each announce time of a deferred request is `>= fillerEnd + MARGIN`.
  - `ModelSwapAtomic`: each request has exactly one `modelVersion`, and it is a version that was active during its `handle` step.
- The invariants of the imported models also hold in the composition.

**Acceptance criteria.**

- AC-16.1-1 [Quint]: `quint typecheck` passes.
- AC-16.1-2 [Quint]: The four system invariants hold in simulation and to 10 steps in `quint verify`.
- AC-16.1-3 [Quint]: The invariants of each imported model hold in the composition, in simulation.
- AC-16.1-4 [Quint]: The test `ac_16_1_4_swap_during_request` passes: a swap between `commit` and `reply` does not change the `modelVersion` of the request.

**Proofs.**

- P-16.1-2. Bounded evidence to 10 steps. `OneHandledPerRequest`: `reply`, `fail`, `defer` and `ask` are the only actions that increment `handled`. Each requires `phase = Handling` and moves the phase out of `Handling`, and no action moves a request back to `Handling`. `ReplyBeforeCall`: `call(r)` is enabled only when `phase` is `Replied` or `Deferred`, so it occurs after `reply(r)` or `defer(r)`, which set `replyTime`, and time does not decrease. The Rust code starts `after` only after the socket write (P-5.2-4). `FillerBeforeAnswer`: `announce(r)` uses the `start` guard of `announce` (P-4.7-2). `ModelSwapAtomic`: `handle(r)` writes `modelVersion` once from `activeModel`. No later action writes it, and `swap` writes only `activeModel`.
- P-16.1-3. Argument: each imported action keeps its own guard and effect, and the composition adds no action that writes the state of an imported model. So each inductive invariant of a component (Phases 4 to 15) still holds.
- P-16.1-1, P-16.1-4. Evidence: model check.

---

### 16.2 System Conformance

**Goal:** Add a conformance test that replays `jarvis.qnt` traces against the application with all port fakes. The composed Rust use cases behave like the system model, not only each part alone.

- **Implements:** G19. **Depends on:** 16.1.
- **Files:** modify `src/testing/e2e.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.** The driver builds `App` with `FakeHome`, `FakeStt`, `FakeTts`, `FakeClassifier`, `FakeReasoner`, `FakeLog`, `FakeTrainer` and `FakeClock`. Each model action maps to one driver step: audio and commit through `Listening`, `handle` through `Assistant::handle`, swaps through the training use case, marks through `Review`. After each step, the driver reads the request phases, the recorded calls and announces, and the active model version.

**Acceptance criteria.**

- AC-16.2-1: When `quint-connect` replays 100 traces of 30 steps of `jarvis.qnt`, the driver state equals the model state after each step.
- AC-16.2-2: When a trace has a swap between `commit` and `reply`, the reply uses the model version from the `handle` step.

**Proofs.**

- P-16.2-1, P-16.2-2. Evidence: test. Together with P-16.1-2, they give the four system invariants for the Rust application on the replayed traces.
