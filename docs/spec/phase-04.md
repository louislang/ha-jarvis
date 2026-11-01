# Phase 4: System One Decisions

This phase selects responses, selects entities and decides the action from a prediction. Then it does the side effects through the ports.

---

### 4.1 Response Selection

**Goal:** Add response pools with `select` and `record_use`. Jarvis now varies its replies: it never repeats the last response and prefers the least used one.

- **Implements:** G4. **Depends on:** 3.12c.
- **Files:** create `src/domain/responses.rs`, `responses.toml`. Modify `src/lib.rs`.

**Model.**

- `Pools: Map<pool name, Pool>`. `Pool { responses: Vec<Response { text, uses }>, last: Option<usize> }`.
- `select(pool, rng) -> usize`: if the pool has two or more responses, the allowed responses exclude `last`. The candidates are the allowed responses with the lowest `uses`. The result is a random candidate.
- `record_use(pools, name, index) -> Pools`: `uses += 1` for that response, and `last := index`.

**Acceptance criteria.**

- AC-4.1-1: When a pool has two or more responses, `select` never returns `last`.
- AC-4.1-2: When responses have different use counts, `select` returns a response with the lowest count among the allowed responses.
- AC-4.1-3: When `select` gets the same seeded RNG and the same pool, it returns the same index.
- AC-4.1-4: When `record_use` runs, the use count of that response increases by 1 and every other count does not change.
- AC-4.1-5: When a pool has one response, `select` returns it.

**Proofs.**

- P-4.1-1, P-4.1-2 (AC-4.1-1, AC-4.1-2). Requires: the pool is not empty. Ensures: the result is not `last` (when `len >= 2`), and its `uses` is the minimum over `responses \ {last}`. Argument: the candidate set is `argmin uses` over `responses \ {last}`. With `len >= 2`, that set is not empty. `select` chooses only from the candidate set.
- P-4.1-3. Ensures: the same pool and the same RNG state give the same index. Argument: `select` reads only the pool and the RNG.
- P-4.1-4. Ensures: `uses[index]` increases by 1, `last = index`, and every other count does not change. Argument: `record_use` does one indexed update.
- P-4.1-5. Ensures: a pool with one response gives index 0. Argument: with `len = 1`, the exclusion of `last` does not apply, so the only response is the only candidate.

---

### 4.2 Templates and Pool Validation

**Goal:** Add `render` with `minijinja` in strict mode and `validate_pools`. Responses and service data now share one template syntax, and a missing variable or a missing pool fails at startup.

- **Implements:** G4. **Depends on:** 4.1.
- **Files:** modify `src/domain/responses.rs`, `src/domain/catalog.rs`, `Cargo.toml`.

**Model.**

- `render(template, context) -> Result<String>`. Undefined variables are errors. The environment uses `UndefinedBehavior::Strict` and debug mode, because `minijinja` names the undefined variable only in debug mode. `Cargo.toml` turns on the `serde` feature of `minijinja`, so the context is a `serde_json` value.
- `validate_pools(pools, catalog) -> Result<()>`: each intent has a pool, the `filler`, `failure` and `fallback` pools exist, each pool has one or more responses, every template parses, and every template uses only the variables of its pool (`minijinja` `undeclared_variables`).
- Template variables of an intent pool: `area` (the area name in lowercase), the slots and the `habit_defaults` keys of its intent, `entity` when the intent has `target = "entity"`, `response` when it has `kind = "fetch"`, and `person` when it has `needs_speaker` (sub-phase 15.7). A pool that is not an intent pool uses no variable. Thus a template that validates also renders. The slots of an intent come from the same function that check 4 of `validate_catalog` uses (sub-phase 3.2b), so `catalog.rs` makes it public.

**Acceptance criteria.**

- AC-4.2-1: When `render` gets `area = kitchen`, "Powering up the {{ area }} lights." becomes "Powering up the kitchen lights."
- AC-4.2-2: When a template uses a variable that the context does not have, `render` returns an error that names the variable.
- AC-4.2-3: When an intent has no pool, `validate_pools` returns an error that contains the intent name.
- AC-4.2-4: When a template has a syntax error, `validate_pools` returns an error that contains the pool name.
- AC-4.2-5: When `filler`, `failure` or `fallback` is missing, `validate_pools` returns an error that names it.
- AC-4.2-6: When a template of an intent pool uses a variable that is not a built-in, a slot or a `habit_defaults` key of the intent, `validate_pools` returns an error with the pool and the variable.
- AC-4.2-7: When a pool has no responses, `validate_pools` returns an error that names the pool.
- AC-4.2-8: When a pool of a `target = "entities"` intent uses `entity`, a pool of an intent that is not `fetch` uses `response`, or a pool that is not an intent pool uses a variable, `validate_pools` returns an error with the pool and the variable.

**Proofs.**

