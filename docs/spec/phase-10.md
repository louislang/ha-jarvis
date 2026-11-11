# Phase 10: Multi-Turn Dialogs

This phase lets Jarvis ask questions. Jarvis asks when the area is missing or when System Two needs more data. It continues to ask until the request is clear, the user cancels, or the turn limit occurs.

---

### 10.1 Dialog Quint Model

**Goal:** Add `quint/dialog.qnt`, the model of open dialogs on several satellites with turns, cancel, timeout and deferred jobs in flight. Prove the turn limit and that a cancelled or replaced request never acts, before the Rust code exists.

- **Implements:** G13, G19. **Depends on:** 17.4.
- **Files:** create `quint/dialog.qnt`. Modify `justfile`.
- **Quint:** `dialog.qnt`.

**Model.**

- Constants: `KEYS = Set("s1", "s2")`, `REQUESTS = 1.to(3)`, `MAX_TURNS = 3`, `TIMEOUT = 3`.
- State: each variable is a map with a fixed domain or a number, so that Apalache can check an inductive invariant.
  - `now`, `nextRequest`.
  - For each key: the dialog `turns` (0 is no open dialog), `asked` and `request`, and `gen`.
  - For each request: its deferred job `running`, `jobKey` and `jobGen`, and `calls` and `cancelled` (booleans).
- Actions:
  - `open(k)` (when `nextRequest` is in `REQUESTS`): a new request on `k`. `gen[k] += 1`, and the dialog of `k` gets `turns = 1`, `asked = now` and the request.
  - `defer(k)` (when `nextRequest` is in `REQUESTS`): a new request `r` on `k` with a deferred job. `gen[k] += 1`, `running[r] := true`, `jobKey[r] := k` and `jobGen[r] := gen[k]`. It also closes the dialog of `k`.
  - `ask(r)` (when `running[r]` and `jobGen[r] = gen[jobKey[r]]`): the job asks the user and ends. `gen[jobKey[r]] += 1`, and the dialog of `jobKey[r]` gets `turns = 1`, `asked = now` and `r`. Only `fill` can then act for `r`.
  - `jobAct(r)` (when `running[r]`): `running[r] := false`. When `jobGen[r] = gen[jobKey[r]]`, `calls[r] := true`.
  - `askAgain(k)`: if `turns < MAX_TURNS`, `turns += 1` and `asked := now`, else close the dialog.
  - `fill(k)`: the answer is a new request on `k`. `gen[k] += 1`, `calls[request[k]] := true`, and close the dialog.
  - `cancel(k)`: `gen[k] += 1`, `cancelled[request[k]] := true`, and close the dialog.
  - `tick`: `now += 1`. Close every dialog with `now - asked >= TIMEOUT`.
- One dialog for each key is a property of the map type, so it needs no invariant.
- Invariants:
  - `TurnLimit`: every key has `turns <= MAX_TURNS`.
  - `NoCallAfterCancel`: no request has both `calls` and `cancelled`. A job that started before a cancel can still try to act after it, so the invariant fails without the generation check.
  - `Inductive`: the inductive invariant that `quint verify` checks (`02-rules.md` section 4, rule 5). It has the range of each variable, `NoCallAfterCancel` and these facts:
    - A request at or after `nextRequest` has no job, dialog, call or cancel.
    - A running job has `jobGen <= gen` of its key.
    - A live job (running, with the current generation of its key) has no dialog, call or cancel.
    - The request of an open dialog has no call or cancel, and no other open dialog.
- A 10-step `quint verify` of this model takes more than 22 minutes on the test machine. The inductive check takes 11 seconds.

**Acceptance criteria.**

