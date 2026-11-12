# Phase 12: Interaction Log and Feedback

This phase logs each request with its audio, action and response. You mark the action and the response as good or bad, with an optional correction, in the Jarvis tab of the HA sidebar. The tab also shows the status of Jarvis and a form of chosen settings, and an HA entity shows the status. Marks change decisions at once, and Phase 13 trains on them.

---

### 12.1 Audio Correlation

**Goal:** Add the pure rules that keep committed utterances for a short window and find one again by its text. Sub-phase 12.1b uses them, so that a request can get its audio, although HA sends audio and text on different connections.

- **Implements:** G15. **Depends on:** 11.5.
- **Files:** modify `src/domain/speech.rs`.

**Model.** `Utterance { text, audio, time, voice_end }`. `expire(utterances, now, window)` keeps the utterances with `now - time <= window`, in order. `correlate(utterances, text, now, window) -> (rest, Option<Utterance>)` first drops the utterances older than `window`. Its result is the utterance with equal normalized text and `now - time <= window`, when exactly one matches. Two or more matches are ambiguous and give `None`, so wrong audio never drives identity or enrollment. A match removes the utterance from `rest`.

**Acceptance criteria.**

- AC-12.1-1: When an utterance has the same normalized text and is within `correlate_s`, `correlate` returns it.
- AC-12.1-2: When the only matching utterance is older than `correlate_s`, `correlate` returns none.
- AC-12.1-3: When two utterances in the window have the same text, `correlate` returns none.
- AC-12.1-4: When an utterance is older than `correlate_s`, `correlate` removes it from `rest`.
- AC-12.1-5: When `correlate` returns an utterance, a second `correlate` for the same text returns none.

**Proofs.**

- P-12.1-1 to P-12.1-3. Ensures: the result is `Some(u)` if and only if `u` is the only element of `{ u | normalize(u.text) = normalize(text) and now - u.time <= window }`. Argument: `correlate` filters by both conditions and matches on the result length: one gives `Some`, any other gives `None`.
- P-12.1-4, P-12.1-5. Ensures: `expire` gives exactly the entries with `now - time <= window`. After a `correlate`, no entry of `rest` is older than the window, and a returned utterance is not in `rest`. Argument: `expire` is a filter with that predicate. `correlate` first calls `expire`, and a match removes its entry, which is the only entry with its text.

---

### 12.1b Utterance Buffer

**Goal:** Keep each committed utterance with its audio for a short window, and correlate it for each voice request. Sub-phase 12.6 can then attach the audio to the request.

- **Implements:** G15. **Depends on:** 12.1.
- **Files:** modify `src/app/speech.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** The speech use case appends each transcript that it gives to HA, at a commit or at `audio-stop`, with the audio of the session. `voice_end` is the time of the last voiced sample: the time of the commit minus the pushed audio after the `voice_end_ms` of the last partial. Each append first drops the utterances older than `log.correlate_s` with `expire`, so expired audio leaves at the next use. `correlate` drops them too (sub-phase 12.1). `handle` of the `Assistant` in `src/lib.rs` correlates only when the context has a `satellite_id`, so a typed request never gets voice audio. Nothing uses the utterance before sub-phase 12.6. This sub-phase adds `[log]` with `correlate_s`.

**Acceptance criteria.**

- AC-12.1b-1: When an utterance is older than `correlate_s`, the next append or `correlate` removes it.
- AC-12.1b-2: When the context has no `satellite_id`, `handle` does not call `correlate`.

**Proofs.**

- P-12.1b-1. Argument: an append replaces the list with `expire` of the list before it adds the new utterance, and `correlate` drops the old entries (P-12.1-4).
- P-12.1b-2. Evidence: test.

---

### 12.2 Feedback Quint Model

**Goal:** Add `quint/feedback.qnt`, the model of good and bad marks on decisions. Prove that a decision with more bad marks than good marks is never chosen again.

- **Implements:** G15, G19. **Depends on:** 12.1b.
- **Files:** create `quint/feedback.qnt`.
- **Quint:** `feedback.qnt`.

**Model.**

- Constants: `TEXTS = Set("t")`, `DECISIONS = Set("d1", "d2")` in a fixed candidate order.
- State: `good: (TEXT, DECISION) -> int`, `bad: (TEXT, DECISION) -> int`, `chosen: Option[DECISION]`, `toSystemTwo: bool`, `chosenWasBlocked: bool` (history: `chosen` was blocked at the `decide` step).
- Actions: `markGood(t, d)`, `markBad(t, d)`, `unmark(t, d, value)`, `decide(t)`: `chosen` is the first candidate that is not blocked, else `toSystemTwo := true`. `decide` sets `chosenWasBlocked := chosen.isSome() and blocked(t, chosen)`.
- `blocked(t, d) = bad[(t, d)] > good[(t, d)]`.
- Invariants:
  - `DecideAvoidsBlocked`: `not chosenWasBlocked`. A later mark can block an earlier choice. The invariant is about the state at the `decide` step.

**Acceptance criteria.**

- AC-12.2-1 [Quint]: `quint typecheck` passes.
- AC-12.2-2 [Quint]: `DecideAvoidsBlocked` holds in simulation and to 10 steps in `quint verify`.
- AC-12.2-3 [Quint]: The test `ac_12_2_3_bad_then_two_good` passes: one bad and two good marks leave the decision not blocked.

**Proofs.**

- P-12.2-2. `DecideAvoidsBlocked`: `decide` selects only from `filter(candidates, not blocked)`, and it evaluates `chosenWasBlocked` in the same step, so the value is false. If the filter is empty, `chosen` is `None`. No other action writes `chosenWasBlocked`.
- P-12.2-1, P-12.2-3. Evidence: model check.

---

### 12.3 Blocked Decisions

**Goal:** Add the pure feedback model that implements `feedback.qnt`, with its types and `blocked`: a decision with more bad marks than good marks for a text. Every later use of feedback reads from this one model.

- **Implements:** G15, G19. **Depends on:** 12.2.
- **Files:** create `src/domain/feedback.rs`. Modify `src/lib.rs`.

**Model.**

- `Mark { interaction, target: Action | Response(index), value: Good | Bad | None, correction: Option<intent>, time }`. `interaction` is the ID of the marked interaction. `index` is the position of the response in the interaction (filler, reply, announce or question). A correction is an intent name or `unknown`. `unknown` means "send this to System Two".
- `Rated { transcript, link, action }`: the parts of the marked interaction that the feedback model reads. `link` is the transcript of the first interaction of the dialog, for a dialog answer. `action` is the intent and the targets. The caller makes `Rated` from the interaction (sub-phase 12.4).
- `Feedback` keeps each mark with its `Rated` and the normalized text that the mark counts for.
- `blocked(text, intent, targets) -> bool`: bad action marks are more than good action marks for that normalized text, intent and targets. `targets` is the sorted set of entity IDs.

**Acceptance criteria.**

- AC-12.3-1: When the action for "turn on the tv" with `media_player.office` is marked bad, `blocked` returns true for that text, intent and target.
- AC-12.3-2: When the same action is marked bad once and good twice, `blocked` returns false.
- AC-12.3-3: When the action is marked bad for "turn on the tv", `blocked` returns false for "turn on the lights".

**Proofs.**

- P-12.3-1 to P-12.3-3. Ensures: `blocked(t, i, g)` is true if and only if the bad action marks for the key `(normalize(t), i, g)` are more than the good ones. Argument: `blocked` is the `blocked` definition of `feedback.qnt`. It compares the two counts for the key `(normalize(text), intent, targets)`. Another text is another key.

---

### 12.3b Apply Marks

**Goal:** Add `Feedback::apply`, which adds, replaces and removes marks, and replay `feedback.qnt` against it. A mark changes `blocked` at once.

- **Implements:** G15, G19. **Depends on:** 12.3.
- **Files:** modify `src/domain/feedback.rs`.
- **Quint:** `feedback.qnt` (conformance).

**Model.**

- `Feedback::apply(self, rated, mark) -> Self`. The mark replaces the earlier mark of that interaction and target. A `None` value removes the earlier mark.
- A dialog answer (an interaction with `link`) is not a request of its own. A mark on it counts for the text of the first interaction of the dialog.

**Acceptance criteria.**

- AC-12.3b-1: When a mark is removed, `blocked` returns the value from before the mark.
- AC-12.3b-2: When `quint-connect` replays 100 traces of `feedback.qnt`, `blocked` equals the model `blocked` after each step.
- AC-12.3b-3: When the action of a dialog answer is marked bad, `blocked` returns true for the text of the first interaction, with the intent and the targets of the answer.

**Proofs.**

- P-12.3b-1 (AC-12.3b-1). Ensures: `apply` of a `None` mark gives the marks from before the removed mark. Argument: each interaction has at most one mark for each target. `apply` first removes the old mark and then adds the new one. A removal adds nothing, so the marks and their counts return to their earlier values.
- P-12.3b-3. Ensures: a mark on a dialog answer counts for the key of the text of the first interaction. Argument: `apply` takes the text that `link` holds when `link` is set.
- P-12.3b-2. Evidence: test.

---

### 12.3c Bad Responses and Corrections

**Goal:** Add the feedback rules for responses and System Two: bad responses, corrections and examples. Sub-phases 12.10 and 12.11 read them.

- **Implements:** G15. **Depends on:** 12.3b.
- **Files:** modify `src/domain/feedback.rs`.

**Model.**

- `Rated` gets `responses`: the text, the pool and the template of each response, in the order of the interaction (sub-phase 12.4).
- `bad_responses() -> Set<(pool, template)>`, from the pool and the template of each marked response.
- `corrections(text, n) -> Vec<String>`: the bad actions and corrections for this text, in plain words.
- `examples(pool, n) -> (good, bad)`: response texts with their marks.

**Acceptance criteria.**

- AC-12.3c-1: When the response at index 1 of an interaction is marked bad, `bad_responses` contains the pool and the template of that response, and no other response of the interaction.
- AC-12.3c-2: When a text has one bad action with the correction `tv_show`, `corrections` returns one line that names the intent, the targets and `tv_show`.

**Proofs.**

- P-12.3c-1, P-12.3c-2. Ensures: `bad_responses` has exactly the pool and template of each response with a bad mark, and `corrections` has one line for each bad action of the text. Argument: both functions are filters over the marks.

---

### 12.4 Interaction Log Port and SQLite Storage

**Goal:** Add the `InteractionLog` port and its SQLite adapter with `append`, `list` and `get`. Each request can now be stored with its audio and found again.

- **Implements:** G15. **Depends on:** 12.3c.
- **Files:** create `src/adapters/sqlite.rs`. Modify `src/ports.rs`, `src/domain/feedback.rs`, `src/domain/state.rs`, `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `Interaction` (in `src/domain/feedback.rs`, because the training domain reads it): `id`, `time`, `satellite`, `transcript`, `model_context`, `model` (the run name of the classifier), `top labels` (three labels with probabilities), `slots` (name, value, byte offsets in the normalized transcript), `action` (the `Action` of sub-phase 12.3: the intent of the decision and its targets), `decision` (the kind of the decision: `Reply`, `Fetch` for `Defer` with `Job::Fetch`, `Llm` for `Defer` with `Job::Llm`, or `Ask`), `service calls`, `responses` (text, pool, template), `latency_ms` (from the `voice_end` of the correlated utterance to the end of `handle`; none without audio), `link` (the ID of the first interaction of a dialog). The intent of `action` can be a next intent (sub-phase 12.10), so it is not always the top label. Sub-phase 12.5 adds `marks`.
- The audio is a BLOB of 16-bit mono PCM at 16 kHz.
- `append(interaction, audio)`, `list(filter, page) -> Vec<Interaction>` (newest first, `web.page_size`), `get(id) -> Option<Interaction>`. `get` gives the interaction that a mark or a `link` names, so the caller can make its `Rated` (sub-phase 12.3).
- The adapter creates the schema when the database file does not exist. It stores the schema version in `PRAGMA user_version`. The version starts at 1. A later sub-phase that changes the schema adds one migration step and increases the version. On start, the adapter runs the steps from the stored version to the current version in one transaction, so marks survive an update. A version newer than the code is an error with both versions.
- This sub-phase adds `log.database`.

