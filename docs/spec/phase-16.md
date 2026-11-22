# Phase 16: System Model

This phase composes the component models into one Quint model of the whole request lifecycle and checks the Rust application against it. Sub-phases 16.1 to 16.2c model and check the lifecycle of requests on two satellites. Sub-phases 16.3 and 16.4 compose it with the component models in `composed.qnt`.

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
- A request whose generation is no longer the generation of its satellite opens no dialog and puts back no dialog, because its question drops. `open` and the put of an unclear answer check the generation under the lock of the dialogs, so a newer `handle` cannot change it between the check and the write.
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

**Goal:** Add a conformance test that replays `jarvis.qnt` traces against the application with all port fakes, and checks the phase and the `Handled` count of each request. The composed Rust use cases behave like the system model, not only each part alone.

- **Implements:** G19. **Depends on:** 16.1b.
- **Files:** modify `src/testing/e2e.rs`, `src/testing/fakes.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.**

- The driver builds `App` with `FakeHome`, `FakeStt`, `FakeTts`, `FakeClassifier`, `FakeReasoner`, `FakeLog`, `FakeTrainer` and `FakeClock`.
- `FakeClassifier` can hold each prediction of one text until the test sends its result. It gives the scripted predictions to the other texts, for example the held-out examples of a training run.
- Each model action maps to one driver step:
  - `audio(r)`: `Assistant::listen` and one audio chunk. `commit(r)`: `Listening::finish`.
  - `handle(r)`: `Assistant::handle` with the transcript and the satellite of `r`, until its prediction waits.
  - `reply(r)`, `defer(r)` and `failRequest(r)`: the held prediction of `r` gets a confident command, `unknown` or an error.
  - `swap`, `call`, `pipelineEnd`, `announce` and `tick` change nothing that this driver compares. Sub-phase 16.2b maps `swap`, and sub-phase 16.2c maps the others.
- After each step, the driver reads the phase and the `Handled` count of each request.

**Acceptance criteria.**

- AC-16.2-1: When `quint-connect` replays 100 traces of 30 steps of `jarvis.qnt`, the phase and the `Handled` count of each request of the driver equal the model state after each step.

**Proofs.**

- P-16.2-1. Evidence: test. Together with P-16.1-2, it gives `OneHandledPerRequest` for the Rust application on the replayed traces.

---

### 16.2b System Conformance of Model Swaps

**Goal:** Extend the system driver to the model swaps. The driver then also checks the model version of each request and the active model version.

- **Implements:** G19. **Depends on:** 16.2.
- **Files:** modify `src/testing/e2e.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.**

- Each model version is one `FakeClassifier`. The model version of a request is the `FakeClassifier` that holds its prediction.
- `swap(v)`: a training run (`Training::run`) whose `FakeModelLoader` gives the `FakeClassifier` of version `v`.
- After each step, the driver also reads the model version of each request and the active model version.

**Acceptance criteria.**

- AC-16.2b-1: When `quint-connect` replays 100 traces of 30 steps of `jarvis.qnt`, the phase, the `Handled` count and the model version of each request and the active model version of the driver equal the model state after each step. This replaces AC-16.2-1.
- AC-16.2b-2: When a trace has a swap between `commit` and `reply`, the reply uses the model version from the `handle` step.

**Proofs.**

- P-16.2b-1, P-16.2b-2. Evidence: test. Together with P-16.1-2, they give `OneHandledPerRequest` and `ModelSwapAtomic` for the Rust application on the replayed traces.

---

### 16.2c System Conformance of Calls and Announces

**Goal:** Extend the system driver to the calls, the end of the HA pipeline and the announces. The driver then compares the full model state.

- **Implements:** G19. **Depends on:** 16.2b.
- **Files:** modify `src/testing/e2e.rs`, `src/testing/fakes.rs`.
- **Quint:** `jarvis.qnt` (conformance).

**Model.**

- `FakeReasoner` can give an answer whose steps the test sends one at a time.
- The driver of sub-phases 16.2 and 16.2b maps the other actions:
  - `call(r)`: for a `Replied` request, the `after` work of its reply runs. For a `Deferred` request, its System Two answer gets one `Call` step.
  - `pipelineEnd(s)`: a state event of `FakeHome` sets the satellite entity of `s` to `idle`, through the sync of the hot cache. `audio(r)` also sends a state event that sets it to `listening`.
  - `announce(r)`: the System Two answer of `r` gets one sentence, and the clock advances by `announce.margin_ms`. A dropped announce gives no announce in `FakeHome`.
  - `tick`: the time of the driver advances by one unit. The driver stamps each recorded call and announce with this time.
