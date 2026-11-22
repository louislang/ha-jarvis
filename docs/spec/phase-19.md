# Phase 19: Voice Timers

This phase adds voice timers. You can set a generic timer ("set a timer for 10 minutes") or a named timer ("set a timer for 5 minutes for the chicken"), ask for the time left, and cancel, pause and resume a timer by voice. The timers run in HA timer helpers that you create once (`01-architecture.md` decision 36). Jarvis keeps the name and the satellite of each timer in `jarvis.db`. When a timer ends, the satellite that set it says "Your chicken timer is done" every 15 s for up to 2 minutes, or until the user says "stop". The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 16.

You chose these rules (2026-11-08, 2026-11-17 and 2026-11-22):

- The requests are "set a timer for 10 minutes", "set a timer for 5 minutes for the chicken", "how long do we have left on the chicken timer?" and "how long is left on the timer?". "Set a timer" with no duration asks "For how long?". A duration has numbers and units only, with no "half".
- The timers are HA timer helpers. You create a fixed set once, for example `timer.jarvis_1` to `timer.jarvis_5`. Jarvis uses the free helpers and keeps the names itself. Jarvis needs no admin rights and creates no helper. On 2026-11-22, HA had no timer entity, so the README gets a setup step for the helpers.
- Jarvis saves its timer table in `jarvis.db` and loads it at the start, so a timer still rings after a restart of Jarvis.
- The satellite that set the timer announces its end. It says "Your chicken timer is done" ("Your timer is done" for a generic timer) every 15 s for up to 2 minutes, until the user says "stop" on that satellite, or "cancel the chicken timer". Other requests on that satellite run normally and do not stop the ring. "Stop" on a ringing satellite commits early.
- "Cancel the chicken timer" cancels by name. "Cancel that timer" cancels the timer when only one runs. "Cancel the most recent timer" cancels the newest. When several run, "cancel that timer" asks which timer and names them ("Which timer, the chicken timer or the pizza timer?"), with a dialog question of phase 10. "Pause the chicken timer" and "resume the chicken timer" work the same way.
- "How long is left on the timer?" with several timers lists them all. This phase does not add or subtract time.
- A later phase (stop and mute) adds a general "stop" and "never mind". This phase uses "stop" only for a ringing timer (sub-phase 19.10).

Each sub-phase that adds an intent adds its seed examples to `intents.toml` and its pool to `responses.toml`, as in phase 18 (the preface of `phase-18.md`). The new intents go at the end of `intents.toml`, so the contexts of the existing examples do not change.

---

### 19.1 Timer Quint Model

**Goal:** Add `quint/timers.qnt`, the model of the timer table, the HA timer helpers and the rings. Prove that a timer starts only on a free helper, that two running timers never have the same name, and that a ring is bounded and stops at "stop", before the Rust code exists.

- **Implements:** G19, G25. **Depends on:** 16.4b.
- **Files:** create `quint/timers.qnt`.
- **Quint:** `timers.qnt`.

**Model.**

- Constants: `HELPERS = List("h1", "h2")` (a list, so that `set` takes the first free helper, as the Rust code takes the lowest entity ID), `SATELLITES = Set("s1", "s2")`, `NAMES = Set("", "chicken")` (`""` is a generic timer), `RINGS = 8`.
- State: each variable is a map with a fixed domain or a number.
  - `ha: HELPER -> str`: the HA state of the helper, `"idle"`, `"active"` or `"paused"`.
  - `timer: HELPER -> { owned: bool, sat: str, name: str, order: int }`: the entry of the Jarvis table for the helper. `owned = false` is no entry.
  - `starting: HELPER -> bool`: the start call of the entry runs. `nextOrder: int`: the order of the next timer.
  - `ring: HELPER -> { sat: str, name: str, left: int, rung: int, stopped: bool }`: the ring of the last timer that finished on the helper. `left` is the number of rings that are left.
  - History: `lastAction` (`Set`, `Ring` or `Other`), `lastHelper`, `lastWasFree` (the helper of a `Set` was free before the step). `lastAction` is `Set` only for a `set` that adds an entry, and `Ring` only for `ringOnce`.