- P-4.2-1. Evidence: test (`minijinja`).
- P-4.2-2. Evidence: test. The error and its text come from `minijinja` (`UndefinedBehavior::Strict` and debug mode), which Verus cannot see.
- P-4.2-3, P-4.2-5, P-4.2-7. Ensures: when `validate_pools` returns `Ok`, each intent has a pool, the `filler`, `failure` and `fallback` pools exist, and each pool has one or more responses. Argument: `validate_pools` checks the intents, then the required pools, then the size and the templates of each pool in loops, and returns the first failure.
- P-4.2-4, P-4.2-6, P-4.2-8. Evidence: test. The template checks depend on `minijinja` `template_from_str` and `undeclared_variables`, and on the slot names, which the `ensures` of `parse_example` does not state (P-3.2b).

---

### 4.3 File Store

**Goal:** Add the `Store<T>` port and `FileStore`, which saves TOML or JSON with an atomic write. Pools, habits and other state now survive a restart and a crash during a save.

- **Implements:** G10. **Depends on:** 4.2.
- **Files:** create `src/adapters/files.rs`. Modify `src/ports.rs`, `src/lib.rs`, `Cargo.toml`.

**Model.** `Store<T>`: `load() -> Result<Option<T>>`, `save(&T) -> Result<()>`. The format comes from the file extension (`.toml` or `.json`). `save` writes to a temporary file in the same folder, calls `fsync` on it, then renames it over the target. A file that does not parse is an error with the path, not `None`.

**Acceptance criteria.**

- AC-4.3-1: When `FileStore` saves to a `.toml` path and loads again, the data is equal.
- AC-4.3-2: When `FileStore` saves to a `.json` path and loads again, the data is equal.
- AC-4.3-3: When the save stops before the rename, the file still contains the last valid data.
- AC-4.3-4: When the file does not exist, `load` returns `None` and no error.
- AC-4.3-5: When the extension is not `.toml` or `.json`, the constructor returns an error with the path.
- AC-4.3-6: When the file does not parse, `load` returns an error that contains the path.

**Proofs.**

- P-4.3-3 (AC-4.3-3). Requires: rename in one folder is atomic on the file system. Ensures: the target is either the old content or the new content. Argument: the target changes only through the rename. A stop before the rename leaves the target unchanged.
- P-4.3-4, P-4.3-5. Argument: `load` maps "not found" to `Ok(None)`. The constructor matches the extension with two allowed values.
- P-4.3-1, P-4.3-2, P-4.3-6. Evidence: test.

---

### 4.4 Entity Filters

**Goal:** Add entity selection for an intent and an area, with domain and device class filters and the `exclude` list. "Turn on the lights" now finds the lights in the area.

- **Implements:** G9. **Depends on:** 4.3.
- **Files:** create `src/domain/select.rs`. Modify `src/domain/state_cache.rs`, `src/lib.rs`.

**Model.**

- A filter has `domain`, optional `name` and optional `device_class`. A filter matches an entity in its `domain` with its `device_class`, if it has one. An entity matches when one filter matches and the entity is not in `exclude`. Until sub-phase 4.4b, `select` does not apply the `name` of a filter.
- `select(intent, area, cache) -> Vec<EntityId>`:
  - `target = "entities"`: all matching entities in the area.
  - Until sub-phase 4.4c, `select` does not read `target` and gives this result for each intent.
- `src/domain/state_cache.rs` makes its functions `in_domain`, `device_class` and `entity_area` public. `select.rs` uses them and does not repeat them.

**Acceptance criteria.**

- AC-4.4-1: When the intent is `lights_on` and the area is the Kitchen, the result is all kitchen lights.
- AC-4.4-7: When an entity is in `exclude`, it is never in the result.

**Proofs.**

- P-4.4-7 (AC-4.4-7). Ensures: every result entity matches a filter and is not excluded. Argument: the result draws only from `matching = filter(cache, filters) \ exclude`.
- P-4.4-1 (AC-4.4-1). Ensures: for `target = "entities"` and a `matching ∩ area` that is not empty, the result is `matching ∩ area`. Argument: `select` returns that set without a further filter.

---

### 4.4b Name Words and Skipped Entities

**Goal:** Match the `name` of a filter to whole words of the entity name or an alias, and skip entities that are not for voice control. "Turn on the lights" now finds switches named as lights and does not switch on the LED of a satellite.

- **Implements:** G9. **Depends on:** 4.4.
- **Files:** modify `src/domain/select.rs`.

**Model.**

- A filter `name` matches an entity when each word of `normalized(name)` is a word of `normalized` of the entity name, or each is a word of `normalized` of one alias (sub-phase 3.1). The words of a text are its parts between spaces. The words can be in any order. Thus the match is on whole words, without case sensitivity. "Kitchen-Light" has the words "kitchen" and "light". `fan` matches "Office Fan", not "Infant Monitor".
- `select` skips an entity that has an entity category, that is hidden or disabled, or that is on a satellite device (a device with an `assist_satellite` entity). Thus "turn on the lights" does not switch on the LED of a satellite, and a query does not read the warm sensor of a satellite.
- `matching` is now `filter(cache, filters) \ (exclude ∪ skipped)`, where `filter` applies the `name` of each filter.

