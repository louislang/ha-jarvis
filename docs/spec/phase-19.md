# Phase 19: Voice Timers

This phase adds voice timers. You can set a timer with or without a name, ask for the time left, pause, resume and cancel it. Each timer runs in an HA timer helper of the timer pool, which you make once in HA (decision 35). Jarvis keeps a label for each timer: the name, the length and the satellite. When a timer ends, the satellite where you set it says "Your chicken timer is done" each 15 s for up to 2 minutes, until you say "stop". A timer that ended while Jarvis was down or disconnected is announced once at the next connection. The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 16.

---

### 19.1 Timer Quint Model

**Goal:** Add `quint/timers.qnt`, the model of timers in a pool of HA helpers, their labels, the late state events of HA and the ring. Prove that a cancelled timer never rings, that only a finished timer rings, that a ring ends at its limit or at a stop, that a timer whose finish Jarvis missed is announced once, and that a running helper always has the label of its timer.

- **Implements:** G19, G25. **Depends on:** 16.4.
- **Files:** create `quint/timers.qnt`.
- **Quint:** `timers.qnt`.

**Model.**

- Constants: `HELPERS = Set("h1", "h2")`, `SATELLITES = Set("s1", "s2")`, `TIMERS = 1.to(3)`, `RINGS = 8` (`RING_FOR / RING_EVERY`, sub-phase 19.7).
- State. For each helper: `ha` (`idle`, `active` or `paused`: the state in HA), `owner` (the timer that HA runs, 0 for none), `seen` (the state that Jarvis saw last) and `label` (the timer of the label, 0 for none). For each timer: `satellite`, `cancelled`, `finished`, `ringing`, `rings`, `stopped`, `ringsAtStop` and `lateRings`. `nextTimer`. `events: List[(helper, state, finished)]`: the state events that HA sent and Jarvis did not process.
- Actions:
  - `start(s)` (when `nextTimer` is in `TIMERS` and a helper `h` has `seen[h] = idle` and `label[h] = 0`): the timer `t := nextTimer` gets `satellite := s`. `label[h] := t`, `owner[h] := t`, `ha[h] := active`, `seen[h] := active`, and `(h, active, false)` joins `events`.
  - `pause(h)` and `resume(h)` (when `label[h] != 0`): `ha[h]` changes from `active` to `paused` or back, with an event.
  - `cancel(h)` (when `label[h] != 0`): `cancelled[label[h]] := true` and `label[h] := 0`. If `ha[h]` is not `idle`, `ha[h] := idle`, `owner[h] := 0`, and `(h, idle, false)` joins `events`.
  - `finish(h)` (HA, when `ha[h] = active`): `finished[owner[h]] := true`, `ha[h] := idle`, `owner[h] := 0`, and `(h, idle, true)` joins `events`.
  - `outsideCancel(h)` (the HA UI, when `ha[h]` is not `idle`): the same without `finished`, and the event has `false`.
  - `observe` (when `events` is not empty): remove the head `(h, x, f)`. `seen[h] := x`. When `x = idle` and `label[h] = t != 0`: `label[h] := 0`, and when `f`, `ringing[t] := true`.
  - `ring(t)` (when `ringing[t]`): `rings[t] += 1`. At `RINGS`, `ringing[t] := false`.
  - `stop(s)`: each ringing timer of `s` gets `ringing := false`, `stopped := true` and `ringsAtStop := rings`.
  - `reconnect` (a restart of Jarvis or a new HA connection): `events := []`, and `seen[h] := ha[h]` for each helper. Each label `t` of an `idle` helper leaves, and when `finished[t]`, `lateRings[t] += 1`.
- Invariants:
  - `NoRingAfterCancel`: no timer has `cancelled` and `rings > 0`.
  - `RingOnlyFinished`: a timer with `rings > 0` or `lateRings > 0` has `finished`. `NoRingAfterCancel` also covers `lateRings`.
  - `LateRingOnce`: each timer has `lateRings <= 1`, and a timer with `lateRings = 1` has `rings = 0`.
  - `RingLimit`: each timer has `rings <= RINGS`.
  - `NoRingAfterStop`: a timer with `stopped` has `rings = ringsAtStop`.
  - `RunningTimerLabeled`: each helper with `ha != idle` has `label = owner`. Without the `seen` guard of `start` or its `seen[h] := active`, a late `idle` event of a cancel removes the label of the next timer on the helper. Without the `label` guard, a new timer takes a helper whose finish Jarvis did not see, and the new timer rings.

**Acceptance criteria.**

- AC-19.1-1 [Quint]: `quint typecheck` passes.
- AC-19.1-2 [Quint]: The six invariants hold in simulation and to 10 steps in `quint verify`.
- AC-19.1-3 [Quint]: The test `ac_19_1_3_cancel_before_finish_event` passes: when HA finishes a timer and Jarvis cancels it before it observes the event, the timer never rings.
- AC-19.1-4 [Quint]: The test `ac_19_1_4_helper_reuse` passes: a helper whose finish Jarvis did not observe gets no new timer. After `observe`, a new timer on `s2` takes it, and the old timer rings on `s1`.
- AC-19.1-5 [Quint]: The test `ac_19_1_5_late_ring` passes: when HA finishes a timer and `reconnect` comes before `observe`, the timer has `lateRings = 1`, `rings = 0` and no label.

