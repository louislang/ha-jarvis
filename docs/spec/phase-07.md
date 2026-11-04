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
- **Files:** modify `src/adapters/openai.rs`, `src/domain/decide.rs`, `src/domain/select.rs`.

**Model.** `Question { text, area, location, entities, now }` is in `src/domain/decide.rs`. The pure function `question(text, area, location, now, cache)` builds it. Sub-phase 7.5 calls it; this sub-phase does not wire it in. `entities` are the entities of every domain whose area (`entity_area`) is `area`, with name, entity ID and state, and without the entities that `select` skips (`is_skipped`: an entity category, hidden, disabled, or on a satellite device). `location` is the HA location name (`Snapshot.config.location_name`). `now` is the local time in the HA time zone (`Snapshot.config.time_zone`). The caller converts the time, so the domain needs no time zone database. `system_prompt()` is a constant. It asks for plain text to speak: short sentences, with no Markdown, lists or emoji. `user_message(question)` has the date, the time, the HA location name, the area, the entities and the text. The system prompt has no date, so prompt caching still works.

**Acceptance criteria.**

- AC-7.2-1: When the prompt builder runs for two different areas, the system messages are equal byte for byte.
- AC-7.2-2: When the prompt builder runs for the Kitchen, the user message has only Kitchen entities.
- AC-7.2-3: When the question has no area, the user message has no entities.
- AC-7.2-4: When `now` is Monday 2026-10-26 08:00 in `America/Chicago`, the user message has "Monday 2026-10-26 08:00" and the HA location name.
- AC-7.2-5: When the system prompt is read, it asks for plain text to speak, with no Markdown, lists or emoji.

**Proofs.**

- P-7.2-1. Ensures: `system_prompt()` gives the same string on every call. Argument: `system_prompt` is a constant and takes no input.
- P-7.2-4, P-7.2-5. Evidence: test.
- P-7.2-2, P-7.2-3. Ensures: the user message lists only the entities of the area, and none without an area. Argument: `question` keeps an entity only when `entity_area` gives `area` (P-2.8-3), and `None` gives an empty list.

---

### 7.3 Sentence Streaming

**Goal:** Add `answer` with streamed output and the `sentences` splitter. Jarvis gets each sentence as soon as it is complete, so it can announce before OpenAI finishes.

- **Implements:** G6, G7. **Depends on:** 7.2.
- **Files:** modify `src/adapters/openai.rs`, `src/ports.rs`, `src/domain/speech.rs`, `src/testing/fakes.rs`.

**Model.**

- `Reasoner::answer(question) -> Stream<Step>`. This sub-phase has `Step::Sentence(text)` and `Step::Done`.
- `sentences(buffer, chunk) -> (Vec<String>, String)`: append the chunk. Each text up to and including `.`, `?` or `!` followed by a space is complete. The rest stays in the buffer. At the end of the stream, the rest is one more sentence if it is not empty.
- The use case limits the wait for each step (sub-phase 7.5). Thus the adapter has no timer while it waits for the socket (`02-rules.md` section 6.2).

**Acceptance criteria.**

- AC-7.3-1: When `sentences` gets "Hello there. It is", it returns "Hello there." and keeps "It is".
- AC-7.3-2: When the stream ends, the rest is given as the last sentence.
- AC-7.3-3: When the joined sentences and the rest are compared with the joined chunks, they are equal after whitespace normalization.
- AC-7.3-4: When the mock streams a response with two sentences, `answer` gives `Sentence`, `Sentence` and `Done`, in that order.

**Proofs.**

- P-7.3-1, P-7.3-2. Ensures: each complete sentence ends with `.`, `?` or `!` before a space, and the rest has no such boundary. At the end of the stream, a rest that is not empty is the last sentence. Argument: `sentences` scans the buffer for the boundary pattern and splits at each match. The text after the last match is the rest.
- P-7.3-3 (AC-7.3-3). Claim: no text is lost or added. Argument: splitting a string at positions and keeping all pieces is a partition of the string. The only removed characters are the single spaces after boundaries, which whitespace normalization ignores.
- P-7.3-4. Evidence: test.

---

### 7.4 Tool Calls

**Goal:** Add the `call_service` tool, so System Two can act on HA. The application runs each call through `Home`, returns the result to the model, and continues the answer.

- **Implements:** G7. **Depends on:** 7.3.
- **Files:** modify `src/adapters/openai.rs`, `src/ports.rs`, `src/lib.rs`, `src/domain/decide.rs`, `jarvis.toml`.

