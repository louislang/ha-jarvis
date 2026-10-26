# Phase 4: System One Decisions

This phase selects responses, selects entities and decides the action from a prediction. Then it does the side effects through the ports.

---

### 4.1 Response Selection

**Goal:** Add response pools with `select` and `record_use`. Jarvis now varies its replies: it never repeats the last response and prefers the least used one.

- **Implements:** G4. **Depends on:** 3.12.
- **Files:** create `src/domain/responses.rs`, `responses.toml`. Modify `src/main.rs`.

**Model.**

- `Pools: Map<pool name, Pool>`. `Pool { responses: Vec<Response { text, uses }>, last: Option<usize> }`.
- `select(pool, rng) -> usize`: the candidates are the responses with the lowest `uses`. If there are two or more responses, the candidates exclude `last`. The result is a random candidate.
- `record_use(pools, name, index) -> Pools`: `uses += 1` for that response, and `last := index`.

**Acceptance criteria.**

- AC-4.1-1: When a pool has two or more responses, `select` never returns `last`.
- AC-4.1-2: When responses have different use counts, `select` returns a response with the lowest count among the allowed responses.
- AC-4.1-3: When `select` gets the same seeded RNG and the same pool, it returns the same index.
- AC-4.1-4: When `record_use` runs, the use count of that response increases by 1 and every other count does not change.
- AC-4.1-5: When a pool has one response, `select` returns it.

**Proofs.**

- P-4.1-1, P-4.1-2 (AC-4.1-1, AC-4.1-2). Requires: the pool is not empty. Ensures: the result is not `last` (when `len >= 2`), and its `uses` is the minimum over `responses \ {last}`. Argument: the candidate set is `argmin uses` over `responses \ {last}`. With `len >= 2`, that set is not empty. `select` chooses only from the candidate set.
- P-4.1-3. Argument: `select` reads only the pool and the RNG.
- P-4.1-4. Argument: `record_use` does one indexed update.
- P-4.1-5. Argument: with `len = 1`, the exclusion of `last` does not apply, so the only response is the only candidate.

---

### 4.2 Templates and Pool Validation

**Goal:** Add `render` with `minijinja` in strict mode and `validate_pools`. Responses and service data now share one template syntax, and a missing variable or a missing pool fails at startup.

- **Implements:** G4. **Depends on:** 4.1.
- **Files:** modify `src/domain/responses.rs`, `Cargo.toml`.

**Model.**

- `render(template, context) -> Result<String>`. Undefined variables are errors.
- `validate_pools(pools, catalog) -> Result<()>`: each intent has a pool, the `filler` and `failure` pools exist, and every template parses.
- Template variables: `area`, slot names, `entity`, `response`, `person` (sub-phase 15.7).

**Acceptance criteria.**

- AC-4.2-1: When `render` gets `area = kitchen`, "Powering up the {{ area }} lights." becomes "Powering up the kitchen lights."
- AC-4.2-2: When a template uses a variable that the context does not have, `render` returns an error that names the variable.
- AC-4.2-3: When an intent has no pool, `validate_pools` returns an error that contains the intent name.
- AC-4.2-4: When a template has a syntax error, `validate_pools` returns an error that contains the pool name.
- AC-4.2-5: When `filler` or `failure` is missing, `validate_pools` returns an error that names it.

**Proofs.**

- P-4.2-1. Evidence: test.
- P-4.2-2. Argument: the environment uses `UndefinedBehavior::Strict`, which turns every undefined lookup into an error.
- P-4.2-3 to P-4.2-5. Argument: `validate_pools` checks each intent name, each template and each required pool name in loops, and returns the first failure.

---

### 4.3 File Store

**Goal:** Add the `Store<T>` port and `FileStore`, which saves TOML or JSON with an atomic write. Pools, habits and other state now survive a restart and a crash during a save.

- **Implements:** G10. **Depends on:** 4.2.
- **Files:** create `src/adapters/files.rs`. Modify `src/ports.rs`, `src/main.rs`, `Cargo.toml`.

**Model.** `Store<T>`: `load() -> Result<Option<T>>`, `save(&T) -> Result<()>`. The format comes from the file extension (`.toml` or `.json`). `save` writes to a temporary file in the same folder, then renames it over the target.

**Acceptance criteria.**