- AC-10.1-1 [Quint]: `quint typecheck` passes.
- AC-10.1-2 [Quint]: `TurnLimit` and `NoCallAfterCancel` hold in simulation. `quint verify` proves that `Inductive` holds in `init`, that each step keeps it, and that it implies both invariants.
- AC-10.1-3 [Quint]: The test `ac_10_1_3_two_satellites_independent` passes: actions on `s1` do not change the dialog of `s2`.
- AC-10.1-4 [Quint]: The test `ac_10_1_4_job_after_cancel` passes: a job that asked before a cancel makes no call after it. After `open(k)` and `defer(k)`, `fill(k)` cannot give the replaced request a call.

**Proofs.**

- P-10.1-2 (AC-10.1-2). `TurnLimit` by induction: `open` and `ask` set 1. `askAgain` increases only when `turns < MAX_TURNS`, so the result is at most `MAX_TURNS`. `NoCallAfterCancel` by induction on `Inductive`: a request gets `calls` only in `fill`, or in `jobAct` with a live job. It gets `cancelled` only in `cancel`. `open` and `defer` use a new request, which has no job, dialog, call or cancel. `ask` needs a live job, so its request has no dialog, call or cancel. `ask` increases the generation, so the job is no longer live. `fill` and `cancel` close the dialog and increase the generation. Thus a request with a live job or an open dialog has no call or cancel, and no request takes both actions.
- P-10.1-1, P-10.1-3, P-10.1-4. Evidence: model check.

---

### 10.2 Dialog State

**Goal:** Add the dialog types and the pure `Dialogs` functions that implement `dialog.qnt`. Jarvis can hold one open dialog per satellite with a turn limit and a timeout.

- **Implements:** G13, G19. **Depends on:** 10.1.
- **Files:** create `src/domain/dialog.rs`. Modify `src/lib.rs`, `jarvis.toml`.
- **Quint:** `dialog.qnt` (conformance).

**Model.**

- `Pending`: `Slot { intent, slots, missing, choices }`, `Llm { response_id, call_id }`.
- `Dialog { pending, turns, asked }`. `asked` is the time of the last question. `Dialogs: Map<Key, Dialog>` and a generation number for each key. `Key` is the `satellite_id`, else the `conversation_id`. The generations of sub-phase 4.9b now use `Key`, so a conversation without a satellite also has one.
- `open(key, pending, now)`, `take(key, now) -> Option<Dialog>` (removes it; `None` when `now - asked >= timeout_s`), `next_turn(dialog, max_turns, now) -> Option<Dialog>` (`turns + 1` and `asked := now`), `close(key)` (removes the dialog and increases the generation of the key).
- `turns` counts the questions of one dialog: System Two questions also count.
- This sub-phase adds `[dialog]` (`max_turns`, `timeout_s`). `validate` checks that each is more than 0 (sub-phase 2.2). With `max_turns = 0`, Jarvis never asks again. With `timeout_s = 0`, `take` drops every answer.

**Acceptance criteria.**

- AC-10.2-1: When `open` runs for a satellite, `take` for that satellite returns the dialog and removes it.
- AC-10.2-2: When `take` runs `timeout_s` after the last question, it returns none. When it runs `timeout_s` after `open` but less than `timeout_s` after the last `next_turn`, it returns the dialog.
- AC-10.2-3: When two satellites have open dialogs, `take` for one does not change the other.
- AC-10.2-4: When the dialog has `max_turns` turns, `next_turn` returns none.
- AC-10.2-5: When `quint-connect` replays 100 traces of `dialog.qnt`, `Dialogs` matches the open dialogs of the model (`turns`, `asked`, `request`) after each step.
- AC-10.2-6: When `close` runs for a key, the generation of that key increases by 1 and other keys do not change.
- AC-10.2-7: When `dialog.max_turns` or `dialog.timeout_s` is 0, `parse` returns an error that contains the field name and the range.

**Proofs.**