**Model.** `Step::Call(ServiceCall, reply)`: the consumer runs the call and sends the result back on `reply`. The adapter sends the tool result to OpenAI and continues the stream. One answer makes at most `openai.max_tool_calls` tool calls. A later tool call gets the tool error "limit reached" and no `Step::Call`. This sub-phase adds `openai.max_tool_calls` (`OpenAiConfig` in `src/lib.rs`). The `target` of `call_service` names entity IDs, so `EntityState` gets `entity_id`, and each entity line of `user_message` shows it, for example "- Overhead Light (light.overhead): off".

- After a response with tool calls, the adapter sends the tool results with `previous_response_id`. Thus `answer` requests keep the OpenAI default `store` (true). `warm` keeps `store: false`.
- A successful call without response data gives the tool result "done". A call with response data gives its JSON. A failed call gives "error: " and the error text.
- Each tool call has `return_response: false`. Sub-phase 7.5 sets it to true for the HA data services in `openai.read_services`, for example `calendar.get_events`. Data from a remote server reaches System Two only through a plugin tool (phase 17).

**Acceptance criteria.**

- AC-7.4-1: When the mock returns a `call_service` tool call, `answer` gives `Step::Call` with the domain, service, target and data.
- AC-7.4-2: When the consumer sends the result, the adapter sends it to the mock and then gives the final text.
- AC-7.4-3: When the tool arguments are not valid, `answer` gives a tool error to the mock and continues.
- AC-7.4-4: When the mock returns one more tool call than `max_tool_calls` in one answer, `answer` gives no `Step::Call` for it and sends the tool error "limit reached".

**Proofs.**

- P-7.4-1, P-7.4-2. Evidence: test.
- P-7.4-3. Argument: argument parsing returns `Result`. The `Err` branch sends an error tool result instead of a `Step::Call`.
- P-7.4-4. Argument: the adapter counts the tool calls of one answer and takes the error branch when the count reaches the limit.

---

### 7.5 Deferred Answers

**Goal:** Connect `Job::Llm` to `answer` and the announce queue. An unknown request now gets a filler at once, then each answer sentence is announced on the same satellite.

- **Implements:** G6, G7. **Depends on:** 7.4.
- **Files:** modify `src/app/conversation.rs`, `src/lib.rs`, `src/domain/state_cache.rs`, `src/domain/contracts.rs`, `src/testing/fakes.rs`.
- **Quint:** `announce.qnt` (the same queue as sub-phase 4.9b).

**Model.**

- `StateCache::load` keeps `location_name` and `time_zone` of `Snapshot.config`. `ExHomeConfig` in `contracts.rs` loses `#[verifier::external_body]`, so Verus can read the fields (rule 5.1.8). The `after` work builds the question with `question` (sub-phase 7.2): the area of the request, the location name, and the current time in the HA time zone.
- `Defer(Llm)`: return the filler. The `after` work consumes `answer`: each `Sentence` goes to the announce queue of sub-phase 4.9b. An error announces from `failure`.
- Each `Call` gets the tool error "not permitted" and makes no call. Sub-phase 7.5b runs the calls.
- The use case waits at most `openai.timeout_ms` for each step of `answer`. A longer wait is a timeout error.
- `FakeReasoner` gives scripted steps: sentences, a `Call` (it records the tool result that comes back on the reply), an error, or a wait.

**Acceptance criteria.**

- AC-7.5-1: When `FakeClassifier` gives `unknown`, `handle` returns a filler before virtual time advances.
- AC-7.5-2: When `FakeReasoner` gives two sentences, `FakeHome` records two announces in the same order.
- AC-7.5-3: When the satellite becomes idle 1.5 s after the reply, the first announce starts at 1.5 s plus `margin_ms` or later.
- AC-7.5-5: When `answer` returns an error, `FakeHome` records an announce from `failure`.
- AC-7.5-9: When `FakeReasoner` gives one sentence and then waits, `FakeHome` records the announce of that sentence before the reasoner continues.
- AC-7.5-10: When `FakeReasoner` gives no step within `timeout_ms`, the job ends with a timeout error, and `FakeHome` records an announce from `failure`.
- AC-7.5-12: When `FakeReasoner` gives a `Call`, `FakeHome` records no call and the tool result is "not permitted".

**Proofs.**