- Actions:
  - `set(s, n)`: a request on `s` sets a timer with the name `n`. When `n` is not `""` and an entry has the name `n`, nothing changes (the reply `timer_taken`). Else, when a helper is free (`ha = "idle"` and no entry), the first free helper `h` gets the entry `{ owned: true, sat: s, name: n, order: nextOrder }`, `starting[h] := true` and `nextOrder += 1`. Else nothing changes (the reply `timer_full`).
  - `started(h)` and `startFailed(h)` (when `starting[h]`): the start call succeeds or fails. `starting[h] := false`. Success sets `ha[h] := "active"`. Failure removes the entry of `h`.
  - `cancel(h)` (when `h` has an entry, does not start, and `ha[h]` is `"active"` or `"paused"`): Jarvis cancels the timer. The entry goes, and `ha[h] := "idle"`.
  - `pause(h)` and `resume(h)` (when `h` has an entry): `"active"` to `"paused"`, and `"paused"` to `"active"`. `outside(h)` (when `h` has no entry and `ha[h] = "idle"`): a person or an automation starts the helper in HA, `ha[h] := "active"`.
  - `finish(h)` (when `ha[h] = "active"`): HA ends the timer and sends `timer.finished`. `ha[h] := "idle"`. When `h` has an entry, `ring[h] := { sat: timer[h].sat, name: timer[h].name, left: RINGS, rung: 0, stopped: false }`, and the entry goes.
  - `haCancel(h)` (when `ha[h]` is `"active"` or `"paused"`): a person cancels the timer in HA, and HA sends `timer.cancelled`. `ha[h] := "idle"`, and the entry goes.
  - `quietEnd(h)` (when `ha[h]` is `"active"` or `"paused"`): the timer ends or is cancelled while Jarvis is down or not connected, so Jarvis gets no event. `ha[h] := "idle"`, and the entry stays.
  - `reconnect`: Jarvis connects to HA again. Each entry whose helper is `"idle"` goes, with `starting := false`. `restart`: Jarvis starts again with its saved table. Each ring gets `left := 0`, then the effect of `reconnect`.
  - `ringOnce(h)` (when `ring[h].left > 0`): one ring on `ring[h].sat`. `left -= 1`, `rung += 1`.
  - `stop(s)`: the user says "stop" on `s`. Each ring with `sat = s` gets `left := 0` and `stopped := true`. `silence(h)` (when `ring[h].left > 0` and `ring[h].name` is not `""`): a request cancels the ringing timer by name. `ring[h]` gets `left := 0` and `stopped := true`.
- A ring is not an entry. Thus a helper can run a new timer while the ring of its last timer continues. `timers.qnt` is not in `composed.qnt` (sub-phase 16.3). A timer request is an ordinary request of the lifecycle, and a ring is an ordinary item of the announce queue, which `announce.qnt` covers. `timers.qnt` covers the timer table and the rings.
- Invariants:
  - `UniqueNames`: no two entries have the same name, except `""`.
  - `StartOnFreeHelper`: when `lastAction = Set`, `lastWasFree` is true.
  - `RingsBounded`: each ring has `rung <= RINGS`.
  - `NoRingAfterStop`: when `lastAction = Ring`, the ring of `lastHelper` is not `stopped`.

**Acceptance criteria.**

- AC-19.1-1 [Quint]: `quint typecheck` passes.
- AC-19.1-2 [Quint]: The four invariants hold in simulation and to 10 steps in `quint verify`.
- AC-19.1-3 [Quint]: The test `ac_19_1_3_full_and_taken` passes: with two entries, `set` changes nothing. With an entry "chicken", `set(s, "chicken")` changes nothing.
- AC-19.1-4 [Quint]: The test `ac_19_1_4_cancel_never_rings` passes: after `cancel(h)`, `haCancel(h)`, or `quietEnd(h)` and `reconnect`, `ring[h]` does not change and `h` has no entry.
- AC-19.1-5 [Quint]: The test `ac_19_1_5_ring_on_setting_satellite` passes: a timer set on `s2` that finishes gets a ring with `sat = "s2"`. After `stop("s1")`, the ring continues. After `stop("s2")`, `ringOnce(h)` is not enabled.
- AC-19.1-6 [Quint]: The test `ac_19_1_6_reuse_while_ringing` passes: after `finish(h)`, `set` can take `h` again, and the ring of `h` keeps its `left`.
- AC-19.1-7 [Quint]: The test `ac_19_1_7_restart_keeps_running_timers` passes: after `restart`, an entry whose helper is `"active"` stays, and its `finish` starts a ring.