**Proofs.**

- P-19.1-2. `NoRingAfterCancel`: `cancel` removes the label in the same step, and only `observe` with a label starts a ring. `RingOnlyFinished`: `observe` rings only for an event with `f`, which only `finish` sends, and the label of the helper is the owner of that finish (`RunningTimerLabeled`). `RingLimit` and `NoRingAfterStop`: `ring` needs `ringing`, which the limit and `stop` set to false. `LateRingOnce`: `reconnect` counts a late ring only for a label, removes the label in the same step, and `observe` rings only for a label. `reconnect` keeps `RunningTimerLabeled`, because it makes `seen` equal to `ha` and removes the labels only of `idle` helpers. `RunningTimerLabeled` by induction: `start` sets `label` and `owner` together. `start` sets `seen` to `active`, and only `observe` of an `idle` event sets it to `idle`. After that event, the helper has no pending event until the next `start`. Thus no `idle` event of an older timer is pending when `start` takes the helper.
- P-19.1-1, P-19.1-3 to P-19.1-5. Evidence: model check.

---

### 19.2 Timer Durations

**Goal:** Add the pure duration functions. "1 hour 30 minutes" gives 5400 s, and Jarvis can say "3 minutes" for 180 s.

- **Implements:** G25. **Depends on:** 19.1.
- **Files:** create `src/domain/timers.rs`. Modify `src/lib.rs`.

**Model.**

- `seconds(text) -> Option<u32>`: `normalize` (sub-phase 3.1), then `text2digits` (as the filter `number`, sub-phase 18.9). The words must be one or more terms, with an optional "and" between two terms. A term is a count and a unit, optionally followed by "and a half". A count is digits, "a" or "an" (1), or a count and "and a half" ("one and a half hours"). "half a", "half an" and "a half" before a unit are a count of one half. A unit is `second`, `minute` or `hour`, singular or plural. The result is the sum. A half of a second, another word, a missing unit, a total of 0 or a total of more than 86400 gives `None`.
- `spoken(seconds) -> String`: the hours, minutes and seconds that are not 0, each with its unit in the singular for 1 and in the plural for more, joined with " and ". 0 gives "0 seconds".
- `rounded(seconds) -> u32`: `seconds` under 60, else the next whole minute. Thus a time left of 2 minutes and 47 seconds is "3 minutes".
- `length(seconds) -> String`: the parts of `spoken` with the units in the singular, joined with a space. It names a timer without a name: "10 minute".
- `remaining(text) -> Option<u32>`: the seconds of the HA duration `H:MM:SS` (the attribute `remaining` of a paused timer). Another form gives `None`.

**Acceptance criteria.**

- AC-19.2-1: `seconds` of "10 minutes", "five minutes", "1 hour 30 minutes", "an hour and 15 minutes", "90 seconds" and "a minute" gives 600, 300, 5400, 4500, 90 and 60.
- AC-19.2-2: `seconds` of "half an hour", "a half hour", "an hour and a half", "one and a half hours" and "a minute and a half" gives 1800, 1800, 5400, 5400 and 90.
- AC-19.2-3: `spoken` of 180, 61 and 3600 gives "3 minutes", "1 minute and 1 second" and "1 hour". `rounded` of 45, 60, 61 and 179 gives 45, 60, 120 and 180.
- AC-19.2-4: `length` of 600 and 5400 gives "10 minute" and "1 hour 30 minute".
- AC-19.2-5: `remaining` of "0:03:00" and "1:00:05" gives 180 and 3605. `remaining` of "3 minutes" and "1 day, 0:00:00" gives `None`.
- AC-19.2-6: `seconds` of "", "minutes", "10 bananas", "0 minutes", "half a second" and "25 hours" gives `None`.

**Proofs.**

- P-19.2-1, P-19.2-2, P-19.2-6. Ensures: `seconds(t) = Some(n)` if and only if the words of the digit text are terms of the grammar of the model, `n` is the sum of `half_units(term) * unit_seconds(unit) / 2` with `half_units` the count in halves, `n` is a whole number of seconds, and `0 < n <= 86400`. Argument: one pass over the words with the state "count expected", "unit expected" or "half expected". The loop invariant states that the total is the sum of the closed pairs and that it is at most 86400, so no addition overflows. The `text2digits` contract gives the digit text.
- P-19.2-3, P-19.2-4. Ensures: `spoken` and `length` are the joins of `parts(seconds)`, where `parts` gives `(seconds / 3600, hour)`, `(seconds % 3600 / 60, minute)` and `(seconds % 60, second)` without the parts of 0. `rounded(s) = s` for `s < 60`, else `(s + 59) / 60 * 60`. Argument: each function is this closed form.
- P-19.2-5. Ensures: `remaining(t) = Some(h * 3600 + m * 60 + s)` if and only if `t` is digits `h`, ":", two digits `m < 60`, ":" and two digits `s < 60`. Argument: `split` on ":" gives three parts, and each part is checked.

