# Phase 16: System Model

This phase composes the component models into one Quint model of the whole request lifecycle and checks the Rust application against it. Sub-phases 16.1 to 16.2b model and check the lifecycle of requests on two satellites. Sub-phases 16.3 and 16.4 add the component models to it.

---

### 16.1 System Quint Model

**Goal:** Add `quint/jarvis.qnt`, the model of the request lifecycle on two satellites. Prove the properties that span components: one answer per request, reply before action, filler before deferred answers, no talk-over between requests, and atomic model swaps.

- **Implements:** G19. **Depends on:** 15.9b.
- **Files:** create `quint/jarvis.qnt`.
- **Quint:** `jarvis.qnt`.

**Model.**

- Constants: two satellites, three requests, two model versions. Each request belongs to one satellite.
- State: for each request: `phase: Listening | Committed | Handling | Replied | Deferred | Done`, `handled: int` (number of `Handled` messages), `replyTime`, `callTime`, `idleAfterReply: bool`, `announceTimes`, `modelVersion`. For each satellite: `idle: bool`, `started` (its last started request) and `newest` (the request that Jarvis handled last on it). Global: `now`, `activeModel`.
- Actions: `audio(r)` (`started[sat(r)] := r`, `idle[sat(r)] := false`), `commit(r)`, `handle(r)` (enabled only when the request before `r` on its satellite is not `Listening` or `Committed`, because HA sends the transcripts of a satellite in order; reads `activeModel` once and stores it as `modelVersion`, and `newest[sat(r)] := r`), `reply(r)`, `failRequest(r)` (an error: `not-handled`, `Handling` to `Done`, `handled += 1`), `defer(r)`, `call(r)` (enabled in `Replied` or `Deferred`, because System Two calls after the filler; in `Deferred` only when `newest[sat(r)] = r`, because System Two calls only for the newest request), `pipelineEnd(s)` (enabled only when the last started request of `s` is `Replied`, `Deferred` or `Done`, because HA ends the pipeline after the reply; `idle[s] := true`, and each request of `s` in `Replied` or `Deferred` gets `idleAfterReply`), `announce(r)` (enabled in `Deferred` when `idle[sat(r)]`; a request that is not `newest[sat(r)]` drops its announce), `swap(v)` (`activeModel := v`), `tick` (to `now = 5`, so that the inductive check is finite). `audio` starts the requests of one satellite in order, by `started`, and `handle` keeps this order. A request that has not started is `Listening`. Requests can leave `Handling` in any order.
- Invariants:
  - `OneHandledPerRequest`: `handled <= 1` for each request, and `handled = 1` when `phase` is `Replied`, `Deferred` or `Done`.
  - `ReplyBeforeCall`: when a request has a call, `callTime >= replyTime`.
  - `FillerBeforeAnswer`: each announce of a deferred request has `idleAfterReply`: its satellite became idle after the reply, so the filler ended.
  - `ModelSwapAtomic`: each request has exactly one `modelVersion`, and it is a version that was active during its `handle` step.
  - `NoTalkOver`: each announce of a request starts when its satellite is idle and the request is the newest of its satellite.

**Acceptance criteria.**

- AC-16.1-1 [Quint]: `quint typecheck` passes.
- AC-16.1-2 [Quint]: The five system invariants hold in simulation, and `quint verify` proves them with the inductive invariant `Inductive`.
- AC-16.1-3 [Quint]: The test `ac_16_1_3_swap_during_request` passes: a swap between `commit` and `reply` does not change the `modelVersion` of the request.
- AC-16.1-4 [Quint]: The test `ac_16_1_4_two_requests_one_satellite` passes: when a second request starts on a satellite before the answer of the first, the first makes no announce.

Sub-phase 16.1b changes `newest` from `audio` to `handle`, adds `started` and adds the `newest` guard of `call` in `Deferred`.

**Proofs.**