**Proofs.**

- P-19.1-2 (AC-19.1-2). `UniqueNames` by induction: only `set` adds an entry, and it adds a name that is not `""` only when no entry has that name. `StartOnFreeHelper`: `set` adds an entry only on a helper with `ha = "idle"` and no entry, and it records that fact in `lastWasFree`. `RingsBounded`: `finish` sets `rung := 0` and `left := RINGS`, and `ringOnce` moves one from `left` to `rung` only when `left > 0`. Thus `rung + left <= RINGS` after each step. `NoRingAfterStop`: `stop` and `silence` set `stopped` and `left := 0` in one step, and only `finish` sets `left > 0`, with `stopped := false`. `ringOnce` needs `left > 0`.
- P-19.1-1, P-19.1-3 to P-19.1-7. Evidence: model check.

---

### 19.2 Timer Durations

**Goal:** Add the pure functions that read a spoken duration and speak a number of seconds. "Ten minutes" then becomes 600 s, and 252 s becomes "4 minutes and 12 seconds".

- **Implements:** G25. **Depends on:** 19.1.
- **Files:** create `src/domain/timers.rs`. Modify `src/lib.rs`.

**Model.**

- `duration(text) -> Option<u32>`: the seconds of the duration slot `text`.
  1. `text2num` `text2digits` with `Language::english()` changes the number words of `normalize(text)` to digits (sub-phase 18.9). "one hour and twenty minutes" becomes "1 hour and 20 minutes". Digits stay.
  2. The words are pairs of a number (digits, "a" or "an") and a unit ("second", "minute" or "hour", also with "s"). "and" can come between two pairs.
  3. The result is the sum of the pairs, from 1 s to 86 400 s (24 hours). Any other word, a number without a unit, or a sum outside this range gives `None`. Thus "half an hour" gives `None`.
- `spoken(seconds) -> String`: the time in hours, minutes and seconds, with digits. A part that is 0 is left out. The seconds are left out at 1 hour or more. One part stands alone, and two parts have "and" between them. A part of 1 is singular ("1 minute"). 0 s gives "0 seconds". The TTS reads digits, as in "It is 21 degrees" (AC-4.6-1).

**Acceptance criteria.**

- AC-19.2-1: `duration` gives 600 for "ten minutes" and "10 minutes", 300 for "five minutes", 3600 for "an hour", 4800 for "one hour and twenty minutes", 30 for "thirty seconds" and 90 for "1 minute and 30 seconds".
- AC-19.2-2: `duration` gives `None` for "zero minutes", "twenty five hours", "ten", "minutes", "ten bananas", "half an hour" and "".
- AC-19.2-3: `spoken` gives "45 seconds" for 45, "1 minute and 1 second" for 61, "4 minutes and 12 seconds" for 252, "1 hour" for 3600, "1 hour and 5 minutes" for 3900, "1 hour and 1 minute" for 3661 and "0 seconds" for 0.

**Proofs.**

- P-19.2-2 (AC-19.2-2). Ensures: `duration(text)` is `None`, or `Some(s)` with `1 <= s <= 86_400`. Argument: the sum uses checked additions and multiplications, and the last step compares the sum with both bounds. A word that is not a number, a unit or "and" returns `None` from the loop.
- P-19.2-1. Evidence: test. The digits come from `text2num`, which Verus cannot see.
- P-19.2-3. Evidence: test. The text comes from `format!`, which Verus cannot see. Verus proves that the divisions and remainders do not overflow.

---

### 19.3 Time Left

**Goal:** Read the time left of one HA timer helper from the hot cache. A running timer then gives the seconds to its end, and a paused timer gives its remaining time.

- **Implements:** G25. **Depends on:** 19.2.
- **Files:** modify `src/domain/timers.rs`.

**Model.**

- An HA timer helper has the state `idle`, `active` or `paused`. An `active` helper has the attribute `finishes_at`, an RFC 3339 time. A `paused` helper has the attribute `remaining`, the text `H:MM:SS`. The `remaining` of an `active` helper is the value at its last start, so `left` does not read it. `left(state, now) -> Option<u32>`: for `active`, the seconds from `now` to `finishes_at` (`jiff` `Timestamp`), rounded up, and 0 when `finishes_at` is before `now`. For `paused`, `H * 3600 + MM * 60 + SS` of `remaining`, with `MM` and `SS` less than 60. Another state, a missing attribute or a value that does not parse gives `None`.