---

### 19.3 Timer Labels

**Goal:** Add the timer label and the pure functions on the timer pool: the free helper, the time left and the finish. Jarvis can then pick a helper for a new timer and tell a finish from a cancel.

- **Implements:** G25. **Depends on:** 19.2.
- **Files:** modify `src/domain/timers.rs`.

**Model.**

- `TIMER_POOL = "timer.jarvis_"`. The timer pool is each `timer` entity whose entity ID starts with it (decision 35). You make a fixed set of helpers once in HA, for example `timer.jarvis_1` to `timer.jarvis_5`. Jarvis needs no admin rights for them. At most that many timers run at the same time. The tests add `timer.jarvis_1` and `timer.jarvis_2` to the hot cache. The reference house has no timer.
- `Label { helper, name: Option<String>, seconds, satellite: Option<String>, ends: Option<Timestamp> }`. `ends` is the end time of a running timer, and `None` for a paused timer. `Book { labels: Vec<Label>, seen: BTreeMap<String, State> }`. `labels` is in start order, so the newest timer is the last. `seen` holds the state of each helper of the pool as Jarvis saw it last. `description(label)` is the name, else `length(seconds)`.
- `free(book) -> Option<String>`: the lowest entity ID of `seen` with the state `idle` and no label.
- `left(label, state, now) -> u32`: for `active`, the seconds from `now` to the attribute `finishes_at`, and 0 after it. For `paused`, `remaining` of the attribute `remaining`. Else, or when the value does not parse, `label.seconds`.
- `finished(old, new) -> bool`: `old` is `active` with a `finishes_at` that parses, `new` is `idle`, and `new.last_updated >= finishes_at`. HA writes `idle` at or after `finishes_at` when a timer ends. A cancel, also from the HA UI, writes it before.
**Acceptance criteria.**

- AC-19.3-1: When `seen` has `timer.jarvis_1` `active`, and `timer.jarvis_2` and `timer.jarvis_3` `idle`, and `timer.jarvis_2` has a label, `free` gives `timer.jarvis_3`. When no idle helper is without a label, `free` gives `None`.
- AC-19.3-2: `left` gives 180 for an `active` state with `finishes_at` 180 s after `now`, 150 for a `paused` state with `remaining` "0:02:30", and `label.seconds` for an `idle` state.
- AC-19.3-3: `finished` gives true for `active` with `finishes_at` `T` to `idle` at `T` + 0.2 s, false for `idle` at `T` - 60 s, and false for `paused` to `idle`.

**Proofs.**

- P-19.3-1. Ensures: `free(book)` is the lowest key of `seen` whose state is `idle` and that no label has, and `None` when no key has both. Argument: one pass over `seen` in key order, with the loop invariant that no earlier key has both.
- P-19.3-2, P-19.3-3. Ensures: `left` and `finished` are the cases of the model. Argument: each function matches on the state and on the parse result. The `jiff` contracts give the time differences.

---

### 19.3b Timer Book Updates

**Goal:** Add the pure effect of a state event and of a snapshot on the book. A finish then gives the label to ring, and a timer that ended while Jarvis was down or disconnected gives a late announce.

- **Implements:** G25. **Depends on:** 19.3.
- **Files:** modify `src/domain/timers.rs`.

**Model.**

- `observe(book, old, new) -> (Book, Option<Label>)`: `seen` gets `new`. For `active` with a `finishes_at` that parses, the label of the helper gets `ends := finishes_at`, and for `paused`, `ends := None`. When `new` is `idle` and a label has the helper, the label leaves the book. It is the result when `finished(old, new)`.
- `load(book, states, now) -> (Book, Vec<Late>)`: `seen` gets the states of the pool. Each label whose helper is not `active` or `paused` leaves. When its `ends` is at or before `now`, the timer ended while Jarvis was down or disconnected, and the result has `Late { label, ago }` with the seconds from `ends` to `now`. Else, for example after a cancel in the HA UI, it leaves with no ring. The labels in `timers.json` keep `ends`, so a restart of Jarvis also finds them.

**Acceptance criteria.**

- AC-19.3b-1: When `observe` gets a finish of a helper with a label, the result has the label, and the book has no label for the helper. For a cancel, the label leaves and the result is `None`. For `idle` to `active` with `finishes_at` `T`, the label gets `ends = T`, and for `active` to `paused`, `ends = None`.
- AC-19.3b-2: When `load` gets `timer.jarvis_1` `idle` with a label, `timer.jarvis_2` `active` with a label and `timer.kitchen` `idle`, the book keeps only the label of `timer.jarvis_2`, and `seen` has only the two helpers of the pool.
- AC-19.3b-3: When the label of `timer.jarvis_1` has `ends` 40 s before `now`, `load` gives one `Late` with `ago = 40`. With `ends` after `now` or `None`, it gives no `Late`.

**Proofs.**

- P-19.3b-1 to P-19.3b-3. Ensures: after `observe`, the labels are the old labels without the label of the helper when `new` is `idle`, and the result is that label exactly when `finished(old, new)`. After `load`, the labels are the old labels whose helper is `active` or `paused` in `states`, and the `Late` entries are the other labels with `ends <= now`, in start order. Argument: one pass over the labels with these conditions.

