# Phase 12: Interaction Log and Feedback

This phase logs each request with its audio, action and response. You mark the action and the response as good or bad, with an optional correction. Marks change decisions at once, and Phase 13 trains on them.

---

### 12.1 Audio Correlation

**Goal:** Keep each committed utterance with its audio for a short window and find it again by its text. The conversation use case can then attach the audio to the request, although HA sends audio and text on different connections.

- **Implements:** G15. **Depends on:** 11.7.
- **Files:** modify `src/domain/speech.rs`, `src/app/speech.rs`, `jarvis.toml`.

**Model.** `Utterance { text, audio, time }`. The speech use case appends each committed utterance and drops those older than `log.correlate_s`. `correlate(utterances, text, now, window) -> Option<&Utterance>`: the newest utterance with equal normalized text and `now - time <= window`. This sub-phase adds `[log]` with `correlate_s`.

**Acceptance criteria.**

- AC-12.1-1: When an utterance has the same normalized text and is within `correlate_s`, `correlate` returns it.
- AC-12.1-2: When the only matching utterance is older than `correlate_s`, `correlate` returns none.
- AC-12.1-3: When two utterances have the same text, `correlate` returns the newer one.
- AC-12.1-4: When an utterance is older than `correlate_s`, the speech use case no longer keeps it.

**Proofs.**

- P-12.1-1 to P-12.1-3. Ensures: the result is `argmax time` over `{ u | normalize(u.text) = normalize(text) and now - u.time <= window }`. Argument: `correlate` filters by both conditions and takes `max_by_key(time)`. An empty filter gives `None`.
- P-12.1-4. Argument: each append first removes the entries with `now - time > window`.

---

### 12.2 Feedback Quint Model

**Goal:** Add `quint/feedback.qnt`, the model of good and bad marks on decisions. Prove that a decision with more bad marks than good marks is never chosen again.

- **Implements:** G15, G19. **Depends on:** 12.1.
- **Files:** create `quint/feedback.qnt`.
- **Quint:** `feedback.qnt`.

**Model.**

- Constants: `TEXTS = Set("t")`, `DECISIONS = Set("d1", "d2")` in a fixed candidate order.
- State: `good: (TEXT, DECISION) -> int`, `bad: (TEXT, DECISION) -> int`, `chosen: Option[DECISION]`, `toSystemTwo: bool`.
- Actions: `markGood(t, d)`, `markBad(t, d)`, `unmark(t, d, value)`, `decide(t)`: `chosen` is the first candidate that is not blocked, else `toSystemTwo := true`.
- `blocked(t, d) = bad[(t, d)] > good[(t, d)]`.
- Invariants:
  - `BlockedIffMoreBad`: `blocked` agrees with the counts in every state.
  - `DecideAvoidsBlocked`: after `decide(t)`, `chosen` is not blocked, or `toSystemTwo` is true.

**Acceptance criteria.**

- AC-12.2-1 [Quint]: `quint typecheck` passes.
- AC-12.2-2 [Quint]: `BlockedIffMoreBad` and `DecideAvoidsBlocked` hold in simulation and to 10 steps in `quint verify`.
- AC-12.2-3 [Quint]: The test `badThenTwoGood` passes: one bad and two good marks leave the decision not blocked.

**Proofs.**

- P-12.2-2. `BlockedIffMoreBad`: `blocked` is defined from the counts, so it holds in every state. `DecideAvoidsBlocked`: `decide` selects only from `filter(candidates, not blocked)`. If the filter is empty, it sets `toSystemTwo`.
- P-12.2-1, P-12.2-3. Evidence: model check.

---

### 12.3 Feedback Rules

**Goal:** Add the pure feedback model that implements `feedback.qnt`: marks, blocked decisions, bad responses, corrections and examples. Every later use of feedback reads from this one model.

- **Implements:** G15, G19. **Depends on:** 12.2.
- **Files:** create `src/domain/feedback.rs`. Modify `src/main.rs`.
- **Quint:** `feedback.qnt` (conformance).

**Model.**

- `Mark { interaction, target: Action | Response, value: Good | Bad | None, correction: Option<intent> }`.
- `Feedback::apply(self, mark) -> Self`. A `None` value removes the earlier mark of that interaction and target.
- `blocked(text, intent, targets) -> bool`: bad action marks are more than good action marks for that normalized text, intent and targets.
- `bad_responses() -> Set<template>`.
- `corrections(text, n) -> Vec<String>`: the bad actions and corrections for this text, in plain words.
- `examples(pool, n) -> (good, bad)`: response texts with their marks.