- P-10.2-1, P-10.2-3. Ensures: `take(k)` returns the dialog that `open(k)` stored and removes only key `k`. Argument: `open` is a map insert and `take` is a map remove on one key. Other keys do not change.
- P-10.2-2. Ensures: `take` returns `None` exactly when `now - asked >= timeout_s`, where `asked` is the time of the last question. Argument: `take` returns `None` when `now - asked >= timeout_s`, and `next_turn` sets `asked := now`.
- P-10.2-6. Ensures: `close(k)` increases the generation of `k` by 1, and the generations of other keys do not change. Argument: `close` does one increment on one key.
- P-10.2-4. Requires: `TurnLimit` (P-10.1-2). Ensures: `next_turn` returns `None` when `turns >= max_turns`, else the dialog with `turns + 1`. Argument: `next_turn` is this one comparison.
- P-10.2-5. Evidence: test.
- P-10.2-7. Ensures: `max_turns = 0` or `timeout_s = 0` gives `Err` with the field name and the range. Argument: `validate` compares each of the two fields with 0 (P-2.2-6).

---

### 10.3 Answer Fill and Cancel

**Goal:** Add `is_cancel` and `fill`, which match a spoken answer to a cancel phrase or to a list of choices with fuzzy matching. "The kitchen" and a slightly misheard "kitchn" both fill the area.

