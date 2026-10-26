# Phase 10: Multi-Turn Dialogs

This phase lets Jarvis ask questions. Jarvis asks when it does not understand, when a slot is missing, or when System Two needs more data. It continues to ask until the request is clear, the user cancels, or the turn limit occurs.

---

### 10.1 Dialog Quint Model

**Goal:** Add `quint/dialog.qnt`, the model of open dialogs on several satellites with turns, cancel and timeout. Prove the turn limit and that a cancelled request never acts, before the Rust code exists.

- **Implements:** G13, G19. **Depends on:** 9.2.
- **Files:** create `quint/dialog.qnt`.
- **Quint:** `dialog.qnt`.

**Model.**

- Constants: `KEYS = Set("s1", "s2")`, `MAX_TURNS = 3`, `TIMEOUT = 3`.
- State: `now`, `dialogs: KEY -> { turns: int, opened: int, request: int }` (a partial map), `calls: Set[int]` (requests that acted), `cancelled: Set[int]`, `nextRequest: int`.
- Actions:
  - `open(k)`: a new request ID. `dialogs[k] := { turns: 1, opened: now, request }`.
  - `askAgain(k)`: if `turns < MAX_TURNS`, `turns += 1`, else remove `k`.
  - `fill(k)`: add the request to `calls` and remove `k`.
  - `cancel(k)`: add the request to `cancelled` and remove `k`.
  - `tick`: `now += 1`. Remove every dialog with `now - opened >= TIMEOUT`.
- Invariants:
  - `OneDialogPerKey`: each key maps to at most one dialog.
  - `TurnLimit`: every dialog has `turns <= MAX_TURNS`.
  - `NoCallAfterCancel`: `calls ∩ cancelled = ∅`.

**Acceptance criteria.**

- AC-10.1-1 [Quint]: `quint typecheck` passes.
- AC-10.1-2 [Quint]: `OneDialogPerKey`, `TurnLimit` and `NoCallAfterCancel` hold in simulation and to 10 steps in `quint verify`.
- AC-10.1-3 [Quint]: The test `twoSatellitesIndependent` passes: actions on `s1` do not change the dialog of `s2`.

**Proofs.**

- P-10.1-2 (AC-10.1-2). `OneDialogPerKey`: `dialogs` is a map, so one key has one value. `TurnLimit` by induction: `open` sets 1. `askAgain` increases only when `turns < MAX_TURNS`, so the result is at most `MAX_TURNS`. `NoCallAfterCancel`: a request enters `calls` only through `fill(k)` and enters `cancelled` only through `cancel(k)`. Both remove `k` in the same step, and a request ID is used by one `open` only, so no request can take both actions.
- P-10.1-1, P-10.1-3. Evidence: model check.

---

### 10.2 Dialog State

**Goal:** Add the dialog types and the pure `Dialogs` functions that implement `dialog.qnt`. Jarvis can hold one open dialog per satellite with a turn limit and a timeout.

- **Implements:** G13, G19. **Depends on:** 10.1.
- **Files:** create `src/domain/dialog.rs`. Modify `src/main.rs`, `jarvis.toml`.
- **Quint:** `dialog.qnt` (conformance).

**Model.**

- `Pending`: `Slot { intent, slots, missing, choices }`, `Llm { response_id }`, `Repeat`.
- `Dialog { pending, turns, opened }`. `Dialogs: Map<Key, Dialog>`. `Key` is the `satellite_id`, else the `conversation_id`.
- `open(key, pending, now)`, `take(key, now) -> Option<Dialog>` (removes it; `None` after `timeout_s`), `next_turn(dialog, max_turns) -> Option<Dialog>`.
- This sub-phase adds `[dialog]` (`max_turns`, `timeout_s`).

**Acceptance criteria.**

- AC-10.2-1: When `open` runs for a satellite, `take` for that satellite returns the dialog and removes it.
- AC-10.2-2: When `take` runs after `timeout_s`, it returns none.
- AC-10.2-3: When two satellites have open dialogs, `take` for one does not change the other.
- AC-10.2-4: When the dialog has `max_turns` turns, `next_turn` returns none.
- AC-10.2-5: When `quint-connect` replays 100 traces of `dialog.qnt`, `Dialogs` matches the model `dialogs` after each step.

**Proofs.**

- P-10.2-1, P-10.2-3. Argument: `open` is a map insert and `take` is a map remove on one key. Other keys do not change.
- P-10.2-2. Argument: `take` returns `None` when `now - opened >= timeout_s`.
- P-10.2-4. Requires: `TurnLimit` (P-10.1-2). Argument: `next_turn` returns `None` when `turns >= max_turns`, else the dialog with `turns + 1`.
- P-10.2-5. Evidence: test.

---

### 10.3 Answer Fill and Cancel

**Goal:** Add `is_cancel` and `fill`, which match a spoken answer to a cancel phrase or to a list of choices with fuzzy matching. "The kitchen" and a slightly misheard "kitchn" both fill the area.