---

### 19.4 Timer Intents

**Goal:** Add the timer intents with `kind = "timer"` and `Decision::Timer`. The System One model then classifies "set a timer for 10 minutes" and the other timer requests, and `decide` gives them to the timer rules.

- **Implements:** G3, G25. **Depends on:** 19.3b.
- **Files:** modify `src/domain/catalog.rs`, `src/domain/decide.rs`, `src/domain/responses.rs`, `src/adapters/onnx.rs`, `intents.toml`, `responses.toml`.

**Model.**

- `Kind::Timer` and `Intent.timer: Option<TimerAction>`, with `TimerAction` one of `start`, `left`, `cancel`, `cancel_last`, `pause` and `resume`. `validate_catalog` checks that each `timer` intent has an action and no `service`, and that no other intent has an action.
- The new intents go at the end of `intents.toml`, so the contexts of the existing examples do not change (phase 18 preface). Each has `optional = ["timer"]`, except `timer_cancel_last`:
  - `timer_start` (`start`): "set a timer for [10 minutes](duration)", "set a timer for [5 minutes](duration) for the [chicken](timer)", "start a [pasta](timer) timer for [8 minutes](duration)", "set a [20 minute](duration) timer", "set a timer for the [eggs](timer)", "timer for [30 seconds](duration)". Pool: `{% if timer %}The {{ timer }} timer is set for {{ duration }}.{% else %}Timer set for {{ duration }}.{% endif %}`
  - `timer_left` (`left`): "how long is left on the timer", "how long do we have left on the [chicken](timer) timer", "how much time is left", "how long until the [pasta](timer) is done". Pool: `{% for t in timers %}{{ "The" if loop.first else (", and the" if loop.last else ", the") }} {{ t.timer }} timer {{ "is paused with" if t.paused else "has" }} {{ t.left }}{{ " left" if loop.first else "" }}{% else %}No timer is running{% endfor %}.`
  - `timer_cancel` (`cancel`): "cancel the [chicken](timer) timer", "cancel that timer", "cancel the timer", "stop the [pasta](timer) timer", "delete the timer". Pool: `Cancelling the {{ timer }} timer.`
  - `timer_cancel_last` (`cancel_last`): "cancel the most recent timer", "cancel the last timer", "cancel the newest timer". Pool: `Cancelling the {{ timer }} timer.`
  - `timer_pause` (`pause`): "pause the [chicken](timer) timer", "pause the timer", "pause my timer". Pool: `Pausing the {{ timer }} timer.`
  - `timer_resume` (`resume`): "resume the [chicken](timer) timer", "resume the timer", "unpause the timer". Pool: `Resuming the {{ timer }} timer.`
- `decide` gives `Decision::Timer { intent, action, slots }` for a confident timer intent, before the area rules, because a timer needs no area.
- The template variables of a timer pool are its slots, `area`, `timer` and `timers` (`pool_variables`). `validate_pools` requires the pools `ask_timer` ("Which timer?"), `ask_duration` ("For how long?"), `timer_late` (`{% if timer %}Your {{ timer }} timer{% else %}Your timer{% endif %} finished {{ ago }} ago.`), `timer_none` ("I could not find that timer."), `timer_full` ("All timers are in use.") and `timer_done` (`{% if timer %}Your {{ timer }} timer is done.{% else %}Your timer is done.{% endif %}`). `timer_done` can use the variable `timer`, and `timer_late` the variables `timer` and `ago`. The other pools that are not intent pools use no variable (sub-phase 4.2).

**Acceptance criteria.**

- AC-19.4-1: When `intents.toml` and `responses.toml` are read, `validate_catalog` and `validate_pools` return `Ok`.
- AC-19.4-2: When a `timer` intent has a `service` or no action, or a `command` has an action, `validate_catalog` returns an error that names the intent.
- AC-19.4-3: When the prediction is `timer_start` 0.9 with the slot `duration = 10 minutes` and the request has no area, the result is `Timer` with the action `start` and the slot.
- AC-19.4-4: When the pool `ask_timer` is missing, `validate_pools` returns an error that names it. When `timer_full` uses `timer`, it returns an error with the pool and the variable.
- AC-19.4-5: When the fixture model gets these phrases with the context `area: none`, the top label has a probability of `model.threshold` or more: "set a timer for 10 minutes" gives `timer_start` and `duration = 10 minutes`. "set a timer for 5 minutes for the chicken" gives `timer_start`, `duration = 5 minutes` and `timer = chicken`. "how long do we have left on the chicken timer" gives `timer_left` and `timer = chicken`. "how long is left on the timer" gives `timer_left`.

**Proofs.**

- P-19.4-2. Ensures: `validate_catalog` returns `Ok` only when each intent has an action exactly when its kind is `timer`, and no `timer` intent has a `service`. Argument: check 4 of `validate_catalog` matches on the kind and the two fields.
- P-19.4-3. Ensures: a top `timer` intent with `p(top) >= threshold` gives `Timer` with its action and the slots of the prediction, and no other kind gives `Timer`. Argument: `decide` matches `Kind::Timer` before the area rules (step 3, sub-phase 4.5b).
- P-19.4-1, P-19.4-4, P-19.4-5. Evidence: test.