- **Implements:** G13. **Depends on:** 10.2.
- **Files:** modify `src/domain/dialog.rs`, `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.** `is_cancel(text, cancel) -> bool`: the normalized text equals a cancel phrase. `fill(answer, choices, similarity) -> Option<String>`: with no choices, the normalized answer. Else, when the words of one or more choices are whole words of the normalized answer, return the longest such choice. Else, remove a leading "the", and return the choice with the highest Jaro-Winkler similarity if it is at least `similarity`. This sub-phase adds `dialog.choice_similarity` and `dialog.cancel`.

**Acceptance criteria.**

- AC-10.3-1: When the text is "never mind", `is_cancel` returns true. When the text is "kitchen", it returns false.
- AC-10.3-2: When the choices are "kitchen" and "living room" and the answer is "the kitchen", `fill` returns "kitchen".
- AC-10.3-3: When the answer is "kitchn" and its similarity to "kitchen" is at least `choice_similarity`, `fill` returns "kitchen".
- AC-10.3-4: When no choice is similar enough, `fill` returns none.
- AC-10.3-5: When there are no choices, `fill` returns the normalized answer.
- AC-10.3-6: When the choices are "kitchen" and "living room" and the answer is "in the kitchen please", `fill` returns "kitchen".

**Proofs.**

- P-10.3-1. Ensures: `is_cancel(t)` is true if and only if `normalize(t)` is a cancel phrase. Argument: `is_cancel` is set membership of the normalized text.
- P-10.3-2, P-10.3-6. Ensures: when the words of a choice are whole words of the answer, `fill` returns the longest such choice. Argument: the whole-word branch comes first and takes the maximum by length. "kitchen" is a whole word of "the kitchen" and of "in the kitchen please".
- P-10.3-3, P-10.3-4. Ensures: when no choice is in the answer as whole words, `fill` returns `Some(c)` if and only if `c` maximizes similarity and the maximum is at least the threshold. Argument: `fill` computes `max_by` similarity, then filters by the threshold. An exact match has similarity 1.
- P-10.3-5. Ensures: with no choices, `fill` returns `normalize(answer)`. Argument: the empty-choices branch returns `normalize(answer)`.

---

### 10.4 Ask Decision and HA Ask

**Goal:** Add `Decision::Ask` and `Home::ask`, which calls `assist_satellite.start_conversation`. Jarvis can now ask a question on a satellite, and the satellite listens without the wake word.

- **Implements:** G13. **Depends on:** 10.3.
- **Files:** modify `src/domain/decide.rs`, `src/domain/responses.rs`, `src/ports.rs`, `src/adapters/ha.rs`, `src/app/conversation.rs`, `responses.toml`, `src/testing/fakes.rs`.

**Model.**

- `Decision::Ask { question: Chosen, pending }`. A missing `area`, or an area slot that matches no area, gives `Ask` from the pool `ask_area`. These cases replace the `Defer` of sub-phase 4.5b. A missing `area` is a real ambiguity: the intent is clear, and only the room is not. Sub-phase 18.15 adds one exception: for a command, an area slot that names a Music Assistant speaker group gives a call on the group.
- A missing `person` and any other missing template slot do not ask. They still give `Defer` with `Job::Llm` (sub-phases 4.5d and 15.7), because a missing value usually means that System One has the wrong intent.
- The choices for `area` are the area names and their aliases. `decide` maps a filled alias to its area with `area_by_name`.
- `Home::ask(satellite, question)` calls `start_conversation` with `preannounce = false` and the deadline `ha.announce_timeout_s`.
- With a `satellite_id`, `handle` returns empty text and puts the question in the announce queue of the satellite (sub-phase 4.9b), which calls `ask` when the satellite is idle. HA cancels a running pipeline when a question starts, so the question waits. Without a `satellite_id`, `handle` returns the question text.
- `validate_pools` checks that the pool `ask_area` exists.
- HA does not send an empty transcript to `handle` (`stt-no-text-recognized`), so Jarvis has no dialog for empty input.

**Acceptance criteria.**

- AC-10.4-1: When a `target = "entities"` intent has no area, `decide` returns `Ask` from `ask_area` with the area names as choices. This replaces AC-4.5b-4.
- AC-10.4-2: When the area slot matches no area, `decide` returns `Ask` from `ask_area`. This replaces AC-4.5b-3.
- AC-10.4-3: When the pool `ask_area` is missing, `validate_pools` returns an error that names it.
- AC-10.4-4: When the result is `Ask` and the context has a `satellite_id`, `handle` returns empty text and `FakeHome` records `ask` with the question.
- AC-10.4-5: When the result is `Ask` and the context has no `satellite_id`, `handle` returns the question text.
- AC-10.4-6: When `HaHome::ask` runs, `FakeHaServer` receives `assist_satellite.start_conversation` with the question and `preannounce = false`.
- AC-10.4-7: When an intent has a template slot that is not `optional` and not `area`, and the prediction does not have it, `decide` returns `Defer` with `Job::Llm`, not `Ask`.
- AC-10.4-8: When `decide` returns `Ask` with a `satellite_id`, `FakeHome` records `ask` after the satellite state becomes idle, and not before.
- AC-10.4-9: When the user answers with an area alias, `decide` uses the area of that alias.

**Proofs.**

- P-10.4-1, P-10.4-2. Ensures: a missing area, or an area slot that matches no area, gives `Ask` from `ask_area` with the area names and aliases as choices. Argument: the two `Defer` returns of sub-phase 4.5b become `Ask { ask_area, choices }`. No other branch changes.
- P-10.4-4, P-10.4-5. Argument: `handle` matches `(Ask, ctx.satellite_id)` with two arms.
- P-10.4-7. Ensures: a missing template slot that is not `optional`, not `area` and has no habit value gives `Defer` with `Job::Llm`. Argument: `decide` checks each template slot of the intent against the prediction, `optional` and `habit_defaults` before it renders, and the only `Ask` is from `ask_area`.
- P-10.4-8. Requires: `AnnounceWhenIdle` (P-4.7-2). Argument: the question is an item of the announce queue, which waits for `idle` (P-4.9c-1).
- P-10.4-9. Ensures: a filled alias gives the area of the alias. Argument: the choices contain the aliases, and `decide` resolves the filled value with `area_by_name` (P-2.8b-1).
- P-10.4-3, P-10.4-6. Evidence: test.

---

### 10.5 Dialog Routing

**Goal:** Process each transcript against the open dialog of its satellite before it is treated as a new request. Answers fill slots, cancel phrases cancel, and a clear new request replaces the dialog.

- **Implements:** G13. **Depends on:** 10.4.
- **Files:** modify `src/app/conversation.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `responses.toml`.

**Model.** For each transcript:

1. If a dialog is open and `is_cancel`, reply from `cancelled` and close the dialog.
2. If a `Slot` dialog with choices is open and `fill` returns a value, add it to the slots and run `decide` again.
3. If the top label is not `unknown` and its probability is at least `model.threshold`, close the dialog and process the text as a new request.
4. If an `Llm` dialog is open, send the text to System Two as the answer to its question (sub-phase 10.6b).
5. If a `Slot` dialog is open, ask again with `next_turn`. If it returns none, reply from `failure`.
6. Else, process the text as a new request.

A choice is a closed set, so step 2 comes before step 3. The answer to System Two is free text, so step 3 comes before step 4. Each `Slot` dialog asks for the area, so it has choices (sub-phase 10.4).

- `handle` takes the dialog of the key of the context (`Key`, sub-phase 10.2) before step 1. Steps 2 to 6 thus close it. Step 5 stores the dialog that `next_turn` gives.
- The filled value replaces each slot with the name of the missing slot, so `decide` reads the filled area (sub-phase 10.4). The intent of the dialog gets the probability 1.
- Until sub-phase 10.6b, step 4 sends the text as a new System Two request.
- Step 5 asks again from `ask_area` as sub-phase 10.4 asks, with the current generation of the satellite.
- When `decide` gives `Ask`, `handle` opens a dialog with its `pending` for the key.
- `start_app` gives the `[dialog]` configuration to the use case. `FakeReasoner` records the text of each question.
- This sub-phase adds the pool `cancelled`.

**Acceptance criteria.**

- AC-10.5-1: When the user answers "the kitchen" to "Which room?", `FakeHome` records `homeassistant.turn_on` with the kitchen lights and the dialog closes.
- AC-10.5-3: When the user answers "turn on the fan" to "Which room?" and `FakeClassifier` gives `fan_on` 0.9, Jarvis closes the dialog and processes the new request.
- AC-10.5-4: When the user answers "never mind", Jarvis replies from `cancelled` and `FakeHome` records no call.
- AC-10.5-5: When the user gives `max_turns` answers that do not fill the slot, Jarvis replies from `failure` and closes the dialog.
- AC-10.5-7: When an `Llm` dialog is open and the answer has the top label `unknown`, `FakeReasoner` receives the answer.

**Proofs.**

- P-10.5-4. Requires: `NoCallAfterCancel` (P-10.1-2). Argument: step 1 returns before any step that dispatches a call.
- P-10.5-5. Requires: `TurnLimit`. Argument: step 5 uses `next_turn` (P-10.2-4).
- P-10.5-1, P-10.5-3, P-10.5-7. Evidence: test.

---

### 10.5b Dialog Generations

**Goal:** Make a closed dialog stop the deferred jobs of its key, and check the jobs and the generations against `dialog.qnt`. A cancel then also stops the System Two job of the dialog.

- **Implements:** G13, G19. **Depends on:** 10.5.
- **Files:** modify `src/app/conversation.rs`.
- **Quint:** `dialog.qnt` (conformance of the jobs and generations).

**Model.**

- `close` increases the generation of the key, as each new request on a satellite does (sub-phase 4.9c). A deferred job keeps the generation of its key when it starts. Before each announce, question or call, it compares the generation. When the generation changed, the job stops and makes no announce, no question and no further call. A call that already started is not undone. A job that asks increases the generation of its key and ends. The answer continues the request through the dialog.
- The announce queues use the generations of `Dialogs` (sub-phase 10.2). A new request on a satellite closes the dialog of its key. Thus each key has one generation.
- The conformance driver maps the model to `handle` on two satellites:
  - `open(k)`: a request on `k` whose decision is `Ask`. Until sub-phase 10.6, `ask(r)` is the same request on the key of the job of `r`.
  - `defer(k)`: a request with a confident label that is not in the catalog, so `decide` gives a System Two job.
  - `jobAct(r)`: the job of `r` runs, and its answer has one call.
  - `askAgain(k)`: an answer that fills nothing, with the top label `unknown`. `fill(k)`: the answer "kitchen". `cancel(k)`: the answer "never mind".
  - `tick`: the clock advances 1 s. `dialog.timeout_s` is 3.
  - Each call that `FakeHome` records in a step, other than an announce or a question, belongs to the request of that step.

