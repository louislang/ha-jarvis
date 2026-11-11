# Phase 11: Request Habits

This phase learns the request habits of the house: which spoken request people make at which time of the week. A request habit is not a device habit. Native HA automations switch devices at set times, so Jarvis does not learn or act on HA state changes. Phase 14 uses the request habits to prepare an answer before its usual time, so a slow System Two request gets System One speed (decision 9).

---

### 11.1 Source and Action

**Goal:** Add the pure functions that classify a state change: who caused it and what action it was. Habits can then ignore automations and count only real use.

This sub-phase is merged. Request habits do not use HA state changes, so sub-phase 11.2 removes its code.

- **Implements:** G11. **Depends on:** 10.7.
- **Files:** create `src/domain/habits.rs`. Modify `src/lib.rs`.

**Model.**

- `source(context, jarvis_user_id) -> Source`. `jarvis_user_id` is `Snapshot.user_id` (sub-phase 2.3).
- `action(domain, old, new) -> Option<(action, value)>`: `on`/`off` from the state, `play` with `media_series_title` when present (the show), else `media_title`, `set_temperature` with the new `temperature`. Other attribute changes give `None`.

**Acceptance criteria.**

- AC-11.1-1: When the context has the user ID of a person, `source` returns `manual`.
- AC-11.1-2: When the context has a `parent_id`, `source` returns `automation`.
- AC-11.1-3: When the context has no `user_id` and no `parent_id`, `source` returns `manual`.
- AC-11.1-4: When the context has the Jarvis user ID, `source` returns `voice`.
- AC-11.1-5: When a light changes from `off` to `on`, `action` returns `on` with no value.
- AC-11.1-6: When a media player changes to `playing` with `media_title = Bluey`, `action` returns `play` with `Bluey`. With `media_series_title = Bluey` and `media_title = Sleepytime`, it returns `play` with `Bluey`.
- AC-11.1-7: When the climate `temperature` changes from 72 to 70, `action` returns `set_temperature` with `70`.
- AC-11.1-8: When only an untracked attribute changes, `action` returns none.

**Proofs.**

- P-11.1-1 to P-11.1-4. Ensures: `source` is a total function with this precedence: Jarvis user, then `parent_id`, then other `user_id`, then none. Argument: `source` is one ordered `match` on `(user_id, parent_id)` with these four arms.
- P-11.1-5 to P-11.1-8. Ensures: `action` gives `on` or `off` for a change of state, `play` with the show or the title, `set_temperature` with the new temperature, and `None` for a change in no tracked field. Argument: `action` matches on the domain and compares the tracked fields of `old` and `new`. A change in no tracked field reaches the `None` arm.

---

### 11.1b Bucket

**Goal:** Add the pure function that gives the time bucket of a state change. Habits can then count use by day type and hour. Request habits use the same bucket for the time of a request.

- **Implements:** G11. **Depends on:** 11.1.
- **Files:** modify `src/domain/habits.rs`.

**Model.**

- `bucket(time, zone) -> (DayType, hour)`. `DayType` is `weekdays` or `weekends`. `zone` is `Snapshot.config.time_zone`.

**Acceptance criteria.**

- AC-11.1b-1: When the time is Saturday 20:15 in the HA time zone, `bucket` returns `(weekends, 20)`.

**Proofs.**

- P-11.1b-1. Evidence: test (time zone data).

---

### 11.2 Remove Device Habits

**Goal:** Remove the merged code that only device habits need: `source`, `action` and the `habit_defaults` field of an intent. Request habits keep only `bucket`.

- **Implements:** G11. **Depends on:** 11.1b.
- **Files:** modify `src/domain/habits.rs`, `src/domain/catalog.rs`, `src/domain/responses.rs`.

**Model.**

- `src/domain/habits.rs` loses `Source`, `Action`, `source`, `action`, their helpers, their spec functions and the tests of AC-11.1-1 to AC-11.1-8. `DayType`, `Bucket`, `bucket`, `day_of` and the test of AC-11.1b-1 stay.
- `src/domain/contracts.rs` loses each contract and axiom that only the removed code uses.
- `Intent` loses `habit_defaults`. `Intent` has `deny_unknown_fields`, so an intent with `habit_defaults` no longer loads. A template variable must now be a built-in or a tagged slot (`check_slots`, sub-phase 3.2b). An intent pool variable must be a built-in or a slot (`variables`, sub-phase 4.2). No intent of `intents.toml` has `habit_defaults`.