**Acceptance criteria.**

- AC-4.4b-1: When the intent is `lights_on` in the Garage, the result includes the switch "Garage Light".
- AC-4.4b-2: When the intent is `lights_on` in the Living Room, the result does not include the switch "Living Room Fan".
- AC-4.4b-3: When a filter has `name = "tv"`, an entity with the alias "the TV" matches.
- AC-4.4b-4: When a filter has `name = "fan"`, an entity named "Infant Monitor" does not match.
- AC-4.4b-5: When a Kitchen light has an entity category, is hidden, is disabled or is on a satellite device, `select` for `lights_on` in the Kitchen does not return it.

**Proofs.**

- P-4.4b-2, P-4.4b-5 (AC-4.4b-2, AC-4.4b-5). Ensures: every result entity matches a filter with its `name`, is not excluded and is not skipped. Argument: the result draws only from `matching = filter(cache, filters) \ (exclude ∪ skipped)`. "Living Room Fan" does not have the word "light", so the filter `{ domain = "switch", name = "light" }` does not match it.
- P-4.4b-1, P-4.4b-3, P-4.4b-4 (AC-4.4b-1, AC-4.4b-3, AC-4.4b-4). Ensures: a filter with a `name` matches an entity if and only if the entity has the domain and the device class of the filter, and each word of `normalized(name)` is a word of `normalized` of the entity name, or each is a word of `normalized` of one alias. Argument: `is_named` compares these words, and `normalize` gives `normalized` (P-3.1-2). The second filter of `lights_on` is `{ domain = "switch", name = "light" }`, and "Garage Light" has the whole word "light". `normalized("the TV")` is "the tv", which has the word "tv". The words of `normalized("Infant Monitor")` are "infant" and "monitor", and neither is "fan".
- P-4.4-1 holds with this `matching`. For `target = "entities"` and a `matching ∩ area` that is not empty, the result is all of `matching ∩ area`, also the entities that match only a filter with a `name`. Argument: `select` returns each entity of `matching ∩ area`.
- The fix PR `Fix: Full normalize Spec` writes these Verus clauses and the clause of P-3.1-2 for the code on `main`. It modifies `src/domain/intent.rs`, `src/domain/contracts.rs` and `src/domain/select.rs`. It does not change behavior.

---

### 4.4c Single Entity and House Fallback

**Goal:** Select one entity for `target = "entity"`, and use the only match in the house when the area has no match. "Turn the air down" now finds the one thermostat of the house from any area.

- **Implements:** G9. **Depends on:** 4.4b.
- **Files:** modify `src/domain/select.rs`.

**Model.**

- `target = "entity"`: the `entity` of the intent, else the matching entity in the area with the lowest entity ID (sub-phase 11.6 uses the highest habit score).
- `entity` is a template, for example `{{ person.calendar }}`. `select` returns the `entity` text unrendered. `decide` (sub-phase 4.5d) renders it with its context before it uses the result.
- If the area has no match and the house has exactly one match, the result is that entity.

**Acceptance criteria.**

- AC-4.4c-1: When the intent is `fan_on` in the Living Room, the result is the switch "Living Room Fan".
- AC-4.4c-2: When the intent is `climate_down` in the Bedroom and the only climate entity is in the Kitchen, the result is that entity.
- AC-4.4c-3: When the area has no match and the house has two matches, the result is empty.
- AC-4.4c-4: When two entities in the area match a `target = "entity"` intent, the result is the one with the lowest entity ID.

**Proofs.**

- P-4.4c-2, P-4.4c-3 (AC-4.4c-2, AC-4.4c-3). Ensures: the fallback applies if and only if `matching ∩ area = ∅` and `|matching| = 1`. Argument: these two conditions guard the fallback branch. The fallback applies only when `matching ∩ area` is empty, so P-4.4-1 still holds. The fallback and the `entity` branch draw only from `matching`, so P-4.4-7 and P-4.4b-2 still hold for an intent without `entity`.
- P-4.4c-1 (AC-4.4c-1). Ensures: `fan_on` in the Living Room gives "Living Room Fan". Argument: the second filter of `fan_on` is `{ domain = "switch", name = "fan" }`. "Living Room Fan" is the only match in the area, and the `entity` branch returns the only match.
- P-4.4c-4 (AC-4.4c-4). Ensures: for an intent without `entity` and two or more matches in the area, `target = "entity"` gives the lowest entity ID. Argument: the `entity` branch takes the minimum of `matching ∩ area` by entity ID.


---

### 4.5 Decide Commands

**Goal:** Add the `Decision` types and the System Two gate of `decide`. An `unknown`, uncertain or removed label now becomes a deferred System Two job, and a confident command becomes a reply.