**Acceptance criteria.**

- AC-12.3-1: When the action for "turn on the tv" with `media_player.office` is marked bad, `blocked` returns true for that text, intent and target.
- AC-12.3-2: When the same action is marked bad once and good twice, `blocked` returns false.
- AC-12.3-3: When the action is marked bad for "turn on the tv", `blocked` returns false for "turn on the lights".
- AC-12.3-4: When a mark is removed, `blocked` returns the value from before the mark.
- AC-12.3-5: When a response is marked bad, `bad_responses` contains its template.
- AC-12.3-6: When a text has one bad action with the correction `tv_show`, `corrections` returns one line that names the intent, the targets and `tv_show`.
- AC-12.3-7: When `quint-connect` replays 100 traces of `feedback.qnt`, `blocked` equals the model `blocked` after each step.

**Proofs.**

- P-12.3-1 to P-12.3-3. Requires: `BlockedIffMoreBad`. Argument: `blocked` compares the two counts for the key `(normalize(text), intent, targets)`. Another text is another key.
- P-12.3-4 (AC-12.3-4). Argument: each interaction has at most one mark for each target. `apply` first subtracts the old mark from the counts and then adds the new one. A removal adds nothing, so the counts return to their earlier values.
- P-12.3-5, P-12.3-6. Argument: both functions are filters over the marks.
- P-12.3-7. Evidence: test.

---

### 12.4 Interaction Log Port and SQLite Storage

**Goal:** Add the `InteractionLog` port and its SQLite adapter with `append`, `update`, `list` and `audio`. Each request can now be stored with its audio and found again.