---

### 19.5 Timer Start and Time Left

**Goal:** Add `act`, the pure timer rule, for the start and the time left. "Set a timer for 5 minutes for the chicken" then gives a reply, a `timer.start` call on a free helper and a new label, and "how long is left" lists each timer.

- **Implements:** G25. **Depends on:** 19.4.
- **Files:** modify `src/domain/timers.rs`.

**Model.**

- `act(timer, request, satellite, book, pools, rng) -> Acted { decision, change }`. `timer` is the `Decision::Timer`. `change` is `None`, `Add(Label)` or `Remove(helper)`. The name is the normalized `timer` slot, and an empty slot is no name.
- `apply(book, change) -> Book`: `Add` appends the label and sets the `seen` state of its helper to `active` with no attributes, so `free` does not give the helper again before HA reports it (`RunningTimerLabeled`). `Remove` removes the label of the helper.
- `start`: with no `duration` slot, the result is `Ask` from `ask_duration` with `Pending::Slot { intent, slots, missing: "duration", choices: [], transcript }`. The answer fills the slot (sub-phase 19.6c), and the name stays in the slots. When `seconds` of the slot is `None`, the result is `Defer` with `Job::Llm`. When `free(book)` is `None`, all helpers of the pool are in use: the result is `Reply` from `timer_full` ("All timers are in use.") with no call, and Jarvis starts no timer. Else the result is `Reply` from the intent pool, with `timer` (the name or "") and `duration` (the slot), and the call `timer.start` on the free helper with `data = { duration: <seconds> }`. HA accepts a number of seconds as a duration. The change is `Add` with the helper, the name, the seconds, the satellite and `ends = request.now + seconds`.
- `left`: the timers are the labels in start order. A name keeps only the labels whose description is the name, and a name that matches no label gives `Reply` from `timer_none`. The pool gets `timers`: for each timer, `timer` (its description), `left` (`spoken(rounded(left))` with its `seen` state and `request.now`) and `paused`. No label gives an empty list.

**Acceptance criteria.**

- AC-19.5-1: When the action is `start` with `duration = 5 minutes`, `timer = chicken` and the satellite `assist_satellite.carson_kitchen`, and `timer.jarvis_1` is free, the reply is "The chicken timer is set for 5 minutes.", the call is `timer.start` on `timer.jarvis_1` with `duration = 300` as a number, and the change adds the label.
- AC-19.5-2: When `start` has no `timer` slot and `duration = 10 minutes`, the reply is "Timer set for 10 minutes.", and the label has no name.
- AC-19.5-3: When no helper is free, the reply is from `timer_full`, with no call and no change. When the duration is "10 bananas", the result is `Defer` with `Job::Llm`. With `timer = chicken` and no `duration` slot, the result is `Ask` from `ask_duration` with no choices, the missing slot `duration` and the slot `timer = chicken`.
- AC-19.5-4: When the action is `left`, the chicken timer is `active` with 170 s left and the pasta timer is `active` with 480 s left, the reply is "The chicken timer has 3 minutes left, and the pasta timer has 8 minutes."
- AC-19.5-5: When one timer of 600 s without a name is `paused` with `remaining` "0:08:00", the reply is "The 10 minute timer is paused with 8 minutes left." With no label, the reply is "No timer is running." With the name "pasta" and only the chicken timer, the reply is from `timer_none`.

**Proofs.**

- P-19.5-1 to P-19.5-3. Ensures: for `start`, the result has a call if and only if `seconds` gives `Some(n)` and `free(book)` gives `Some(h)`. Then the call is `timer.start` with the target `[h]` and `duration = n`, and the change is `Add` with `helper = h` and `seconds = n`. Else the change is `None`. After `apply` of `Add(l)`, the labels are the old labels and then `l`, and `seen[l.helper]` is `active`. After `Remove(h)`, they are the old labels without the label of `h`. Argument: `act` checks `seconds`, then `free`, before it builds the call (P-19.2-1, P-19.3-1). `apply` is a push and an insert, or one `retain` pass.
- P-19.5-4, P-19.5-5. Ensures: for `left`, the change is `None`, the result has no call, and the list has one entry for each label that the name keeps, in start order. Argument: one pass over the labels with the loop invariant that the list has the kept labels before the index.

---

### 19.5b Timer Choice

**Goal:** Add cancel, pause and resume to `act`. When the request names no timer and several can match, Jarvis asks "Which timer?" with the phase 10 dialog.

- **Implements:** G13, G25. **Depends on:** 19.5.
- **Files:** modify `src/domain/timers.rs`.

**Model.**