- **Implements:** G3, G7. **Depends on:** 4.4c.
- **Files:** create `src/domain/decide.rs`. Modify `src/lib.rs`, `jarvis.toml`.

**Model.**

- `Decision`: `Reply { reply: Chosen, call: Option<ServiceCall> }`, `Defer { filler: Chosen, job: Job }`, `Fallback { reply: Chosen }`. `Chosen { text, pool, index }` is the selected response (sub-phase 4.1), so dispatch can record its use and the log can store its pool and template. `Fallback` means that Jarvis knows the intent but cannot act: the reply is from the pool `fallback`, and dispatch makes no call.
- `Job::Llm(question)`: the question is the transcript. Sub-phase 7.2 makes it a `Question`. Sub-phase 4.6 adds `Job::Fetch`, because 4.6 creates it.
- `Request { transcript, area: Option<AreaId> }` is the request that `decide` gets: the transcript and the satellite area. Its name is not `Context`, because `state::Context` exists.
- `decide(prediction, request, catalog, pools, threshold, rng) -> Decision`. `threshold` is `model.threshold`. `decide` requires that each intent of the catalog has a pool, and that the pools `filler` and `fallback` exist with one or more responses (P-4.2-3, P-4.2-5, P-4.2-7).
  1. If the top label is `unknown`, is not in the catalog, or its probability is less than `threshold`, return `Defer` with a filler and `Job::Llm`. A label of the model that the catalog no longer has (an intent removed before a retrain) thus goes to System Two.
  2. An intent that is not `kind = "command"` also gives `Defer` with a filler and `Job::Llm`. Sub-phase 4.6 handles `query` and `fetch`, and sub-phase 13.11 handles `train`.
  3. A command gives `Reply` with a response from its pool and no call. Sub-phases 4.5b to 4.5d replace this step.
- The pools `filler` and `fallback` use no variables (sub-phase 4.2), so their `Chosen.text` is the template as written. Until sub-phase 4.5b, the reply text of a command is also the template as written.
- `Decision` holds a `ServiceCall`. Thus `src/domain/contracts.rs` gets a `#[verifier::external_type_specification]` for `ServiceCall`, the same as for the other types of `src/domain/state.rs` (`02-rules.md` section 5.1, rule 8).
- This sub-phase adds `[model]` with `threshold` to `jarvis.toml` and to `Config` in `src/lib.rs`.
- `Config::validate` (sub-phase 2.2) rejects a `model.threshold` outside `[0, 1]`. The error names the field and the range. Sub-phase 4.5 did not add this check, so the fix PR `Fix: Threshold Range` adds it to the code on `main`.

**Acceptance criteria.**

- AC-4.5-2: When the top label is `unknown`, the result is `Defer` with a filler and `Job::Llm` with the transcript.
- AC-4.5-3: When the top probability is less than `model.threshold`, the result is `Defer` with `Job::Llm`.
- AC-4.5-12: When the top label is not in the catalog, the result is `Defer` with `Job::Llm`.
- AC-4.5-13: When `model.threshold` is outside `[0, 1]`, `parse` returns an error that contains `model.threshold` and the range.

**Proofs.**

- P-4.5-2, P-4.5-3 (AC-4.5-2, AC-4.5-3). Requires: the pools of the model (P-4.2-3, P-4.2-5, P-4.2-7). Ensures: `Reply` implies `top != unknown` and `p(top) >= threshold`. Argument: step 1 returns `Defer` in every other case, before any step that can return `Reply`.
- P-4.5-12 (AC-4.5-12). Ensures: a top label that is not in the catalog gives `Defer` with `Job::Llm`. Argument: step 1 checks catalog membership of the top label before any step that can return `Reply`.
- P-4.5-13 (AC-4.5-13). Ensures: a `model.threshold` outside `[0, 1]` gives `Err` with the field name and the range. Argument: `parse` returns the first `Err` of `validate`, which compares `model.threshold` with `[0, 1]` (P-2.2-6).

---

### 4.5b Area Rules

**Goal:** Add the area rules to `decide`. A command now gets its area from the area slot or the satellite, and its reply renders the area name.

- **Implements:** G3, G9. **Depends on:** 4.5.
- **Files:** modify `src/domain/decide.rs`.

**Model.**

- `decide` gets the parameter `cache` after `catalog`. Step 3 of sub-phase 4.5 becomes steps 3 to 5 for a command:
  3. The area is the area slot if present, else `request.area`. `area_by_name` gives the area of the slot (sub-phase 2.8b). An area slot that matches no area gives `Fallback`.
  4. A command without `service` gives `Reply` with the rendered reply and no call. It does not select entities.
  5. A command with a `service` needs an area for both targets, because `select` needs an area (sub-phase 4.5c). No area gives `Fallback`.
