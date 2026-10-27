# Phase 7: System Two

This phase sends `unknown` and low-probability requests to OpenAI. Jarvis speaks a filler, then announces the answer sentence by sentence.

---

### 7.1 Reasoner Port and Warm Connection

**Goal:** Add the `Reasoner` port and `OpenAiReasoner` with one long-lived HTTP client and `warm`. The first System Two request of a conversation does not pay for a new TLS connection.

- **Implements:** G7. **Depends on:** 6.10.
- **Files:** create `src/adapters/openai.rs`. Modify `src/ports.rs`, `src/app/speech.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `Reasoner::warm()`. Later sub-phases add `answer`, `generate`, `paraphrase` and `propose`.
- One HTTP client for the process life, with a connection pool without idle timeout and TCP keepalive.
- `warm` sends one small request when `now - last_request >= warm_idle_s`. Every request updates `last_request`.
- `listen` calls `warm` when a new utterance starts.
- This sub-phase adds `[openai]` (`api_key_env`, `model`, `timeout_ms`, `warm_idle_s`, `max_output_tokens`, `bulk_max_output_tokens`) and resolves the key in `Secrets`. `max_output_tokens` limits answers. `bulk_max_output_tokens` limits the structured outputs (`generate`, `paraphrase`, `propose`). A response with the status `incomplete` is an error.

**Acceptance criteria.**

- AC-7.1-1: When `warm` runs for the first time, the mock receives one request.
- AC-7.1-2: When `warm` runs again within `warm_idle_s`, the mock receives no new request.
- AC-7.1-3: When `warm` runs after `warm_idle_s`, the mock receives one new request.
- AC-7.1-4: When `listen` starts, `FakeReasoner` records one `warm` call.
- AC-7.1-5: When the mock returns a response with the status `incomplete`, the call returns an error.

**Proofs.**

- P-7.1-1 to P-7.1-3. Requires: `last_request` starts at minus infinity. Ensures: a request is sent if and only if `now - last_request >= warm_idle_s`. Argument: `warm` is one guarded send. The guard is that comparison, and the send updates `last_request`.
- P-7.1-4, P-7.1-5. Evidence: test.

---

### 7.2 Prompt Builder

**Goal:** Add the prompt builder with a static system prompt and a user message with only the area entities. OpenAI prompt caching can reuse the system prompt, and the request stays small.

- **Implements:** G7. **Depends on:** 7.1.
- **Files:** modify `src/adapters/openai.rs`, `src/ports.rs`, `src/domain/decide.rs`.

**Model.** `Question { text, area, entities, now }` is in `src/domain/decide.rs`, because `decide` builds it. `entities` are the entities in the area with name and state. `now` is the local time in the HA time zone (`Snapshot.config`). `system_prompt()` is a constant. `user_message(question)` has the date, the time, the HA location name, the area, the entities and the text. The system prompt has no date, so prompt caching still works.

**Acceptance criteria.**

- AC-7.2-1: When the prompt builder runs for two different areas, the system messages are equal byte for byte.
- AC-7.2-2: When the prompt builder runs for the Kitchen, the user message has only Kitchen entities.
- AC-7.2-3: When the question has no area, the user message has no entities.
- AC-7.2-4: When `now` is Monday 2026-10-26 08:00 in `America/Chicago`, the user message has "Monday 2026-10-26 08:00" and the HA location name.

**Proofs.**

- P-7.2-1. Argument: `system_prompt` is a constant and takes no input.
- P-7.2-4. Evidence: test.
- P-7.2-2, P-7.2-3. Argument: the entities come from `entities(area, ...)`, which returns only entities in the area (P-2.8-3), and `None` gives an empty list.

---

### 7.3 Sentence Streaming

**Goal:** Add `answer` with streamed output and the `sentences` splitter. Jarvis gets each sentence as soon as it is complete, so it can announce before OpenAI finishes.

- **Implements:** G6, G7. **Depends on:** 7.2.
- **Files:** modify `src/adapters/openai.rs`, `src/ports.rs`, `src/domain/speech.rs`, `src/testing/fakes.rs`.

**Model.**

- `Reasoner::answer(question) -> Stream<Step>`. This sub-phase has `Step::Sentence(text)` and `Step::Done`.
- `sentences(buffer, chunk) -> (Vec<String>, String)`: append the chunk. Each text up to and including `.`, `?` or `!` followed by a space is complete. The rest stays in the buffer. At the end of the stream, the rest is one more sentence if it is not empty.
- `answer` stops with a timeout error after `timeout_ms`.

**Acceptance criteria.**

- AC-7.3-1: When `sentences` gets "Hello there. It is", it returns "Hello there." and keeps "It is".
- AC-7.3-2: When the stream ends, the rest is given as the last sentence.
- AC-7.3-3: When the joined sentences and the rest are compared with the joined chunks, they are equal after whitespace normalization.
- AC-7.3-4: When the mock streams two sentences with a 1 s gap, `answer` gives the first sentence before the gap ends.
- AC-7.3-5: When the mock does not answer within `timeout_ms`, `answer` returns a timeout error.

**Proofs.**

- P-7.3-1, P-7.3-2. Argument: `sentences` scans the buffer for the boundary pattern and splits at each match. The text after the last match is the rest.
- P-7.3-3 (AC-7.3-3). Claim: no text is lost or added. Argument: splitting a string at positions and keeping all pieces is a partition of the string. The only removed characters are the single spaces after boundaries, which whitespace normalization ignores.
- P-7.3-4, P-7.3-5. Evidence: test.

---

### 7.4 Tool Calls

**Goal:** Add the `call_service` tool, so System Two can act on HA. The application runs each call through `Home`, returns the result to the model, and continues the answer.

- **Implements:** G7. **Depends on:** 7.3.
- **Files:** modify `src/adapters/openai.rs`, `src/ports.rs`, `src/testing/fakes.rs`.

**Model.** `Step::Call(ServiceCall, reply)`: the consumer runs the call and sends the result back on `reply`. The adapter sends the tool result to OpenAI and continues the stream.

**Acceptance criteria.**

- AC-7.4-1: When the mock returns a `call_service` tool call, `answer` gives `Step::Call` with the domain, service, target and data.
- AC-7.4-2: When the consumer sends the result, the adapter sends it to the mock and then gives the final text.
- AC-7.4-3: When the tool arguments are not valid, `answer` gives a tool error to the mock and continues.

**Proofs.**

- P-7.4-1, P-7.4-2. Evidence: test.
- P-7.4-3. Argument: argument parsing returns `Result`. The `Err` branch sends an error tool result instead of a `Step::Call`.

---

### 7.5 Deferred Answers

**Goal:** Connect `Job::Llm` to `answer` and the announce queue. An unknown request now gets a filler at once, then each answer sentence is announced on the same satellite.

- **Implements:** G6, G7. **Depends on:** 7.4.
- **Files:** modify `src/app/conversation.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `announce.qnt` (the same queue as sub-phase 4.9).