**Acceptance criteria.**

- AC-11.2-1: When an intent of the catalog text has `habit_defaults`, the catalog does not load, and the error names `habit_defaults`.
- AC-11.2-2: When a template of an intent uses a variable that is not a built-in or a tagged slot, `validate_catalog` returns an error that contains the variable. This replaces AC-3.2b-1.
- AC-11.2-3: When a template of an intent pool uses a variable that is not a built-in or a slot of the intent, `validate_pools` returns an error with the pool and the variable. This replaces AC-4.2-6.

**Proofs.**

- P-11.2-1. Evidence: test (the `serde` derive).
- P-11.2-2. Evidence: test. Argument: `check_slots` inserts only the tagged slots and the built-ins into its set of allowed names before it checks the templates. Verus cannot state the claim, because the set is local to `check_slots`, and the `ensures` of `parse_example` does not state the slot names (P-3.2b).
- P-11.2-3. Evidence: test. Argument: `variables` inserts only the slots of the intent and the built-ins of its kind and target. Verus cannot state the claim, because the `ensures` of `parse_example` does not state the slot names (P-3.2b, P-4.2-6).

---

### 11.3 Request Habit Scores

**Goal:** Add the request habit model: a decayed score for each request and bucket, in which each day counts once. Jarvis can now count the requests of the house by time of the week.