- **Implements:** G13. **Depends on:** 10.2.
- **Files:** modify `src/domain/dialog.rs`, `jarvis.toml`.

**Model.** `is_cancel(text, cancel) -> bool`: the normalized text equals a cancel phrase. `fill(answer, choices, similarity) -> Option<String>`: with no choices, the normalized answer. Else, remove a leading "the", and return the choice with the highest Jaro-Winkler similarity if it is at least `similarity`. This sub-phase adds `dialog.choice_similarity` and `dialog.cancel`.

**Acceptance criteria.**

- AC-10.3-1: When the text is "never mind", `is_cancel` returns true. When the text is "kitchen", it returns false.
- AC-10.3-2: When the choices are "kitchen" and "living room" and the answer is "the kitchen", `fill` returns "kitchen".
- AC-10.3-3: When the answer is "kitchn" and its similarity to "kitchen" is at least `choice_similarity`, `fill` returns "kitchen".
- AC-10.3-4: When no choice is similar enough, `fill` returns none.
- AC-10.3-5: When there are no choices, `fill` returns the normalized answer.

**Proofs.**

- P-10.3-1. Argument: `is_cancel` is set membership of the normalized text.
- P-10.3-2 to P-10.3-4. Ensures: `fill` returns `Some(c)` if and only if `c` maximizes similarity and the maximum is at least the threshold. Argument: `fill` computes `max_by` similarity, then filters by the threshold. An exact match has similarity 1.
- P-10.3-5. Argument: the empty-choices branch returns `normalize(answer)`.

---

### 10.4 Ask Decision and HA Ask

**Goal:** Add `Decision::Ask` and `Home::ask`, which calls `assist_satellite.start_conversation`. Jarvis can now ask a question on a satellite, and the satellite listens without the wake word.

- **Implements:** G13. **Depends on:** 10.3.
- **Files:** modify `src/domain/decide.rs`, `src/domain/responses.rs`, `src/ports.rs`, `src/adapters/ha.rs`, `src/app/conversation.rs`, `responses.toml`, `src/testing/fakes.rs`.

**Model.**

- `Decision::Ask { question, pending }`. A missing slot (`area`, `person` or a `habit_defaults` key) gives `Ask` from the pool `ask_<slot>`. The choices for `area` are the area names. The choices for `person` are the names in `[[people]]`. Other slots have no choices.
- An empty transcript gives `Ask` from `repeat` with `Pending::Repeat`.
- `Home::ask(satellite, question)` calls `start_conversation` with `preannounce = false`.
- With a `satellite_id`, `handle` returns empty text, then calls `ask`. Without it, `handle` returns the question text.
- `validate_pools` checks that each slot that can be missing has an `ask_<slot>` pool.

**Acceptance criteria.**

- AC-10.4-1: When a `target = "entities"` intent has no area, `decide` returns `Ask` from `ask_area` with the area names as choices. This replaces AC-4.5-6.
- AC-10.4-2: When the area slot matches no area, `decide` returns `Ask` from `ask_area`. This replaces AC-4.5-5.
- AC-10.4-3: When a slot that can be missing has no pool `ask_<slot>`, `validate_pools` returns an error with the slot name.
- AC-10.4-4: When the result is `Ask` and the context has a `satellite_id`, `handle` returns empty text and `FakeHome` records `ask` with the question.
- AC-10.4-5: When the result is `Ask` and the context has no `satellite_id`, `handle` returns the question text.
- AC-10.4-6: When `HaHome::ask` runs, `FakeHaServer` receives `assist_satellite.start_conversation` with the question and `preannounce = false`.
- AC-10.4-7: When the transcript is empty, `decide` returns `Ask` from `repeat` with `Pending::Repeat`.
- AC-10.4-8: When `decide` returns `Ask`, `handle` calls `ask` before virtual time advances.

**Proofs.**

- P-10.4-1, P-10.4-2. Argument: the two `Fallback` returns of sub-phase 4.5 become `Ask { ask_area, choices: area names }`. No other branch changes.
- P-10.4-4, P-10.4-5. Argument: `handle` matches `(Ask, ctx.satellite_id)` with two arms.
- P-10.4-7. Argument: `decide` checks for an empty transcript first.
- P-10.4-8. Argument: `handle` spawns `ask` after it builds the reply, with no timer.
- P-10.4-3, P-10.4-6. Evidence: test.

---

### 10.5 Dialog Routing

**Goal:** Process each transcript against the open dialog of its satellite before it is treated as a new request. Answers fill slots, cancel phrases cancel, and a clear new request replaces the dialog.

- **Implements:** G13. **Depends on:** 10.4.
- **Files:** modify `src/app/conversation.rs`, `responses.toml`.

**Model.** For each transcript:

1. If a dialog is open and `is_cancel`, reply from `cancelled` and close the dialog.
2. If a `Slot` dialog is open and `fill` returns a value, add it to the slots and run `decide` again.
3. If the top label is not `unknown` and its probability is at least `model.threshold`, close the dialog and process the text as a new request.
4. If a dialog is open, ask again with `next_turn`. If it returns none, reply from `failure`.
5. Else, process the text as a new request.