- AC-4.3-1: When `FileStore` saves to a `.toml` path and loads again, the data is equal.
- AC-4.3-2: When `FileStore` saves to a `.json` path and loads again, the data is equal.
- AC-4.3-3: When the save stops before the rename, the file still contains the last valid data.
- AC-4.3-4: When the file does not exist, `load` returns `None` and no error.
- AC-4.3-5: When the extension is not `.toml` or `.json`, the constructor returns an error with the path.

**Proofs.**

- P-4.3-3 (AC-4.3-3). Requires: rename in one folder is atomic on the file system. Ensures: the target is either the old content or the new content. Argument: the target changes only through the rename. A stop before the rename leaves the target unchanged.
- P-4.3-4, P-4.3-5. Argument: `load` maps "not found" to `Ok(None)`. The constructor matches the extension with two allowed values.
- P-4.3-1, P-4.3-2. Evidence: test.

---

### 4.4 Entity Selection

**Goal:** Add entity selection for an intent and an area, with filters, exclusions and the single-entity house fallback. "Turn on the lights" now finds the right lights, including switches named as lights.

- **Implements:** G9. **Depends on:** 4.3.
- **Files:** create `src/domain/select.rs`. Modify `src/main.rs`.

**Model.**

- A filter has `domain`, optional `name` (matches the entity name or an alias, without case sensitivity, as a substring) and optional `device_class`. An entity matches when one filter matches and it is not in `exclude`.
- `select(intent, area, cache) -> Vec<EntityId>`:
  - `target = "entities"`: all matching entities in the area.
  - `target = "entity"`: the fixed `entity`, else the first matching entity in the area (sub-phase 11.6 uses the highest habit score).
  - If the area has no match and the house has exactly one match, the result is that entity.

**Acceptance criteria.**

- AC-4.4-1: When the intent is `lights_on` and the area is the Kitchen, the result is all kitchen lights.
- AC-4.4-2: When the intent is `lights_on` in the Garage, the result includes the switch "Garage Light".
- AC-4.4-3: When the intent is `lights_on` in the Living Room, the result does not include the switch "Living Room Fan".
- AC-4.4-4: When the intent is `fan_on` in the Living Room, the result is the switch "Living Room Fan".
- AC-4.4-5: When the intent is `climate_down` in the Bedroom and the only climate entity is in the Kitchen, the result is that entity.
- AC-4.4-6: When a filter has `name = "tv"`, an entity with the alias "the TV" matches.
- AC-4.4-7: When an entity is in `exclude`, it is never in the result.
- AC-4.4-8: When the area has no match and the house has two matches, the result is empty.

**Proofs.**

- P-4.4-3, P-4.4-7 (AC-4.4-3, AC-4.4-7). Ensures: every result entity matches a filter and is not excluded. Argument: both `target` branches and the fallback draw only from `matching = filter(cache, filters) \ exclude`.
- P-4.4-5, P-4.4-8 (AC-4.4-5, AC-4.4-8). Ensures: the fallback applies if and only if `matching ∩ area = ∅` and `|matching| = 1`. Argument: the fallback branch is guarded by these two conditions.
- P-4.4-1, P-4.4-2, P-4.4-4, P-4.4-6. Evidence: test on the reference house.

---

### 4.5 Decide Commands

**Goal:** Add `decide` for command intents and the System Two threshold. A confident prediction now becomes a reply and a service call, and an uncertain or `unknown` one becomes a deferred System Two job.

- **Implements:** G3, G7, G9. **Depends on:** 4.4.
- **Files:** create `src/domain/decide.rs`. Modify `src/main.rs`, `jarvis.toml`.

**Model.**

- `Decision`: `Reply { text, call: Option<ServiceCall> }`, `Defer { filler, job: Fetch(ServiceCall, pool) | Llm(question) }`, `Fallback`.
- `decide(prediction, context, catalog, cache, pools, rng)`:
  1. If the top label is `unknown` or its probability is less than `model.threshold`, return `Defer` with a filler and `Job::Llm`.
  2. The area is the area slot if present, else the satellite area. An area slot that matches no area gives `Fallback`.
  3. A command needs an area when `target = "entities"`. No area gives `Fallback`.
  4. Render the service data. A value that parses as JSON is a JSON value, else a string. Return `Reply` with a pool response and the call.