- **Implements:** G11. **Depends on:** 11.2.
- **Files:** modify `src/domain/habits.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- A request key is a name: the intent name of a request that System One acts on. Sub-phase 14.5 adds the name of a learned entry for a cluster of System Two requests.
- `RequestHabits` maps each pair of key and bucket to a `Strength { score, last }`. `last` is the local date of the last day that counted. `Bucket` and `DayType` derive `Eq` and `Ord` for the map key: weekdays before weekends, then the hour.
- `decayed(score, last, today, half_life_days) = score * 0.5^(days(last, today) / half_life_days)`. `days` is the number of calendar days from `last` to `today`.
- `observe(self, key, time, zone, half_life_days) -> Self`. `b = bucket(time, zone)`, and `d` is the local date of `time` in `zone`:
  - Without a `Strength` for `(key, b)`, it adds `{ score: 1, last: d }`.
  - When `d <= last`, it returns `self` unchanged. Thus a day counts once for each key and bucket, and an older request changes nothing.
  - Else the `Strength` becomes `{ score: decayed(score, last, d) + 1, last: d }`.
- This sub-phase adds `[habits]` (`half_life_days`, `min_score`). The defaults are `half_life_days = 14` and `min_score = 2.0` (sub-phase 11.3b). `validate` checks that both are more than 0 (sub-phase 2.2).

**Acceptance criteria.**

- AC-11.3-1: When `today` is 14 days after `last`, `decayed(4.0, last, today, 14)` returns 2.0.
- AC-11.3-2: When `observe` gets a time on the date of `last` or before it, for the same key and bucket, the model does not change.
- AC-11.3-3: When `get_weather` is observed on Saturday at 09:30, the model has a `Strength` for `(weekends, 9)` and none for `(weekdays, 9)`.

**Proofs.**

- P-11.3-1. Ensures: `decayed(s, l, t, h) = s * 0.5^(days(l, t) / h)`. Argument: `decayed` is that expression. The test checks the value (`02-rules.md` section 5.1, rule 9).
- P-11.3-2. Ensures: when `(k, b)` has a `Strength` and `d <= last`, `observe` returns the model unchanged. Argument: `observe` returns `self` in the arm with the guard `d <= last`.
- P-11.3-3. Ensures: `observe` changes only the entry of `(key, bucket(time, zone))`. Argument: `observe` reads and writes only that map entry, and P-11.1b-1 gives the bucket.

---

### 11.3b Usual Times and Habit File

**Goal:** Add the usual times of a request, the removal of faded scores and the file format of the model. Jarvis can now tell when the house usually asks for something.

- **Implements:** G11. **Depends on:** 11.3.
- **Files:** modify `src/domain/habits.rs`.

**Model.**

- `usual_times(key, today, half_life_days, min_score) -> Vec<Bucket>`: the buckets of `key` with `decayed(score, last, today) >= min_score`, in bucket order.
- `prune(today, half_life_days) -> Self` removes each `Strength` with a decayed score less than 0.01. Thus the model does not grow with old requests.
- With the defaults of sub-phase 11.3, a request in one bucket on three days within 14 days has a score of more than `1 + 0.5 + 0.5 = 2.0` on the day of the third request, so the bucket is a usual time. Requests on two days give at most `1 + 0.5^(1/14)`, which is less than 2.0. A usual time fades when no new request comes: a habit of three days in a row ends 8 days after the last request.
- `habits.json` keeps a list of `{ key, day, hour, score, last }` objects, because a JSON map key must be a string.

**Acceptance criteria.**

- AC-11.3b-1: When `get_traffic` is observed on Monday, Tuesday and Wednesday at 08:10 in the HA time zone, `usual_times` on that Wednesday returns `[(weekdays, 8)]`.
- AC-11.3b-2: When `get_traffic` is observed five times on Monday and five times on Tuesday at 08:10, `usual_times` on that Tuesday returns no bucket.
- AC-11.3b-3: When `get_traffic` is observed at 08:10 on three weekdays, the first one 14 days before the third, `usual_times` on the day of the third returns `[(weekdays, 8)]`.
- AC-11.3b-4: When the model of AC-11.3b-1 gets no new request, `usual_times` returns `[(weekdays, 8)]` 7 days after that Wednesday, and no bucket 8 days after it.
- AC-11.3b-5: When the decayed score of a `Strength` is less than 0.01, `prune` removes it, and the other entries do not change.

**Proofs.**

- P-11.3b-1 to P-11.3b-4. Ensures: `usual_times(k, t)` has `b` if and only if `(k, b)` has a `Strength` with `decayed(score, last, t) >= min_score`. Argument: `usual_times` filters the entries of `k` with that predicate. The tests check the values.
- P-11.3b-5. Ensures: `prune` keeps a `Strength` if and only if its decayed score is 0.01 or more. Argument: `prune` is a filter with that predicate.

---

### 11.4 Request Recording and Store

**Goal:** Record each spoken request that System One acts on in the request habit model, and save the model on an interval and at shutdown. Request habits now build up from normal use of the house.

- **Implements:** G11. **Depends on:** 11.3b.
- **Files:** modify `src/app/conversation.rs`, `src/app/background.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- After `decide`, `handle` observes the intent of the decision at the `Clock` time when all of these are true:
  - The context has a `satellite_id`. Thus only a spoken request counts. Text from the HA UI does not.
  - Routing processes the text as a new request (steps 3 and 6 of sub-phase 10.5). An answer to a dialog is not a request.
  - The decision is not `Defer` with `Job::Llm`. Sub-phase 14.5 counts the requests that System Two answers.
- A request that asks for the area counts once, with its intent. The answer that fills the area does not count.
- An HA state change or an automation never reaches `handle`, so it never counts.
- `handle` and the habit task share the model behind one `std::sync::Mutex`. No code holds the lock across an `await`. A poisoned lock logs one `warn` event, and the request does not count.
- At startup, `run` loads the model from a `Store<RequestHabits>` at `habits.file`, or starts with an empty model. The habit task in `src/app/background.rs` saves `prune(today)` of the model each `save_interval_s` and at shutdown.
- This sub-phase adds `habits.file` and `habits.save_interval_s`. `validate` checks that `save_interval_s` is more than 0.

**Acceptance criteria.**