**Acceptance criteria.**

- AC-19.3-1: When the state is `active` with `finishes_at` 252 s after `now`, `left` gives 252. With 251.2 s, it gives 252. With a `finishes_at` before `now`, it gives 0.
- AC-19.3-2: When the state is `paused` with `remaining = "0:04:12"`, `left` gives 252. With `"1:00:00"`, it gives 3600.
- AC-19.3-3: When the state is `idle`, `left` gives `None`. When the state is `active` without `finishes_at`, or `paused` with `remaining = "0:61:00"`, `left` gives `None`.

**Proofs.**

- P-19.3-2, P-19.3-3 (AC-19.3-2, AC-19.3-3). Ensures: `left` gives `None` when the state is not `active` or `paused`. For `paused`, `Some(r)` implies that `remaining` has three parts `h`, `m`, `s` with `m < 60`, `s < 60` and `r = h * 3600 + m * 60 + s`. Argument: `left` matches the state with two arms. The `paused` arm splits `remaining` at `:`, parses three numbers with checked arithmetic, and compares `m` and `s` with 60.
- P-19.3-1. Evidence: test. The time comes from the `jiff` parse of `finishes_at`, which a contract states only as documented.

---

### 19.4 Timer Table

**Goal:** Add the timer table and the pure `plan` of a timer request. Jarvis then knows which helper a new timer takes, which timer a request means, and when it must ask a question.

- **Implements:** G25. **Depends on:** 19.3.
- **Files:** modify `src/domain/timers.rs`.
- **Quint:** `timers.qnt` (the rules of `set`, `cancel`, `pause`, `resume` and `reconnect`).

**Model.**

- `TimerAction`: `Set`, `Cancel`, `CancelLast`, `Pause`, `Resume`, `Left`. In `intents.toml` they are `set`, `cancel`, `cancel_last`, `pause`, `resume` and `left`.
- `TimerRequest { intent, action, timer: Option<String>, duration: Option<String>, transcript }`: `timer` is the `timer` slot (the name), and `duration` is the `duration` slot.
- `Timer { satellite: Option<String>, name: Option<String>, duration: String, order: u64 }`. `duration` is the duration slot of the set request, for example "ten minutes". The label of a timer is its name, else its `duration`. `Timers { timers: BTreeMap<helper entity ID, Timer>, next: u64 }`: the timer table.
- The helpers are the entities of the hot cache whose entity ID starts with `timer.jarvis_`, in entity ID order. A helper is free when the hot cache shows it `idle` and the table has no entry for it. Thus a helper that a person starts in HA is not free.
- The candidates of an action are the entries whose helper has these HA states: `active` or `paused` for `Cancel`, `CancelLast` and `Left`, `active` for `Pause`, and `paused` for `Resume`. An entry whose start call runs has an `idle` helper, so it is never a candidate.
- `plan(request, timers, cache, now, satellite, similarity) -> (Plan, Timers)`:
  - `Set`: no duration slot gives `NoDuration`. A duration that `duration` (sub-phase 19.2) does not read gives `Unclear`. The name is not empty and an entry has the same normalized name: `Taken`. No helper is free: `Full`. Else `Start { helper, seconds }` on the first free helper, and the table gets the entry `{ satellite, name, duration, order: next }` and `next + 1`.
  - `CancelLast`: the candidate with the highest `order`, else `Missing`. `Cancel`, `Pause`, `Resume`: with a name, `dialog::fill(name, labels, similarity)` (sub-phase 10.3) gives a label of a candidate, and the result is the candidate with that label and the highest `order`, else `Missing`. Without a name: no candidate gives `Missing`, one gives that candidate, and two or more give `Several { timers }` with copies of the candidates in `order`.
  - A chosen candidate gives `Act { helper, service, timer }` with a copy of its entry: `timer.cancel` for `Cancel` and `CancelLast`, `timer.pause` for `Pause`, and `timer.start` for `Resume` (`timer.start` resumes a paused helper). `Cancel` and `CancelLast` remove the entry from the table.
  - `Left`: the candidates, or with a name only the candidate that `fill` gives. None gives `Missing`. Else `Left { timers }` in `order`, each with its name, its `duration` and `left` (sub-phase 19.3). A candidate whose `left` is `None` is not in the list.