- This sub-phase adds `[model]` with `threshold` to `jarvis.toml`.

**Acceptance criteria.**

- AC-4.5-1: When the prediction is `lights_on` 0.89 and the satellite is in the Kitchen, the call is `homeassistant.turn_on` with all kitchen lights.
- AC-4.5-2: When the top label is `unknown`, the result is `Defer` with a filler and `Job::Llm` with the transcript.
- AC-4.5-3: When the top probability is less than `model.threshold`, the result is `Defer` with `Job::Llm`.
- AC-4.5-4: When the prediction has an area slot, the slot area replaces the satellite area.
- AC-4.5-5: When the area slot matches no area, the result is `Fallback`.
- AC-4.5-6: When a `target = "entities"` intent has no slot area and no satellite area, the result is `Fallback`.
- AC-4.5-7: When the intent is `climate_down` and the target temperature is 72, the call data has `temperature = 70` as a number.
- AC-4.5-8: When a command has no `service`, the result is `Reply` with no call.

**Proofs.**

- P-4.5-2, P-4.5-3 (AC-4.5-2, AC-4.5-3). Ensures: `Reply` implies `top != unknown` and `p(top) >= threshold`. Argument: step 1 returns `Defer` in every other case, before any step that can return `Reply`.
- P-4.5-4 to P-4.5-6. Argument: step 2 computes `area = slot.map(area_by_name).unwrap_or(satellite)`. A slot with `area_by_name = None` returns `Fallback`. Step 3 returns `Fallback` when the area is `None` and the target needs one.
- P-4.5-7 (AC-4.5-7). Argument: "70" parses as a JSON number, so the data value is a number.
- P-4.5-8. Argument: `call = intent.service.map(...)`, which is `None` when there is no service.
- P-4.5-1. Evidence: test.

---

### 4.6 Decide Queries and Fetches

**Goal:** Add `decide` for query and fetch intents. Questions about the house are answered from the hot cache, and questions that need outside data get a filler and a background fetch.

- **Implements:** G3, G6. **Depends on:** 4.5.
- **Files:** modify `src/domain/decide.rs`.

**Model.** `query`: select the entity, render the pool response with `entity`, return `Reply` with no call. A missing entity gives `Fallback`. `fetch`: return `Defer` with a filler and `Job::Fetch(call, pool)`.

**Acceptance criteria.**

- AC-4.6-1: When the intent is `temperature_query` and the sensor state is 21, the text is "It is 21 degrees in the kitchen."
- AC-4.6-2: When the query entity is not in the cache, the result is `Fallback`.
- AC-4.6-3: When the intent is `weather`, the result is `Defer` with a filler and `Job::Fetch` with `return_response = true`.
- AC-4.6-4: When the intent is a query, the result has no service call.

**Proofs.**

- P-4.6-2. Argument: the query branch returns `Fallback` when `select` is empty.
- P-4.6-3. Argument: the fetch branch builds the call with `return_response = true`.
- P-4.6-4. Argument: the query branch constructs `Reply { call: None }`.
- P-4.6-1. Evidence: test.

---

### 4.7 Announce Quint Model

**Goal:** Add `quint/announce.qnt`, the model of the filler and the announces that follow it. It proves that no announce talks over the filler and that sentences play in order.

- **Implements:** G6, G19. **Depends on:** 4.6.
- **Files:** create `quint/announce.qnt`.
- **Quint:** `announce.qnt`.

**Model.**

- Constants: `MARGIN = 1`, at most 3 sentences.
- State: `now`, `fillerEnd`, `arrived: List[int]` (sentence IDs), `started: List[(int, int)]` (ID, start time), `busy: bool`.
- Actions: `tick`, `arrive(id)`, `start` (enabled when `not busy`, `now >= fillerEnd + MARGIN`, and an arrived sentence is not started; it starts the oldest such sentence), `finish`.
- Invariants:
  - `AnnounceAfterFiller`: every start time is `>= fillerEnd + MARGIN`.
  - `SentencesInOrder`: the IDs in `started` are a prefix of `arrived`.

**Acceptance criteria.**