- Until sub-phase 4.5c, a command with a `service` and an area gives `Reply` with the rendered reply and no call.
- The render context of the reply has `area` (the area name in lowercase) when the area is known. An area ID that is not in the area registry gives no `area` in the context. Thus a template with `area` does not render for it. When the reply does not render, the result is `Fallback`. `Chosen.text` of the reply is the rendered text.
- The render context is a JSON object. Thus `src/domain/contracts.rs` gets contracts for the `serde_json` functions `Map::new`, `Map::insert` and `Value::from` (`02-rules.md` section 5.1, rule 8).
- Verus treats the `f32` comparison `>=` as an uninterpreted relation. Thus a clause cannot state that a confident top command gets to step 3. The clauses of `decide` state the reverse: each `Reply` and each `Fallback` comes from the top command and obeys steps 3 to 5. Sub-phase 4.5c adds steps 6 to 8 to these clauses.

**Acceptance criteria.**

- AC-4.5b-3: When the area slot matches no area, the result is `Fallback`.
- AC-4.5b-4: When a `target = "entities"` intent has no slot area and no satellite area, the result is `Fallback`.
- AC-4.5b-5: When a command has no `service`, the result is `Reply` with no call.

**Proofs.**

- P-4.5b-3, P-4.5b-4 (AC-4.5b-3, AC-4.5b-4). Ensures: each `Reply` and each `Fallback` comes from the top command. For this command, an area slot that matches no area gives `Fallback`. No area slot and no `request.area` for a command with a `service` also gives `Fallback`. Thus each of these requests gives `Fallback` or `Defer`. Argument: step 3 takes `area_by_name(slot)` for the first area slot of the prediction, else `request.area`. A slot with `area_by_name = None` returns `Fallback`. Step 5 returns `Fallback` when the area is `None`. Steps 1 and 2 return `Defer` before step 3.
- P-4.5b-5 (AC-4.5b-5). Ensures: an intent without `service` gives `Reply` with `call = None`, `Fallback` or `Defer`, and never a call. Argument: step 4 returns `Reply { call: None }`, or `Fallback` when the reply does not render, before any step that builds a call. Step 3 is the only earlier step of a command that can return `Fallback`.

---

### 4.5c Service Call

**Goal:** Add the entity selection and the service call to `decide`. "Turn on the lights" now becomes a reply and one service call for the lights in the area of the satellite.

- **Implements:** G3, G9. **Depends on:** 4.5b.
- **Files:** modify `src/domain/decide.rs`, `src/lib.rs`.

**Model.**

- A command with a `service` and an area continues after step 5 of sub-phase 4.5b with steps 6 to 8. They replace the `Reply` with no call of sub-phase 4.5b:
  6. The `service` of the intent is `domain.service`. `str::split_once` splits it at the first dot. The text before the dot is `ServiceCall.domain`, and the text after it is `ServiceCall.service`. A `service` without a dot gives `Fallback`.
  7. `select(intent, area, cache)` gives the selection. An empty selection (sub-phases 4.4 to 4.4c) gives `Fallback`. Until sub-phase 4.5d, `decide` does not render the `entity` text of an intent.
  8. Render the reply from the pool of the intent. Return `Reply` with the reply and `ServiceCall { domain, service, target: selection, data: {}, return_response: false }`. Sub-phase 4.5d adds the data.
- With a selection and `target = "entity"`, the render context also has `entity`: the state, the attributes and the name of the selected entity. An entity without an entry in the entity registry has no `entity.name`. When the cache has no state for the selected entity, the result is `Fallback`.
- `src/domain/contracts.rs` gets contracts for `Map::clone`, `Value::from` of a `Map` and `str::split_once` (`02-rules.md` section 5.1, rule 8). The contract of `split_once` needs the unstable `pattern` feature. Thus `src/lib.rs` turns on `pattern` for Verus only (`02-rules.md` section 5.1, rule 10).
- The clauses of `decide` have the form of sub-phase 4.5b: each `Reply` and each `Fallback` comes from the top command and obeys steps 3 to 8.

**Acceptance criteria.**

- AC-4.5c-1: When the prediction is `lights_on` 0.89 and the satellite is in the Kitchen, the call is `homeassistant.turn_on` with all kitchen lights.
- AC-4.5c-2: When the prediction has an area slot, the slot area replaces the satellite area.
- AC-4.5c-3: When the selection of a command is empty, the result is `Fallback`.

**Proofs.**

- P-4.5c-2 (AC-4.5c-2). Ensures: the target of a call is the selection in the area of the first area slot when the prediction has an area slot, else in `request.area`. Argument: step 3 takes `area_by_name(slot)` for the first area slot, else `request.area` (P-4.5b-3). Step 7 selects in this area.
- P-4.5c-3 (AC-4.5c-3). Ensures: a `Reply` with a call has a target that is not empty. Thus an empty selection gives `Fallback` or `Defer`. Argument: step 7 returns `Fallback` when `select` is empty, before step 8 builds the call.
- P-4.5c-1 (AC-4.5c-1). Ensures: a `Reply` with a call has the domain and the service of the intent: its `domain`, a dot and its `service` are the `service` of the intent, and its `domain` has no dot. Its target is the `select` result. Argument: step 6 splits the `service` of the intent at the first dot (the contract of `split_once`). Step 7 takes the selection, and step 8 builds the call from both. For `lights_on` in the Kitchen, the selection is all kitchen lights (P-4.4-1).

