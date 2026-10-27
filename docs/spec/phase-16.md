# Phase 16: System Model

This phase composes the component models into one Quint model of the whole request lifecycle and checks the Rust application against it. Sub-phases 16.1 and 16.2 model and check the lifecycle of requests on two satellites. Sub-phases 16.3 and 16.4 add the component models to it.

---

### 16.1 System Quint Model

**Goal:** Add `quint/jarvis.qnt`, the model of the request lifecycle on two satellites. Prove the properties that span components: one answer per request, reply before action, filler before deferred answers, no talk-over between requests, and atomic model swaps.

- **Implements:** G19. **Depends on:** 15.7.
- **Files:** create `quint/jarvis.qnt`.
- **Quint:** `jarvis.qnt`.

**Model.**

- Constants: two satellites, three requests, two model versions. Each request belongs to one satellite.
- State: for each request: `phase: Listening | Committed | Handling | Replied | Deferred | Done`, `handled: int` (number of `Handled` messages), `replyTime`, `callTime`, `idleAfterReply: bool`, `announceTimes`, `modelVersion`. For each satellite: `idle: bool` and `newest` (its newest request). Global: `now`, `activeModel`.
- Actions: `audio(r)` (`newest[sat(r)] := r`, `idle[sat(r)] := false`), `commit(r)`, `handle(r)` (reads `activeModel` once and stores it as `modelVersion`), `reply(r)`, `fail(r)` (an error: `not-handled`, `Handling` to `Done`, `handled += 1`), `defer(r)`, `call(r)` (enabled in `Replied` or `Deferred`, because System Two and fetch jobs call after the filler), `pipelineEnd(s)` (`idle[s] := true`, and each request of `s` in `Replied` or `Deferred` gets `idleAfterReply`), `announce(r)` (enabled in `Deferred` when `idle[sat(r)]`; a request that is not `newest[sat(r)]` drops its announce), `swap(v)` (`activeModel := v`), `tick`.
- Invariants:
  - `OneHandledPerRequest`: `handled <= 1` for each request, and `handled = 1` when `phase` is `Replied`, `Deferred` or `Done`.
  - `ReplyBeforeCall`: when a request has a call, `callTime >= replyTime`.
  - `FillerBeforeAnswer`: each announce of a deferred request has `idleAfterReply`: its satellite became idle after the reply, so the filler ended.
  - `ModelSwapAtomic`: each request has exactly one `modelVersion`, and it is a version that was active during its `handle` step.
  - `NoTalkOver`: each announce of a request starts when its satellite is idle and the request is the newest of its satellite.

**Acceptance criteria.**

- AC-16.1-1 [Quint]: `quint typecheck` passes.
- AC-16.1-2 [Quint]: The five system invariants hold in simulation and to 10 steps in `quint verify`.
- AC-16.1-3 [Quint]: The test `ac_16_1_3_swap_during_request` passes: a swap between `commit` and `reply` does not change the `modelVersion` of the request.
- AC-16.1-4 [Quint]: The test `ac_16_1_4_two_requests_one_satellite` passes: when a second request starts on a satellite before the answer of the first, the first makes no announce.

**Proofs.**

- P-16.1-2. Bounded evidence to 10 steps. `OneHandledPerRequest`: `reply`, `fail` and `defer` are the only actions that increment `handled`. Each requires `phase = Handling` and moves the phase out of `Handling`, and no action moves a request back to `Handling`. `ReplyBeforeCall`: `call(r)` is enabled only when `phase` is `Replied` or `Deferred`, so it occurs after `reply(r)` or `defer(r)`, which set `replyTime`, and time does not decrease. The Rust code starts `after` only after the socket write (P-5.2-4). `FillerBeforeAnswer`: `announce(r)` requires `idle[sat(r)]`. `idle` becomes true only in `pipelineEnd`, which sets `idleAfterReply` for a replied request, and `audio` of a newer request sets `idle` to false. `ModelSwapAtomic`: `handle(r)` writes `modelVersion` once from `activeModel`. No later action writes it, and `swap` writes only `activeModel`. `NoTalkOver`: `announce(r)` requires `idle[sat(r)]` and drops the announce of a request that is not `newest[sat(r)]` (P-4.7-2, P-4.7-4).
- P-16.1-1, P-16.1-3, P-16.1-4. Evidence: model check.