- P-16.1-2. Inductive proof: `Inductive` implies the five invariants, holds in the initial state and is kept by each step. A bounded check to 10 steps takes about 19 minutes, and the inductive check about 1 minute and covers all lengths. `OneHandledPerRequest`: `reply`, `failRequest` and `defer` are the only actions that increment `handled`. Each requires `phase = Handling` and moves the phase out of `Handling`, and no action moves a request back to `Handling`. `ReplyBeforeCall`: `call(r)` is enabled only when `phase` is `Replied` or `Deferred`, so it occurs after `reply(r)` or `defer(r)`, which set `replyTime`, and time does not decrease. The Rust code starts `after` only after the socket write (P-5.2-4). `FillerBeforeAnswer`: `announce(r)` requires `idle[sat(r)]`. `idle` becomes true only in `pipelineEnd`, which runs only after the last started request of the satellite replied and sets `idleAfterReply` for a replied request, and `audio` of a newer request sets `idle` to false. `ModelSwapAtomic`: `handle(r)` writes `modelVersion` once from `activeModel`. No later action writes it, and `swap` writes only `activeModel`. `NoTalkOver`: `announce(r)` requires `idle[sat(r)]` and drops the announce of a request that is not `newest[sat(r)]` (P-4.7-2, P-4.7-4).
- P-16.1-1, P-16.1-3, P-16.1-4. Evidence: model check.

---

### 16.1b Newest Request at Handle

**Goal:** Make the newest request of a satellite the request that Jarvis started to handle last. A newer request then drops the older pending answers of its satellite, also when it fails, and requests that finish out of order keep the order of `handle`.

- **Implements:** G6, G19. **Depends on:** 16.1.
- **Files:** modify `src/app/conversation.rs`, `quint/jarvis.qnt`.
- **Quint:** `jarvis.qnt`, and the conformance drivers of `conversation.rs` that the change affects.

**Model.**

- Rust: `handle` increases the generation of the satellite once for a request with a satellite, after `take_dialog` and before the prediction. Before this sub-phase, `dispatch` increased it (sub-phase 4.9c), so a failed request did not increase it, and a request that finished later took the newest generation.
- The `Origin` of the request keeps this generation. `dispatch` gets it and does not increase the generation. The `after` work, the deferred jobs and the questions of the request use it.
- The increase closes the dialog of the satellite. Thus a cancel on a satellite does not close it again, and each request on a satellite increases the generation once, as before (sub-phase 10.5b).
- `jarvis.qnt`: `handle(r)` sets `newest[sat(r)]`, not `audio(r)`. `handle(r)` requires that the request before `r` on its satellite is not `Listening` or `Committed`. The Wyoming server runs each `handle` connection on its own task, so the order comes from HA, which sends the transcripts of a satellite in order. The new variable `started` keeps `audio` in order and gives the guard of `pipelineEnd`. `call(r)` in `Deferred` requires `newest[sat(r)] = r`. `Inductive` changes with the new variable and guards.
- The PR changes the Rust code, `jarvis.qnt` and each conformance driver that the change affects (`02-rules.md` section 4, rule 7).

**Acceptance criteria.**

- AC-16.1b-1 [Quint]: The five system invariants hold in simulation, and `quint verify` proves them with the inductive invariant `Inductive`. This replaces AC-16.1-2.
- AC-16.1b-2: When request 1 on a satellite defers, and request 2 on the same satellite then starts `handle` and its prediction fails, `FakeHome` records no announce of the answer of request 1.
- AC-16.1b-3: When request 2 on a satellite starts `handle` after request 1, request 2 defers, and request 1 then replies, `FakeHome` records the announce of the answer of request 2 when the satellite becomes idle.
- AC-16.1b-4: When a second request on the satellite starts `handle` before the `failure` answer of the first is announced, `FakeHome` records no announce of the first. This replaces AC-4.9c-3.

**Proofs.**