- AC-11.4-1: When `handle` gets a request with a `satellite_id` on Monday at 08:10 and `FakeClassifier` gives `lights_on` 0.9, the model has a `Strength` for `lights_on` in `(weekdays, 8)` when `handle` returns.
- AC-11.4-2: When the context has no `satellite_id`, the model does not change.
- AC-11.4-3: When the decision is `Defer` with `Job::Llm`, the model does not change.
- AC-11.4-4: When "turn on the lights" asks for the area and the answer "the kitchen" fills it, the model has one observation of `lights_on`, from the request.
- AC-11.4-5: When `save_interval_s` passes, the habit store has the new observations.
- AC-11.4-6: When Jarvis stops, the habit store receives a save.
- AC-11.4-7: When the habit store has a model at startup, `usual_times` of its keys gives the stored usual times.

**Proofs.**

- P-11.4-2, P-11.4-3. Argument: `handle` calls `observe` only inside the guard `satellite_id.is_some() && !is_llm(decision)`.
- P-11.4-4. Argument: the routing arms of an answer (steps 1, 2, 4 and 5) do not call `observe`. Only the arm of a new request does.
- P-11.4-1, P-11.4-5 to P-11.4-7. Evidence: test.

---

### 11.5 Remove Habits From the Model Context

**Goal:** Remove the `habits` part from the context of the System One model. Request habits are not a model input (`01-architecture.md` decision 9), so the part is always `none` and gives the model no information.

- **Implements:** G3, G11. **Depends on:** 11.4.
- **Files:** modify `src/domain/training.rs`, `src/app/conversation.rs`, `src/app/speech.rs`, `src/lib.rs`, `src/adapters/onnx.rs`, `train/test_train.py`.

**Model.**

- `context_text(area) -> String`: `"area: <area or none>"`. The parameter `habit_lines`, the helper `join`, the spec function `joined` and the test of AC-3.3-2 go away. The spec function `context_spec` loses its `habit_lines` parameter.
- `sample_context` (sub-phase 3.3), `handle` (sub-phase 5.3), the speculative prediction (sub-phase 6.8) and the test of AC-3.4-2 call `context_text` with the area only. Thus the training set file of the `dataset` command, the fixture model and the training sets of sub-phase 13.1 get the new context.
- `train/train.py` gives the context to the tokenizer as one text segment, and it does not parse it. Thus it does not change. The fixtures of `train/test_train.py` use the new context, as the training set file does.
- The tests that give a context directly use the new context: the adapter tests of `src/adapters/onnx.rs`, and the tests of AC-5.3-3, AC-5.4c-1 and AC-6.8-11.
- The training set file changes, so `just fixture-model` and the CI job `fixture-model` train the fixture model again (sub-phase 3.9). Every criterion that uses the fixture model must pass with the new model. When one fails, change the specification first.
- The live Jarvis on the GPU host serves the fixture model (sub-phase 5.4) that it trained with the old context. Thus the model must train again after the release (field log).

**Acceptance criteria.**

- AC-11.5-1: When `context_text(Some("kitchen"))` runs, it returns `"area: kitchen"`. When `context_text(None)` runs, it returns `"area: none"`. This replaces AC-3.3-1 and AC-3.3-2.
- AC-11.5-2: When `jarvis dataset` runs without `--house`, each context is `"area: none"`. With `--house`, no context contains `habits`. This replaces AC-3.4-3.
- AC-11.5-3: When `handle` runs with a Kitchen `device_id`, `FakeClassifier` records the context `"area: Kitchen"`. This replaces AC-5.3-2.
- AC-11.5-4: When the fixture model gets each phrase of the table of AC-18.16-7 with the context `area: none`, the top label is the new intent, with a probability of `model.commit_threshold` or more. This replaces AC-18.16-7.

**Proofs.**

- P-11.5-1. Ensures: the result is `area: <area or none>`. Argument: `context_text` is a `format!` with one branch for no area.
- P-11.5-2. Argument: the contexts come from `sample_context`, which calls `context_text` with an area of the list, or with no area when the list is empty (P-11.5-1).
- P-11.5-3. Argument: `handle` gives the classifier `context_text` of the area of the device (P-11.5-1).
- P-11.5-4. Evidence: test (the fixture model).

**Field log.**

1. After the release with this sub-phase, stop the `jarvis` unit on the GPU host, run `just fixture-model` in its checkout, and start the unit again. Record that "turn on the lights" from the Office satellite turns on the Office lights.