- P-7.5-2, P-7.5-3. Requires: `SentencesInOrder` and `AnnounceWhenIdle` (P-4.7-2, P-4.7-3) and the conformance of sub-phase 4.9b. Argument: sentences enter the same queue that the model describes, in stream order.
- P-7.5-5. Argument: the error arm of the consumer selects from `failure`.
- P-7.5-9. Argument: the consumer puts each sentence in the queue when it arrives, with no wait for the next step.
- P-7.5-10. Argument: the wait for each step has the deadline `timeout_ms`, and the timeout arm takes the error arm (P-7.5-5).
- P-7.5-1. Argument: `Defer` returns the filler with the job as `after`, with no await on the reasoner.
- P-7.5-12. Argument: the `Call` arm sends "not permitted" and does not call `Home`.

---

### 7.5b Tool Calls Through HA

**Goal:** Run the `Call` steps of System Two through `Home`, after the domain and hot-cache checks. System Two can now act on HA and read HA data services.

- **Implements:** G7. **Depends on:** 7.5.
- **Files:** modify `src/app/conversation.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- Each `Call` runs through `Home` before the next step. This replaces the "not permitted" rule of sub-phase 7.5 and AC-7.5-12.
- A `Call` runs only when its service domain and the domain of each target entity are in `openai.allowed_domains`, and each target entity is in the hot cache. Else the consumer sends the tool error "not permitted" and makes no call. HA also takes targets from the keys `entity_id`, `device_id`, `area_id`, `floor_id` and `label_id` in the call data, so a `Call` whose data has one of these keys is also "not permitted". A misheard request thus cannot unlock a door. Leave `switch` out of the list when a switch drives a door, a gate or a lock. This sub-phase adds `openai.allowed_domains`.
- A `Call` whose service is in `openai.read_services` runs with `return_response: true`, and its response data is the tool result. Every other `Call` runs with `return_response: false`. Jarvis sets the flag from the list, not the model. A read service passes the same domain and hot-cache checks. HA has this data already, so it needs no plugin (phase 17). This sub-phase adds `openai.read_services`.

**Acceptance criteria.**

- AC-7.5b-1: When `FakeReasoner` gives `Call`, `FakeHome` records the call before the next step.
- AC-7.5b-2: When `FakeReasoner` gives a `Call` for the domain `lock`, which is not in `allowed_domains`, `FakeHome` records no call and the tool result is "not permitted".
- AC-7.5b-3: When `FakeReasoner` gives `light.turn_on` with the target `lock.front_door`, `FakeHome` records no call and the tool result is "not permitted".
- AC-7.5b-4: When `FakeReasoner` gives `calendar.get_events`, which is in `read_services`, `FakeHome` records the call with `return_response: true`, and the tool result is its response data. When it gives `light.turn_on`, `FakeHome` records `return_response: false`.

**Proofs.**

- P-7.5b-1. Argument: the consumer awaits the call result before it reads the next step.
- P-7.5b-2, P-7.5b-3. Argument: the `Call` arm checks the service domain, the domain of each target entity and the hot cache before it calls `Home`.
- P-7.5b-4. Argument: the `Call` arm sets `return_response` to whether `read_services` has the service.

---

### 7.5c Answers Without a Satellite

**Goal:** Return the answer of a deferred job as text when the request has no satellite. A typed request in HA now gets the answer instead of a filler.

- **Implements:** G6, G7. **Depends on:** 7.5b.
- **Files:** modify `src/app/conversation.rs`.

**Model.**

- Without a `satellite_id`, there is no satellite for an announce. `handle` then waits for the job and returns the joined sentences as the text, with no filler. A fetch job (sub-phase 4.9d) works the same way: `handle` returns the fetch answer as the text. This replaces the 4.9d rule that a fetch without a satellite only logs its answer.
- This narrows AC-5.3-4: `handle` returns the filler of a `Defer` only when the context has a `satellite_id`.
- The wait for an LLM job ends with the `openai.timeout_ms` error of each step (sub-phase 7.5). The wait for a fetch job ends with the HA request timeout (`ha.request_timeout_ms`).

**Acceptance criteria.**

- AC-7.5c-1: When the context has no `satellite_id` and `FakeReasoner` gives two sentences, `handle` returns both sentences as the text and `FakeHome` records no announce.
- AC-7.5c-2: When the context has no `satellite_id` and the job is a fetch, `handle` returns the fetch answer as the text.

**Proofs.**

- P-7.5c-1, P-7.5c-2. Argument: `handle` matches on `ctx.satellite_id`. The `None` arm awaits the job instead of returning the filler.