- **Implements:** G15. **Depends on:** 12.3.
- **Files:** create `src/adapters/sqlite.rs`. Modify `src/ports.rs`, `src/main.rs`, `src/testing/fakes.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `Interaction`: `id`, `time`, `satellite`, `transcript`, `model_context`, `top labels` (three labels with probabilities), `slots`, `embedding`, `decision`, `service calls`, `responses`, `latency_ms`, `link` (the first interaction of a dialog), `marks`.
- The audio is a BLOB of 16-bit mono PCM. The embedding is a BLOB of `f32`.
- `append`, `update(id, change)`, `list(filter, page) -> Vec<Interaction>` (newest first, `review.page_size`), `audio(id) -> Option<Audio>`.
- The adapter creates the schema when the database file does not exist.
- This sub-phase adds `log.database`.

**Acceptance criteria.**

- AC-12.4-1: When `append` runs, `list` returns the interaction with every field.
- AC-12.4-2: When `update` adds an announce text, `list` returns the interaction with that text.
- AC-12.4-3: When `list` gets a page size of 50 with 60 interactions, it returns 50, newest first.
- AC-12.4-4: When `audio` runs for an interaction with audio, it returns the same samples that `append` stored.
- AC-12.4-5: When the database file does not exist, the adapter creates it with the schema.

**Proofs.**

- P-12.4-3. Argument: the query is `ORDER BY time DESC LIMIT page_size OFFSET page * page_size`.
- P-12.4-1, P-12.4-2, P-12.4-4, P-12.4-5. Evidence: test.

---

### 12.5 Marks, Unknowns and Pruning

**Goal:** Add `mark`, `marked`, `unknowns` and `prune` to the log. Marks are stored with corrections, unknown requests can be clustered later, and old audio is deleted while marked records are kept.

- **Implements:** G14, G15. **Depends on:** 12.4.
- **Files:** modify `src/ports.rs`, `src/adapters/sqlite.rs`, `src/testing/fakes.rs`, `jarvis.toml`.

**Model.** `mark(mark)`, `marked() -> Vec<Interaction>`, `unknowns(since) -> Vec<Unknown { id, text, embedding, time }>` (System Two interactions after `since` without a bad action mark), `prune(now)`: delete audio older than `audio_days` and unmarked records older than `record_days`. This sub-phase adds `log.audio_days` and `log.record_days`.

**Acceptance criteria.**

- AC-12.5-1: When `mark` sets the action mark to `bad` with the correction `tv_show`, `marked` returns the interaction with the correction.
- AC-12.5-2: When `list` gets the filter `bad`, it returns only interactions with a bad mark.
- AC-12.5-3: When `unknowns(since)` runs, it returns only System Two interactions after `since` without a bad action mark, with their embeddings.
- AC-12.5-4: When `prune` runs, it deletes audio older than `audio_days` and unmarked records older than `record_days`.
- AC-12.5-5: When `prune` runs, it keeps every marked record, at any age.

**Proofs.**

- P-12.5-3. Argument: the query has the three conditions in its `WHERE` clause.
- P-12.5-4, P-12.5-5. Argument: the record delete has `marks IS NULL AND time < now - record_days`. A marked record fails the first condition.
- P-12.5-1, P-12.5-2. Evidence: test.

---

### 12.6 Record Interactions

**Goal:** Record every request in the log after the reply is sent, update the record with later announces, and prune once a day. The log is complete without adding latency.

- **Implements:** G15. **Depends on:** 12.5.
- **Files:** modify `src/app/conversation.rs`, `src/app/speech.rs`, `src/app/background.rs`.

**Model.** After `handle` returns, a spawned task correlates the audio (sub-phase 12.1) and appends the interaction. Deferred announces call `update` on the same ID. A dialog answer is a new interaction with `link` set to the first interaction. The background use case calls `prune` once a day.

**Acceptance criteria.**

- AC-12.6-1: When a voice request completes, `FakeLog` has one interaction with the audio, transcript, model context, prediction, decision, service calls, response and latency.
- AC-12.6-2: When a request comes from text with no audio, the interaction has no audio.
- AC-12.6-3: When a deferred answer is announced, the same interaction gets the announce text.
- AC-12.6-4: When a dialog asks questions, each answer is a new interaction with `link` set to the first interaction.
- AC-12.6-5: When `FakeLog` takes 500 ms for each call, `handle` returns before virtual time advances.
- AC-12.6-6: When one day passes on `FakeClock`, `FakeLog` records one `prune` call.

**Proofs.**

- P-12.6-5. Argument: the log write is spawned after the `Handled` value is built, and `handle` does not await it.
- P-12.6-2. Argument: `correlate` returns `None` when no utterance has the text (P-12.1-2).
- P-12.6-1, P-12.6-3, P-12.6-4, P-12.6-6. Evidence: test.

---

### 12.7 Review Port and Use Case

**Goal:** Add the `Review` driving port and its use case: list, audio, mark and the intent list. Marks reach the log and the feedback model through one path.

- **Implements:** G15. **Depends on:** 12.6.
- **Files:** create `src/app/review.rs`. Modify `src/ports.rs`, `src/main.rs`, `src/testing/fakes.rs`.

**Model.** `Review`: `list(filter, page)`, `audio(id)`, `mark(mark)`, `intents()`. `mark` checks that a correction is an intent name, writes the mark to the log and applies it to the feedback model.

**Acceptance criteria.**

- AC-12.7-1: When `mark` runs, `FakeLog` records the mark and the feedback model contains it.
- AC-12.7-2: When the mark value is `None`, the feedback model no longer has the earlier mark.
- AC-12.7-3: When the correction is not an intent name, `mark` returns an error and changes nothing.
- AC-12.7-4: When `intents` runs, it returns the catalog and active learned intent names.

**Proofs.**

- P-12.7-3. Argument: the name check runs before both writes and returns on failure.
- P-12.7-2. Argument: `Feedback::apply` with `None` (P-12.3-4).
- P-12.7-1, P-12.7-4. Evidence: test.

---

### 12.8 Review Web API

**Goal:** Add the review web server with token authentication and the JSON and WAV endpoints. You can now read and mark interactions over HTTP.

- **Implements:** G15. **Depends on:** 12.7.
- **Files:** create `src/adapters/web.rs`. Modify `src/main.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `GET /api/interactions?filter=&page=`, `GET /api/interactions/{id}/audio` (WAV), `GET /api/intents`, `POST /api/interactions/{id}/mark` with `{ "target": "action" | "response", "value": "good" | "bad" | null, "correction": "<intent>" | null }`.
- Each request needs `Authorization: Bearer <token>`.
- This sub-phase adds `[review]` (`listen`, `token_env`, `page_size`) and resolves the token in `Secrets`.

**Acceptance criteria.**

- AC-12.8-1: When a request has no token or a wrong token, the adapter returns 401.
- AC-12.8-2: When a request has the correct token, `GET /api/interactions` returns the page from `FakeReview`.
- AC-12.8-3: When `GET /api/interactions/{id}/audio` runs, the result is a valid WAV file with the stored samples.
- AC-12.8-4: When the interaction has no audio, the audio endpoint returns 404.
- AC-12.8-5: When `POST /mark` has a correction that is not an intent name, the adapter returns 400.