- AC-4.7-1 [Quint]: `quint typecheck` passes.
- AC-4.7-2 [Quint]: `AnnounceAfterFiller` holds in simulation and to 10 steps in `quint verify`.
- AC-4.7-3 [Quint]: `SentencesInOrder` holds in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-4.7-2 (AC-4.7-2). Argument: `start` is the only action that appends to `started`, and its guard requires `now >= fillerEnd + MARGIN`. `now` never decreases, and no action changes `fillerEnd`.
- P-4.7-3 (AC-4.7-3). Argument by induction: `arrive` appends to `arrived` and keeps the prefix property. `start` appends the oldest arrived sentence that is not started, which is the next element of `arrived` after the prefix.
- P-4.7-1. Evidence: model check.

---

### 4.8 Dispatch Replies

**Goal:** Add the `Clock` port and the dispatch of `Reply` decisions. Jarvis returns the reply at once and runs the service call in the background, so a slow HA never delays the answer.

- **Implements:** G1, G3. **Depends on:** 4.7.
- **Files:** create `src/app/conversation.rs`. Modify `src/ports.rs`, `src/main.rs`, `src/testing/fakes.rs`.

**Model.** `Clock::now()`. `SystemClock` is in `src/main.rs`. `FakeClock` is in `fakes.rs`. `dispatch(decision)` for `Reply`: spawn `home.call(call)`, save the pools with the new use count through `Store`, return the text.

**Acceptance criteria.**

- AC-4.8-1: When dispatch gets a `Reply` with a call and `FakeHome` delays the call by 500 ms, dispatch returns before virtual time advances.
- AC-4.8-2: When dispatch gets a `Reply` with a call, `FakeHome` records the call.
- AC-4.8-3: When dispatch gets a `Reply`, the response store receives the pools with the new use count.
- AC-4.8-4: When dispatch gets a `Reply` with no call, `FakeHome` records no call.

**Proofs.**

- P-4.8-1. Requires: `spawn` does not wait. Argument: dispatch spawns the call and does not await it, so it returns without a timer.
- P-4.8-4. Argument: the spawn is inside `if let Some(call)`.
- P-4.8-2, P-4.8-3. Evidence: test.

---

### 4.9 Dispatch Fetches and Announce

**Goal:** Add `announce` to `Home` and the dispatch of fetch jobs. Jarvis speaks a filler, fetches the data in the background, and announces the answer after the filler ends.

- **Implements:** G6, G19. **Depends on:** 4.8.
- **Files:** create `src/domain/speech.rs`. Modify `src/ports.rs`, `src/adapters/ha.rs`, `src/app/conversation.rs`, `src/main.rs`, `src/testing/fakes.rs`, `jarvis.toml`.
- **Quint:** `announce.qnt` (conformance).

**Model.**

- `Home::announce(satellite, text)` calls `assist_satellite.announce` with `preannounce = false` and waits for it to end.
- `announce_delay(filler_audio_duration, margin) -> Duration = duration + margin`.
- `Defer(Fetch)`: return the filler, then in the background call the service, render the answer pool with `response`, wait until `announce_delay` has passed since the reply, and announce. On error, announce from `failure`.
- Announces of one request run one at a time, in order.
- This sub-phase adds `[announce]` with `margin_ms` to `jarvis.toml`.

**Acceptance criteria.**

- AC-4.9-1: When dispatch gets `Defer` with `Job::Fetch`, `FakeHome` records the call with `return_response`.
- AC-4.9-2: When the fetch result arrives, `FakeHome` records an announce with the rendered answer and `preannounce = false`.
- AC-4.9-3: When the filler audio is 1.5 s, `announce_delay` returns 1.5 s plus `margin_ms`.
- AC-4.9-4: When the fetch fails, `FakeHome` records an announce from `failure`.
- AC-4.9-5: When `HaHome::announce` runs, `FakeHaServer` receives `assist_satellite.announce` for the satellite entity.
- AC-4.9-6: When `quint-connect` replays 100 traces of `announce.qnt`, the announces that `FakeHome` records match the model `started` after each step.

**Proofs.**

- P-4.9-3 (AC-4.9-3). Argument: `announce_delay` is the sum.
- P-4.9-2 (AC-4.9-2). Requires: `AnnounceAfterFiller` (P-4.7-2). Argument: the job sleeps until `reply time + announce_delay` before it calls `announce`, which matches the `start` guard of the model.
- P-4.9-4. Argument: the error branch of the fetch selects from `failure`.
- P-4.9-1, P-4.9-5, P-4.9-6. Evidence: test.