---

### 16.2 System Conformance

**Goal:** Add a conformance test that replays `jarvis.qnt` traces against the application with all port fakes. The composed Rust use cases behave like the system model, not only each part alone.

- **Implements:** G19. **Depends on:** 16.1.
- **Files:** modify `src/testing/e2e.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.** The driver builds `App` with `FakeHome`, `FakeStt`, `FakeTts`, `FakeClassifier`, `FakeReasoner`, `FakeLog`, `FakeTrainer` and `FakeClock`. Each model action maps to one driver step: audio and commit through `Listening`, `handle` through `Assistant::handle`, `pipelineEnd` through a satellite state event of `FakeHome`, and swaps through the training use case. After each step, the driver reads the request phases, the recorded calls and announces, and the active model version.

**Acceptance criteria.**

- AC-16.2-1: When `quint-connect` replays 100 traces of 30 steps of `jarvis.qnt`, the driver state equals the model state after each step.
- AC-16.2-2: When a trace has a swap between `commit` and `reply`, the reply uses the model version from the `handle` step.

**Proofs.**

- P-16.2-1, P-16.2-2. Evidence: test. Together with P-16.1-2, they give the five system invariants for the Rust application on the replayed traces.

---

### 16.3 Composed Quint Model

**Goal:** Add the component models to `quint/jarvis.qnt`. Prove that the invariants of each component still hold when the components run together in the request lifecycle.

- **Implements:** G19. **Depends on:** 16.2.
- **Files:** modify `quint/jarvis.qnt`.
- **Quint:** `jarvis.qnt`, which imports one instance of `commit` for each request, one instance of `announce` for each satellite, and one instance of `dialog`, `feedback`, `training`, `learned` and `prefetch`, with small constants. `cache_sync`, `backfill` and `speaker` are not in the composition: the request reads the cache, a backfill changes only pools, and the speaker is a slot value. Their own models cover them.

**Model.**

- `commit(r)` uses the `commit` instance of `r`. `announce(r)` and `pipelineEnd(s)` use the `announce` instance of `s`.
- New actions: `ask(r)` and `answer(r)` (from `dialog`; `ask` increments `handled` and moves `Handling` to `Replied`), `mark` (from `feedback`), and `swap` (from `training`: `activeModel := v`).
- The invariants of the imported models also hold in the composition.

**Acceptance criteria.**

- AC-16.3-1 [Quint]: `quint typecheck` passes.
- AC-16.3-2 [Quint]: The invariants of each imported model hold in the composition, in simulation.
- AC-16.3-3 [Quint]: The five system invariants of sub-phase 16.1 hold in the composition, in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-16.3-2. Argument: each imported action keeps its own guard and effect, and the composition adds no action that writes the state of an imported model. So each inductive invariant of a component (Phases 4 to 15) still holds.
- P-16.3-3. Argument: the imported actions replace the simple actions of sub-phase 16.1 with the same guards (P-4.7-2, P-4.7-4), and `ask` increments `handled` only from `Handling`. So the arguments of P-16.1-2 still apply.
- P-16.3-1. Evidence: model check.

---

### 16.4 Composed Conformance

**Goal:** Extend the system driver to the composed actions: questions, answers, marks and training swaps. The whole Rust application now follows the composed model on the replayed traces.

- **Implements:** G19. **Depends on:** 16.3.
- **Files:** modify `src/testing/e2e.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.** The driver of sub-phase 16.2 maps `ask` and `answer` through `Assistant::handle` with an open dialog, `mark` through `Review`, and `swap` through the training use case with `FakeTrainer`. After each step, it also reads the dialogs, the marks and the training state.

**Acceptance criteria.**

- AC-16.4-1: When `quint-connect` replays 100 traces of 30 steps of the composed `jarvis.qnt`, the driver state equals the model state after each step.

**Proofs.**

- P-16.4-1. Evidence: test. Together with P-16.3-2 and P-16.3-3, it gives the system invariants and the component invariants for the Rust application on the replayed traces.