- `forget(timers, helper, cache)` removes the entry of `helper` only when the hot cache shows the helper `idle` (a failed start call). `ended(timers, helper) -> (Option<Timer>, Timers)` removes the entry of `helper` and gives it (the HA events). `reconcile(timers, cache)` removes each entry whose helper the hot cache shows `idle` (a reconnect, sub-phase 19.9).
- The timers of all satellites are one table. Thus a request on any satellite can cancel, pause, resume or ask about any timer. Only the ring uses the satellite of a timer.

**Acceptance criteria.**

- AC-19.4-1: When `timer.jarvis_1` and `timer.jarvis_2` are `idle` and the table is empty, `Set` with `duration = "five minutes"` and `timer = "chicken"` from the Kitchen satellite gives `Start { timer.jarvis_1, 300 }`, and the table has the entry with the Kitchen satellite, the name "chicken" and the order 0. A second `Set` with no name gives `Start` on `timer.jarvis_2` with the order 1.
- AC-19.4-2: When the table has a "chicken" entry, `Set` with `timer = "Chicken"` gives `Taken`, and the table does not change.
- AC-19.4-3: When each helper has an entry or is `active` in HA, `Set` gives `Full`. The entity `timer.kitchen` is never a helper.
- AC-19.4-4: When the duration slot is "ten bananas", `Set` gives `Unclear`. With no duration slot, it gives `NoDuration`.
- AC-19.4-5: When one timer is `active`, `Cancel` with no name gives `Act` with `timer.cancel` on its helper, and the table has no entry for it. When "chicken" and a generic "ten minutes" timer are `active`, `Cancel` with no name gives `Several` with both timers in order, and the table does not change.
- AC-19.4-6: `Cancel` with `timer = "the chicken"` gives `Act` on the helper of "chicken". With `timer = "pizza"`, it gives `Missing`.
- AC-19.4-7: With two `active` timers, `CancelLast` gives `Act` on the helper of the timer with the higher order.
- AC-19.4-8: When "chicken" is `active` and "pasta" is `paused`, `Pause` with no name gives `Act` with `timer.pause` on "chicken", and `Resume` with no name gives `Act` with `timer.start` on "pasta".
- AC-19.4-9: When "chicken" is `active` with `finishes_at` 252 s after `now` and a generic "ten minutes" timer is `paused` with `remaining = "0:10:00"`, `Left` with no name gives both, in order, with the left times 252 and 600. With `timer = "chicken"`, it gives only "chicken". With no candidate, it gives `Missing`.
- AC-19.4-10: `forget` removes an entry whose helper is `idle` and keeps an entry whose helper is `paused`. `ended` removes the entry and gives it. `reconcile` removes the entries whose helper is `idle` and keeps the entries whose helper is `active` or `paused`.

**Proofs.**

- P-19.4-1, P-19.4-3 (AC-19.4-1, AC-19.4-3). Ensures: `Start { helper, .. }` implies that `helper` starts with `timer.jarvis_`, the hot cache shows it `idle`, the old table has no entry for it, and the new table is the old table with one new entry for it (`StartOnFreeHelper`). `Full` implies that no helper is free, and the table does not change. Argument: `free` keeps only the helpers with both conditions, and `Set` inserts on the first of them. The loop invariant of `free` states that no helper before `index` is free.
- P-19.4-2 (AC-19.4-2). Requires: no two entries of the table have the same name, except none. Ensures: no two entries of the new table have the same name, except none (`UniqueNames`). Argument: only `Set` adds an entry, and it returns `Taken` first when an entry has the same normalized name.
- P-19.4-5 to P-19.4-8 (AC-19.4-5 to AC-19.4-8). Ensures: `Act { helper, .. }` implies that `helper` is a candidate of the action. `Several { timers }` implies two or more candidates, and the table does not change. Only `Cancel` and `CancelLast` remove an entry, and only the entry of `helper`. Argument: `choose` draws only from the candidates of the action. The arms of `plan` remove an entry only for `Act` of `Cancel` and `CancelLast`. `CancelLast` takes the maximum `order`.
- P-19.4-10 (AC-19.4-10). Ensures: `forget` removes the entry of `helper` if and only if the hot cache shows `helper` `idle`. `ended` removes only the entry of `helper`. `reconcile` keeps an entry if and only if the hot cache does not show its helper `idle`. Argument: `forget` and `ended` are one map remove on one key. `reconcile` is a filter over the entries.
- P-19.4-4, P-19.4-9. Evidence: test. The seconds and the left times come from P-19.2-1 and P-19.3-1, and the name match from `strsim` in `fill`.