**Acceptance criteria.**

- AC-12.4-1: When `append` runs, `list` and `get` return the interaction with every field.
- AC-12.4-2: When `list` gets a page size of 50 with 60 interactions, it returns 50, newest first.
- AC-12.4-3: When the database file does not exist, the adapter creates it with the schema.
- AC-12.4-4: When the database has a schema version newer than the code, the adapter returns an error with both versions.

**Proofs.**

- P-12.4-2. Argument: the query is `ORDER BY time DESC LIMIT page_size OFFSET page * page_size`.
- P-12.4-1, P-12.4-3, P-12.4-4. Evidence: test.

---

### 12.4b Interaction Updates and Audio

**Goal:** Add `update` and `audio` to the log, and `FakeLog`. Later announces and questions reach the stored interaction, and its audio can be played again.

- **Implements:** G15. **Depends on:** 12.4.
- **Files:** modify `src/ports.rs`, `src/adapters/sqlite.rs`, `src/testing/fakes.rs`.

**Model.** `update(id, change)`: a change adds a later announce or question to `responses`. `audio(id) -> Option<Audio>` gives the stored samples at 16 kHz. `FakeLog` keeps the interactions and their audio in memory, for the application tests of sub-phase 12.6.

**Acceptance criteria.**

- AC-12.4b-1: When `update` adds an announce text, `list` returns the interaction with that text.
- AC-12.4b-2: When `audio` runs for an interaction with audio, it returns the same samples that `append` stored.

**Proofs.**

- P-12.4b-1, P-12.4b-2. Evidence: test.

---

### 12.5 Marks

**Goal:** Add `mark` and `marked` to the log, and the filter `bad` to `list`. Marks are stored with their corrections and survive an update of the schema.

- **Implements:** G15. **Depends on:** 12.4b.
- **Files:** modify `src/ports.rs`, `src/domain/feedback.rs`, `src/adapters/sqlite.rs`, `src/testing/fakes.rs`.

**Model.** `Interaction` gets `marks`: its marks, at most one for each target. `mark(mark)` stores the mark with its time instead of the earlier mark of its interaction and target. A `None` value removes the earlier mark. `marked() -> Vec<Interaction>` gives the interactions with a mark, oldest first. The filter `bad` of `list` keeps the interactions with a bad mark. Migration step 2 of the schema (sub-phase 12.4) adds a `marks` table with one row for each mark. A read adds the marks of the interaction to its record. `FakeLog` keeps the marks in its interactions.

**Acceptance criteria.**

- AC-12.5-1: When `mark` sets the action mark to `bad` with the correction `tv_show`, `marked` returns the interaction with the correction.
- AC-12.5-2: When `list` gets the filter `bad`, it returns only interactions with a bad mark.
- AC-12.5-3: When the database has schema version 1 with records, the adapter migrates it to version 2 and keeps every record.

