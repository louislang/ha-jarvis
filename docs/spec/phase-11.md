# Phase 11: Habit Learning

This phase learns the habits of the house. Habits go into the model context, help select the entity and missing slot values, and go into the System Two prompt.

---

### 11.1 Source, Action and Bucket

**Goal:** Add the pure functions that classify a state change: who caused it, what action it was, and which time bucket it falls in. Habits can then ignore automations and count only real use.

- **Implements:** G11. **Depends on:** 10.7.
- **Files:** create `src/domain/habits.rs`. Modify `src/lib.rs`.

**Model.**

- `source(context, jarvis_user_id) -> Source` (decision 10). `jarvis_user_id` is `Snapshot.user_id` (sub-phase 2.3).
- `action(domain, old, new) -> Option<(action, value)>`: `on`/`off` from the state, `play` with `media_series_title` when present (the show), else `media_title`, `set_temperature` with the new `temperature`. Other attribute changes give `None`.
- `bucket(time, zone) -> (DayType, hour)`. `DayType` is `weekdays` or `weekends`. `zone` is `Snapshot.config.time_zone`.

**Acceptance criteria.**

- AC-11.1-1: When the context has the user ID of a person, `source` returns `manual`.
- AC-11.1-2: When the context has a `parent_id`, `source` returns `automation`.
- AC-11.1-3: When the context has no `user_id` and no `parent_id`, `source` returns `manual`.
- AC-11.1-4: When the context has the Jarvis user ID, `source` returns `voice`.
- AC-11.1-5: When a light changes from `off` to `on`, `action` returns `on` with no value.
- AC-11.1-6: When a media player changes to `playing` with `media_title = Bluey`, `action` returns `play` with `Bluey`. With `media_series_title = Bluey` and `media_title = Sleepytime`, it returns `play` with `Bluey`.
- AC-11.1-7: When the climate `temperature` changes from 72 to 70, `action` returns `set_temperature` with `70`.
- AC-11.1-8: When only an untracked attribute changes, `action` returns none.
- AC-11.1-9: When the time is Saturday 20:15 in the HA time zone, `bucket` returns `(weekends, 20)`.

**Proofs.**

- P-11.1-1 to P-11.1-4. Ensures: `source` is a total function with this precedence: Jarvis user, then `parent_id`, then other `user_id`, then none. Argument: `source` is one ordered `match` on `(user_id, parent_id)` with these four arms.
- P-11.1-5 to P-11.1-8. Argument: `action` matches on the domain and compares the tracked fields of `old` and `new`. A change in no tracked field reaches the `None` arm.
- P-11.1-9. Evidence: test (time zone data).

---

### 11.2 Scores and Summary

**Goal:** Add the habit model with decayed counts, scores with neighbor hours, the plain-text summary and the established habit set. Jarvis can now say what the house tends to do at a given time.

- **Implements:** G11, G16. **Depends on:** 11.1.
- **Files:** modify `src/domain/habits.rs`, `src/lib.rs`, `jarvis.toml`, `benches/domain.rs`.

**Model.**

- `decayed(count, last, now, half_life) = count * 0.5^((now - last) / half_life)`.
- `observe(self, entity, action, value, source, time) -> Self` adds 1 to the decayed count of `(entity, action, value, source, bucket)`. An observation older than the last update of its key is ignored.
- `score(entity, action, time) = c(bucket(time)) + 0.5 * (c(bucket(time - 1 h)) + c(bucket(time + 1 h)))`, without `automation` counts. Thus the neighbors cross midnight with the day type of that hour.
- `summary(area, time, n) -> Vec<String>`: the top `n` habits for the area at that time. Each line is `<entity name> <action> [<value>] <HH:00> <weekdays|weekends> <score with one decimal>`, in lowercase.
- `established(min_score) -> BTreeSet<HabitKey>`: the keys with a score of `min_score` or more in one or more buckets.
- This sub-phase adds `[habits]` (`domains`, `half_life_days`, `min_score`).