**Acceptance criteria.**

- AC-10.5-1: When the user answers "the kitchen" to "Which room?", `FakeHome` records `homeassistant.turn_on` with the kitchen lights and the dialog closes.
- AC-10.5-2: When an intent has two missing slots, Jarvis asks for the first, then the second, then acts.
- AC-10.5-3: When the user answers "turn on the fan" to "Which room?" and `FakeClassifier` gives `fan_on` 0.9, Jarvis closes the dialog and processes the new request.
- AC-10.5-4: When the user answers "never mind", Jarvis replies from `cancelled` and `FakeHome` records no call.
- AC-10.5-5: When the user gives `max_turns` answers that do not fill the slot, Jarvis replies from `failure` and closes the dialog.
- AC-10.5-6: When the transcript is empty `max_turns` times in a row, Jarvis stops asking.

**Proofs.**

- P-10.5-4. Requires: `NoCallAfterCancel` (P-10.1-2). Argument: step 1 returns before any step that dispatches a call.
- P-10.5-5, P-10.5-6. Requires: `TurnLimit`. Argument: step 4 uses `next_turn` (P-10.2-4).
- P-10.5-1 to P-10.5-3. Evidence: test.

---

### 10.6 System Two Questions

**Goal:** Add the `ask_user` tool and `Step::Ask`, so System Two can ask the user for missing data. The next turn sends only the new text and `previous_response_id`.

- **Implements:** G7, G13. **Depends on:** 10.5.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/app/conversation.rs`, `src/testing/fakes.rs`.

**Model.** `Step::Ask { question, response_id }`. The consumer asks after the filler ends and opens a dialog with `Pending::Llm { response_id }`. An answer to that dialog sends a `Question` with `previous_response_id` and only the new text.

**Acceptance criteria.**

- AC-10.6-1: When the mock returns an `ask_user` tool call, `answer` gives `Ask` with the question and the response ID.
- AC-10.6-2: When the `Question` has a previous response ID, the request has `previous_response_id` and only the new user text.
- AC-10.6-3: When `FakeReasoner` gives `Ask`, `FakeHome` records `ask` after the filler ends and the dialog has `Pending::Llm`.
- AC-10.6-4: When the user answers, `handle` returns a filler before `FakeReasoner` completes.
- AC-10.6-5: When `FakeReasoner` asks three questions and then answers, `FakeHome` records three `ask` calls, then the announce.
- AC-10.6-6: When `FakeReasoner` asks more than `max_turns` questions, `FakeHome` records an announce from `failure` and the dialog closes.
- AC-10.6-7: When the user says "cancel" during a System Two dialog, `FakeReasoner` records no more calls for that dialog.

**Proofs.**

- P-10.6-2. Argument: the request builder has two branches. With a previous ID, it sends only the new text.
- P-10.6-6. Requires: `TurnLimit`. Argument: each `Ask` step goes through `next_turn`.
- P-10.6-7. Requires: `NoCallAfterCancel`. Argument: routing step 1 closes the dialog before step 2 can call `answer`.
- P-10.6-1, P-10.6-3 to P-10.6-5. Evidence: test.

---

### 10.7 Early Commit for Answers

**Goal:** Let short answers use the early commit wait. "Kitchen" or "never mind" while a dialog is open ends listening after 200 ms instead of 600 ms.

- **Implements:** G1, G13. **Depends on:** 10.6.
- **Files:** modify `src/domain/speech.rs`, `src/app/speech.rs`.
- **Quint:** `commit.qnt` (unchanged: `confident` is abstract).

**Model.** The audio stream has no satellite ID. Thus `confident` is also true when the partial text is a choice or a cancel phrase of any open dialog.

**Acceptance criteria.**

- AC-10.7-1: When a dialog has the choice "kitchen" and the partial text is "kitchen", the commit uses `early_commit_ms`.
- AC-10.7-2: When no dialog is open and the top label is `unknown`, the partial text "kitchen" uses `endpoint_ms`.
- AC-10.7-3: When a dialog is open, the partial text "never mind" uses `early_commit_ms`.
- AC-10.7-4 [GPU host]: In the Phase 9 harness, the answer "the kitchen" to "Which room?" has a response latency p95 of 300 ms or less.

**Proofs.**

- P-10.7-1 to P-10.7-3. Argument: `confident = model_confident or any(dialog matches)`. `should_commit` does not change (P-6.5-1), so the commit invariants of `commit.qnt` still hold.
- P-10.7-4. Evidence: test.

## Field Log

These steps are not tests and gate nothing.

1. Make sure the satellite firmware supports `assist_satellite.start_conversation`.
2. Make sure `handled` with empty text makes the satellite speak nothing.
3. For each dialog in `00-overview.md` section 1.4, make sure the satellite listens after each question without the wake word.