**Proofs.**

- P-12.5-1 to P-12.5-3. Evidence: test.

---

### 12.5b Deferred Requests and Pruning

**Goal:** Add `deferred` and `prune` to the log. Requests that System Two answered can be clustered later, and old audio is deleted while marked records are kept.

- **Implements:** G14, G15. **Depends on:** 12.5.
- **Files:** modify `src/ports.rs`, `src/domain/feedback.rs`, `src/adapters/sqlite.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `deferred(since) -> Vec<Deferred { id, text, time, spoken }>` (interactions after `since` whose decision is `Defer` with `Job::Llm`, without a bad action mark and without a `link`; `spoken` is true when the interaction has a `satellite`; `Deferred` is in `src/domain/feedback.rs`), `prune(now)`: delete audio older than `audio_days` and unmarked records older than `record_days`. `prune` keeps a record that a marked record links to, so `get` of the `link` of a marked dialog answer still makes its `Rated` (sub-phase 12.7). Thus `deferred` gives each request that System Two answered: a top label `unknown`, a top probability less than `model.threshold`, a label that is not in the catalog, a blocked decision (sub-phase 12.10), or an intent that System One cannot act on (sub-phase 4.5). A dialog answer is not a request, so `deferred` leaves it out. `FakeLog` records each `prune` call for sub-phase 12.6. This sub-phase adds `log.audio_days` and `log.record_days`.

**Acceptance criteria.**

- AC-12.5b-1: When `deferred(since)` runs, it returns only interactions after `since` whose decision is `Defer` with `Job::Llm`, without a bad action mark and without a `link`. A System Two interaction whose top label is a known intent with a low probability, or a known intent that System One cannot act on, is in the result. A fetch (`Defer` with `Job::Fetch`) is not in the result. `spoken` is true only for an interaction with a `satellite`.
- AC-12.5b-2: When `prune` runs, it deletes audio older than `audio_days` and unmarked records older than `record_days`.
- AC-12.5b-3: When `prune` runs, it keeps every marked record, at any age.
- AC-12.5b-4: When `prune` runs, it keeps an unmarked first interaction of a dialog, at any age, when a marked answer of the dialog links to it.

**Proofs.**

- P-12.5b-1. Argument: the query has the four conditions in its `WHERE` clause.
- P-12.5b-2 to P-12.5b-4. Argument: the record delete has `time < now - record_days AND id NOT IN (SELECT interaction FROM marks) AND id NOT IN (the link of each marked record)`. A marked record fails the second condition. A first interaction that a marked record links to fails the third condition. The delete never removes a marked record, so the link stays.

---

### 12.6 Record Interactions

**Goal:** Record every decided request in the log after the reply is sent. The log gets each request with its audio without adding latency.

- **Implements:** G15. **Depends on:** 12.5b.
- **Files:** modify `src/app/conversation.rs`, `src/app/speech.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `src/testing/e2e.rs`.

**Model.** `handle` makes the interaction ID before dispatch. The first ID is the clock time in microseconds at the start, and each request adds one. Sub-phase 12.6b also starts above the stored IDs. When the context has a `satellite_id`, `handle` correlates the audio once. This call moves from `src/lib.rs` (sub-phase 12.1b) into the conversation use case. A match removes the utterance, so `handle` is the only caller: it sends the utterance with the append, and sub-phase 15.6 uses the same utterance for the speaker. Before it returns, it sends the append to the log writer task on an unbounded channel. The writer runs the messages in the order it receives them. The record has the ID, the time, the satellite, the transcript, the model context, the model `fixture` (the model folder that Jarvis loads), the three top labels, the decision and the latency. Its action is the top label with no targets. Sub-phase 12.6b adds the targets, the slots, the service calls and the responses. `FakeLog` gets `delay`: each `append` and `update` waits it. The end-to-end harness (sub-phase 9.1) keeps the log database in its temporary folder.

**Acceptance criteria.**

- AC-12.6-1: When a voice request completes, `FakeLog` has one interaction with the audio, transcript, model context, top labels, decision and latency.
- AC-12.6-2: When a request comes from text with no audio, the interaction has no audio.
- AC-12.6-3: When `FakeLog` waits 500 ms for each call, `handle` returns before virtual time advances.

**Proofs.**

- P-12.6-3. Argument: a send on an unbounded channel does not wait, and `handle` does not await the writer.
- P-12.6-2. Argument: `correlate` returns `None` when no utterance has the text (P-12.1-2).
- P-12.6-1. Evidence: test.

---

### 12.6b Record Decisions and Daily Prune

**Goal:** Add the parts of the decision to each record, keep new IDs above the stored IDs, and prune the log once a day. A mark can then rate the action and each response.

- **Implements:** G15. **Depends on:** 12.6.
- **Files:** modify `src/app/conversation.rs`, `src/app/background.rs`, `src/adapters/sqlite.rs`, `src/lib.rs`.

**Model.** The record gets these values:

- `action` is the top label and the sorted targets of the service call of the decision. Sub-phase 12.10 changes it. `service calls` is the service call of the decision.
- A slot gets the byte offsets of the first match of its value in the normalized transcript. A slot without a match is left out.
- `responses` has the text that `handle` returns, with its pool and template: the reply, the filler or the question of the decision, or the deferred answer for a request without a satellite. A System Two text has an empty pool and template.

The first interaction ID is the larger of the clock time in microseconds at the start and the largest stored ID plus one. Thus a clock that moved back before a restart does not reuse a stored ID. At the start, before the log writer task starts, `SqliteLog` gives the largest stored ID with `SELECT MAX(id)`. `list` cannot give it, because it orders by time. The background use case calls `prune` once a day, the first time one day after the start.

**Acceptance criteria.**

- AC-12.6b-1: When a voice request with the area slot `kitchen` completes, the interaction has the slot with its byte offsets, the action with its sorted targets, the service calls, and the response with its pool and template.
- AC-12.6b-2: When one day passes on `FakeClock`, `FakeLog` records one `prune` call.
- AC-12.6b-3: When the log has an interaction whose ID is larger than the start time in microseconds, the first new interaction gets that ID plus one.

**Proofs.**

- P-12.6b-3. Argument: the counter starts at the larger of the start time and the largest stored ID plus one, and each request adds one. Thus each new ID is larger than every stored ID.
- P-12.6b-1, P-12.6b-2. Evidence: test.

---

### 12.6c Interaction Updates

**Goal:** Add later announces and questions to the record of their request. The record holds everything that Jarvis said for the request.

- **Implements:** G15. **Depends on:** 12.6b.
- **Files:** modify `src/app/conversation.rs`.

**Model.** Deferred work gets the interaction ID. After an announce or a question is spoken, the work sends an `update` to the writer on the same channel, so it comes after the append. An announce that the queue drops adds nothing. A System Two sentence or question has an empty pool and template.

**Acceptance criteria.**

- AC-12.6c-1: When a deferred answer is announced, the same interaction gets the announce text.
- AC-12.6c-2: When a deferred announce sends its update before the writer has run the append, the record has the announce text.

**Proofs.**

- P-12.6c-2. Argument: the append message is sent before `handle` returns, and every update is sent later on the same channel. A channel keeps the order.
- P-12.6c-1. Evidence: test.

---

### 12.6d Dialog Links

**Goal:** Record each dialog answer with a link to the first interaction of the dialog. A mark on an answer can then count for the request of the dialog.

- **Implements:** G15. **Depends on:** 12.6c.
- **Files:** modify `src/app/conversation.rs`.