---

### 19.5 Timer Intents

**Goal:** Add the intent kind `timer`, the `Timer` decision and the six timer intents with their seed examples. System One then classifies the timer requests and gives their slots.

- **Implements:** G3, G25. **Depends on:** 19.4.
- **Files:** modify `src/domain/catalog.rs`, `src/domain/decide.rs`, `src/domain/responses.rs`, `src/app/conversation.rs`, `intents.toml`, `responses.toml`, `src/adapters/onnx.rs` (tests only).

**Model.**

- `Kind::Timer` (`kind = "timer"`), and the intent field `timer: Option<TimerAction>`. `validate_catalog` checks that a `timer` intent has `timer` and no `service`, and that another kind has no `timer`. The error names the intent.
- `decide`, step 2 of sub-phase 4.5: a confident `timer` intent reads its slots as a command does (sub-phase 4.5d). The result is `Decision::Timer(TimerRequest)` with the intent name, the action, the `timer` and `duration` slots (an empty optional slot is `None`) and the transcript. A `timer` intent needs no area.
- `pool_variables` (sub-phase 4.2): the pool of a `timer` intent can use `timer`, `duration` and `timers`, and no other variable. Sub-phase 19.6 renders the pools with these variables.
- The six intents, at the end of `intents.toml`. Each has `kind = "timer"` and `optional = ["timer", "duration"]`, so a missing duration gives the question of sub-phase 19.6, not System Two. The second pass writes numbers as words (sub-phase 18.9), so the durations of the examples are words.
  - `timer_set` (`set`): "set a timer for [ten minutes](duration)", "set a timer for [five minutes](duration) for the [chicken](timer)", "set a [pasta](timer) timer for [eight minutes](duration)", "start a timer for [thirty seconds](duration)", "timer for [an hour](duration)", "set a [laundry](timer) timer for [one hour and twenty minutes](duration)", "set a timer".
  - `timer_cancel` (`cancel`): "cancel that timer", "cancel the timer", "cancel the [chicken](timer) timer", "stop the [pasta](timer) timer", "delete my [laundry](timer) timer". `timer_cancel_last` (`cancel_last`): "cancel the most recent timer", "cancel the last timer", "cancel the newest timer".
  - `timer_pause` (`pause`): "pause the timer", "pause the [chicken](timer) timer", "pause my [pasta](timer) timer", "hold the timer". `timer_resume` (`resume`): "resume the timer", "resume the [chicken](timer) timer", "continue the [pasta](timer) timer", "unpause the timer".
  - `timer_left` (`left`): "how long is left on the timer", "how long do we have left on the [chicken](timer) timer", "how much time is left on the [pasta](timer) timer", "how much time is left", "how long until the timer goes off".
- `responses.toml` gets one pool for each intent. `timer_set`: `{% if timer %}Your {{ timer }} timer is set for {{ duration }}.{% else %}Timer set for {{ duration }}.{% endif %}`
  - `timer_cancel` and `timer_cancel_last`: `{% if timer %}I cancelled the {{ timer }} timer.{% else %}I cancelled the timer for {{ duration }}.{% endif %}`
  - `timer_pause` and `timer_resume`: the template of `timer_cancel` with "Pausing the" and "Resuming the" in place of "I cancelled the".
  - `timer_left`: `{% for t in timers %}{% if t.timer %}The {{ t.timer }} timer{% else %}The timer for {{ t.duration }}{% endif %} has {{ t.left }} left.{{ " " if not loop.last else "" }}{% endfor %}`
- Until sub-phase 19.9, `Conversation::act` gives a `Timer` decision the `Defer` with `Job::Llm` of step 1. `just fixture-model` trains the fixture model again. The tests of AC-3.2-1 and AC-3.2c-2 get the six names. New seeds can lower the probability of other intents (sub-phase 18.19f). Before the merge, the PR measures the light commands of AC-9.2-5 with the new fixture model. When one is less than `model.commit_threshold`, the PR changes the timer examples, not the tests.