**Acceptance criteria.**

- AC-11.2-1: When the time is one half-life after the last update, `decayed(4.0, ...)` returns 2.0.
- AC-11.2-2: When the house turns on a light at 18:00 on 10 weekdays, `score` at 18:00 on a weekday is higher than at 09:00.
- AC-11.2-3: When all observations come from `automation`, `score` returns 0.
- AC-11.2-4: When `summary` runs at 18:00 on a weekday, its first line has the form `living room lamp on 18:00 weekdays 4.2`.
- AC-11.2-5: When a habit has a score of `min_score` in one bucket, `established` contains it.
- AC-11.2-6 [Bench]: `score` with 500 habit keys stays within the instruction budget in `benches/domain.rs`.
- AC-11.2-7: When `score` runs at Monday 00:30, it adds half of the count of the Sunday 23:00 (`weekends`) bucket.
- AC-11.2-8: When `observe` gets a time older than the last update of the key, the count does not change.

**Proofs.**

- P-11.2-1. Argument: `0.5^1 = 0.5`, and `4.0 * 0.5 = 2.0`.
- P-11.2-2 (AC-11.2-2). Argument: the 18:00 bucket has 10 observations, so `c(18) > 0`. The 09:00 bucket and its neighbors have none, so `score(09:00) = 0`. Decay keeps positive counts positive.
- P-11.2-3. Argument: `score` sums only non-`automation` counts, which are all 0.
- P-11.2-5. Argument: `established` filters keys by `max over buckets of score >= min_score`.
- P-11.2-4, P-11.2-7, P-11.2-8. Evidence: test.
- P-11.2-6. Evidence: benchmark.

---

### 11.3 Habit Learning and Store

**Goal:** Feed each live state change into the habit model and save the model on an interval and at shutdown. Habits now build up from normal use of the house.

- **Implements:** G11. **Depends on:** 11.2.
- **Files:** modify `src/app/background.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** The sync use case gives each `StateChanged` event to the cache and, for a domain in `habits.domains`, to `observe`. A `Store<Habits>` saves to `habits.file` each `save_interval_s` and at shutdown. This sub-phase adds `habits.file` and `habits.save_interval_s`.

**Acceptance criteria.**

- AC-11.3-1: When `FakeHome` sends `StateChanged` for a tracked domain, the habit model has the observation before virtual time advances.
- AC-11.3-2: When the event is for a domain not in `habits.domains`, the model does not change.
- AC-11.3-3: When `save_interval_s` passes, the habit store has the new observations.
- AC-11.3-4: When Jarvis stops, the habit store receives a save.

**Proofs.**

- P-11.3-2. Argument: `observe` is called inside `if domains.contains(domain)`.
- P-11.3-1, P-11.3-3, P-11.3-4. Evidence: test.

---

### 11.4 History Bootstrap

**Goal:** Add `Home::history` and load `bootstrap_days` of HA history when no habit data exists. Jarvis has useful habits on its first day.

- **Implements:** G11. **Depends on:** 11.3.
- **Files:** modify `src/ports.rs`, `src/adapters/ha.rs`, `src/app/background.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `Home::history(start, end, entities) -> Result<Vec<StateChange>>` uses `history/history_during_period`. When the habit store has no data, the use case loads the history and observes each change with source `unknown`. The history ends at the time of the first live subscription, so history and live events do not overlap. The first state of each entity in the history is a start state, not a change, so it gives no observation. This sub-phase adds `habits.bootstrap_days`.

**Acceptance criteria.**

- AC-11.4-1: When the habit store has no data, the use case calls `history` for `bootstrap_days`, and all bootstrap observations have source `unknown`.
- AC-11.4-2: When `HaHome::history` runs, `FakeHaServer` receives `history/history_during_period` with the start and end times.
- AC-11.4-3: When the habit store has data, the use case does not call `history`.
- AC-11.4-4: When the history has three states for one entity, the use case observes two changes, and the history request ends at the subscription time.