**Acceptance criteria.**

- AC-10.5b-1: When the user cancels a dialog while its System Two job runs, `FakeHome` records no announce and no `ask` from that job.
- AC-10.5b-2: When `quint-connect` replays 100 traces of `dialog.qnt`, the calls that `FakeHome` records and the generations match the model after each step.

**Proofs.**

- P-10.5b-1. Requires: P-10.2-6. Argument: the job compares its generation before each announce, question and call, and `close` changed it.
- P-10.5b-2. Evidence: test. With P-10.1-2, it gives `NoCallAfterCancel` for the Rust code on the replayed traces.

---

### 10.5c Dialog Fill Context

**Goal:** Keep the transcript of the request that asks for the area. When a filled request falls back to System Two, System Two then gets the original command and the filled area, not only the answer. `validate_pools` also requires the pool `cancelled`.

- **Implements:** G13. **Depends on:** 10.5b.
- **Files:** modify `src/domain/dialog.rs`, `src/domain/decide.rs`, `src/domain/responses.rs`, `src/app/conversation.rs`.
- **Quint:** `dialog.qnt` (unchanged: the model has no text, and `fill` already continues the `request` of the dialog).

**Model.**

- `Pending::Slot` gets `transcript`: the transcript of the request that asked. `ask` copies `request.transcript` into it.
- Routing step 2 (sub-phase 10.5) runs `decide` with a request whose transcript is `<transcript> in <value>`: the `transcript` of the dialog, a space, "in", a space, and the value that `fill` returns. When the filled decision is a System Two job, `Job::Llm` thus has the original command and the filled area. For example, the request "turn on the fan" and the answer "kitchen" give "turn on the fan in Kitchen".
- `validate_pools` checks that the pool `cancelled` exists, as it checks `ask_area` (sub-phase 10.4). Thus a `responses.toml` without `cancelled` fails at startup, not at the first cancel.

**Acceptance criteria.**

- AC-10.5c-1: When `decide` returns `Ask` for the transcript "turn on the lights", its `Pending::Slot` has the transcript "turn on the lights".
- AC-10.5c-2: When the user answers "kitchen" to "Which room?" for "turn on the fan" and the kitchen has no fan, `FakeReasoner` receives "turn on the fan in Kitchen".
- AC-10.5c-3: When the pool `cancelled` is missing, `validate_pools` returns an error that names it.

**Proofs.**

- P-10.5c-1. Ensures: when the decision is `Ask`, its `Pending::Slot` has `transcript == request.transcript`. Argument: `situation` gives its request to `ask`, and `ask` copies `request.transcript`. `asked` (P-10.4-1) gets the request and states the transcript, so `placed` and `situation` also state it.
- P-10.5c-2. Argument: routing step 2 builds the request with the transcript `<transcript> in <value>`, and each System Two job of `decide` has the transcript of its request (`deferred`, sub-phase 4.5).
- P-10.5c-3. Ensures: when `validate_pools` returns `Ok`, the pool `cancelled` exists. Argument: `validate_pools` calls `require` for `cancelled` after `ask_area`, and `valid_pools` names `cancelled`.

---

### 10.6 System Two Questions

**Goal:** Add `Step::Ask`, so a System Two job can ask the user for missing data. The question waits in the announce queue of the satellite, and a dialog opens for the answer.

- **Implements:** G7, G13. **Depends on:** 10.5c.
- **Files:** modify `src/ports.rs`, `src/app/conversation.rs`, `src/testing/fakes.rs`, `src/adapters/openai.rs` (only the test that matches each step).
- **Quint:** `dialog.qnt` (conformance of the questions of the jobs).

**Model.**