---

### 4.5d Slots and Service Data

**Goal:** Add the slots, the `entity` template and the service data to `decide`. "Turn the air down" now sends the new temperature as a number, and "Put the TV on 24" sends the show name.

- **Implements:** G3. **Depends on:** 4.5c.
- **Files:** modify `src/domain/decide.rs`.

**Model.**

- Step 3 also reads the slots of the intent (`catalog::insert_slots`, sub-phase 4.2). The `area` slot is not part of this check, because step 3 already gives the area or `Fallback`. Another missing slot that is not `optional` gives `Fallback`. A missing `optional` slot is an empty string. The render context of steps 4, 7 and 8 gets each other slot. Its `area` stays the area name.
- Step 7: when the intent has `entity`, `decide` renders the text that `select` returns with the context (sub-phase 4.4c). The rendered text is the selected entity. An `entity` template that does not render gives `Fallback`.
- Step 8 also renders the `data` of the intent. A data value that is one `{{ expression }}` keeps the type of the result (`minijinja` `compile_expression`). Other values render as strings. A data value that does not render gives `Fallback`. The call gets the rendered data.

**Acceptance criteria.**

- AC-4.5d-1: When the intent is `climate_down` and the target temperature is 72, the call data has `temperature = 70` as a number.
- AC-4.5d-2: When the `show` slot is "24", the rendered `media_content_id` is a string that contains `"show_name": "24"`.
- AC-4.5d-3: When an `optional` slot is missing, the template renders it as an empty string. When a slot that is not `optional` and not `area` is missing, the result is `Fallback`.

**Proofs.**

- P-4.5d-1 (AC-4.5d-1). Evidence: test. A data value that is one `{{ expression }}` keeps the type of its result through `minijinja` `compile_expression`, which Verus cannot see. `{{ entity.attributes.temperature - 2 }}` is one expression whose result is a number.
- P-4.5d-2 (AC-4.5d-2). Ensures: a data value that is not one `{{ expression }}` becomes a JSON string. Argument: step 8 renders each such value with `render`, which returns a `String`. The `media_content_id` value of `tv_show` has text around `{{ show }}`, so it is not one expression.
- P-4.5d-3 (AC-4.5d-3). Evidence: test. The slot names come from `catalog::insert_slots`, which has no `ensures`, and from `parse_example`, whose `ensures` does not state the slot names (P-3.2b). The empty string reaches the text through `minijinja`.
- P-4.5c-1 now holds for an intent without `entity`. For an intent with `entity`, the target is the rendered `entity` text, which Verus cannot see.

---

### 4.6 Decide Queries and Fetches

**Goal:** Add `decide` for query and fetch intents. Questions about the house are answered from the hot cache, and questions that need outside data get a filler and a background fetch.

- **Implements:** G3, G6. **Depends on:** 4.5d.
- **Files:** modify `src/domain/decide.rs`.

**Model.** `query`: select the entity (`target = "entity"`), render the pool response with `entity`, return `Reply` with no call. A missing entity gives `Fallback`. `fetch`: return `Defer` with a filler and `Job::Fetch(call, pool)`. This sub-phase adds `Job::Fetch` to `Job`.

**Acceptance criteria.**

- AC-4.6-1: When the intent is `temperature_query` and the sensor state is 21, the text is "It is 21 degrees in the kitchen."
- AC-4.6-2: When the query entity is not in the cache, the result is `Fallback`.
- AC-4.6-3: When the intent is `weather`, the result is `Defer` with a filler and `Job::Fetch` with `return_response = true`.
- AC-4.6-4: When the intent is a query, the result has no service call.
- AC-4.6-5: When the area is the Office, which has two temperature sensors, the query reads the sensor with the lowest entity ID among the sensors that are not on a satellite device and not in `exclude`.

**Proofs.**

- P-4.6-2. Ensures: an empty selection gives `Fallback`. Argument: the query branch returns `Fallback` when `select` is empty.
- P-4.6-3. Ensures: a fetch intent gives `Defer` with `Job::Fetch` and `return_response = true`. Argument: the fetch branch builds the call with `return_response = true`.
- P-4.6-4. Ensures: a query gives `Reply` with `call = None`. Argument: the query branch constructs `Reply { call: None }`.
- P-4.6-1 (AC-4.6-1). Ensures: the text is the pool template rendered with `entity` and `area`. Argument: the query branch renders "It is {{ entity.state }} degrees in the {{ area }}." with `entity.state = 21` and `area = kitchen`.
- P-4.6-5 (AC-4.6-5). Ensures: the query reads the lowest entity ID of the sensors that are not skipped and not excluded. Argument: the query uses `select` with `target = "entity"` (P-4.4c-4, P-4.4b-5).