- P-16.1b-1. Argument: the arguments of P-16.1-2 hold with `started` in place of `newest` for `audio` and `pipelineEnd`. `announce(r)` and `call(r)` in `Deferred` compare `newest`, which only `handle` writes. `handle` keeps the order of `audio`, so when `idle[s]` is true, `newest[s]` is the last started request or an earlier one, and each of them left `Handling` before `pipelineEnd`.
- P-16.1b-2 (AC-16.1b-2). Requires: `NoStaleAnnounce` (P-4.7-4). Argument: `handle` of request 2 increased the generation before the prediction, and the job of request 1 compares its generation before the announce (P-10.5b-1).
- P-16.1b-3 (AC-16.1b-3). Requires: `AnnounceWhenIdle` (P-4.7-2). Argument: the `handle` of request 2 gave the last increase. The dispatch of request 1 does not increase the generation, so the generation of request 2 stays current.
- P-16.1b-4 (AC-16.1b-4). Requires: `NoStaleAnnounce` (P-4.7-4). Argument: the item compares its generation before the announce, and the `handle` of the second request increased it.

---

### 16.2 System Conformance

**Goal:** Add a conformance test that replays `jarvis.qnt` traces against the application with all port fakes, and checks the request phases and the model versions. The composed Rust use cases behave like the system model, not only each part alone.

- **Implements:** G19. **Depends on:** 16.1b.
- **Files:** modify `src/testing/e2e.rs`, `src/testing/fakes.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.**

- The driver builds `App` with `FakeHome`, `FakeStt`, `FakeTts`, `FakeClassifier`, `FakeReasoner`, `FakeLog`, `FakeTrainer` and `FakeClock`. Each model version is one `FakeClassifier`.
- `FakeClassifier` can hold each prediction of one text until the test sends its result. It gives the scripted predictions to the other texts, for example the held-out examples of a training run.
- Each model action maps to one driver step:
  - `audio(r)`: `Assistant::listen` and one audio chunk. `commit(r)`: `Listening::finish`.
  - `handle(r)`: `Assistant::handle` with the transcript and the satellite of `r`, until its prediction waits. The model version of `r` is the `FakeClassifier` that holds its prediction.
  - `reply(r)`, `defer(r)` and `failRequest(r)`: the held prediction of `r` gets a confident command, `unknown` or an error.
  - `swap(v)`: a training run (`Training::run`) whose `FakeModelLoader` gives the `FakeClassifier` of version `v`.
  - `call`, `pipelineEnd`, `announce` and `tick` change nothing that this driver compares. Sub-phase 16.2b maps them.
- After each step, the driver reads the phase, the `Handled` count and the model version of each request, and the active model version.

**Acceptance criteria.**

- AC-16.2-1: When `quint-connect` replays 100 traces of 30 steps of `jarvis.qnt`, the phase, the `Handled` count and the model version of each request and the active model version of the driver equal the model state after each step.
- AC-16.2-2: When a trace has a swap between `commit` and `reply`, the reply uses the model version from the `handle` step.

**Proofs.**

- P-16.2-1, P-16.2-2. Evidence: test. Together with P-16.1-2, they give `OneHandledPerRequest` and `ModelSwapAtomic` for the Rust application on the replayed traces.

---

### 16.2b System Conformance of Calls and Announces

**Goal:** Extend the system driver to the calls, the end of the HA pipeline and the announces. The driver then compares the full model state.

- **Implements:** G19. **Depends on:** 16.2.
- **Files:** modify `src/testing/e2e.rs`, `src/testing/fakes.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.**

- `FakeReasoner` can give an answer whose steps the test sends one at a time.
- The driver of sub-phase 16.2 maps the other actions:
  - `call(r)`: for a `Replied` request, the `after` work of its reply runs. For a `Deferred` request, its System Two answer gets one `Call` step.
  - `pipelineEnd(s)`: a state event of `FakeHome` sets the satellite entity of `s` to `idle`, through the sync of the hot cache. `audio(r)` also sends a state event that sets it to `listening`.
  - `announce(r)`: the System Two answer of `r` gets one sentence, and the clock advances by `announce.margin_ms`. A dropped announce gives no announce in `FakeHome`.
  - `tick`: the time of the driver advances by one unit. The driver stamps each recorded call and announce with this time.