**Model.** A dialog answer is a new interaction with `link` set to the first interaction of the dialog. The queues keep the first ID for each dialog key: a new request with a key sets it, and an answer reads it. These dialog answers do not reach `decide`, and each is still a new interaction:

- An unclear answer to a `Slot` question has its prediction, the top label as its action, and the decision `Ask`, or `Reply` with the `failure` answer at the turn limit.
- An unclear answer to a System Two question has its prediction, the top label as its action, and the decision `Llm`.

A cancel phrase is not logged, because it has no prediction and no action.

**Acceptance criteria.**

- AC-12.6d-1: When a dialog asks questions, each answer is a new interaction with `link` set to the first interaction. This includes an unclear answer to a `Slot` question and an answer to a System Two question.
- AC-12.6d-2: When a cancel phrase closes a dialog, `FakeLog` has no interaction for it.

**Proofs.**

- P-12.6d-2. Argument: the cancel branch returns before the append.
- P-12.6d-1. Evidence: test.

---

### 12.7 Review Port and Use Case

**Goal:** Add the `Review` driving port and its use case: list, audio, mark and the intent list. Marks reach the log and the feedback model through one path.

- **Implements:** G15. **Depends on:** 12.6d.
- **Files:** create `src/app/review.rs`. Modify `src/ports.rs`, `src/lib.rs`, `src/testing/fakes.rs`.

**Model.** `Review`: `list(filter, page)`, `audio(id)`, `mark(mark)`, `intents()`. `mark` checks that a correction is an intent name or `unknown`, writes the mark to the log and applies it to the feedback model. It makes the `Rated` of the marked interaction with `get`: its transcript, action and responses, and the transcript of the interaction that its `link` names.

**Acceptance criteria.**

- AC-12.7-1: When `mark` runs, `FakeLog` records the mark and the feedback model contains it.
- AC-12.7-2: When the mark value is `None`, the feedback model no longer has the earlier mark.
- AC-12.7-3: When the correction is not an intent name, `mark` returns an error and changes nothing.
- AC-12.7-4: When `intents` runs, it returns the catalog intent names and `unknown`. Sub-phase 14.7 adds the active learned intent names.

**Proofs.**

- P-12.7-3. Argument: the name check runs before both writes and returns on failure.
- P-12.7-2. Argument: `Feedback::apply` with `None` (P-12.3b-1).
- P-12.7-1, P-12.7-4. Evidence: test.

---

### 12.8 Web API

**Goal:** Add the web server of the Jarvis tab with token authentication and the review endpoints. You can now read and mark interactions over HTTP, and the Jarvis add-on (sub-phase 12.9g) can put the server into the HA sidebar.

- **Implements:** G15, G24. **Depends on:** 12.7.
- **Files:** create `src/adapters/web.rs`. Modify `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `GET api/interactions?filter=&page=`, `GET api/interactions/{id}/audio` (WAV), `GET api/intents`, `POST api/interactions/{id}/mark` with `{ "target": "action" | "response", "index": <n> | null, "value": "good" | "bad" | null, "correction": "<intent>" | null }`. `index` is the position of the response in the interaction (sub-phase 12.3). It is required for `response`.
- HA ingress serves the Jarvis tab under a path prefix (`/api/hassio_ingress/<session>/`). The add-on removes the prefix before it sends a request to Jarvis. Thus the routes of the server are the same with and without the add-on.
- Each request under `api/` needs `Authorization: Bearer <token>`. The add-on adds this header to each request that it sends to Jarvis (sub-phase 12.9g). Thus the page has no token prompt and keeps no token. A request from another machine on the LAN needs the header too.
- `run` serves the web server on `web.listen` next to the Wyoming server. The shutdown future of `run` stops both (sub-phase 5.4c).
- This sub-phase adds `[web]` (`listen`, `token_env`, `page_size`) and resolves the token in `Secrets`. `web.page_size` is the page size of `list` (sub-phase 12.4).

**Acceptance criteria.**

- AC-12.8-1: When a request under `api/` has no token or a wrong token, the adapter returns 401.
- AC-12.8-2: When a request has the correct token, `GET api/interactions` returns the page from `FakeReview`.
- AC-12.8-3: When `GET api/interactions/{id}/audio` runs, the result is a valid WAV file with the stored samples.
- AC-12.8-4: When the interaction has no audio, the audio endpoint returns 404.
- AC-12.8-5: When `POST api/interactions/{id}/mark` has a correction that is not an intent name, the adapter returns 400.
- AC-12.8-6: When `run` starts, the web server answers on `web.listen`. When the shutdown future completes, the web server stops.
- AC-12.8-7: When the mark request has `"target": "response"` and `"index": 2`, `FakeReview` receives a mark with `Response(2)`. Without `index`, the adapter returns 400.

**Proofs.**

- P-12.8-1. Argument: one middleware layer wraps every route under `api/` and compares the token before the handler runs.
- P-12.8-4, P-12.8-5. Argument: the handlers map `None` to 404 and the `Review::mark` error to 400.
- P-12.8-7. Argument: the handler maps `("response", Some(n))` to `Response(n)` and `("response", None)` to 400.
- P-12.8-2, P-12.8-3, P-12.8-6. Evidence: test.

---

### 12.9 Review Page

**Goal:** Add the page of the Jarvis tab with the review section. It plays the audio of each request and lets you mark the action and the response, with an optional correction. Marking takes one click per interaction, and the page works under the path prefix of HA ingress.

- **Implements:** G15, G24. **Depends on:** 12.8.
- **Files:** create `src/adapters/review.html`. Modify `src/adapters/web.rs`.

**Model.**

- `GET /` returns `review.html` (embedded with `include_str!`) without a token. The page has no data. The page script is inline JavaScript in `review.html` (`CLAUDE.md` permits it).
- Every URL in the page is relative, for example `api/interactions`, not `/api/interactions`. HA ingress serves the page under a path prefix, so an absolute URL misses the add-on.
- The page sends no `Authorization` header, because the add-on adds it. Thus the `audio` element can load `api/interactions/{id}/audio` directly.
- For each interaction, the page shows the time, satellite, transcript, an audio player, the action and the response, good and bad buttons for each, and a list of intents for the correction after a bad action mark. It has a filter for bad and unmarked interactions.

**Acceptance criteria.**

- AC-12.9-1: When `GET /` runs without a token, the result is HTML that contains the elements `#interactions`, `#filter`, `template#interaction`, an `audio` element in the template, and the buttons `.mark-good`, `.mark-bad` and the `select.correction` list.
- AC-12.9-2: When the inline script of `review.html` is extracted, `node --check` exits with code 0.
- AC-12.9-3: When `review.html` is checked, no `src`, `href` or `action` attribute and no string in the script starts with `/`, `http:` or `https:`, and the script does not contain `Authorization`.

**Proofs.**

- P-12.9-1 to P-12.9-3. Evidence: test.

---

### 12.9b Status

**Goal:** Add the status of Jarvis to the API and to the page. You see in the Jarvis tab that Jarvis is online. Later sub-phases add training, restarting and the rollback message.

- **Implements:** G24. **Depends on:** 12.9.
- **Files:** modify `src/ports.rs`, `src/lib.rs`, `src/adapters/web.rs`, `src/adapters/review.html`, `src/testing/fakes.rs`.

**Model.**