- The System Two timeout of the driver is longer than a trace, so a waiting answer does not time out.
- The driver does not compare the mute `switch` calls of a training run, because `jarvis.qnt` has no action for them.

**Acceptance criteria.**

- AC-16.2c-1: When `quint-connect` replays 100 traces of 30 steps of `jarvis.qnt`, the driver state equals the model state after each step.

**Proofs.**

- P-16.2c-1. Evidence: test. Together with P-16.1-2 and P-16.2b-1, it gives the five system invariants for the Rust application on the replayed traces.

---

### 16.3 Composed Quint Model

**Goal:** Add `quint/composed.qnt`, the composition of the system model with the component models. Prove that the invariants of each component and the system invariants still hold when the components run together in the request lifecycle.

- **Implements:** G19. **Depends on:** 16.2c.
- **Files:** create `quint/composed.qnt`, modify `justfile`.
- **Quint:** `composed.qnt`. It imports `jarvis.qnt`, one instance of `commit` for each request, one instance of `announce` for each satellite, and one instance of `dialog`, `training`, `learned` and `prefetch`. `cache_sync`, `backfill` and `speaker` are not in the composition: the request reads the cache, a backfill changes only pools, and the speaker is a slot value. Their own models cover them. `jarvis.qnt` does not change, and the driver of sub-phases 16.2 to 16.2c continues to replay it.

**Model.**

- Quint instantiates only a module that has a constant. Thus `composed.qnt` has one wrapper module for each imported model (`commitOf`, `announceOf`, `dialogOf`, `trainingOf`, `learnedOf`, `prefetchOf`). Each wrapper has the constant `ID`, imports and exports its model, and adds an action that keeps the state of the model (`unchanged`).
- Each lifecycle action of `jarvis.qnt` becomes an action of `composed.qnt` with the same name. It applies the action of `jarvis.qnt` with its guards and effects, the component effects below and the pause guard. A component that the action does not name does not change.
  - `audio(r)`, `commit(r)`, `reply(r)`, `failRequest(r)`, `defer(r)`, `call(r)`: no component effect.
  - `handle(r)`: `request` on the announce queue of `sat(r)` (the queue numbers the requests of a satellite from 1), and `request` on `prefetch`. Thus the newest request of the queue changes at `handle`, as in sub-phase 16.1b.
  - `pipelineEnd(s)`: `pipelineEnd` on the announce queue of `s`.
  - `announce(r)`: also requires that the head of the announce queue of `sat(r)` is a sentence of `r`. It applies `start` on that queue.
  - `swap(v, a)`: the current training attempt (`localDone` or `runpodDone`) gives a model of accuracy `a`, and `a` is not less than the accuracy of the installed model. `activeModel := v`.
  - `tick`: `tick` on `dialog` and `prefetch`, and `finish` on each announce queue.
- New actions. Each keeps the lifecycle state, except `ask` and `answer`:
  - `listen(r, kind, voiced, nonEmpty, conf)`: while `r` is `Listening`, the commit policy of `r` takes one event: a chunk for `kind` 0, a VAD decision for 1, else a partial. `commit(r)` ends the utterance: the policy committed, or HA ended the audio. `commit(r)` does not wait for the commit of the policy, and the policy of `r` stops at `commit(r)`. A commit that waits for the policy needs 7 events or more, so the lifecycle does not get to `handle` in a trace of 30 steps.
  - `sentence(r)`: the next sentence of the System Two answer of `Deferred` request `r` arrives in the announce queue of `sat(r)`.
  - `ask(r)`: request `r` asks the user a question. `open` on `dialog` for `sat(r)`. `Handling` to `Replied`, `handled += 1`.
  - `answer(r, outcome)`: request `r` answers the open dialog of `sat(r)`: `fill` for `outcome` 0, `askAgain` for 1, else `cancel`. `Handling` to `Replied`, `handled += 1`.
  - `autoStart`, `voiceStart`, `change`, `localFail`, `runpodFail`: the actions of `training` with the same names.
  - `propose(c, suitable)`, `approve(c)`, `reject(c)`, `writeRequest(c)`, `failWrite(c)`: the actions of `learned` (`failWrite` is `failRequest`).
  - `prefetch`, `prefetchStep(answerStep)`: the actions of `prefetch`.