- The candidates of `cancel` and `cancel_last` are all labels. The candidates of `pause` are the labels whose `seen` state is `active`, and of `resume` the labels whose state is `paused`.
- The target. With a name: the newest candidate whose description is the name. No such candidate gives `Reply` from `timer_none`. `cancel_last`: the newest candidate. Else, one candidate is the target. No candidate gives `timer_none`. Two or more give `Ask` from `ask_timer` with `Pending::Slot { intent, slots, missing: "timer", choices, transcript }`, and the choices are the descriptions of the candidates.
- The answer fills the slot `timer` (sub-phase 10.5), so `decide` gives `Timer` again with the name.
- With a target, the result is `Reply` from the intent pool with `timer` (the description of the target). `cancel` and `cancel_last`: the call `timer.cancel`, and the change `Remove(helper)`. `pause`: the call `timer.pause`. `resume`: the call `timer.start` with no data. HA resumes a paused timer with its remaining time.

**Acceptance criteria.**

- AC-19.5b-1: When `cancel` has the name "chicken" and the chicken and pasta timers run, the reply is "Cancelling the chicken timer.", the call is `timer.cancel` on the helper of the chicken timer, and the change removes that helper.
- AC-19.5b-2: When `cancel` has no name and one timer of 600 s without a name runs, the reply is "Cancelling the 10 minute timer."
- AC-19.5b-3: When `cancel` has no name and the chicken and pasta timers run, the result is `Ask` from `ask_timer` with the choices "chicken" and "pasta" and the missing slot `timer`, with no call and no change.
- AC-19.5b-4: When `cancel_last` runs and the pasta timer started after the chicken timer, the call is `timer.cancel` on the helper of the pasta timer.
- AC-19.5b-5: When the chicken timer is `active` and the pasta timer is `paused`, `pause` with no name gives `timer.pause` on the chicken helper, and `resume` with no name gives `timer.start` on the pasta helper with no data.
- AC-19.5b-6: When no label is a candidate, or the name matches no candidate, the reply is from `timer_none`, with no call and no change.

**Proofs.**

- P-19.5b-1 to P-19.5b-6. Ensures: a `Reply` with a call targets exactly one helper of a candidate. Its service is the service of the action, and the change is `Remove` of that helper exactly for `cancel` and `cancel_last`. `Ask` occurs only with no name, not for `cancel_last`, and with two or more candidates, and its choices are their descriptions in start order. Argument: `act` filters the candidates, then matches on `(name, action, count)`. The pass that finds the newest match runs from the last label back, with the loop invariant that no label after the index matches.

---

### 19.6 Timer Events

**Goal:** Add the timer use case with the book. It loads the labels from `timers.json`, follows the helpers of the pool through the sync use case, and removes the label of a timer that ends.

- **Implements:** G25. **Depends on:** 19.5b.
- **Files:** create `src/app/timers.rs`. Modify `src/app/background.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- `Timers` holds the `Book` in a `std::sync::Mutex` and a `Store<Vec<Label>>` for the labels. `start_timers(store, receiver)` loads the labels (no file gives no labels) and starts the task.
- After each snapshot load, `sync` (sub-phase 2.9) sends `TimerEvent::Loaded` with the states of the pool. For each `StateChanged` of a pool helper, it sends `TimerEvent::Changed { old, new }` after it applies the event to the cache. The channel is an unbounded `mpsc`, so the sync never waits.
- The task applies `load` or `observe` (sub-phase 19.3b) under the mutex, in the order of the events. When the labels change, it saves them. A save error logs one `warn` event and keeps the book. Until sub-phase 19.7, a finished label only logs one `info` event.
- The book changes only in the task and in `act` (sub-phase 19.6b), each under the mutex. Thus `free` reads the `seen` state of the task (`RunningTimerLabeled`).
- `run` builds the use case. This sub-phase adds `timers = "timers.json"` to `jarvis.toml`, as `pools`.

**Acceptance criteria.**

- AC-19.6-1: When the store has two labels at the start, the book has them. When the store has no file, the book has no labels.
- AC-19.6-2: When `FakeHome` sends `Reconnected` with `timer.jarvis_1` `idle` and `timer.jarvis_2` `active`, and both have labels, `seen` has both, and the store receives only the label of `timer.jarvis_2`.
- AC-19.6-3: When `FakeHome` sends a finish of `timer.jarvis_2`, the book and the store have no label for it.
- AC-19.6-4: When `FakeHome` sends a state change of `timer.kitchen` or of a light, the timer task gets no event.
- AC-19.6-5: When the store fails, the log has one `warn` event, and the book has the change.

**Proofs.**

- P-19.6-3. Requires: `RunningTimerLabeled` (P-19.1-2). Argument: the task applies each event with `observe` in the order of the stream (P-19.3b-1).
- P-19.6-4. Argument: `sync` sends only the events whose entity ID starts with `TIMER_POOL`.
- P-19.6-1, P-19.6-2, P-19.6-5. Evidence: test.

---

### 19.6b Timer Requests

**Goal:** Act on a `Timer` decision. "Set a timer for 10 minutes" now starts a helper in HA, and "cancel that timer" asks which timer when several run.

- **Implements:** G13, G25. **Depends on:** 19.6.
- **Files:** modify `src/app/timers.rs`, `src/app/conversation.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `README.md` (the timer requests, and the setup step: make the timer helpers `timer.jarvis_1` to `timer.jarvis_5` in HA once, `CLAUDE.md` workflow item 15).