- `Status { state: State, message: Option<String> }` and `State { Online, Training, Restarting, Offline }` are in `src/ports.rs`. The JSON form is `{ "state": "online", "message": null }`, with the state in lowercase.
- The composition root keeps the status in a `tokio::sync::watch` channel. The channel starts with `Online` and no message. The web server starts only when the start is ready (sub-phase 12.8), so the API never gives a status before that. Sub-phases 12.9c, 12.9f and 13.10b read or change the status.
- The new driving port `Control` has `status() -> Status`. `src/lib.rs` implements it with the receiver of the channel. `adapters/web.rs` calls it. `FakeControl` in `src/testing/fakes.rs` gives a set status.
- `GET api/status` returns the status.
- The page has a status section `#status` with `.state` and `.message`. The page reads `api/status` at load and each 5 s. When the request fails, for example when the add-on returns its down page (sub-phase 12.9g), the section shows `offline`.

**Acceptance criteria.**

- AC-12.9b-1: When `run` has started, `GET api/status` returns `{ "state": "online", "message": null }`.
- AC-12.9b-2: When `FakeControl` gives `training` with the message `run 7`, `GET api/status` returns both.
- AC-12.9b-3: When `GET /` runs, the HTML contains `#status` with the elements `.state` and `.message`.

**Proofs.**

- P-12.9b-1. Argument: the channel starts with `Online` and no message, and nothing changes it in this sub-phase.
- P-12.9b-2, P-12.9b-3. Evidence: test.

---

### 12.9c Status Entity

**Goal:** Publish the status of Jarvis as the HA entity `sensor.jarvis_status` through MQTT discovery, with a retained state and a last will. Dashboards and automations see `online`, `training`, `restarting` and `offline`, also when Jarvis dies, and the entity stays after an HA restart.

- **Implements:** G24. **Depends on:** 12.9b.
- **Files:** create `src/adapters/mqtt.rs`. Modify `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- This sub-phase adds the optional section `[mqtt]`: `url` (for example `mqtt://homeassistant.local:1883`), `username_env` and `password_env`. `Secrets` resolves the user name and the password. Without the section, Jarvis publishes no status.
- `adapters/mqtt.rs` connects to the broker with `rumqttc`, with the client ID `jarvis` and a keep-alive of 30 s. It reads the status from a receiver of the status channel. No use case calls it, so it has no port (`01-architecture.md` section 1).
- The last will is the payload `{ "state": "offline", "message": null }` on the topic `jarvis/status`, with QoS 1 and retain. The broker publishes it when the connection ends without `DISCONNECT`, at the latest 1.5 keep-alive intervals (45 s) after the last packet.
- At each `ConnAck`, the first connection and each reconnection, the adapter publishes these two messages with QoS 1 and retain:
  - On `homeassistant/sensor/jarvis/status/config`: `{ "name": "Status", "unique_id": "jarvis_status", "device": { "identifiers": ["jarvis"], "name": "Jarvis" }, "device_class": "enum", "options": ["online", "training", "restarting", "offline"], "state_topic": "jarvis/status", "value_template": "{{ value_json.state }}", "json_attributes_topic": "jarvis/status" }`. HA makes the entity ID `sensor.jarvis_status` from the device name and the entity name. The field `message` of the status becomes an attribute of the entity.
  - On `jarvis/status`: the current status.
- Each change of the status publishes `jarvis/status` again, with retain.
- The broker keeps both messages. The entity has a `unique_id`, so HA makes it again from the retained discovery message after an HA restart, with the retained state.
- After a connection error, the adapter waits 5 s, and its next poll connects again.
- When the sender of the status channel closes, the adapter publishes the last status, sends `DISCONNECT` and ends. After `DISCONNECT` the broker does not publish the will. At the end of `run`, the root sets `Offline` and then drops the sender. Thus a stop gives `offline`.
- `run` starts the adapter when the start is ready. Thus the entity shows `online` only when Jarvis answers. Before that, the entity keeps the retained state of the earlier process.
- The tests use an in-process `rumqttd` broker on a local port and a `rumqttc` client that subscribes. They do not pause the clock, and they do not sleep: each test waits for the messages that it expects.

**Acceptance criteria.**

- AC-12.9c-1: When the adapter connects, a client that subscribes after it gets the retained discovery message with `unique_id` `jarvis_status`, `state_topic` `jarvis/status` and the four options, and the retained state `{ "state": "online", "message": null }`.
- AC-12.9c-2: When the status changes to `training`, the client gets `{ "state": "training", "message": null }` with retain.
- AC-12.9c-3: When the adapter task is aborted, so that the connection ends without `DISCONNECT`, the client gets `{ "state": "offline", "message": null }`.
- AC-12.9c-4: When the status is `restarting` and the sender of the channel closes, the adapter ends, and a client that subscribes after the end gets `restarting`.
- AC-12.9c-5: When the shutdown future of `run` completes, the last status in the channel is `offline`.
- AC-12.9c-6: When `[mqtt]` is not set, `run` starts and opens no MQTT connection.

**Proofs.**

- P-12.9c-1. Argument: the `ConnAck` arm of the poll loop publishes both messages, so each connection publishes them again.
- P-12.9c-5. Argument: `run` sends `Offline` after the servers stop and before it drops the sender.
- P-12.9c-2 to P-12.9c-4, P-12.9c-6. Evidence: test.

**Field log.** In the MQTT broker on the HA host, make the user of Jarvis (for the Mosquitto add-on, an HA user or the `logins` option). Add `MQTT_USERNAME` and `MQTT_PASSWORD` to `jarvis.env`, and set `mqtt.url` to the broker. Restart Jarvis. Record that `sensor.jarvis_status` is `online` and belongs to the device Jarvis. Stop the unit `jarvis` and record `offline`. Restart HA and record that the entity keeps its state.

---

### 12.9d Settings Fields

**Goal:** Add the fields of the settings form and the pure functions that read them from the text of `jarvis.toml` and change them in it. A change keeps the comments and the order of the file, and no secret or path is a field.

- **Implements:** G24. **Depends on:** 12.9c.
- **Files:** create `src/adapters/settings.rs`. Modify `src/lib.rs`, `Cargo.toml`.

**Model.**

- `FIELDS` is the fixed list of the form fields, in groups, in this order:

  | Group | Fields |
  |---|---|
  | Speech | `asr.vad_threshold`, `asr.vad_min_silence_ms`, `asr.early_commit_ms`, `asr.endpoint_ms`, `asr.max_utterance_ms` |
  | Voice | `tts.voice`, `tts.num_steps` |
  | System One | `model.threshold`, `model.commit_threshold` |
  | System Two | `openai.model`, `openai.timeout_ms`, `openai.max_output_tokens`, `openai.max_tool_calls` |
  | Dialogs | `dialog.max_turns`, `dialog.timeout_s`, `dialog.cancel` |
  | Announce | `announce.margin_ms`, `announce.idle_wait_s` |
  | Habits | `habits.half_life_days`, `habits.min_score` |
  | Log | `log.audio_days`, `log.record_days` |

- The form does not have these fields:
  - `listen` and `web.listen`: a wrong value cuts off the Jarvis tab itself.
  - `device`, each file and folder, `ha.url`, the plugin URLs, `mqtt.url` and `[train.runpod]`: a wrong value stops the start.
  - Each `*_env` field: it names a secret.
  - `openai.allowed_domains`, `openai.read_services` and `learn.prefetch_services`: they decide which HA services OpenAI and a prefetch can call.
  - `train.max_vram_gb` and `train.python`: they protect the GPU host.
  - `[[people]]` and `[[satellites]]`: lists of tables.
  - The backfill values, `plugins.cache_s`, `plugins.timeout_ms` and the HA time limits: internal values.
  - Each field that a later sub-phase adds to `jarvis.toml`. A field joins the form only when you ask.