**Proofs.**

- P-11.4-1, P-11.4-3. Argument: the bootstrap runs inside `if store.load()? == None`, and it passes the constant source `unknown`.
- P-11.4-2, P-11.4-4. Evidence: test.

---

### 11.5 Habits in the Model Context

**Goal:** Put the habit summary for the satellite area and the current time into the System One model context. The model can now prefer what the house usually does at this time.

- **Implements:** G3, G11. **Depends on:** 11.4.
- **Files:** modify `src/app/conversation.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `handle` builds `context_text(area, summary(area, now, model.habits))`. The speech use case still predicts partials with no habit lines, because the audio stream has no satellite. This sub-phase adds `model.habits`.

**Acceptance criteria.**

- AC-11.5-1: When `handle` runs at 18:00 on a weekday in the Living Room, `FakeClassifier` records a context with the top `model.habits` habit lines for that area and time.
- AC-11.5-2: When the speech use case predicts a partial text, the context has `habits: none`.
- AC-11.5-3 [GPU host]: With 500 habit keys, all Phase 9.2 latency tests pass.

**Proofs.**

- P-11.5-1. Argument: `handle` passes `summary(area, now, n)` to `context_text` (P-3.3-2).
- P-11.5-2. Argument: the speech use case passes an empty list.
- P-11.5-3. Evidence: test.

---

### 11.6 Habits in Entity and Slot Selection

**Goal:** Use habit scores to choose between entities and to fill a missing slot. "Turn on the TV" picks the usual TV and the usual show for this time.

- **Implements:** G11. **Depends on:** 11.5.
- **Files:** modify `src/domain/select.rs`, `src/domain/decide.rs`, `intents.toml`.

**Model.** The habit action of an intent comes from its service: `turn_on` gives `on`, `turn_off` gives `off`, `media_player.play_media` gives `play`, and `climate.set_temperature` gives `set_temperature`. Another service scores 0. `target = "entity"` picks the matching entity with the highest score of that action now. Equal scores pick the lowest entity ID. `habit_defaults` maps a slot to a habit action. An empty slot takes the value of that action with the highest score now, for the selected entity. No such habit gives `Ask` from `ask_<slot>`.

**Acceptance criteria.**

- AC-11.6-1: When an area has two media players, `select` returns the one with the higher score now.
- AC-11.6-2: When two entities have equal scores, `select` returns the one with the lowest entity ID.
- AC-11.6-3: When `tv_on` has no `show` slot and the top `play` value at 20:00 is `Bluey`, the call data `media_content_id` has `"show_name": "Bluey"`.
- AC-11.6-4: When no habit exists for a `habit_defaults` slot, `decide` returns `Ask` from the pool `ask_<slot>`.

**Proofs.**

- P-11.6-1, P-11.6-2. Argument: `select` sorts the candidates by entity ID, then uses a `max_by` on the score that keeps the first of equal elements.
- P-11.6-3, P-11.6-4. Argument: the slot value is `slot.or(top_value(habits, action, now))`. `None` from both gives `Ask`.

---

### 11.7 Habits in the System Two Prompt

**Goal:** Add the habit summary to the System Two question. OpenAI answers with the same knowledge of the house that System One has.

- **Implements:** G7, G11. **Depends on:** 11.6.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/app/conversation.rs`.

**Model.** `Question` gets `habits: Vec<String>`. The user message lists them. The system message does not change.

**Acceptance criteria.**

- AC-11.7-1: When the prompt builder runs, the user message contains the habit summary for the area.
- AC-11.7-2: When the prompt builder runs, the system message equals the system message of sub-phase 7.2 byte for byte.

**Proofs.**

- P-11.7-1. Argument: the user message builder iterates `question.habits`.
- P-11.7-2. Argument: `system_prompt` is still a constant (P-7.2-1).