- `Step::Ask { question, response_id, call_id }`. Before the question, the consumer compares the generation (sub-phase 10.5b). The consumer increases the generation of the key and ends the job. It puts the question in the announce queue (sub-phase 4.9b) with the new generation, so it asks when the satellite is idle after the filler. It opens a dialog with `Pending::Llm { response_id, call_id }` for the key.
- Without a `satellite_id`, the answer of the job is the question text, and the dialog has the key of the conversation (decision 18).
- `FakeReasoner` gives `Ask` from its script. It keeps one script for each answer, in order.
- Until sub-phase 10.6b, no adapter gives `Ask`, and an answer to an `Llm` dialog is a new System Two request (routing step 4, sub-phase 10.5).
- The conformance driver of sub-phase 10.5b maps `ask(r)` to the job of `r`, whose answer gives `Ask`. A `jobAct(r)` after `ask(r)` does nothing, because the job ended. On an `Llm` dialog:
  - `fill(k)` is the answer "kitchen", and its System Two job has one call.
  - `askAgain(k)` is an answer with the top label `unknown`, and its System Two job asks again. This answer is a new request with a new question, so it increases the generation of the key by 2. The model has no new generation in `askAgain`, so the driver subtracts these 2 from the generation that it compares.

**Acceptance criteria.**

- AC-10.6-3: When `FakeReasoner` gives `Ask`, `FakeHome` records `ask` after the satellite becomes idle, and the dialog has `Pending::Llm`.
- AC-10.6-7: When the user says "cancel" during a System Two dialog, `FakeReasoner` records no more calls for that dialog.

**Proofs.**

- P-10.6-7. Requires: `NoCallAfterCancel`. Argument: routing step 1 closes the dialog before step 4 can call `answer`.
- P-10.6-3. Evidence: test.

---

### 10.6b System Two Answers

**Goal:** Add the `ask_user` tool, and send the answer of the user as the output of the `ask_user` call, with `previous_response_id`. System Two can then ask several questions, up to the turn limit.

- **Implements:** G7, G13. **Depends on:** 10.6.
- **Files:** modify `src/domain/decide.rs`, `src/adapters/openai.rs`, `src/app/conversation.rs`, `src/testing/fakes.rs`.
- **Quint:** `dialog.qnt` (the driver of sub-phase 10.6 does not change).

**Model.**

- `Question` gets `answers: Option<AskCall>`, the `ask_user` call that the text answers. `AskCall` has `response_id` and `call_id`. `question` gives `None`.
- `ask_user(question)` is a strict tool. When a response completes with an `ask_user` call, `answer` gives `Ask` with the question, the ID of the response and the call ID, and ends. Each request sets `parallel_tool_calls = false`, so one response has at most one open tool call.
- The request builder has two branches. Without `answers`, the input is the user message (sub-phase 7.2). With `answers`, the request has `previous_response_id`, and its input is only one `function_call_output` item with `call_id` and the text. Both branches have the system message and the tools.
- Routing step 4 (sub-phase 10.5) sends the answer to an `Llm` dialog as a new request on the satellite: a filler, and a System Two job whose `Question` has the call of the dialog. Without a `satellite_id`, `handle` waits for the answer and returns its text (sub-phase 7.5c).
- The job keeps the turns of the dialog. Each question of the job goes through `next_turn`. At the turn limit, the job announces from `failure`, and the dialog stays closed.
- When the dialog times out or is cancelled, Jarvis sends nothing for the open call.
- `FakeReasoner` records the call ID that each question answers.

**Acceptance criteria.**

- AC-10.6b-1: When the mock returns an `ask_user` tool call, `answer` gives `Ask` with the question, the response ID and the call ID.
- AC-10.6b-2: When the `Question` has a previous response ID and a call ID, the request has `previous_response_id`, one `function_call_output` item with that call ID and the user text, and `parallel_tool_calls = false`.
- AC-10.6b-3: When the user answers, `handle` returns a filler before `FakeReasoner` completes.
- AC-10.6b-4: When `FakeReasoner` asks three questions and then answers, `FakeHome` records three `ask` calls, then the announce.
- AC-10.6b-5: When `FakeReasoner` asks more than `max_turns` questions, `FakeHome` records an announce from `failure` and the dialog closes.