- `Setting { group, name, value }`. `value` is a JSON number, a JSON string or a JSON list of strings.
- `read(text) -> Result<Vec<Setting>>` gives each field of `FIELDS` with its value in `text`, in the order of `FIELDS`.
- `edit(text, changes: BTreeMap<String, serde_json::Value>) -> Result<String, String>` sets each changed field in `text` with `toml_edit`. All other text stays the same, also the comments. A name that is not in `FIELDS` is an error that names it. A new value keeps the TOML type of the old value: integer, float, string or array of strings. A JSON number for a float field becomes a float. A JSON value of another kind is an error that names the field and the type. `edit` does not check ranges: sub-phase 12.9e checks the result with `Config::parse`.

**Acceptance criteria.**

- AC-12.9d-1: When `read` gets the `jarvis.toml` of the repository, it returns exactly the fields of `FIELDS`, in order, with the values of the file, and no name ends with `_env`.
- AC-12.9d-2: When `edit` changes `asr.endpoint_ms` to 700, the result differs from the input only in that line, and the comment of the line stays.
- AC-12.9d-3: When `edit` gets `ha.url` or `ha.token_env`, it returns an error that names the field, for each.
- AC-12.9d-4: When `edit` gets a string for `asr.endpoint_ms`, it returns an error that names the field. When it gets the JSON number 3 for `habits.min_score`, the result has `min_score = 3.0`.

**Proofs.**

- P-12.9d-3. Argument: `edit` looks up each name in `FIELDS` before it changes the document, and returns the error on the first name that is not there.
- P-12.9d-4. Argument: `edit` matches on the pair of the old TOML type and the JSON kind. Only the matching pairs and the pair of a float and a JSON number give a value.
- P-12.9d-1, P-12.9d-2. Evidence: test.

---

### 12.9e Settings Form

**Goal:** Show the settings fields in the Jarvis tab and save a change with the same checks as at the start. Jarvis refuses a bad value with a message and does not change the file.

- **Implements:** G24. **Depends on:** 12.9d.
- **Files:** modify `src/ports.rs`, `src/lib.rs`, `src/adapters/web.rs`, `src/adapters/review.html`, `src/testing/fakes.rs`.

**Model.**

- `Control` gets `settings() -> Result<Vec<Setting>>` and `save(changes) -> Result<(), Refusal>`. `Refusal::Invalid(message)`. `Setting` moves to `src/ports.rs`.
- In `src/lib.rs`, `settings` reads the configuration file that `run` got and calls `read`. `save` reads the file, calls `edit`, and checks the new text with `Config::parse`. Thus the rules of the start apply (sub-phase 1.1 and later). Only when the check passes, `save` writes the new text with an atomic write (`tempfile`). Else it returns `Invalid` with the message of `edit` or `Config::parse`. The new values take effect at the next start. Sub-phase 12.9f restarts Jarvis.
- `GET api/settings` returns the fields. `POST api/settings` with `{ "<name>": <value>, ... }` returns 204 on success, and 400 with `{ "error": "<message>" }` for `Invalid`.
- The page has `form#settings` with one `fieldset` for each group and one input for each field: a number input for a number, a text input for a string, and a `textarea` with one line for each string of a list. `button#save` sends only the changed fields. `#settings-error` shows the message of a 400.

**Acceptance criteria.**

- AC-12.9e-1: When `save` sets `asr.endpoint_ms` to 700 in a temporary copy of `jarvis.toml`, the file has the new value, and `settings` returns it.
- AC-12.9e-2: When `save` sets `asr.early_commit_ms` to 700 while `asr.endpoint_ms` is 600, it returns `Invalid` with the message of `Config::parse`, and the file does not change.
- AC-12.9e-3: When `save` gets `ha.token_env`, it returns `Invalid`, and the file does not change.
- AC-12.9e-4: When `FakeControl` refuses a save with a message, `POST api/settings` returns 400 with that message. When it accepts, the result is 204.
- AC-12.9e-5: When `GET /` runs, the HTML contains `form#settings`, `button#save` and `#settings-error`.

**Proofs.**

- P-12.9e-2, P-12.9e-3. Argument: `save` calls `edit` and `Config::parse` before the write, and returns on the first error.
- P-12.9e-1, P-12.9e-4, P-12.9e-5. Evidence: test.

---

### 12.9f Restart and Rollback

**Goal:** Restart Jarvis after a save, so that the new settings take effect. When the start with the new file fails or crashes, put the last good file back and start again, and show the rollback in the status.

- **Implements:** G24. **Depends on:** 12.9e.
- **Files:** modify `src/ports.rs`, `src/lib.rs`, `src/adapters/settings.rs`, `src/adapters/web.rs`, `src/adapters/review.html`, `.gitignore`.

**Model.**

- Three files can be next to the configuration file `<config>`:
  - `<config>.bak`: the last good text, from a save until the next start is ready.
  - `<config>.trying`: the marker of the trial start, the first start after a save. It holds the error text of a failed trial start, or nothing.
  - `<config>.rollback`: the reason of the last rollback.
- `Activity { Idle, Restarting }` is in `src/ports.rs`. The root keeps one `Arc<Mutex<Activity>>`. Each change of it locks the mutex, checks the old value and sets the new value in one critical section. Sub-phase 13.10b adds `Training`.
- `save` first changes the activity from `Idle` to `Restarting`. With another value, it returns `Refusal::Busy` and reads and writes nothing. When the check of sub-phase 12.9e fails, `save` sets `Idle` again. Else the activity stays `Restarting` until the process ends, so a second save cannot replace `<config>.bak` with an untested file. `POST api/settings` maps `Busy` to 409 with `{ "error": "Jarvis is busy" }`.
- After the check, `save` writes the old text to `<config>.bak`, writes the new text, and removes `<config>.rollback`. Then it sets the status to `Restarting` and notifies the shutdown.
- The shutdown future of `serve` completes on SIGINT, SIGTERM or this notification. `run` returns `Ended::Restart` after the notification and `Ended::Stop` after a signal. At `Restart`, `run` keeps the status `Restarting`, so the entity shows `restarting` while Jarvis is down (sub-phase 12.9c). At `Stop`, it sets `Offline`.
- `run` calls `settings::begin(config)` first, before it loads the configuration:
  - When `<config>.trying` exists, the trial start failed or crashed. `begin` renames `<config>.bak` to `<config>` and `<config>.trying` to `<config>.rollback`. An empty reason becomes `start after save crashed`. Jarvis logs one `error` event and starts with the old file.
  - Else, when `<config>.bak` exists, this start is the trial start. `begin` makes an empty `<config>.trying`.
- `serve_until` calls `settings::confirm(config)` when the start is ready: after the models load and after the Wyoming listener and the web listener bind, before it accepts the first connection. `confirm` removes `<config>.trying` and `<config>.bak`. Thus a bind error of the trial start is an error with `<config>.trying`, and the next start rolls back.
- `start` gives the exit code from the result of `serve`:
  - `Ok(Restart)`: 75.
  - `Ok(Stop)`: 0.
  - `Err(error)` while `<config>.trying` exists: the trial start failed. `start` writes the error text to `<config>.trying`. The exit code is 75.
  - `Err(error)` without `<config>.trying`: 1, as before.