- The pause of sub-phase 13.10e: `autoStart` and `voiceStart` are enabled only when no started request is in `Listening` or `Handling`. `release` waits for the uses in progress, and the mute calls do not wait for a satellite. Thus the driver completes a start action without a later trace step. While `training.paused`, `audio`, `handle` and `answer` are not enabled. Thus only the RunPod attempt can swap while a request is in `Handling`.
- `training` starts with `initRunPod`: the RunPod fallback is configured.
- Invariants: `SystemInvariants` (the five system invariants of `jarvis.qnt`) and `ComponentInvariants` (`allInvariants` of each instance). `allInvariants` is both.
- Limits: `composed.qnt` defines `pure val VERIFY_STEPS = 5` and `pure val SAMPLES = 1000`. A step of the composition takes about 100 times longer in simulation than a step of `jarvis.qnt`, and `quint verify` takes about 2 minutes at 5 steps and about 3 times longer for each further step. The `quint` recipe of the `justfile` reads `SAMPLES` as it reads `VERIFY_STEPS` (`02-rules.md` section 4, rule 5).

**Acceptance criteria.**

- AC-16.3-1 [Quint]: `quint typecheck` passes.
- AC-16.3-2 [Quint]: `ComponentInvariants` holds in simulation.
- AC-16.3-3 [Quint]: `SystemInvariants` holds in simulation and to 5 steps in `quint verify`.

**Proofs.**

- P-16.3-2. Argument: each imported action keeps its own guard and effect, and the composition adds no action that writes the state of an imported model. The pause only adds guards. So each inductive invariant of a component (Phases 4 to 15) still holds.
- P-16.3-3. Argument: each lifecycle action applies the action of `jarvis.qnt` with its guards and effects, and only adds guards and component effects. `ask` and `answer` increment `handled` only from `Handling`, as `reply` does. The new component actions do not change the lifecycle state. So the arguments of P-16.1-2 still apply.
- P-16.3-1. Evidence: model check.

---

### 16.4 Composed Conformance

**Goal:** Point the system driver at `composed.qnt` and extend it to the composed actions: questions, answers, training runs and announce queues. The whole Rust application now follows the composed model on the replayed traces.

- **Implements:** G19. **Depends on:** 16.3.
- **Files:** modify `src/testing/e2e.rs`.
- **Quint:** `composed.qnt` (conformance).

**Model.**

- The conformance tests of the system driver replay `composed.qnt` in place of `jarvis.qnt`. The lifecycle variables of `composed.qnt` have the names of `jarvis.qnt`.
- The driver of sub-phases 16.2 to 16.2c keeps its steps for `audio`, `commit`, `handle`, `reply`, `defer`, `failRequest`, `call`, `pipelineEnd` and `tick`. It maps the other actions:
  - `ask(r)` and `answer(r, outcome)`: `Assistant::handle` of `r` ends with a question, or with a fill, an unclear answer or a cancel of the open dialog of its satellite.
  - `sentence(r)`: the System Two answer of `r` gets one sentence. `announce(r)`: the clock advances by `announce.margin_ms`.
  - `swap(v, a)`: the training use case with `FakeTrainer` ends its attempt with a model of measure `a`, and its loader gives the classifier of version `v`. `autoStart` and `voiceStart` start a run, and the local attempt runs the pause with `FakeModels`. `change` adds one action mark. `localFail` and `runpodFail` fail the attempt.
  - No-op, because the driver does not compare that state: `listen`, `propose`, `approve`, `reject`, `writeRequest`, `failWrite`, `prefetch` and `prefetchStep`.
- The driver does not compare the `switch` calls of the mute, because `composed.qnt` has no action for them.
- After each step, the driver also reads the dialogs and the training state.

**Acceptance criteria.**

- AC-16.4-1: When `quint-connect` replays 100 traces of 30 steps of `composed.qnt`, the driver state equals the model state after each step. This replaces AC-16.2b-1 and AC-16.2c-1.

**Proofs.**

- P-16.4-1. Evidence: test. Together with P-16.3-2 and P-16.3-3, it gives the system invariants and the component invariants for the Rust application on the replayed traces.