**Acceptance criteria.**

- AC-19.5-1: When `intents.toml` and `responses.toml` are read, `validate_catalog` and `validate_pools` return `Ok`.
- AC-19.5-2: When a `timer` intent has no `timer`, or a `command` intent has `timer`, `validate_catalog` returns an error that names the intent.
- AC-19.5-3: When the prediction is `timer_set` 0.95 with the slots `duration = five minutes` and `timer = chicken`, `decide` gives `Decision::Timer` with `timer_set`, `Set`, `Some("chicken")`, `Some("five minutes")` and the transcript, also with no area.
- AC-19.5-4: When the prediction is `timer_set` 0.95 with no slot, `decide` gives `Decision::Timer` with `timer = None` and `duration = None`.
- AC-19.5-5: When the pool of a `timer` intent uses `area`, `validate_pools` returns an error with the pool and the variable.
- AC-19.5-6: When the fixture model gets each trained example of the six timer intents with the context `area: none`, the top label is the intent of the example, and the slots are those of its markup.
- AC-19.5-7: When the fixture model gets "turn on the office lights", "office lights on" and "turn the office lights on" with the context `area: none`, the top label is `lights_on` with a probability of `model.commit_threshold` or more.

**Proofs.**

- P-19.5-3, P-19.5-4 (AC-19.5-3, AC-19.5-4). Ensures: `Decision::Timer(r)` comes from the top intent, which is a `timer` intent with a probability of `threshold` or more, and `r.action` is its `timer`. Argument: step 1 returns `Defer` for every other top label. The `timer` arm of step 2 reads the optional slots (P-4.5d-3) and does not read the area.
- P-19.5-2 (AC-19.5-2). Ensures: when `validate_catalog` returns `Ok`, each `timer` intent has `timer` and no `service`, and no other intent has `timer`. Argument: one check for each intent in the loop of `validate_catalog`.
- P-19.5-1, P-19.5-5. Evidence: test. The template checks use `minijinja` (P-4.2-6).
- P-19.5-6, P-19.5-7. Evidence: test. The probabilities come from the model file (`02-rules.md` section 5, rule 5).

---

### 19.6 Timer Answers

**Goal:** Turn the plan of a timer request into a decision: a reply with an HA call, a reply with no call, a question, or System Two. "Set a timer for five minutes for the chicken" then gives "Your chicken timer is set for five minutes." and a `timer.start` call.

- **Implements:** G13, G25. **Depends on:** 19.5.
- **Files:** modify `src/domain/timers.rs`, `src/domain/responses.rs`, `responses.toml`.

**Model.**

- `answer(plan, request, pools, rng) -> Decision`. The render context has `timer` (the name of the entry of `Act`, else of the request, else an empty string), `duration` (the `duration` of the entry of `Act`, else of the request, else an empty string) and `timers` (for `Left` and `Several`, each with `timer`, `duration` and, for `Left`, `left`).
  - `Start { helper, seconds }`: `Reply` from the pool of the intent, with the call `timer.start` on `helper` and the data `{ duration: seconds }` as a number. HA reads a number of seconds as a duration.
  - `Act { helper, service, timer }`: `Reply` from the pool of the intent, with the call `service` on `helper` and no data.
  - `Left { timers }`: `Reply` from the pool of the intent. `left` is `spoken` (sub-phase 19.2). No call.
  - `Taken`: `timer_taken`. `Full`: `timer_full`. `Missing`: `timer_missing`. No call. `Unclear`: `Defer` with a filler and `Job::Llm` with the transcript. System Two cannot set a timer, so it says so (sub-phase 10.6b).
  - `Several { timers }`: `Ask` from `ask_timer` with `Pending::Slot { intent, slots: [], missing: "timer", choices, transcript }`. The choices are the labels of `timers`. Routing step 2 of sub-phase 10.5 fills it as it fills the area: the answer "the chicken" adds the slot `timer = chicken`, and `decide` runs again.
  - `NoDuration`: `Ask` from `ask_duration` with `Pending::Slot { intent, slots, missing: "duration", choices: [], transcript }`. `slots` has the `timer` slot of the request, if any. Sub-phase 19.9 fills it.
  - A pool that does not render gives `Defer` with `Job::Llm`, as in `decide`.