- A panic in a `tokio` task is an error of `serve`. For a panic of the main thread, `serve` sets a panic hook with `panic::set_hook`. The hook calls the default hook, and then exits with `settings::panic_code(config)`: 75 when `<config>.trying` exists, else 101. Thus a crash of the trial start gives 75, and the next start rolls back with the reason `start after save crashed`.
- The unit `jarvis` has `RestartForceExitStatus=75` (`01-architecture.md` section 10). systemd starts Jarvis again after the exit code 75 only.
- The restarts do not loop. Jarvis exits with 75 only after a save or when the trial start fails. The start after a failed trial start rolls back and removes `<config>.trying` and `<config>.bak`, so it is not a trial start. When it fails, it exits with 1 or 101, and systemd does not start it again. Thus one save gives at most two automatic restarts.
- When the trial start dies from a signal (for example `SIGSEGV` in native code), systemd does not start Jarvis again. `<config>.trying` stays, so the next start, by hand or at boot, rolls back.
- When `<config>.rollback` exists at the start, the status starts with the message `settings rolled back: <reason>`. The message stays until the next save removes the file.
- After a save gives 204, the page shows `restarting` in `#status` until `api/status` gives `online`. Then it reads `api/settings` again, so a rollback shows the old values.
- `.gitignore` ignores `/jarvis.toml.bak`, `/jarvis.toml.trying` and `/jarvis.toml.rollback`.

**Acceptance criteria.**

- AC-12.9f-1: When `save` succeeds, `<config>.bak` has the old text, `<config>` has the new text, the status is `restarting`, and `run` returns `Ended::Restart`.
- AC-12.9f-2: When `serve` returns `Ended::Restart`, the exit code is 75. When it returns `Ended::Stop`, the exit code is 0.
- AC-12.9f-3: When `begin` runs while `<config>.bak` exists and `<config>.trying` does not exist, `<config>.trying` exists after it. When the start is ready, neither `<config>.bak` nor `<config>.trying` exists.
- AC-12.9f-4: When `serve` returns an error while `<config>.trying` exists, `<config>.trying` has the error text, and the exit code is 75. When `panic_code` runs while `<config>.trying` exists, it returns 75.
- AC-12.9f-5: When `begin` runs while `<config>.trying` has an error text, `<config>` has the text of the backup, `<config>.rollback` has the error text, and neither `<config>.bak` nor `<config>.trying` exists. When `<config>.trying` is empty, `<config>.rollback` has `start after save crashed`.
- AC-12.9f-6: When a save, a failed trial start and a failed start after the rollback occur in sequence, the exit codes are 75, 75 and 1, and `<config>` has the old text. When `panic_code` runs without `<config>.trying`, it returns 101.
- AC-12.9f-7: When `<config>.rollback` exists at the start, `GET api/status` gives the message `settings rolled back: <reason>`. After the next save, `<config>.rollback` does not exist.
- AC-12.9f-8: When two saves run at the same time, one succeeds and the other returns `Busy`, and `<config>.bak` has the text from before both saves. When a save fails the check, the next valid save succeeds. When `FakeControl` returns `Busy`, `POST api/settings` returns 409.
- AC-12.9f-9: When the web listener cannot bind during a trial start, because its port is in use, `<config>.trying` has the bind error, `<config>.bak` exists, and the exit code is 75. The next `begin` restores the backup.

**Proofs.**

- P-12.9f-1. Argument: `save` writes the backup before the new text, and both after the check (P-12.9e-2). The notification comes after the writes.
- P-12.9f-2, P-12.9f-4. Argument: `start` matches on the result of `serve` and on the existence of `<config>.trying`. `panic_code` checks the same file.
- P-12.9f-3, P-12.9f-5. Argument: `begin` checks `<config>.trying` first and `<config>.bak` second. `serve_until` calls `confirm` after both binds and before it accepts a connection.
- P-12.9f-9. Argument: a bind error returns from `serve_until` before `confirm`, so `<config>.trying` stays, and the error arm of `start` writes it (P-12.9f-4). The next `begin` rolls back (P-12.9f-5).
- P-12.9f-6. Argument: an exit code of 75 needs `Ended::Restart`, which only a save gives, or `<config>.trying`, which only `begin` makes at the start after a save. The start after a failed trial start removes `<config>.trying` and `<config>.bak` in `begin`, and only a new save writes `<config>.bak` again. Thus that start exits with 1 or 101.
- P-12.9f-8. Argument: only the change from `Idle` to `Restarting` lets `save` read the file, and it runs in one critical section, so one save at a time gets it. Only a failed check sets `Idle` again, before any write.
- P-12.9f-7. Evidence: test.

**Field log.** Add `RestartForceExitStatus=75` to the unit `jarvis` and run `systemctl daemon-reload`. In the Jarvis tab, change `dialog.timeout_s` and save. Record that the status shows `restarting`, then `online`, and that the new value is in `jarvis.toml`. Then set `tts.voice` to a voice that the TTS model does not have and save. Record that the status and the attribute `message` of `sensor.jarvis_status` show `settings rolled back` with the error, and that `jarvis.toml` has the old value.

---

### 12.9g Home Assistant Add-on

**Goal:** Add the Jarvis add-on, which puts the Jarvis tab into the HA sidebar through HA ingress. It is a reverse proxy to the web server of Jarvis on the GPU host, and it shows a down page when Jarvis does not answer.

- **Implements:** G24. **Depends on:** 12.9f.
- **Files:** create `repository.yaml`, `addon/config.yaml`, `addon/build.yaml`, `addon/Dockerfile`, `addon/run.sh`, `addon/nginx.conf`, `addon/down.html`. Modify `xtask/src/gates.rs`.

**Model.**

- The repository is an HA add-on repository. `repository.yaml` names it, and the folder `addon/` holds the one add-on. The Supervisor builds the image on the HA host from `addon/Dockerfile`.
- `addon/config.yaml`: `name: Jarvis`, `slug: jarvis`, a `version`, `arch: [amd64, aarch64]`, `init: false`, `ingress: true`, `ingress_port: 8099`, `panel_title: Jarvis`, `panel_icon: mdi:robot`, the options `url` (default `http://192.168.1.65:10701`) and `token`, and the schema `url: url` and `token: password`. Each change of a file in `addon/` increases `version`, so HA offers the update.
- `addon/build.yaml` names the base image of each architecture: `ghcr.io/home-assistant/<arch>-base` with a fixed version tag. The base image has `bashio` and the s6 init.
- `addon/Dockerfile` starts with `ARG BUILD_FROM` and `FROM $BUILD_FROM`, installs `nginx` with `apk`, copies `nginx.conf` to `/etc/nginx/nginx.conf`, `down.html` to `/www/down.html` and `run.sh` to `/run.sh`, and runs `/run.sh`.
- `addon/run.sh` has at most 10 lines. It reads the options `url` and `token` with `bashio::config`, puts them in place of `@URL@` and `@TOKEN@` in `/etc/nginx/nginx.conf` with `sed`, and runs `exec nginx -g 'daemon off;'`.
- `addon/nginx.conf` listens on port 8099. It accepts only `172.30.32.2`, the ingress gateway of the Supervisor, and denies all other clients. It sends each request to `@URL@` with `proxy_set_header Authorization "Bearer @TOKEN@"` and a connect time limit of 5 s. `error_page 502 503 504 /down.html` serves `/www/down.html` when Jarvis does not answer. The status code stays 502, 503 or 504, so the page script sees the failure (sub-phase 12.9b).
- `addon/down.html` is a static page with the text "Jarvis is down" and `<meta http-equiv="refresh" content="10">`. It has no script, so `src/adapters/review.html` stays the only JavaScript. The refresh loads the same relative URL again, so the tab shows Jarvis again when it answers.
- The add-on does not publish the status entity. Jarvis publishes it through MQTT (sub-phase 12.9c), so the entity does not depend on the add-on.
- CI has no Supervisor. Structure tests in `xtask/src/gates.rs` check the files with `serde_yaml` and text checks. The field log checks the add-on in HA.