**Proofs.**

- P-12.8-1. Argument: one middleware layer wraps every route and compares the token before the handler runs.
- P-12.8-4, P-12.8-5. Argument: the handlers map `None` to 404 and the `Review::mark` error to 400.
- P-12.8-2, P-12.8-3. Evidence: test.

---

### 12.9 Review Page

**Goal:** Add the review page that plays each request's audio and lets you mark the action and the response, with an optional correction. Marking takes one click per interaction.

- **Implements:** G15. **Depends on:** 12.8.
- **Files:** create `src/adapters/review.html`. Modify `src/adapters/web.rs`.

**Model.** `GET /` returns `review.html` (embedded with `include_str!`). For each interaction, the page shows the time, satellite, transcript, an audio player, the action and the response, good and bad buttons for each, and a list of intents for the correction after a bad action mark. It has a filter for bad and unmarked interactions.

**Acceptance criteria.**

- AC-12.9-1: When `GET /` runs with the correct token, the result is HTML that contains the elements `#interactions`, `#filter`, `template#interaction`, an `audio` element in the template, and the buttons `.mark-good`, `.mark-bad` and the `select.correction` list.

**Proofs.**

- P-12.9-1. Evidence: test.

---

### 12.10 Feedback in Decide

**Goal:** Make `decide` skip blocked decisions. After a bad mark, the same request goes to the next entity, then the next likely intent, then System Two with the correction.

- **Implements:** G15, G19. **Depends on:** 12.9.
- **Files:** modify `src/domain/decide.rs`, `src/app/conversation.rs`.
- **Quint:** `feedback.qnt` (`DecideAvoidsBlocked`).

**Model.** Candidates in order: for the top intent, each entity choice in selection order. Then the same for each next intent with probability at least `model.threshold`. `decide` returns the first candidate that is not `blocked`. If none remains, it returns `Defer` with `Job::Llm`, and the `Question` contains `corrections(text, 3)`.

**Acceptance criteria.**

- AC-12.10-1: When the action for "turn on the tv" with the Office media player is marked bad, the next "turn on the tv" targets the next media player candidate.
- AC-12.10-2: When all entity candidates of the top intent are blocked, `decide` uses the next intent with probability at least `model.threshold`.
- AC-12.10-3: When all candidates are blocked, `decide` returns `Defer` with `Job::Llm`, and the `Question` contains the correction.

**Proofs.**

- P-12.10-1 to P-12.10-3. Requires: `DecideAvoidsBlocked` (P-12.2-2). Argument: `decide` is `candidates.find(not blocked)`, with `None` mapped to `Defer(Llm)`. This is the `decide` action of the model, with the candidate order of this sub-phase.

---

### 12.11 Feedback in Responses and System Two

**Goal:** Remove bad responses from selection, guide backfill with marked examples, and load all marks at startup. A response marked bad is never spoken again.

- **Implements:** G4, G5, G15. **Depends on:** 12.10.
- **Files:** modify `src/domain/responses.rs`, `src/adapters/openai.rs`, `src/app/background.rs`, `src/main.rs`.

**Model.** `select` excludes `bad_responses`. `needs_backfill` counts only responses that are not bad. The `generate` prompt includes `examples(pool, 5)`. At startup, the feedback model is built from `marked()`.

**Acceptance criteria.**

- AC-12.11-1: When a response is marked bad, `select` never returns it.
- AC-12.11-2: When all responses in a pool are marked bad, `needs_backfill` returns true.
- AC-12.11-3: When the backfill prompt runs, it contains the good and bad examples of the pool.
- AC-12.11-4: When Jarvis restarts, the feedback model contains every mark from the log.
- AC-12.11-5 [GPU host]: With 10,000 interactions and 1,000 marks, all Phase 9.2 latency tests pass.

**Proofs.**

- P-12.11-1. Argument: the candidate set of `select` (P-4.1-1) is now intersected with the complement of `bad_responses`.
- P-12.11-2. Argument: with all responses bad, the count of fresh responses that are not bad is 0, which is less than `min_fresh >= 1`.
- P-12.11-4. Argument: startup folds `apply` over `marked()`.
- P-12.11-3, P-12.11-5. Evidence: test.

---

## Field Log

These steps are not tests and gate nothing.

1. In a browser, open the review page, play the audio of an interaction, and send a mark with a correction.