**Model.**

- `handle` gives a `Timer` decision to `Timers::act`. It locks the book, calls `act` (sub-phases 19.5 and 19.5b) with the `satellite_id` of the context, calls `apply`, saves the labels and gives the decision. `handle` dispatches it as each other decision: a `Reply` first and its call after (P-4.8-1), an `Ask` through the dialog (sub-phase 10.4), a `Defer` to System Two. The interaction log records this decision.
- A `Remove` applies before the dispatch, so the label leaves before the `timer.cancel` call starts (`NoRingAfterCancel`).
- When the `timer.start` call of an `Add` fails, the background work removes the label and saves, then announces `failure` (sub-phase 4.9c).
- `FakeHome` keeps the state of each pool helper and sends its `StateChanged` events for `timer.start`, `timer.pause` and `timer.cancel`.

**Acceptance criteria.**

- AC-19.6b-1: When `FakeClassifier` gives `timer_start` with `duration = 5 minutes` and `timer = chicken` for the Kitchen satellite, `handle` returns "The chicken timer is set for 5 minutes.", `FakeHome` records `timer.start` on `timer.jarvis_1` with `duration = 300`, and the store has the label with the satellite.
- AC-19.6b-2: When two timers run and the request is `timer_cancel` with no name, `handle` returns empty text and `FakeHome` records `ask` with "Which timer?". When the user answers "the chicken", `FakeHome` records `timer.cancel` on the chicken helper, and the store has only the pasta label.
- AC-19.6b-3: When `timer_cancel` runs, the store has no label for the helper before `FakeHome` records `timer.cancel`.
- AC-19.6b-4: When `FakeHome` fails the `timer.start` call, the store has no label, and `FakeHome` records an announce from `failure`.

**Proofs.**

- P-19.6b-3. Requires: `NoRingAfterCancel` (P-19.1-2). Argument: `Timers::act` applies the change before it gives the decision, and the call runs only in the `after` work of the dispatch.
- P-19.6b-4. Argument: the `Err` arm of the start call removes the label before it queues the `failure` answer.
- P-19.6b-1, P-19.6b-2. Evidence: test.

---

### 19.6c Duration Answers

**Goal:** Let the answer to "For how long?" start the timer, and ask each dialog question again with its own text. "Set a timer for the chicken", "For how long?", "Half an hour" then starts a chicken timer of 1800 s.

- **Implements:** G13, G25. **Depends on:** 19.6b.
- **Files:** modify `src/app/conversation.rs`.

**Model.**

- An answer to "For how long?" fills the slot `duration` in routing step 2 (sub-phase 10.5) when `seconds` of the answer gives a value. Else the routing continues with step 3. Step 5 asks again from the pool of the missing slot: `ask_area`, `ask_timer` or `ask_duration`, not always `ask_area`. A dialog for `duration` does not count for the early commit of `is_answer` (sub-phase 10.7), because "1 hour" can continue with "and a half".

**Acceptance criteria.**

- AC-19.6c-1: When `FakeClassifier` gives `timer_start` with only `timer = chicken`, `FakeHome` records `ask` with "For how long?". When the user answers "half an hour", `FakeHome` records `timer.start` with `duration = 1800`, and the label has the name "chicken". When the user answers "bananas", `FakeHome` records `ask` with "For how long?" again. An answer to "Which timer?" that fills nothing gives "Which timer?" again.
- AC-19.6c-2: When a `duration` dialog is open, the partial text "1 hour" uses `endpoint_ms`.

**Proofs.**

- P-19.6c-1, P-19.6c-2. Requires: `TurnLimit`. Argument: step 2 fills `duration` only when `seconds` gives a value, step 5 uses `next_turn` (P-10.2-4) with the pool of the missing slot, and `is_answer` skips a `duration` dialog.

---

### 19.7 Timer Ring

**Goal:** Announce the end of a timer on the satellite where you set it, each 15 s for up to 2 minutes, until you say "stop".

- **Implements:** G6, G25. **Depends on:** 19.6c.
- **Files:** modify `src/app/timers.rs`, `src/app/conversation.rs`, `src/lib.rs`.

**Model.**

- `RING_EVERY` (15 s) and `RING_FOR` (120 s) are constants.
- For each finished label with a satellite, the task starts a ring with a copy of the label. Thus a new timer on the same helper does not change the ring. For `k = 0, 1, ...` while `k * RING_EVERY < RING_FOR`, the ring waits until `k * RING_EVERY` after the task observed the finish. Then it puts the text of `timer_done` with `timer` (the name, or "") in the announce queue of the satellite, with the current generation of the satellite (sub-phase 4.9b), and waits until the item ends. Thus a ring has at most 8 announces, and each waits for `idle` and the margin (`AnnounceWhenIdle`). A finished label without a satellite logs one `info` event and does not ring.
- For each `Late` of `load` with a satellite, the task puts one text of `timer_late` with `timer` and `ago` (`spoken(rounded(ago))`) in the announce queue of the satellite. A late ring has no repeats.
- A new request on the satellite drops a queued ring item (sub-phase 4.9c), but the ring continues at the next step.
- `Timers::stop(satellite) -> bool` ends each ring of the satellite and gives true when a ring was active.
- `handle`: before routing step 1 (sub-phase 10.5), when `is_cancel(text)` and `stop` of the satellite gives true, Jarvis replies from `cancelled`. A `Timer` decision `cancel` with no name on a satellite with a ring also stops the ring and replies from `cancelled`, with no call. Thus "stop the timer" ends the ring.