---

### 4.7 Announce Quint Model

**Goal:** Add `quint/announce.qnt`, the model of the announce queue of one satellite with two requests. It proves that an announce starts only when the satellite is idle, that a newer request stops the announces of an older one, and that sentences play in order.

- **Implements:** G6, G19. **Depends on:** 4.6.
- **Files:** create `quint/announce.qnt`.
- **Quint:** `announce.qnt`.

**Model.**

- Constants: `REQUESTS = Set(1, 2)`, at most 2 sentences for each request.
- State: `idle: bool` (the satellite entity is `idle` in the hot cache), `newest: int` (the newest request on the satellite, 0 before any), `arrived: List[(int, int)]` and `queue: List[(int, int)]` (request, sentence), `started: List[(int, int, int)]` (request, sentence, `newest` at the start), `busy: bool`.
- Actions:
  - `request(r)` (when `r > newest`): a user starts request `r`. `newest := r`, `idle := false`, `busy := false` (a new pipeline stops a playing announce).
  - `pipelineEnd`: `idle := true`. HA ended the reply or the filler.
  - `arrive(r, s)`: append `(r, s)` to `arrived` and to `queue`.
  - `start` (when `idle`, `not busy` and `queue` is not empty): remove the head. If its request is `newest`, append it with `newest` to `started` and set `busy`. Else drop it.
  - `finish`: `busy := false`.
- Invariants:
  - `AnnounceWhenIdle`: `busy` implies `idle`.
  - `NoStaleAnnounce`: each entry of `started` has the request that was `newest` when it started.
  - `SentencesInOrder`: for each request, its sentences in `started` are a prefix of its sentences in `arrived`.

**Acceptance criteria.**

- AC-4.7-1 [Quint]: `quint typecheck` passes.
- AC-4.7-2 [Quint]: `AnnounceWhenIdle` holds in simulation and to 10 steps in `quint verify`.
- AC-4.7-3 [Quint]: `SentencesInOrder` holds in simulation and to 10 steps in `quint verify`.
- AC-4.7-4 [Quint]: `NoStaleAnnounce` holds in simulation and to 10 steps in `quint verify`.
- AC-4.7-5 [Quint]: The test `ac_4_7_5_new_request_drops_old_answer` passes: a sentence of request 1 that arrives after request 2 starts never plays.

**Proofs.**

- P-4.7-2 (AC-4.7-2). Argument by induction: `start` sets `busy` only when `idle`. The only action that sets `idle := false` is `request`, which also sets `busy := false`. So no step makes `busy` true while `idle` is false.
- P-4.7-3 (AC-4.7-3). Argument by induction: `arrive` appends to the tail, and `start` takes the head, so the sentences of each request leave the queue in arrival order. A dropped sentence belongs to a request that is no longer `newest`. `newest` only grows, so every later sentence of that request is dropped too. Thus the started sentences of a request are a prefix of its arrived sentences.
- P-4.7-4 (AC-4.7-4). Argument: `start` appends an entry only when its request equals `newest`, and it records that value.
- P-4.7-1, P-4.7-5. Evidence: model check.

---

### 4.8 Dispatch Replies

**Goal:** Add the `Clock` port and the dispatch of `Reply` decisions. Jarvis returns the reply at once and runs the service call in the background, so a slow HA never delays the answer.