- The System Two timeout of the driver is longer than a trace, so a waiting answer does not time out.
- The driver does not compare the mute `switch` calls of a training run, because `jarvis.qnt` has no action for them.

**Acceptance criteria.**

- AC-16.2b-1: When `quint-connect` replays 100 traces of 30 steps of `jarvis.qnt`, the driver state equals the model state after each step.

**Proofs.**

- P-16.2b-1. Evidence: test. Together with P-16.1-2 and P-16.2-1, it gives the five system invariants for the Rust application on the replayed traces.

---

### 16.3 Composed Quint Model

**Goal:** Add the component models to `quint/jarvis.qnt`. Prove that the invariants of each component still hold when the components run together in the request lifecycle.

- **Implements:** G19. **Depends on:** 16.2b.
- **Files:** modify `quint/jarvis.qnt`.
- **Quint:** `jarvis.qnt`, which imports one instance of `commit` for each request, one instance of `announce` for each satellite, and one instance of `dialog`, `training`, `learned` and `prefetch`, with small constants. `cache_sync`, `backfill` and `speaker` are not in the composition: the request reads the cache, a backfill changes only pools, and the speaker is a slot value. Their own models cover them.

**Model.**

- `commit(r)` uses the `commit` instance of `r`. `announce(r)` and `pipelineEnd(s)` use the `announce` instance of `s`.
- New actions: `ask(r)` and `answer(r)` (from `dialog`; `ask` increments `handled` and moves `Handling` to `Replied`), and `swap` (from `training`: `activeModel := v`).
- The pause of sub-phase 13.10e: the `training` start actions are enabled only when no request is in `Listening` or `Handling`. `release` waits for the uses in progress, and the mute calls do not wait for a satellite. Thus the driver completes a start action without a later trace step. While `training.paused`, `audio`, `handle` and `answer` are not enabled.
- The invariants of the imported models also hold in the composition.

**Acceptance criteria.**

- AC-16.3-1 [Quint]: `quint typecheck` passes.
- AC-16.3-2 [Quint]: The invariants of each imported model hold in the composition, in simulation.
- AC-16.3-3 [Quint]: The five system invariants of sub-phase 16.1 hold in the composition, in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-16.3-2. Argument: each imported action keeps its own guard and effect, and the composition adds no action that writes the state of an imported model. The pause only adds guards. So each inductive invariant of a component (Phases 4 to 15) still holds.
- P-16.3-3. Argument: the imported actions replace the simple actions of sub-phase 16.1 with the same guards (P-4.7-2, P-4.7-4), and `ask` increments `handled` only from `Handling`. So the arguments of P-16.1-2 still apply.
- P-16.3-1. Evidence: model check.

---

### 16.4 Composed Conformance

**Goal:** Extend the system driver to the composed actions: questions, answers and training swaps. The whole Rust application now follows the composed model on the replayed traces.

- **Implements:** G19. **Depends on:** 16.3.
- **Files:** modify `src/testing/e2e.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.** The driver of sub-phases 16.2 and 16.2b maps `ask` and `answer` through `Assistant::handle` with an open dialog, and `swap` through the training use case with `FakeTrainer`. A start action of `training` also runs the pause with `FakeModels`. The driver does not compare the `switch` calls of the mute, because `jarvis.qnt` has no action for them. After each step, it also reads the dialogs and the training state.

**Acceptance criteria.**

- AC-16.4-1: When `quint-connect` replays 100 traces of 30 steps of the composed `jarvis.qnt`, the driver state equals the model state after each step.

**Proofs.**

- P-16.4-1. Evidence: test. Together with P-16.3-2 and P-16.3-3, it gives the system invariants and the component invariants for the Rust application on the replayed traces.