**Proofs.**

- P-10.6b-2. Argument: the request builder has two branches. With a previous ID, it sends only the tool output for the call ID.
- P-10.6b-5. Requires: `TurnLimit`. Argument: each `Ask` step goes through `next_turn`.
- P-10.6b-1, P-10.6b-3, P-10.6b-4. Evidence: test.

---

### 10.7 Early Commit for Answers

**Goal:** Let short answers to a System One question use the early commit wait. "Kitchen" while a `Slot` dialog is open, or "never mind" while any dialog is open, ends listening after 100 ms instead of 600 ms.

- **Implements:** G1, G13. **Depends on:** 10.6b.
- **Files:** modify `src/app/speech.rs`, `src/app/conversation.rs`, `src/lib.rs`, `src/testing/e2e.rs`.
- **Quint:** `commit.qnt` (unchanged: `confident` is abstract).

**Model.**

- The audio stream has no satellite ID. Thus the listen use case checks the open dialogs of all keys. A dialog is open when `timeout_s` did not pass since its last question, as `take` gives it (sub-phase 10.2).
- `Queues::is_answer(text, now)` is true when `is_cancel(text)` and one or more dialogs are open, or when `fill(text, choices)` of an open `Pending::Slot` dialog returns a value.
- A `Pending::Llm` dialog has no choices, and `fill` with no choices returns the normalized answer. Thus `fill` accepts any partial text for such a dialog. An answer to System Two is free text with several words, so a `Pending::Llm` dialog does not count for `fill`. Such an answer keeps the `endpoint_ms` wait. A cancel phrase counts for each open dialog.
- `push` sets `confident` to true when `is_answer(partial, now)` is true. Else `confident` comes from the prediction (sub-phase 6.8). `Listener` gets the `Queues` of the conversation and the `Clock`.
- `Queues::open(key, pending, now)` opens a dialog. `handle` uses it when `decide` gives `Ask`.
- AC-10.7-4 opens the dialog with a `transcript` from the kitchen satellite with no `device_id`. Thus "turn on the lights" has no area and asks "Which room?". Then the harness runs the speech "the kitchen" from the kitchen satellite.

**Acceptance criteria.**

- AC-10.7-1: When a `Slot` dialog has the choice "kitchen" and the partial text is "kitchen", the commit uses `early_commit_ms`.
- AC-10.7-2: When no dialog is open, or the only dialog timed out, and the top label is `unknown`, the partial text "kitchen" uses `endpoint_ms`.
- AC-10.7-3: When a `Slot` dialog or a System Two dialog is open, the partial text "never mind" uses `early_commit_ms`.
- AC-10.7-4 [GPU host]: In the Phase 9 harness, the answer "the kitchen" to "Which room?" has a response latency p95 of 300 ms or less.
- AC-10.7-5: When only a System Two dialog is open and the top label is `unknown`, the partial text "fifteen" uses `endpoint_ms`.

**Proofs.**

- P-10.7-1 to P-10.7-3, P-10.7-5. Argument: `push` sets `confident = is_answer(partial, now) or model_confident`. `is_answer` is true only for a cancel phrase while a dialog is open (P-10.3-1), or for a value of `fill` of an open `Slot` dialog (P-10.3-2). An `Llm` dialog does not count for `fill`, and "fifteen" is not a cancel phrase. `take` gives no dialog after the timeout (P-10.2-2). `should_commit` does not change (P-6.5-1), so the commit invariants of `commit.qnt` still hold.
- P-10.7-4. Evidence: test.

## Field Log

These steps are not tests and gate nothing.

1. Make sure the satellite firmware supports `assist_satellite.start_conversation`.
2. Make sure `handled` with empty text makes the satellite speak nothing.
3. For each dialog in `00-overview.md` section 1.4, make sure the satellite listens after each question without the wake word.