- **Implements:** G1, G3. **Depends on:** 4.7.
- **Files:** create `src/app/conversation.rs`. Modify `src/ports.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `jarvis.toml`.

**Model.**

- `Clock::now()`. `SystemClock` is in `src/lib.rs`. `FakeClock` is in `fakes.rs`.
- `dispatch(decision) -> Handled { text, after: Option<Work> }`. `after` is the background work. The caller sends the text first and starts `after` only after the send. Thus no call can come before the reply (`ReplyBeforeCall`, sub-phase 16.1). For the `Chosen` of the decision, dispatch sends `record_use(pool, index)` to the pools task.
- `Reply`: `after` is `home.call(call)`. `Fallback`: the text is from `fallback`, and `after` is none.
- A pools task owns `Pools`. `record_use` and the later backfill merges and marks are messages to it. It applies each change in order, publishes the result through `ArcSwap` and saves it to `pools.json` through `Store`. Thus no change overwrites another.
- `responses.toml` is the source and stays unchanged in Git. At startup, the pools task merges it into the saved pools: a response in both keeps its use count; a new source response starts at 0; a source response that is no longer in `responses.toml` leaves its pool; a generated response stays. This sub-phase adds `responses` and `pools`.

**Acceptance criteria.**

- AC-4.8-1: When dispatch gets a `Reply` with a call and `FakeHome` delays the call by 500 ms, dispatch returns before virtual time advances.
- AC-4.8-2: When dispatch gets a `Reply` with a call and the caller starts `after`, `FakeHome` records the call. Before the start, `FakeHome` records no call.
- AC-4.8-3: When dispatch gets a `Reply`, the response store receives the pools with the new use count.
- AC-4.8-4: When dispatch gets a `Reply` with no call, `FakeHome` records no call.
- AC-4.8-5: When dispatch gets `Fallback`, the text is from `fallback`, and `after` is none.
- AC-4.8-6: When 100 dispatches record uses at the same time, the saved pools have all 100 uses.
- AC-4.8-7: When `responses.toml` gets one new response and loses one old response, the merged pools have the new response with `uses = 0`, do not have the old one, and keep the use counts of the others. `responses.toml` does not change.

**Proofs.**

- P-4.8-1. Argument: dispatch returns the call as `after` and does not await it, so it returns without a timer.
- P-4.8-4, P-4.8-5. Argument: `after` is `Some` only inside `if let Some(call)`.
- P-4.8-6. Argument: the pools task applies messages one at a time, so each use is applied to the result of the one before.
- P-4.8-2, P-4.8-3, P-4.8-7. Evidence: test.

---

### 4.9 Dispatch Fetches and Announce

**Goal:** Add `announce` to `Home`, the announce queue of each satellite and the dispatch of fetch jobs. Jarvis speaks a filler, fetches the data in the background, and announces the answer when the satellite is idle again.

- **Implements:** G6, G19. **Depends on:** 4.8.
- **Files:** modify `src/ports.rs`, `src/adapters/ha.rs`, `src/app/conversation.rs`, `src/app/background.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `jarvis.toml`.
- **Quint:** `announce.qnt` (conformance).

**Model.**

- `Home::announce(satellite, text)` calls `assist_satellite.announce` with `preannounce = false`. HA returns the result after the satellite plays the audio, so the call has the deadline `ha.announce_timeout_s`, not `ha.request_timeout_ms`.
- The sync use case publishes each new cache through a `tokio::sync::watch` channel.
- Each satellite has a generation number and one announce queue. `handle` with a `satellite_id` increases the generation of that satellite. A deferred job keeps the generation of its request. Before each announce or call, the job compares the generation. When the generation changed, the job stops and makes no announce and no further call.
- An item of the queue waits until the watch channel shows the satellite entity `idle`. Then it waits `announce.margin_ms` and announces. When the satellite is not idle within `announce.idle_wait_s`, the item is dropped and logged. The items of one satellite run one at a time, in order.
- `Defer(Fetch)`: return the filler. `after` calls the service, renders the answer pool with `response`, and puts the answer in the queue. On error, it puts an answer from `failure` in the queue.
- When the call of a `Reply` fails, `after` logs the error and puts an answer from `failure` in the queue. Jarvis does not retry a failed call.
- This sub-phase adds `ha.announce_timeout_s` and `[announce]` with `margin_ms` and `idle_wait_s`.

**Acceptance criteria.**

- AC-4.9-1: When dispatch gets `Defer` with `Job::Fetch`, `FakeHome` records the call with `return_response`.
- AC-4.9-2: When the fetch result arrives and the satellite becomes idle, `FakeHome` records an announce with the rendered answer and `preannounce = false`.
- AC-4.9-3: When the satellite becomes idle 1.5 s after the reply, the announce starts at 1.5 s plus `margin_ms`, and not before.
- AC-4.9-4: When the fetch fails, `FakeHome` records an announce from `failure`.
- AC-4.9-5: When `HaHome::announce` runs, `FakeHaServer` receives `assist_satellite.announce` for the satellite entity.
- AC-4.9-6: When `quint-connect` replays 100 traces of `announce.qnt`, the announces that `FakeHome` records match the model `started` after each step.
- AC-4.9-7: When `FakeHome` returns an error for the call of a `Reply`, `FakeHome` records an announce from `failure` and no second call.
- AC-4.9-8: When `FakeHaServer` returns the announce result 10 s after the request, `HaHome::announce` returns `Ok`.
- AC-4.9-9: When a second request starts on the satellite before the answer of the first is announced, `FakeHome` records no announce of the first.
- AC-4.9-10: When the satellite is not idle within `idle_wait_s`, `FakeHome` records no announce, and the job ends.

**Proofs.**

- P-4.9-2, P-4.9-3 (AC-4.9-2, AC-4.9-3). Requires: `AnnounceWhenIdle` (P-4.7-2). Argument: the item waits for `idle` on the watch channel and then for `margin_ms` before it calls `announce`, which matches the `start` guard of the model.
- P-4.9-4. Argument: the error branch of the fetch selects from `failure`.
- P-4.9-9. Requires: `NoStaleAnnounce` (P-4.7-4). Argument: the job compares its generation before the announce, and the second `handle` increased it.
- P-4.9-10. Argument: the wait for `idle` has the deadline `idle_wait_s`, and its timeout arm drops the item.
- P-4.9-1, P-4.9-5 to P-4.9-8. Evidence: test.