- The answer pools, which `validate_pools` requires, as it requires `ask_area`: `ask_timer` (`Which timer, {% for t in timers %}the {% if t.timer %}{{ t.timer }} timer{% else %}timer for {{ t.duration }}{% endif %}{{ " or " if loop.revindex == 2 else (", " if not loop.last else "") }}{% endfor %}?`), `ask_duration` (`For how long?`), `timer_full` (`All timers are in use.`), `timer_taken` (`There is already a {{ timer }} timer.`), `timer_missing` (`{% if timer %}There is no {{ timer }} timer.{% else %}No timer is running.{% endif %}`) and `timer_done` (`{% if timer %}Your {{ timer }} timer is done.{% else %}Your timer is done.{% endif %}`, the ring of sub-phase 19.10). They can use `timer`, `duration` and `timers`, and no other variable (`01-architecture.md` section 9).

**Acceptance criteria.**

- AC-19.6-1: When the plan is `Start { timer.jarvis_1, 300 }` for `timer_set` with `timer = chicken` and `duration = five minutes`, the result is `Reply` with "Your chicken timer is set for five minutes." and the call `timer.start` on `timer.jarvis_1` with the data `{ duration: 300 }` as a number. With no name and "ten minutes", the reply is "Timer set for ten minutes."
- AC-19.6-2: When the plan is `Act` with `timer.cancel` on the generic "ten minutes" timer for `timer_cancel`, the result is `Reply` with "I cancelled the timer for ten minutes." and the call `timer.cancel` with no data.
- AC-19.6-3: When the plan is `Taken`, `Full` or `Missing`, the result is `Reply` with no call: "There is already a chicken timer.", "All timers are in use.", and "There is no pizza timer." or, with no name, "No timer is running."
- AC-19.6-4: When the plan is `Several` with "chicken" and "pizza", the result is `Ask` with "Which timer, the chicken timer or the pizza timer?" and `Pending::Slot` with `missing = "timer"`, the choices "chicken" and "pizza" and the transcript of the request. With "chicken", "pizza" and a generic "ten minutes" timer, the question is "Which timer, the chicken timer, the pizza timer or the timer for ten minutes?"
- AC-19.6-5: When the plan is `Left` with "chicken" (252 s) and a generic "ten minutes" timer (600 s), the reply is "The chicken timer has 4 minutes and 12 seconds left. The timer for ten minutes has 10 minutes left."
- AC-19.6-6: When the plan is `Unclear`, the result is `Defer` with `Job::Llm` and the transcript. When it is `NoDuration` with `timer = chicken`, the result is `Ask` with "For how long?" and `Pending::Slot` with `missing = "duration"`, no choices and the slot `timer = chicken`.
- AC-19.6-7: When `ask_timer`, `ask_duration`, `timer_full`, `timer_taken`, `timer_missing` or `timer_done` is missing, `validate_pools` returns an error that names it. When `timer_done` uses `area`, it returns an error with the pool and the variable.

**Proofs.**

- P-19.6-1, P-19.6-2 (AC-19.6-1, AC-19.6-2). Ensures: a `Reply` with a call comes only from `Start` or `Act`. Its target is the helper of the plan, its domain is `timer`, and its service is `start` for `Start`. Argument: `answer` builds a call only in these two arms, from the helper of the plan.
- P-19.6-3, P-19.6-6 (AC-19.6-3, AC-19.6-6). Ensures: `Taken`, `Full`, `Missing` and `Left` give `Reply` with `call = None` or `Defer`. `Unclear` gives `Defer` with `Job::Llm`. `NoDuration` gives `Ask` with `missing = "duration"` and no choices. Argument: these arms build `Reply { call: None }`, `Ask` or `Defer`.
- P-19.6-4 (AC-19.6-4). Ensures: `Several { timers }` gives `Ask` with `Pending::Slot` whose `missing` is `timer` and whose `choices` are the labels of `timers`, in order. Argument: the arm maps `timers` to their labels.
- P-19.6-7 (AC-19.6-7). Ensures: when `validate_pools` returns `Ok`, the six answer pools exist. Argument: `validate_pools` calls `require` for each, after `training_busy`.
- P-19.6-5. Evidence: test. The text comes from `minijinja` and P-19.2-3.