**Acceptance criteria.**

- AC-12.9g-1: When `repository.yaml` is parsed, it has a `name` and the `url` `https://github.com/louislang/ha-jarvis`.
- AC-12.9g-2: When `addon/config.yaml` is parsed, it has `slug: jarvis`, `ingress: true`, `ingress_port: 8099`, `panel_title`, `panel_icon`, `init: false`, the architectures `amd64` and `aarch64`, a `version` of the form `x.y.z`, the options `url` and `token`, and the schema `url: url` and `token: password`.
- AC-12.9g-3: When `addon/build.yaml` is parsed, `build_from` names `ghcr.io/home-assistant/<arch>-base` for `amd64` and `aarch64`, each with a version tag and not `latest`.
- AC-12.9g-4: When `addon/nginx.conf` is checked, it listens on the `ingress_port` of `addon/config.yaml`, has `allow 172.30.32.2;` before `deny all;`, `proxy_pass @URL@;`, `proxy_set_header Authorization "Bearer @TOKEN@";` and `error_page 502 503 504 /down.html;`.
- AC-12.9g-5: When `addon/run.sh` and `addon/Dockerfile` are checked, `run.sh` has at most 10 lines, reads the options `url` and `token`, replaces `@URL@` and `@TOKEN@`, and ends with `exec nginx`. The `Dockerfile` copies `nginx.conf`, `down.html` and `run.sh` to the paths that `run.sh` and `nginx.conf` use.
- AC-12.9g-6: When `addon/down.html` is checked, it has the text "Jarvis is down", a `meta` refresh, no `<script` and no URL that starts with `/`, `http:` or `https:`.

**Proofs.**

- P-12.9g-1 to P-12.9g-6. Evidence: test.

**Field log.** Add `JARVIS_WEB_TOKEN` to `jarvis.env` with a value from `openssl rand -hex 32` (the token goes through `sed`, so use only hex characters), and restart Jarvis. In HA, open Settings, Add-ons, Add-on Store, Repositories, and add `https://github.com/louislang/ha-jarvis`. Install the add-on Jarvis. Set `url` to `http://192.168.1.65:10701` and `token` to the same value. Start the add-on and turn on "Show in sidebar". Record that the Jarvis tab shows the interactions, the status and the settings, also in the HA app outside the house. Stop the unit `jarvis` and record that the tab shows "Jarvis is down". Start the unit and record that the tab shows Jarvis again.

---

### 12.10 Feedback in Decide

**Goal:** Make `decide` skip blocked decisions. After a bad mark, the same request goes to the next entity, then the next likely intent, then System Two with the correction.

- **Implements:** G15, G19. **Depends on:** 12.9g.
- **Files:** modify `src/domain/decide.rs`, `src/app/conversation.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `feedback.qnt` (`DecideAvoidsBlocked`).

**Model.** Candidates in order: for the top intent, its target choices. With `target = "entity"`, each matching entity is one choice, in selection order. With `target = "entities"`, the set of all matching entities is the only choice. Then the same for each next intent with probability at least `model.next_threshold`. A next intent with a missing slot that is not `optional` is skipped, because Jarvis does not ask about a second guess. Softmax probabilities sum to 1 and `model.threshold > 0.5` (sub-phase 2.2), so a next intent cannot also reach `model.threshold`. This sub-phase adds `model.next_threshold`. `decide` returns the first candidate that is not `blocked`. If none remains, it returns `Defer` with `Job::Llm`, and the `Question` contains `corrections(text, 3)`.

**Acceptance criteria.**

- AC-12.10-1: When the action for "turn on the tv" with the Office media player is marked bad, the next "turn on the tv" targets the next media player candidate.
- AC-12.10-2: When all entity candidates of the top intent are blocked and the probabilities are 0.7 for the top intent and 0.25 for the next intent, `decide` uses the next intent.
- AC-12.10-3: When all candidates are blocked, `decide` returns `Defer` with `Job::Llm`, and the `Question` contains the correction.
- AC-12.10-4: When a `target = "entities"` action with all kitchen lights is blocked, `decide` does not try a single light.
- AC-12.10-5: When the next intent needs a slot that the prediction does not have, `decide` does not use it.

**Proofs.**

- P-12.10-4. Ensures: a `target = "entities"` intent gives one candidate, the set of all matching entities. Argument: a `target = "entities"` intent has one choice, the whole set, so no single entity is a candidate.
- P-12.10-1 to P-12.10-3. Requires: `DecideAvoidsBlocked` (P-12.2-2). Ensures: `decide` gives the first candidate that is not blocked, else `Defer` with `Job::Llm` and the corrections. Argument: `decide` is `candidates.find(not blocked)`, with `None` mapped to `Defer(Llm)`. This is the `decide` action of the model, with the candidate order of this sub-phase.
- P-12.10-5. Ensures: no candidate comes from a next intent with a missing slot that is not `optional`. Argument: the candidate list filters the next intents by their slots before it adds their choices.

---

### 12.11 Feedback in Responses and System Two

**Goal:** Remove bad responses from selection, guide backfill with marked examples, and load all marks at startup. A response marked bad is never spoken again.

- **Implements:** G4, G5, G15. **Depends on:** 12.10.
- **Files:** modify `src/domain/responses.rs`, `src/adapters/openai.rs`, `src/app/background.rs`, `src/lib.rs`.

**Model.** `select` now returns `Option<usize>`. The allowed responses are those that are not bad. If two or more are allowed, `last` is also excluded. With none allowed, `select` returns `None`, and dispatch replies with empty text. No-bad takes precedence over no-repeat. `needs_backfill` counts only responses that are not bad. The `generate` prompt includes `examples(pool, 5)`. At startup, the feedback model is built from `marked()`, with the `Rated` of each interaction as in sub-phase 12.7.

**Acceptance criteria.**

- AC-12.11-1: When a response is marked bad, `select` never returns it.
- AC-12.11-2: When all responses in a pool whose templates use only `area` are marked bad, `needs_backfill` returns true.
- AC-12.11-3: When the backfill prompt runs, it contains the good and bad examples of the pool.
- AC-12.11-4: When Jarvis restarts, the feedback model contains every mark from the log.
- AC-12.11-5 [GPU host]: With 10,000 interactions and 1,000 marks, all Phase 9.2 latency tests pass.
- AC-12.11-6: When every response of a pool is bad, `select` returns none and the reply text is empty. When a pool of two has one bad response, `select` returns the other one, also when it is `last`. AC-4.1-1 now applies to the responses that are not bad.

**Proofs.**

- P-12.11-1, P-12.11-6. Ensures: `select` never returns a bad response, excludes `last` only when two or more responses are not bad, and returns `None` when every response is bad. Argument: `select` first removes `bad_responses`, then removes `last` only when two or more remain, then takes the minimum. An empty set gives `None`.
- P-12.11-2. Ensures: a pool with every response bad needs a backfill. Argument: with all responses bad, the count of fresh responses that are not bad is 0, which is less than `min_fresh >= 1`.
- P-12.11-4. Ensures: the feedback model after startup has every mark of the log. Argument: startup folds `apply` over `marked()`.
- P-12.11-3, P-12.11-5. Evidence: test.

---

## Field Log

These steps are not tests and gate nothing.

1. In the Jarvis tab of HA, play the audio of an interaction, and send a mark with a correction.
2. Do the field logs of sub-phases 12.9c, 12.9f and 12.9g.