**Model.**

- `Defer(Llm)`: return the filler. The `after` work consumes `answer`: each `Sentence` goes to the announce queue of sub-phase 4.9. Each `Call` runs through `Home` before the next step. An error announces from `failure`.
- A `Call` runs only when its domain is in `openai.allowed_domains`. Else the consumer sends the tool error "not permitted" and makes no call. A misheard request thus cannot unlock a door. This sub-phase adds `openai.allowed_domains`.
- Without a `satellite_id`, there is no satellite for an announce. `handle` then waits for the job and returns the joined sentences as the text, with no filler. A fetch job (sub-phase 4.9) works the same way. The wait ends with the `timeout_ms` error.

**Acceptance criteria.**

- AC-7.5-1: When `FakeClassifier` gives `unknown`, `handle` returns a filler before virtual time advances.
- AC-7.5-2: When `FakeReasoner` gives two sentences, `FakeHome` records two announces in the same order.
- AC-7.5-3: When the filler audio is 1.5 s, the first announce starts at 1.5 s plus `margin_ms` or later.
- AC-7.5-4: When `FakeReasoner` gives `Call`, `FakeHome` records the call before the next step.
- AC-7.5-5: When `answer` returns an error, `FakeHome` records an announce from `failure`.
- AC-7.5-6: When `FakeReasoner` gives a `Call` for the domain `lock`, which is not in `allowed_domains`, `FakeHome` records no call and the tool result is "not permitted".
- AC-7.5-7: When the context has no `satellite_id` and `FakeReasoner` gives two sentences, `handle` returns both sentences as the text and `FakeHome` records no announce.

**Proofs.**

- P-7.5-2, P-7.5-3. Requires: `SentencesInOrder` and `AnnounceAfterFiller` (P-4.7-2, P-4.7-3) and the conformance of sub-phase 4.9. Argument: sentences enter the same queue that the model describes, in stream order.
- P-7.5-4. Argument: the consumer awaits the call result before it reads the next step.
- P-7.5-5. Argument: the error arm of the consumer selects from `failure`.
- P-7.5-6. Argument: the `Call` arm checks the domain before it calls `Home`.
- P-7.5-7. Argument: `handle` matches on `ctx.satellite_id`. The `None` arm awaits the job instead of returning the filler.
- P-7.5-1. Argument: `Defer` returns the filler with the job as `after`, with no await on the reasoner.