**Acceptance criteria.**

- AC-19.7-1: When the chicken timer of the Kitchen satellite finishes, `FakeHome` records 8 announces of "Your chicken timer is done." on that satellite, each `k * 15` s after the finish event plus `margin_ms`, and none after 120 s.
- AC-19.7-2: When a timer without a name finishes, the announces are "Your timer is done."
- AC-19.7-3: When the user says "stop" on the Kitchen satellite 20 s after the finish, Jarvis replies from `cancelled`, and `FakeHome` records no later ring announce on the Kitchen satellite. The ring of another satellite continues.
- AC-19.7-4: When `timer_cancel` with no name comes during a ring, Jarvis replies from `cancelled`, the ring ends, and `FakeHome` records no `timer.cancel`.
- AC-19.7-5: When a timer is cancelled in the HA UI, `FakeHome` records no ring announce.
- AC-19.7-6: When a new timer from the Office satellite starts on the same helper during the ring, the ring continues on the Kitchen satellite with the chicken text.
- AC-19.7-7: When the store has the chicken label of the Kitchen satellite with `ends` 40 s before the start, and the first snapshot has its helper `idle`, `FakeHome` records one announce "Your chicken timer finished 40 seconds ago." on the Kitchen satellite and no other ring announce, and the store has no label.

**Proofs.**

- P-19.7-1, P-19.7-2. Requires: `RingLimit`, `AnnounceWhenIdle` (P-4.7-2). Argument: the loop runs while `k * RING_EVERY < RING_FOR`, which gives 8 steps, and each item goes through the announce queue.
- P-19.7-3, P-19.7-4. Requires: `NoRingAfterStop`. Argument: `stop` ends the ring before the reply, and the reply increases the generation, so a queued ring item drops (P-4.9c-3).
- P-19.7-5. Requires: `RingOnlyFinished`. Argument: only a label that `observe` gives starts a ring (P-19.3b-1).
- P-19.7-6. Argument: the ring uses its copy of the label, not the book.
- P-19.7-7. Requires: `LateRingOnce`. Argument: `load` removes each label that it gives as `Late` (P-19.3b-1), and the task queues one item for each `Late`.

---

### 19.8 Timer Conformance

**Goal:** Check the timer use case against `timers.qnt`, so the labels, the late events and the rings of the Rust code follow the model.

- **Implements:** G19, G25. **Depends on:** 19.7.
- **Files:** modify `src/app/timers.rs`.
- **Quint:** `timers.qnt` (conformance).

**Model.** The driver maps the model to the use case with `FakeHome`, `FakeClock` and two satellites:

- `start(s)`, `pause(h)`, `resume(h)` and `cancel(h)`: `handle` with the timer intent and the name of the label of `h`. `FakeHome` holds the `StateChanged` events of its helpers until the driver releases them. `finish(h)`: an event from `active` to `idle` with `last_updated = finishes_at`. `outsideCancel(h)`: the same event with `last_updated` before `finishes_at`. `observe`: `FakeHome` releases the first held event, and the driver waits until the task applies it. `ring(t)`: virtual time advances to the next ring step of `t`. `stop(s)`: `handle` with "stop" on `s`. `reconnect`: `FakeHome` drops the held events and sends `Reconnected`.
- After each step, the driver compares the labels, `seen`, the HA states of `FakeHome`, and the ring and late announces of each timer with the model.

**Acceptance criteria.**

- AC-19.8-1: When `quint-connect` replays 100 traces of `timers.qnt`, the labels, the helper states and the ring announces match the model after each step.

**Proofs.**

- P-19.8-1. Evidence: test. With P-19.1-2, it gives the six timer invariants for the Rust code on the replayed traces.

## Field Log

These steps are not tests and gate nothing.

1. In HA, open Settings, Devices and services, Helpers. Make five timer helpers with the names "Jarvis 1" to "Jarvis 5" and turn on "Restore" (the setup step of the README). Record that their entity IDs are `timer.jarvis_1` to `timer.jarvis_5`.
2. Say "Set a timer for 1 minute for the eggs" in the Office. Record that a helper becomes `active` with `finishes_at`, and that the attribute `remaining` of a paused helper has the form `H:MM:SS`.
3. Wait for the end. Record that the Office satellite says "Your eggs timer is done." each 15 s, and that "Hey Jarvis, stop" ends it. Record the time from the end to the first announce.
4. Set two timers. Ask "How long is left on the timer?" and "Cancel that timer". Record the list and the question "Which timer?".
5. Cancel a timer in the HA UI. Record that no satellite announces it. Say "Set a timer for the tea" and answer "a minute and a half". Stop Jarvis before the end and start it after. Record "Your tea timer finished ... ago."
