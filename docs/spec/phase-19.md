# Phase 19: Voice Timers

This phase adds voice timers. You can set a generic timer ("set a timer for 10 minutes") or a named timer ("set a timer for 5 minutes for the chicken"), ask for the time left, and cancel, pause and resume a timer by voice. The timers run in HA timer helpers that you create once (`01-architecture.md` decision 36). Jarvis keeps the name and the satellite of each timer in `jarvis.db`. When a timer ends, the satellite that set it says "Your chicken timer is done" every 15 s for up to 2 minutes, or until the user says "stop". The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 16.

You chose these rules (2026-11-08, 2026-11-17 and 2026-11-22):

- The requests are "set a timer for 10 minutes", "set a timer for 5 minutes for the chicken", "how long do we have left on the chicken timer?" and "how long is left on the timer?". "Set a timer" with no duration asks "For how long?". A duration has numbers and units only, with no "half".
- The timers are HA timer helpers. You create a fixed set once, for example `timer.jarvis_1` to `timer.jarvis_5`. Jarvis uses the free helpers and keeps the names itself. Jarvis needs no admin rights and creates no helper. On 2026-11-22, HA had no timer entity, so the README gets a setup step for the helpers.
- Jarvis saves its timer table in `jarvis.db` and loads it at the start, so a timer still rings after a restart of Jarvis.
- The satellite that set the timer announces its end. It says "Your chicken timer is done" ("Your timer is done" for a generic timer) every 15 s for up to 2 minutes, until the user says "stop" on that satellite, or "cancel the chicken timer". Other requests on that satellite run normally and do not stop the ring. "Stop" on a ringing satellite commits early.
- "Cancel the chicken timer" cancels by name. "Cancel that timer" cancels the timer when only one runs. "Cancel the most recent timer" cancels the newest. When several run, "cancel that timer" asks which timer and names them ("Which timer, the chicken timer or the pizza timer?"), with a dialog question of phase 10. "Pause the chicken timer" and "resume the chicken timer" work the same way.
- "How long is left on the timer?" with several timers lists them all. This phase does not add or subtract time.
- A later phase (stop and mute) adds a general "stop" and "never mind". This phase uses "stop" only for a ringing timer (sub-phase 19.10b).

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

**Goal:** Add the timer table and the plan of a request that sets a timer. Jarvis then knows which helper a new timer takes, and when a name is taken or all helpers are in use. Sub-phases 19.4b and 19.4c add the other requests.

- **Implements:** G25. **Depends on:** 19.3.
- **Files:** modify `src/domain/timers.rs`.
- **Quint:** `timers.qnt` (the rules of `set`).

**Model.**

- `TimerAction`: `Set`, `Cancel`, `CancelLast`, `Pause`, `Resume`, `Left`. In `intents.toml` they are `set`, `cancel`, `cancel_last`, `pause`, `resume` and `left`.
- `TimerRequest { intent, action, timer: Option<String>, duration: Option<String>, transcript }`: `timer` is the `timer` slot (the name), and `duration` is the `duration` slot.
- `Timer { satellite: Option<String>, name: Option<String>, duration: String, order: u64 }`. `duration` is the duration slot of the set request, for example "ten minutes". The label of a timer is its name, else its `duration`. `Timers { timers: BTreeMap<helper entity ID, Timer>, next: u64 }`: the timer table.
- The helpers are the entities of the hot cache whose entity ID starts with `timer.jarvis_`, in entity ID order. A helper is free when the hot cache shows it `idle` and the table has no entry for it. Thus a helper that a person starts in HA is not free.
- `Plan`: `Start { helper, seconds }`, `NoDuration`, `Unclear`, `Taken` and `Full`. Sub-phase 19.4c adds the other variants.
- `set(request, timers, cache, satellite) -> (Plan, Timers)`: the plan of a `Set` request. No duration slot gives `NoDuration`. A duration that `duration` (sub-phase 19.2) does not read gives `Unclear`. The name is not empty and an entry has the same normalized name: `Taken`. No helper is free: `Full`. Else `Start { helper, seconds }` on the first free helper, and the table gets the entry `{ satellite, name, duration, order: next }` and `next + 1`.

**Acceptance criteria.**

- AC-19.4-1: When `timer.jarvis_1` and `timer.jarvis_2` are `idle` and the table is empty, `set` with `duration = "five minutes"` and `timer = "chicken"` from the Kitchen satellite gives `Start { timer.jarvis_1, 300 }`, and the table has the entry with the Kitchen satellite, the name "chicken" and the order 0. A second `set` with no name gives `Start` on `timer.jarvis_2` with the order 1.
- AC-19.4-2: When the table has a "chicken" entry, `set` with `timer = "Chicken"` gives `Taken`, and the table does not change.
- AC-19.4-3: When each helper has an entry or is `active` in HA, `set` gives `Full`. The entity `timer.kitchen` is never a helper.
- AC-19.4-4: When the duration slot is "ten bananas", `set` gives `Unclear`. With no duration slot, it gives `NoDuration`.

**Proofs.**

- P-19.4-1, P-19.4-3 (AC-19.4-1, AC-19.4-3). Ensures: `Start { helper, .. }` implies that `helper` starts with `timer.jarvis_`, the hot cache shows it `idle`, the old table has no entry for it, and the new table is the old table with one new entry for it (`StartOnFreeHelper`). `Full` implies that no helper is free, and the table does not change. Argument: `free` keeps only the helpers with both conditions, and `set` inserts on the first of them. The loop invariant of `free` states that no helper before `index` is free.
- P-19.4-2 (AC-19.4-2). Requires: no two entries of the table have the same name, except none. Ensures: no two entries of the new table have the same name, except none (`UniqueNames`). Argument: only `set` adds an entry, and it returns `Taken` first when an entry has the same normalized name.
- P-19.4-4. Evidence: test. The seconds come from P-19.2-1.

---

### 19.4b Timer Candidates

**Goal:** Add the candidates of a timer action and the rules that remove an entry when HA ends its helper. Jarvis then knows which timers a request can act on, and its table follows HA.

- **Implements:** G25. **Depends on:** 19.4.
- **Files:** modify `src/domain/timers.rs`.
- **Quint:** `timers.qnt` (the rules of `startFailed`, `finish`, `haCancel` and `reconnect`).

**Model.**

- The candidates of an action are the entries whose helper has these HA states: `active` or `paused` for `Cancel`, `CancelLast` and `Left`, `active` for `Pause`, and `paused` for `Resume`. `Set` has no candidates. An entry whose start call runs has an `idle` helper, so it is never a candidate.
- `candidates(action, timers, cache) -> Vec<Candidate>`: the candidates in `order`. `Candidate { helper, timer }` has the helper entity ID and a copy of the entry.
- `forget(timers, helper, cache)` removes the entry of `helper` only when the hot cache shows the helper `idle` (a failed start call). `ended(timers, helper) -> (Option<Timer>, Timers)` removes the entry of `helper` and gives it (the HA events). `reconcile(timers, cache)` removes each entry whose helper the hot cache shows `idle` (a reconnect, sub-phase 19.9b).

**Acceptance criteria.**

- AC-19.4b-1: When "chicken" has the order 1 on `timer.jarvis_1`, which is `active`, "pasta" has the order 0 on `timer.jarvis_2`, which is `paused`, and an entry on `timer.jarvis_3` is `idle`, `candidates` gives "pasta" and "chicken", in this order, for `Cancel`, `CancelLast` and `Left`. It gives only "chicken" for `Pause`, only "pasta" for `Resume`, and none for `Set`.
- AC-19.4b-2: `forget` removes an entry whose helper is `idle` and keeps an entry whose helper is `paused`. `ended` removes the entry and gives it. `reconcile` removes the entries whose helper is `idle` and keeps the entries whose helper is `active` or `paused`.

**Proofs.**

- P-19.4b-1 (AC-19.4b-1). Ensures: each candidate is an entry of the table with its helper, and the hot cache shows the helper in a state of the action. Each such entry is a candidate exactly once. The candidates are in increasing `order`. Argument: a loop over the entries in entity ID order keeps the entries in a state of the action. Its loop invariant states that the kept entries are the candidates among the entries before `index`. A stable sort by `order` (the contract of `sort_by`) keeps each entry once.
- P-19.4b-2 (AC-19.4b-2). Ensures: `forget` removes the entry of `helper` if and only if the hot cache shows `helper` `idle`. `ended` removes only the entry of `helper`. `reconcile` keeps an entry if and only if the hot cache does not show its helper `idle`. Argument: `forget` and `ended` are one map remove on one key. `reconcile` is a filter over the entries.

---

### 19.4c Timer Plan

**Goal:** Add the pure `plan` of a timer request. Jarvis then knows which timer a request means, and when it must ask a question.

- **Implements:** G25. **Depends on:** 19.4b.
- **Files:** modify `src/domain/timers.rs`.
- **Quint:** `timers.qnt` (the rules of `cancel`, `pause` and `resume`).

**Model.**

- `Plan` gets the variants `Act { helper, service, timer }`, `Several { timers }`, `Left { timers }` and `Missing`.
- `plan(request, timers, cache, now, satellite, similarity) -> (Plan, Timers)`:
  - `Set`: the result of `set` (sub-phase 19.4).
  - `CancelLast`: the candidate with the highest `order`, else `Missing`. `Cancel`, `Pause`, `Resume`: with a name, `dialog::fill(name, labels, similarity)` (sub-phase 10.3) gives a label of a candidate, and the result is the candidate with that label and the highest `order`, else `Missing`. Without a name: no candidate gives `Missing`, one gives that candidate, and two or more give `Several { timers }` with copies of the candidates in `order`.
  - A chosen candidate gives `Act { helper, service, timer }` with a copy of its entry: `timer.cancel` for `Cancel` and `CancelLast`, `timer.pause` for `Pause`, and `timer.start` for `Resume` (`timer.start` resumes a paused helper). `Cancel` and `CancelLast` remove the entry from the table.
  - `Left`: the candidates, or with a name only the candidate that `fill` gives. None gives `Missing`. Else `Left { timers }` in `order`, each with its name, its `duration` and `left` (sub-phase 19.3). A candidate whose `left` is `None` is not in the list.
- The timers of all satellites are one table. Thus a request on any satellite can cancel, pause, resume or ask about any timer. Only the ring uses the satellite of a timer.

**Acceptance criteria.**

- AC-19.4c-1: When one timer is `active`, `Cancel` with no name gives `Act` with `timer.cancel` on its helper, and the table has no entry for it. When "chicken" and a generic "ten minutes" timer are `active`, `Cancel` with no name gives `Several` with both timers in order, and the table does not change.
- AC-19.4c-2: `Cancel` with `timer = "the chicken"` gives `Act` on the helper of "chicken". With `timer = "pizza"`, it gives `Missing`.
- AC-19.4c-3: With two `active` timers, `CancelLast` gives `Act` on the helper of the timer with the higher order.
- AC-19.4c-4: When "chicken" is `active` and "pasta" is `paused`, `Pause` with no name gives `Act` with `timer.pause` on "chicken", and `Resume` with no name gives `Act` with `timer.start` on "pasta".
- AC-19.4c-5: When "chicken" is `active` with `finishes_at` 252 s after `now` and a generic "ten minutes" timer is `paused` with `remaining = "0:10:00"`, `Left` with no name gives both, in order, with the left times 252 and 600. With `timer = "chicken"`, it gives only "chicken". With no candidate, it gives `Missing`.

**Proofs.**

- P-19.4c-1 to P-19.4c-4 (AC-19.4c-1 to AC-19.4c-4). Ensures: `Act { helper, .. }` implies that `helper` is a candidate of the action. `Several { timers }` implies two or more candidates, and the table does not change. Only `Cancel` and `CancelLast` remove an entry, and only the entry of `helper`. Argument: `choose` draws only from the candidates of the action (P-19.4b-1). The arms of `plan` remove an entry only for `Act` of `Cancel` and `CancelLast`. `CancelLast` takes the maximum `order`.
- P-19.4c-5. Evidence: test. The left times come from P-19.3-1, and the name match from `strsim` in `fill`.

---

### 19.5 Timer Intents

**Goal:** Add the intent kind `timer`, the `Timer` decision and the six timer intents with their seed examples. System One then classifies the timer requests and gives their slots.

- **Implements:** G3, G25. **Depends on:** 19.4c.
- **Files:** modify `src/domain/catalog.rs`, `src/domain/decide.rs`, `src/domain/responses.rs`, `src/app/conversation.rs`, `intents.toml`, `responses.toml`, `src/adapters/onnx.rs` (tests only).

**Model.**

- `Kind::Timer` (`kind = "timer"`), and the intent field `timer: Option<TimerAction>`. `validate_catalog` checks that a `timer` intent has `timer` and no `service`, and that another kind has no `timer`. The error names the intent.
- `decide`, step 2 of sub-phase 4.5: a confident `timer` intent reads its slots as a command does (sub-phase 4.5d). The result is `Decision::Timer(TimerRequest)` with the intent name, the action, the `timer` and `duration` slots (an empty optional slot is `None`) and the transcript. A `timer` intent needs no area.
- `pool_variables` (sub-phase 4.2): the pool of a `timer` intent can use `timer`, `duration` and `timers`, and no other variable. Sub-phases 19.6 and 19.6b render the pools with these variables.
- The six intents, at the end of `intents.toml`. Each has `kind = "timer"` and `optional = ["timer", "duration"]`, so a missing duration gives the question of sub-phase 19.6b, not System Two. The second pass writes numbers as words (sub-phase 18.9), so the durations of the examples are words.
  - `timer_set` (`set`): "set a timer for [ten minutes](duration)", "set a timer for [five minutes](duration) for the [chicken](timer)", "set a [pasta](timer) timer for [eight minutes](duration)", "start a timer for [thirty seconds](duration)", "timer for [an hour](duration)", "set a [laundry](timer) timer for [one hour and twenty minutes](duration)", "set a timer", "set a timer for the [chicken](timer)", "set a [pizza](timer) timer", "timer for the [pasta](timer)".
  - `timer_cancel` (`cancel`): "cancel that timer", "cancel the timer", "cancel the [chicken](timer) timer", "stop the [pasta](timer) timer", "delete my [laundry](timer) timer". `timer_cancel_last` (`cancel_last`): "cancel the most recent timer", "cancel the last timer", "cancel the newest timer".
  - `timer_pause` (`pause`): "pause the timer", "pause the [chicken](timer) timer", "pause my [pasta](timer) timer", "hold the timer". `timer_resume` (`resume`): "resume the timer", "resume the [chicken](timer) timer", "continue the [pasta](timer) timer", "unpause the timer".
  - `timer_left` (`left`): "how long is left on the timer", "how long do we have left on the [chicken](timer) timer", "how much time is left on the [pasta](timer) timer", "how much time is left", "how long until the timer goes off".
- `responses.toml` gets one pool for each intent. `timer_set`: `{% if timer %}Your {{ timer }} timer is set for {{ duration }}.{% else %}Timer set for {{ duration }}.{% endif %}`
  - `timer_cancel` and `timer_cancel_last`: `{% if timer %}I cancelled the {{ timer }} timer.{% else %}I cancelled the timer for {{ duration }}.{% endif %}`
  - `timer_pause` and `timer_resume`: the template of `timer_cancel` with "Pausing the" and "Resuming the" in place of "I cancelled the".
  - `timer_left`: `{% for t in timers %}{% if t.timer %}The {{ t.timer }} timer{% else %}The timer for {{ t.duration }}{% endif %} has {{ t.left }} left.{{ " " if not loop.last else "" }}{% endfor %}`
- Until sub-phase 19.9c, `Conversation::act` gives a `Timer` decision the `Defer` with `Job::Llm` of step 1. `just fixture-model` trains the fixture model again. The tests of AC-3.2-1 and AC-3.2c-2 get the six names. New seeds can lower the probability of other intents (sub-phase 18.19f). Before the merge, the PR measures the light commands of AC-9.2-5 with the new fixture model. When one is less than `model.commit_threshold`, the PR changes the timer examples, not the tests.

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

**Goal:** Turn the plan of a timer request into a reply with an HA call, a reply with no call, or System Two. "Set a timer for five minutes for the chicken" then gives "Your chicken timer is set for five minutes." and a `timer.start` call. Sub-phase 19.6b adds the time left and the questions.

- **Implements:** G13, G25. **Depends on:** 19.5.
- **Files:** modify `src/domain/timers.rs`, `src/domain/responses.rs`, `responses.toml`.

**Model.**

- `answer(plan, request, pools, rng) -> Decision`. The render context has `timer` (the name of the entry of `Act`, else of the request, else an empty string) and `duration` (the `duration` of the entry of `Act`, else of the request, else an empty string).
  - `Start { helper, seconds }`: `Reply` from the pool of the intent, with the call `timer.start` on `helper` and the data `{ duration: seconds }` as a number. HA reads a number of seconds as a duration.
  - `Act { helper, service, timer }`: `Reply` from the pool of the intent, with the call `service` on `helper` and no data.
  - `Taken`: `timer_taken`. `Full`: `timer_full`. `Missing`: `timer_missing`. No call. `Unclear`: `Defer` with a filler and `Job::Llm` with the transcript. System Two cannot set a timer, so it says so (sub-phase 10.6b).
  - Until sub-phase 19.6b, `Left`, `Several` and `NoDuration` give `Defer` with a filler and `Job::Llm` with the transcript.
  - A pool that does not render gives `Defer` with `Job::Llm`, as in `decide`.
- The answer pools, which `validate_pools` requires, as it requires `ask_area`: `ask_timer` (`Which timer, {% for t in timers %}the {% if t.timer %}{{ t.timer }} timer{% else %}timer for {{ t.duration }}{% endif %}{{ " or " if loop.revindex == 2 else (", " if not loop.last else "") }}{% endfor %}?`), `ask_duration` (`For how long?`), `timer_full` (`All timers are in use.`), `timer_taken` (`There is already a {{ timer }} timer.`), `timer_missing` (`{% if timer %}There is no {{ timer }} timer.{% else %}No timer is running.{% endif %}`) and `timer_done` (`{% if timer %}Your {{ timer }} timer is done.{% else %}Your timer is done.{% endif %}`, the ring of sub-phase 19.10). They can use `timer`, `duration` and `timers`, and no other variable (`01-architecture.md` section 9). Sub-phase 19.6b uses `ask_timer` and `ask_duration`.

**Acceptance criteria.**

- AC-19.6-1: When the plan is `Start { timer.jarvis_1, 300 }` for `timer_set` with `timer = chicken` and `duration = five minutes`, the result is `Reply` with "Your chicken timer is set for five minutes." and the call `timer.start` on `timer.jarvis_1` with the data `{ duration: 300 }` as a number. With no name and "ten minutes", the reply is "Timer set for ten minutes."
- AC-19.6-2: When the plan is `Act` with `timer.cancel` on the generic "ten minutes" timer for `timer_cancel`, the result is `Reply` with "I cancelled the timer for ten minutes." and the call `timer.cancel` with no data.
- AC-19.6-3: When the plan is `Taken`, `Full` or `Missing`, the result is `Reply` with no call: "There is already a chicken timer.", "All timers are in use.", and "There is no pizza timer." or, with no name, "No timer is running."
- AC-19.6-4: When the plan is `Unclear`, the result is `Defer` with `Job::Llm` and the transcript.
- AC-19.6-5: When `ask_timer`, `ask_duration`, `timer_full`, `timer_taken`, `timer_missing` or `timer_done` is missing, `validate_pools` returns an error that names it. When `timer_done` uses `area`, it returns an error with the pool and the variable.

**Proofs.**

- P-19.6-1, P-19.6-2 (AC-19.6-1, AC-19.6-2). Ensures: a `Reply` with a call comes only from `Start` or `Act`. Its target is the helper of the plan, its domain is `timer`, and its service is `start` for `Start` and the service of the plan for `Act`. Argument: `answer` builds a call only in these two arms, from the helper of the plan.
- P-19.6-3, P-19.6-4 (AC-19.6-3, AC-19.6-4). Ensures: `answer` gives `Reply` or `Defer` with `Job::Llm` and the transcript. `Unclear` gives `Defer` with `Job::Llm`. Thus, with P-19.6-1, `Taken`, `Full` and `Missing` give `Reply` with `call = None` or `Defer`. Argument: the arms of `Taken`, `Full` and `Missing` build `Reply { call: None }`, and the other arms without a call give `Defer`.
- P-19.6-5 (AC-19.6-5). Ensures: when `validate_pools` returns `Ok`, the six answer pools exist. Argument: `validate_pools` calls `require` for each, after `training_busy`.
- The texts of AC-19.6-1 to AC-19.6-3 are evidence by test. They come from `minijinja`.

---

### 19.6b Timer Questions

**Goal:** Add the time left and the questions to `answer`. "How long is left on the timer?" then lists the timers with their time left, "cancel that timer" with two timers asks which timer, and "set a timer" asks "For how long?".

- **Implements:** G13, G25. **Depends on:** 19.6.
- **Files:** modify `src/domain/timers.rs`.

**Model.**

- The render context of `answer` (sub-phase 19.6) also has `timers` for `Left` and `Several`: each timer with `timer` (its name, else an empty string), `duration` and, for `Left`, `left`.
  - `Left { timers }`: `Reply` from the pool of the intent. `left` is `spoken` (sub-phase 19.2). No call.
  - `Several { timers }`: `Ask` from `ask_timer` with `Pending::Slot { intent, slots: [], missing: "timer", choices, transcript }`. The choices are the labels of `timers`. Routing step 2 of sub-phase 10.5 fills it as it fills the area: the answer "the chicken" adds the slot `timer = chicken`, and `decide` runs again.
  - `NoDuration`: `Ask` from `ask_duration` with `Pending::Slot { intent, slots, missing: "duration", choices: [], transcript }`. `slots` has the `timer` slot of the request, if any. Sub-phase 19.9d fills it.
  - A pool that does not render gives `Defer` with `Job::Llm`, as in sub-phase 19.6.

**Acceptance criteria.**

- AC-19.6b-1: When the plan is `Several` with "chicken" and "pizza", the result is `Ask` with "Which timer, the chicken timer or the pizza timer?" and `Pending::Slot` with `missing = "timer"`, the choices "chicken" and "pizza" and the transcript of the request. With "chicken", "pizza" and a generic "ten minutes" timer, the question is "Which timer, the chicken timer, the pizza timer or the timer for ten minutes?"
- AC-19.6b-2: When the plan is `Left` with "chicken" (252 s) and a generic "ten minutes" timer (600 s), the reply is "The chicken timer has 4 minutes and 12 seconds left. The timer for ten minutes has 10 minutes left."
- AC-19.6b-3: When the plan is `NoDuration` with `timer = chicken`, the result is `Ask` with "For how long?" and `Pending::Slot` with `missing = "duration"`, no choices and the slot `timer = chicken`.

**Proofs.**

- P-19.6b-1 (AC-19.6b-1). Ensures: `Several { timers }` gives `Ask` with `Pending::Slot` whose `missing` is `timer` and whose `choices` are the labels of `timers`, in order, or `Defer` with `Job::Llm` when `ask_timer` does not render. Argument: the arm maps `timers` to their labels.
- P-19.6b-2 (AC-19.6b-2). Ensures: `Left` gives `Reply` with `call = None`, or `Defer` with `Job::Llm`. Argument: the arm of `Left` builds `Reply { call: None }`. The text is evidence by test: it comes from `minijinja` and P-19.2-3.
- P-19.6b-3 (AC-19.6b-3). Ensures: `NoDuration` gives `Ask` with `missing = "duration"` and no choices, or `Defer` with `Job::Llm` when `ask_duration` does not render. Argument: the arm builds `Ask` with an empty list of choices.

---

### 19.7 Timer Events

**Goal:** Give the timer events of HA to Jarvis. Jarvis then knows when a timer finishes and when a person cancels it in HA. Sub-phase 19.7b sends them to the timer use case.

- **Implements:** G25. **Depends on:** 19.6b.
- **Files:** modify `src/domain/state.rs`, `src/adapters/ha.rs`, `src/app/background.rs`.

**Model.**

- HA fires `timer.finished` when a timer ends, and `timer.cancelled` when a timer is cancelled. The data of each has `entity_id`. A state change does not tell a finish from a cancel: both go from `active` to `idle`.
- `Event::TimerEnded { entity_id, end: TimerEnd }`, with `TimerEnd::Finished` or `TimerEnd::Cancelled`. After `Ready`, the HA adapter also subscribes to `timer.finished` and `timer.cancelled` (sub-phase 2.4b), before it gives `Reconnected`. Until sub-phase 19.7b, `sync` (sub-phase 2.9) ignores `TimerEnded`, which does not change the hot cache.
- After each connect, the HA adapter sends a service call only after HA gives the result of each subscription. A call that comes earlier waits, and its timeout includes the wait. HA sends the state change that a service call makes before the result of the call, on the same socket. Thus, when a `timer.start` call succeeds, `events` already has the `active` state of its helper, also after a reconnect.

**Acceptance criteria.**

- AC-19.7-1: When `FakeHaServer` sends `timer.finished` with `entity_id = timer.jarvis_1`, `events` gives `TimerEnded` with `timer.jarvis_1` and `Finished`. For `timer.cancelled`, it gives `Cancelled`.
- AC-19.7-2: When the HA adapter connects, `FakeHaServer` receives `subscribe_events` for `timer.finished` and for `timer.cancelled` before `events` gives `Reconnected`.
- AC-19.7-3: When `FakeHaServer` holds the results of the subscriptions after a connect, a `call` sends nothing until the server gives them. When the server then sends a `state_changed` of `timer.jarvis_1` and the result of the `timer.start` call, `events` gives the state change before `call` returns.

**Proofs.**

- P-19.7-1, P-19.7-2, P-19.7-3. Evidence: test, and step 4 of the field log for the order of HA.

**Field log.** Before the first release with sub-phase 19.9c, on the live HA, in the developer tools, with a helper `timer.jarvis_1`:

1. Call `timer.start` with the data `duration: 5` (a number). Make sure that the timer runs 5 s, and that its state has `finishes_at` while it is `active`.
2. Listen to `timer.finished`, `timer.cancelled` and `state_changed`. Make sure that the end of the timer fires `timer.finished` with `entity_id`, and that the state change to `idle` comes before it. Start the timer again and cancel it. Make sure that HA fires `timer.cancelled` and no `timer.finished`. Start it again at once. Make sure that `timer.cancelled` comes before the state change to `active`.
3. Start a timer of 1 minute and pause it. Make sure that `remaining` has the form `H:MM:SS`. Call `timer.start` with no data, and make sure that the timer continues.
4. On one WebSocket connection, subscribe to `state_changed` and call `timer.start` on an `idle` helper. Make sure that the state change to `active` comes before the result of the call.

---

### 19.7b Timer Inputs

**Goal:** Send the changes of the timer helpers and the timer events to the timer use case, in the order of HA. Jarvis then knows when the connection to HA starts again, and which timers ran out while it was not connected.

- **Implements:** G25. **Depends on:** 19.7.
- **Files:** modify `src/domain/state.rs`, `src/domain/timers.rs`, `src/adapters/ha.rs`, `src/app/background.rs`, `src/lib.rs`, `src/testing/fakes.rs`.

**Model.**

- `sync` (sub-phase 2.9) gets a sender of `TimerInput` (in `src/domain/timers.rs`): `Event(Event)` or `Snapshot(Vec<State>)`. It sends each `StateChanged` of an entity in the domain `timer` after it applies it to the hot cache, and each `TimerEnded` (sub-phase 19.7), which does not change the hot cache. When the HA adapter delivers the result of a `get_states` request, it first gives `Event::Loaded { states }` in `events`, with the states of the entities in the domain `timer` from that result. The read loop delivers the messages one at a time in the order of the socket, so `Loaded` comes after each event that HA sends before the states, and before each later event. `sync` sends the states of each `Loaded` as `Snapshot` when it reads it. Thus each `Snapshot` is the states of one `get_states` result, at the place of that result in the socket, also when two loads overlap or a load fails after its `get_states`. Thus a timer that ends while `load` runs gives its `TimerEnded` before the `Snapshot` that shows its helper `idle`. HA writes the `idle` state and fires the timer event in one step of its event loop, so no result of `get_states` comes between them. Thus the receiver gets the changes of the timer helpers and the timer events in the order of HA. A closed receiver does not stop `sync`. Until sub-phase 19.9c, `run` gives `sync` a sender whose receiver it drops. A timer event and `Loaded` do not change the cache, so `cache_sync.qnt` and its driver do not change.

**Acceptance criteria.**

- AC-19.7b-1: When `sync` gets a `StateChanged` of `timer.jarvis_1`, then a `TimerEnded`, the receiver gets both in this order, and the hot cache gets only the state. A `StateChanged` of a light does not reach the receiver. When `sync` gets `Loaded`, the receiver gets `Snapshot` with its states.
- AC-19.7b-2: When `FakeHaServer` sends `timer.finished` of `timer.jarvis_1` after a reconnect and before the result of `get_states`, the receiver gets the `TimerEnded` before the `Snapshot`. A `StateChanged` that the server sends after the result comes after the `Snapshot`.
- AC-19.7b-3: When `sync` gets `RegistryUpdated` while the load of a `Reconnected` runs, and `FakeHaServer` sends `timer.finished` of `timer.jarvis_1` between the two `get_states` results, the receiver gets the `Snapshot` of the first result with `timer.jarvis_1` `active`, then the `TimerEnded`, then the `Snapshot` of the second result with `timer.jarvis_1` `idle`.

**Proofs.**

- P-19.7b-1, P-19.7b-2, P-19.7b-3. Argument: `sync` reads one event at a time and sends it before it reads the next. The `TimerEnded` arm only sends. The `Loaded` arm sends only its own states. The adapter gives `Loaded` in the order of the socket.

---

### 19.8 Timer Store

**Goal:** Add the `TimerStore` port and its SQLite adapter, which keep the timer table in `jarvis.db`. The table then survives a restart of Jarvis.

- **Implements:** G25. **Depends on:** 19.7b.
- **Files:** modify `src/ports.rs`, `src/adapters/sqlite.rs`, `src/domain/timers.rs`, `src/testing/fakes.rs`.

**Model.**

- `TimerStore`: `load() -> Result<Timers>` and `save(&Timers) -> Result<()>`. Its only production adapter is the SQLite adapter of the interaction log (sub-phase 12.4), on the same database `jarvis.db`. Thus Jarvis has no new data file.
- The adapter adds the table `timers` with one row: the JSON of `Timers`. `save` replaces the row in one statement. `load` gives an empty table when the row does not exist. The calls run in `spawn_blocking`, as the other SQLite calls (decision 29).
- `Timers` and `Timer` get `Serialize` and `Deserialize`. The rings are not saved: a ring is at most 2 minutes long.
- `FakeTimerStore` in `src/testing/fakes.rs` keeps the last saved table and can fail `save`.

**Acceptance criteria.**

- AC-19.8-1: When the adapter saves a table with two entries and a new adapter on the same database file loads it, the table is equal.
- AC-19.8-2: When the database has no `timers` row, `load` gives an empty table. When the database also has interactions, `save` and `load` do not change them.

**Proofs.**

- P-19.8-1, P-19.8-2. Evidence: test.

---

### 19.9 Timer Start Rules

**Goal:** Let the start call of a timer decide its own entry. A late timer event, a reconnect or a failed call then cannot remove a new timer on the same helper.

- **Implements:** G25. **Depends on:** 19.8.
- **Files:** modify `src/domain/timers.rs`, `src/adapters/sqlite.rs` (tests only).

**Model.**

- `Timer` gets `starting: bool`: the start call of the entry runs. `set` (sub-phase 19.4) sets it on the new entry.
- `running(timers, state) -> Timers`: when `state` is `active` or `paused`, it clears `starting` on the entry of its helper. Else the table does not change. `restored(timers) -> Timers` clears `starting` on each entry, because no start call survives a restart.
- `ended(timers, helper)` removes and gives the entry of `helper` only when it does not start. Else it gives `None` and the same table. HA sends the end of an older timer on the helper before the `active` of the new one, so the late `Cancelled` of a voice cancel cannot remove a new entry on the same helper.
- `forget(timers, helper, order)` removes the entry of `helper` only while it starts and has `order`. It does not read the cache.
- `reconcile(timers, cache)` keeps each entry that starts: its start call decides it. It removes each entry that does not start and whose helper the cache shows `idle`. It clears `starting` on each entry whose helper the cache shows `active` or `paused`.
- These rules replace the `forget` and `reconcile` parts of AC-19.4b-2.

**Acceptance criteria.**

- AC-19.9-1: When `set` adds an entry, the entry starts. `running` with the state `active` or `paused` of its helper clears `starting`, and with `idle` does not change the table. `restored` clears `starting` on each entry.
- AC-19.9-2: `forget` with the helper and the order of an entry that starts removes it. It keeps an entry that does not start, and an entry that starts with another order.
- AC-19.9-3: `ended` keeps an entry that starts and gives `None`. It removes an entry that does not start and gives it.
- AC-19.9-4: When an entry that starts and an entry that does not start have `idle` helpers, and an entry that starts has an `active` helper, `reconcile` keeps the first, removes the second, and clears `starting` on the third.

**Proofs.**

- P-19.9-1 (AC-19.9-1). Ensures: the entry of `Start` starts. `running` changes only `starting` of the entry of the helper of `state`, and only when `state` is `active` or `paused`. `restored` keeps each key, and no entry of its result starts. Argument: `set` inserts one entry. `running` is one guarded change on one key. `restored` is a loop over the entries, whose invariant states that the entries before `index` do not start.
- P-19.9-2 to P-19.9-4 (AC-19.9-2 to AC-19.9-4). Ensures: `ended` changes no entry that starts. `forget` removes an entry only when it starts and has the order of the call. `reconcile` keeps each entry that starts, and removes an entry that does not start if and only if the cache shows its helper `idle`. Argument: each is one guarded remove or a filter.

---

### 19.9b Timer Desk

**Goal:** Add `TimerDesk`, which plans the timer requests, keeps the timer table in its store and follows the timer helpers of HA. The table then follows HA, also after a restart of Jarvis.

- **Implements:** G25. **Depends on:** 19.9.
- **Files:** create `src/app/timers.rs`.

**Model.**

- `TimerDesk` holds, in one async mutex, the timer table, the view and the rings (sub-phase 19.10). It also holds the `TimerStore` and `dialog.choice_similarity`. Each change of the table saves it under the lock, so the saves keep the order of the changes. A failed save logs a `warn` event, and the table in memory stays.
- The view is a `StateCache` of the timer helpers. Only `run` changes it, from the receiver (sub-phase 19.7b): `apply` (sub-phase 2.7) for each state change, and a new cache of the states of each `Snapshot`. `TimerDesk` does not read the hot cache. Thus each state change of a helper and each timer event reach the table in the order of HA.
- `TimerDesk::decide(request, satellite, now, pools, rng) -> Decision`: takes the lock, runs `plan` (sub-phase 19.4c) with the view, stores and saves the new table, and gives `answer` (sub-phase 19.6). One lock, so two requests at the same time cannot take the same free helper.
- `TimerDesk::new` loads the table from the store and runs `restored` (sub-phase 19.9). A failed load logs a `warn` event and starts with an empty table. Until the first `Snapshot`, the view is empty, so `Set` gives `Full`.
- `TimerDesk::run(events)` reads the receiver. A state change goes into the view and runs `running`. `TimerEnded` runs `ended`, so a finished timer and a timer that a person cancels in HA leave the table. Sub-phase 19.10 adds the ring. `Snapshot` replaces the view and runs `reconcile`.
- A `Finished`, and a cancel in HA, leave the entry in the table until `run` reads the event, so the helper is not free before then.
- At the start and after each reconnect, `reconcile` removes each entry that does not start and whose helper is `idle`, with no ring: its timer ended or was cancelled while Jarvis was not connected, and Jarvis cannot tell which. An entry whose helper is `active` or `paused` stays, and its end rings on its satellite (sub-phase 19.10). A reservation that `decide` makes after `sync` loads a snapshot, but before `run` reads it, starts, so it stays.
- A start call succeeds only after `events` has the `active` state of its helper (sub-phase 19.7), and `sync` sends that state before a later `Snapshot`. Thus a successful start always clears `starting`, also when a reconnect comes between the call and the result.
- A request without a satellite (text from the HA UI) can set a timer, and its entry has no satellite.

**Acceptance criteria.**

- AC-19.9b-1: When `run` gets `TimerEnded` with `Cancelled` for the helper of "chicken", the next `timer_left` for "chicken" replies "There is no chicken timer.", and `FakeTimerStore` has no entry for it.
- AC-19.9b-2: When `FakeTimerStore` has "chicken" on an `active` helper and "pasta" on an `idle` helper, and a new `TimerDesk` gets `Snapshot`, `timer_left` gives only "chicken", and `FakeTimerStore` has no entry for "pasta".
- AC-19.9b-3: When two `timer_set` requests run `decide` at the same time with one free helper, one gives the call `timer.start` and the other replies "All timers are in use."
- AC-19.9b-4: When the Kitchen satellite set "chicken" and the Office satellite asks `timer_left` for "chicken", the reply gives the time left of "chicken".
- AC-19.9b-5: When `decide` cancels "chicken" on `timer.jarvis_1`, the view shows `timer.jarvis_1` `idle`, and a `timer_set` for "pasta" takes `timer.jarvis_1` before `run` reads the `Cancelled` of "chicken", the table still has "pasta" after that `Cancelled`. After the state `active` of `timer.jarvis_1`, `timer_left` gives "pasta".
- AC-19.9b-6: When `sync` sends a `Snapshot` with `timer.jarvis_1` `idle`, and a `timer_set` for "pasta" takes `timer.jarvis_1` before `run` reads that `Snapshot`, the table still has "pasta" after it.
- AC-19.9b-7: When `FakeTimerStore` has "chicken" with `starting` on an `idle` helper and "pasta" with `starting` on an `active` helper, a new `TimerDesk` that gets `Snapshot` has only "pasta", which does not start.
- AC-19.9b-8: When `FakeTimerStore` has "chicken", the first `timer_set` for "chicken" after `TimerDesk::new` replies "There is already a chicken timer."

**Proofs.**

- P-19.9b-3. Requires: P-19.4-1. Argument: `TimerDesk::decide` runs `plan` and stores the table under one lock, so the second `plan` sees the entry of the first.
- P-19.9b-2, P-19.9b-7, P-19.9b-8. Argument: `TimerDesk::new` loads the table and runs `restored` (P-19.9-1) before any request or input, and `Snapshot` runs `reconcile` (P-19.9-4) and saves.
- P-19.9b-5, P-19.9b-6. Requires: P-19.9-1 to P-19.9-4. Argument: the new entry starts until `run` reads the `active` of its helper, and HA sends that state after the end of the older timer, so the old `Cancelled` and the old `Snapshot` meet an entry that starts.
- P-19.9b-1, P-19.9b-4. Evidence: test.

---

### 19.9c Timer Requests

**Goal:** Give the timer requests to `TimerDesk`, and the timer inputs of `sync` to its `run`. "Set a timer", "cancel that timer", "pause", "resume" and "how long is left" then act on the HA timer helpers.

- **Implements:** G13, G25. **Depends on:** 19.9b.
- **Files:** modify `src/app/timers.rs` (tests only), `src/app/conversation.rs`, `src/app/background.rs` (tests only), `src/lib.rs`, `src/testing/fakes.rs`, `src/testing/e2e.rs`, `README.md` (the timer requests and the setup step of the timer helpers, `CLAUDE.md` workflow item 15).

**Model.**

- `Conversation::act` gives a `Timer` decision to `TimerDesk::decide` with the satellite of the request, the time of the request, the current pools and its RNG. This replaces the `Defer` of sub-phase 19.5. The result goes through `dispatch` as each other decision: the reply first, then the call (`ReplyBeforeCall`), and the question through the announce queue (sub-phase 10.4).
- Until sub-phase 19.9e, a failed or dropped `timer.start` call does not change the table. Its entry starts until a restart of Jarvis, so its helper and its name stay taken. A failed cancel, pause or resume does not change the table.
- `start_sync` also gives the receiver of the `TimerInput`. `run` makes the `TimerDesk` with the SQLite adapter as its `TimerStore` before the conversation use case gets it, gives it to the conversation use case, and starts `TimerDesk::run` with this receiver. Thus no request changes a table that a later load replaces.
- The README gets the timer requests and the setup step: in HA, create five timer helpers with the names "Jarvis 1" to "Jarvis 5", so that their entity IDs are `timer.jarvis_1` to `timer.jarvis_5`, and turn on "Restore" for each. Jarvis uses each `timer.jarvis_` helper. The tests add the helpers to the snapshot of the reference house before they load the cache, because the reference house has no timer entity.

**Acceptance criteria.**

- AC-19.9c-1: When `FakeClassifier` gives `timer_set` with `duration = five minutes` and `timer = chicken` on the Kitchen satellite, `handle` returns "Your chicken timer is set for five minutes.", `FakeHome` records `timer.start` on `timer.jarvis_1` with `{ duration: 300 }` after the reply, and `FakeTimerStore` has the entry.
- AC-19.9c-2: When "chicken" and "pizza" are `active` and the user says "cancel that timer", `handle` returns empty text, and `FakeHome` records `ask` with "Which timer, the chicken timer or the pizza timer?". When the user then answers "the chicken", `FakeHome` records `timer.cancel` on the helper of "chicken", and the reply is "I cancelled the chicken timer."
- AC-19.9c-3: When the view shows the helper `active` before its `timer.start` call fails, the entry stays.
- AC-19.9c-4: When `FakeHome` holds a `timer.pause` call on a helper, the timer of the helper ends, a new `timer_set` takes the same helper, and then the pause call fails, the new entry stays and starts.
- AC-19.9c-5: When `FakeHome` holds a `timer.start` call, the view shows the helper `active`, the timer ends, a new `timer_set` takes the same helper, and then the held start call fails, the new entry stays and starts.

**Proofs.**

- P-19.9c-1. Requires: `ReplyBeforeCall` (sub-phase 16.1). Argument: the timer decision is a `Reply` with a call, and `dispatch` gives the call as `after` work (P-4.8-1).
- P-19.9c-2. Argument: `Several` gives `Ask` (P-19.6b-1), `act` opens the dialog (sub-phase 10.5), and the filled answer runs `decide` again with the `timer` slot (P-10.5-1).
- P-19.9c-3 to P-19.9c-5. Argument: a failed call changes no entry of the table. A new entry starts until `run` reads the `active` of its helper (P-19.9-1).

---

### 19.9e Timer Start Guard

**Goal:** Free the helper and the name of a timer whose start call fails or drops. "Set a timer for the chicken" then works again after a failed start.

- **Implements:** G25. **Depends on:** 19.9c.
- **Files:** modify `src/app/timers.rs`, `src/app/conversation.rs`.

**Model.**

- `TimerDesk::decide` puts a guard with the helper and the order of the new entry in a `Start` decision, and the `after` work of its `timer.start` call takes it. When that call fails, the `after` work runs `forget` (sub-phase 19.9) with the helper and the order, before the `failure` answer (sub-phase 4.9c). When the guard drops before the call ends, it runs `forget` the same way in a new task. Thus the decision or the `after` work can drop for any reason, for example a failed write of the reply in the Wyoming adapter, and the helper and the name become free. After the call ends, the guard does nothing. Thus a failed or dropped start frees the helper and the name. A failed cancel, pause or resume does not run `forget` and does not change the table. Its timer can end, and a new timer can take the helper, before the failure comes. An `active` state clears `starting` before the start call ends, so a timer can also end, and a new timer can take the helper, before a start call fails. `next` only grows while Jarvis runs, and a restart drops each start call, so no new entry has the order of a start call that runs. Thus the failure of a start call meets only its own entry. When the connection fails after HA starts a timer but before Jarvis gets the result, or when the start call drops after it reaches HA, the helper runs with no entry and no ring.
- The tests of AC-19.9c-3 to AC-19.9c-5 also pass with the guard: it does not remove a new entry, or an entry whose helper is `active`.

**Acceptance criteria.**

- AC-19.9e-1: When `FakeHome` fails the `timer.start` call, `FakeHome` records an announce from `failure`, and the next `timer_set` with the same name gives `timer.start` on the same helper, not `timer_taken`.
- AC-19.9e-2: When the `after` work of a `timer_set` drops before it runs, `FakeHome` gets no `timer.start` call, and the next `timer_set` with the same name gives `timer.start` on the same helper, not `timer_taken`.

**Proofs.**

- P-19.9e-1, P-19.9e-2. Requires: P-19.9-1, P-19.9-2. Argument: the failure arm of `after` and the drop of the guard run `forget` only for a `timer.start` call, with the order of its entry. The entry of that call still starts, unless the `active` state of its helper cleared `starting`. A new entry on the same helper has a higher order. A failed pause, cancel or resume runs no `forget`.

---

### 19.9d Timer Waits

**Goal:** Fill the duration of a timer from the answer to "For how long?", ask a timer question again with its own text, and let a cancel, pause or resume that waits act only on its own timer.

- **Implements:** G13, G25. **Depends on:** 19.9e.
- **Files:** modify `src/domain/timers.rs`, `src/app/timers.rs`, `src/app/conversation.rs`, `README.md` (the answer to "For how long?", `CLAUDE.md` workflow item 15).

**Model.**

- Routing step 2 of sub-phase 10.5 also fills a `Slot` dialog whose missing slot is `duration`: when `duration` (sub-phase 19.2) reads the answer, the normalized answer is the `duration` slot, and `decide` runs again with the slots of the dialog. Else steps 3 to 6 apply, so Jarvis asks again up to `dialog.max_turns`. `is_answer` (sub-phase 10.7) does not count this dialog, so its answer keeps the `endpoint_ms` wait: "one hour" can continue with "and twenty minutes".
- Step 5 of sub-phase 10.5 asks a `Slot` dialog for `duration` or `timer` again with its own question, not from `ask_area`: `ask_duration`, or `ask_timer` with the choices of the dialog as the `timer` of each item of `timers`. A choice of a generic timer is its duration, so the question again says "the ten minutes timer". A `Slot` dialog for the area asks from `ask_area`, as before.
- `TimerDesk` counts the runs of each helper. The count increases when the view changes the helper from `idle`, or from no state, to `active` or `paused`, in a state change or in a `Snapshot`. Each new timer on a helper starts from `idle`, so it is a new run. A pause or a resume in HA is the same run. A `Snapshot` also increases the count of each helper whose `last_updated` differs from the view, because a snapshot cannot show that the run continued: HA can restart the helper while Jarvis is disconnected.
- `TimerDesk::decide` puts the helper and its run count in the view in an `Act` decision. The `after` work of its call takes the lock of `TimerDesk`, makes the call under the lock, and then releases it. It makes the call only when the view still shows the helper with the same run count, in a state that the action acts on (the candidates of sub-phase 19.4b). Else it makes no call. Thus a cancel, pause or resume that waits, for example for the write of its reply, cannot act on a new timer on the helper, from Jarvis or from HA. A cancel that waits still cancels its timer when a person pauses or resumes that timer in HA, because the entry of a cancel already left the table and the reply said "cancelled". Under the lock, no request can take the helper and the view does not change during the call. HA has no conditional call, so a change in HA that the view does not have yet can still meet the call.
- `fits` (sub-phase 19.4b) becomes public, so that the `after` work checks the state with the rule of the candidates.

**Acceptance criteria.**

- AC-19.9d-1: When the user says "set a timer for the chicken", `FakeHome` records `ask` with "For how long?". When the user answers "ten minutes", `FakeHome` records `timer.start` with `{ duration: 600 }`, and the reply is "Your chicken timer is set for ten minutes." When the user answers "soon", `FakeHome` records `ask` with "For how long?" again. When "chicken" and "pizza" are `active` and the user answers "soon" to "Which timer, the chicken timer or the pizza timer?", `FakeHome` records `ask` with the same question again.
- AC-19.9d-2: When the `after` work of a `timer_pause` waits, its timer ends, and a `timer_set` from another satellite takes the same helper, the `after` work makes no call, and the new timer stays `active`. When the helper starts again from HA instead, with no entry, a waiting `timer_cancel` also makes no call. When a person pauses the timer in HA while a `timer_cancel` waits, the `after` work makes the call `timer.cancel`. When Jarvis reconnects while a `timer_cancel` waits, and the `Snapshot` shows another `last_updated` of the helper, the `after` work makes no call. With no change of the helper, it makes the call.

**Proofs.**

- P-19.9d-1. Argument: `NoDuration` gives `Ask` (P-19.6b-3), `act` opens the dialog (sub-phase 10.5), and the filled answer runs `decide` again with the `duration` slot (P-10.5-1). Step 5 picks the pool from the missing slot of the dialog.
- P-19.9d-2. Argument: the `after` work of an `Act` checks the run count and the state in the view and calls under the lock of `TimerDesk`, and `decide` and `run` take the same lock. A new run of the helper increases the run count. A pause in HA keeps the run count and a state that `timer.cancel` acts on. A `Snapshot` with another `last_updated` increases the run count.

---

### 19.10 Timer Ring

**Goal:** Announce the end of a timer on the satellite that set it, every 15 s for up to 2 minutes. Sub-phase 19.10b adds "stop" and the cancel of a ringing timer by name.

- **Implements:** G25. **Depends on:** 19.9d.
- **Files:** modify `src/domain/timers.rs`, `src/app/timers.rs`, `src/app/conversation.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `README.md` (the ring, `CLAUDE.md` workflow item 15).
- **Quint:** `timers.qnt` (the rules of `finish` and `ringOnce`).

**Model.**

- `RING_INTERVAL` is 15 s, `RINGS` is 8 and `RING_FOR` is 120 s, constants in `src/app/timers.rs`. Thus the last ring starts 105 s after the end when the satellite is idle. `TimerEnded` with `Finished` runs `ended`. When the entry has a satellite, a ring starts. The ring has the satellite, the entry and the time of the event. Its text is `done(timer, pools, rng) -> Option<Chosen>` (in `src/domain/timers.rs`): a response of `timer_done` with the `timer` and `duration` of the entry. Each ring picks an answer and records its use, as a `failure` answer does (sub-phase 4.9c). When `timer_done` does not render, the ring task ends.
- Ring `k` (from 0 to 7) starts `k * RING_INTERVAL` after the event, or when ring `k - 1` ends, if that is later. Each ring calls `Queues::queued` (sub-phase 4.9b) with the current generation of the satellite. Thus each ring waits for its turn and until the satellite is idle, and a new request on the satellite drops only the ring that waits at that time. The next ring continues.
- `Queues::queued` runs its `speak` future only after the wait for idle, the margin and the generation check (sub-phase 4.9b). The `speak` future of a ring first checks that its ring, by its number, still exists and that the virtual time is less than the event plus 120 s (`RING_FOR`). Only then does it call `Home::announce`. Else it makes no announce. The ring task makes the same check before each ring, and ends when it fails. Thus a long wait for idle, or a new ring on the helper, drops a ring that already waits in the queue. The model has no time: `ringOnce` is this check when it passes. The driver of sub-phase 19.12 makes each ring at once on an idle satellite, so its traces do not reach `RING_FOR`, and no new Quint action is necessary.
- `TimerDesk` keeps the rings by helper, each with a number that it counts up. A new finish on the same helper replaces its ring, so the task of the old ring stops at its next check. When the ring task ends, it removes its ring, only when the ring of the helper still has its number.
- The conversation use case runs the ring task, because it holds the pools, the random source, the backfill and the announce queues. `TimerDesk::run(events, ring)` gives each new ring and its helper to the function `ring`. `Conversation::run_timers(events)` runs it with a function that spawns the ring task. `run` (sub-phase 19.9c) starts `run_timers` with the receiver of the timer inputs after it makes the conversation use case. Until then, the receiver keeps the inputs in their order.
- During a local training run, Jarvis releases its TTS (sub-phase 13.10e). An announce then fails, and Jarvis logs it. The next ring tries again. A restart of Jarvis ends each ring, because the rings are not saved.
- `FakeHome` records the virtual time of each announce.

**Acceptance criteria.**

- AC-19.10-1: When the Kitchen satellite set "chicken" and the receiver gets `TimerEnded` with `Finished` for its helper, `FakeHome` records the announce "Your chicken timer is done." on the Kitchen satellite, then the same announce 15 s of virtual time after the start of the one before, 8 in total, and no announce 120 s or more after the event. When the Kitchen satellite is not idle from 100 s to 130 s, `FakeHome` records no announce after 100 s.
- AC-19.10-2: When the timer is generic, the announce is "Your timer is done." When the timer was in `FakeTimerStore` at the start of the `TimerDesk`, the announce is on its satellite.
- AC-19.10-3: When the user makes another request on the Kitchen satellite during the ring, the request gets its normal reply, and the next ring is announced.
- AC-19.10-4: When the timer has no satellite, or the event is `Cancelled`, `FakeHome` records no announce.
- AC-19.10-5: When a new timer on the same helper finishes during a ring, `FakeHome` records each later announce only once for each 15 s, with the text of the new timer.

**Proofs.**

- P-19.10-1. Requires: `RingsBounded` (P-19.1-2), `AnnounceWhenIdle` (P-4.7-2). Argument: the ring task loops `RINGS` times at most, and each ring goes through the announce queue. The `speak` future announces only before the event plus `RING_FOR`, and `queued` runs it last, so no announce starts at or after 120 s.
- P-19.10-4. Argument: `run` starts a ring only in the `Finished` arm, and only for an entry with a satellite.
- P-19.10-5. Argument: the check compares the number of the ring, and the new finish gave the helper a new number.
- P-19.10-2, P-19.10-3. Evidence: test.

---

### 19.10b Timer Stop

**Goal:** Stop the ring of a timer when the user says "stop" on the satellite that rings, or cancels that timer by name on any satellite.

- **Implements:** G25. **Depends on:** 19.10.
- **Files:** modify `src/domain/timers.rs`, `src/app/timers.rs`, `src/app/conversation.rs`, `README.md` ("stop" and the cancel of a ringing timer, `CLAUDE.md` workflow item 15).
- **Quint:** `timers.qnt` (the rules of `stop` and `silence`).

**Model.**

- `TimerDesk::stop(satellite) -> bool` removes each ring of the satellite, and gives true when it removed one. `TimerDesk::ringing() -> bool` gives true while a ring exists. The check of the ring task (sub-phase 19.10) then fails, so a "stop" or a cancel by name drops a ring that already waits in the queue.
- `Conversation::route` checks the ring first (sub-phase 10.5, before step 1). When the request has a satellite, `is_cancel(text, dialog.cancel)` is true (sub-phase 10.3), and `stop` of the satellite gives true, `handle` returns empty text. Jarvis records no interaction for it, as for a cancel. `handle` takes the open dialog before `route` (sub-phase 16.1b), so this "stop" also closes the dialog of the satellite. "Stop" is a phrase of `dialog.cancel`. "Stop" on another satellite does not stop the ring. Without a ring, "stop" is routed as before.
- `plan` (sub-phase 19.4c) also gets the timers of the rings. `Cancel` with a name first matches the labels of the ringing timers with `fill`. A match gives `Silence { helper, timer }`, before the candidates of the table. `answer` gives `Reply` from the pool of the intent with no call. `TimerDesk::decide` then removes the ring of `helper`. A ringing timer is no longer in HA, so it needs no call. This applies on any satellite.
- The later stop and mute phase replaces the check of `route` with its general "stop". Its "stop" calls `TimerDesk::stop` for the satellite, so a ring stays one of the things that "stop" ends.

**Acceptance criteria.**

- AC-19.10b-1: When the user says "stop" on the Kitchen satellite after the second announce, `handle` returns empty text, the interaction log gets no record, and `FakeHome` records no further announce.
- AC-19.10b-2: When the user says "stop" on the Office satellite, the announces on the Kitchen satellite continue.
- AC-19.10b-3: When the user says "cancel the chicken timer" on the Office satellite while a ring waits in the queue of the Kitchen satellite, the reply is "I cancelled the chicken timer.", and `FakeHome` records no `timer.cancel` and no further announce.
- AC-19.10b-4: When no ring runs, "stop" with an open dialog gives the reply from `cancelled` (AC-10.5-4).

**Proofs.**

- P-19.10b-1, P-19.10b-3. Requires: `NoRingAfterStop` (P-19.1-2). Argument: `route` calls `stop`, and `TimerDesk::decide` removes the ring of a `Silence` plan, before they return. The `speak` future of each ring checks its ring after every wait of `queued`, just before `Home::announce`.
- P-19.10b-2. Argument: `stop` removes only the rings of the satellite of the request.
- P-19.10b-4. Evidence: test.

---

### 19.11 Early Stop Commit

**Goal:** Let "stop" on a ringing satellite use the early commit wait. The ring then stops 100 ms after the word, not 600 ms.

- **Implements:** G1, G25. **Depends on:** 19.10b.
- **Files:** modify `src/app/speech.rs`, `src/lib.rs`.
- **Quint:** `commit.qnt` (unchanged: `confident` is abstract).

**Model.**

- The audio stream has no satellite ID (sub-phase 10.7). Thus `push` sets `confident` to true also when `is_cancel(partial, dialog.cancel)` is true and `TimerDesk::ringing()` gives true. Else `confident` comes from `is_answer` and the prediction, as before.
- `run` gives the `TimerDesk` to the listen use case.

**Acceptance criteria.**

- AC-19.11-1: When a ring runs and the partial text is "stop", the commit uses `early_commit_ms`.
- AC-19.11-2: When no ring runs and no dialog is open, and the top label of "stop" is `unknown`, the commit uses `endpoint_ms`.

**Proofs.**

- P-19.11-1, P-19.11-2. Argument: `push` sets `confident = is_answer(partial, now) or (is_cancel(partial) and ringing) or model_confident`. `should_commit` does not change (P-6.5-1), so the invariants of `commit.qnt` still hold.

---

### 19.12 Timer Conformance

**Goal:** Replay the traces of `timers.qnt` against the timer use case. The Rust table, the helper states and the rings then match the model, so its invariants hold for the code on the replayed traces.

- **Implements:** G19, G25. **Depends on:** 19.11.
- **Files:** modify `src/app/timers.rs`.
- **Quint:** `timers.qnt` (conformance).

**Model.** The driver plays the part of the conversation use case and of HA. It maps `h1` and `h2` to `timer.jarvis_1` and `timer.jarvis_2`, `s1` and `s2` to the Kitchen and Office satellites, and `""` and `"chicken"` to no name and "chicken".

- `set(s, n)`: `TimerDesk::decide` with `timer_set`, the name `n`, and the duration "one minute" for `timer.jarvis_1` or "two minutes" for `timer.jarvis_2`, so the labels of two generic timers differ. `started(h)` sends the state `active` of the helper, and `startFailed(h)` runs `forget`.
- `cancel(h)`, `pause(h)`, `resume(h)`: `TimerDesk::decide` with the action and the label of the timer of `h`, then the state change of the call. For `cancel(h)`, the driver then sends `TimerEnded` with `Cancelled` in the same step, because the `cancel` of the model is one step. `outside(h)` sends the state `active`. `quietEnd(h)` sends the state `idle` with no `TimerEnded`.
- `finish(h)` and `haCancel(h)`: the state `idle`, then `TimerEnded` with `Finished` or `Cancelled`. `reconnect`: `Snapshot` with the states of the model. `restart`: a new `TimerDesk` with the same `FakeTimerStore`, then `Snapshot`.
- `ringOnce(h)`: virtual time advances to the next ring, and the satellite is idle. `stop(s)`: `TimerDesk::stop` of the satellite of `s`. `silence(h)`: `TimerDesk::decide` with `timer_cancel` and the name of the ring of `h`.
- After each step, the driver compares the table (helper, satellite, name, order and `starting`) and the rings (satellite and left rings) with the model. A held start call stays across `reconnect`. `restart` drops it.
- This sub-phase changes `connect` of `timers.qnt` (sub-phase 19.1) to the rule of sub-phase 19.9: `reconnect` keeps an entry that starts and keeps `starting`, and `restart` first sets `starting := false` on each entry. The invariants do not change.

**Acceptance criteria.**

- AC-19.12-1: When `quint-connect` replays 100 traces of `timers.qnt`, the timer table, the helper states and the rings match the model after each step.

**Proofs.**

- P-19.12-1. Evidence: test. With P-19.1-2, it gives `UniqueNames`, `StartOnFreeHelper`, `RingsBounded` and `NoRingAfterStop` for the Rust code on the replayed traces.

---

### 19.12b Timer Ring Model

**Goal:** Add the labels of the timers and the time of the rings to `timers.qnt`, and let a cancel by name stop the newest ring with that name. The model then has the cancel of a ringing timer (sub-phase 19.10b) and the end of a ring at `RING_FOR` (sub-phase 19.10), as the code does.

- **Implements:** G19, G25. **Depends on:** 19.12.
- **Files:** modify `quint/timers.qnt`, `src/app/timers.rs`.
- **Quint:** `timers.qnt` (the rules of `cancel`, `ringOnce` and `silence`, and conformance).

**Model.**

- Constants: `RING_INTERVAL = 15` and `RING_FOR = 120`, in seconds, as in `src/app/timers.rs`. `DURATIONS = Map("h1" -> "one minute", "h2" -> "two minutes")`: the duration of a generic timer on each helper, as the driver of sub-phase 19.12 sets it.
- State: `now: int` is the virtual time in seconds, from 0 at `init`. A ring gets `order` (the order of the ended entry) and `start` (`now` at its `finish`).
- The label of an entry or a ring on `h` is its name, or `DURATIONS[h]` when the name is `""` (sub-phase 19.4). A ring is live when `left > 0`.
- `cancel(h)` also needs that no live ring has the label of the entry of `h`. Else the cancel by that label gives `Silence` (sub-phase 19.10b), which is `silence`.
- `silence(h)` (when `ring[h]` is live, and no other live ring with the same label has a higher `order`): a request cancels the ringing timer by its label. This replaces the condition of sub-phase 19.1 that the name is not `""`, so a generic ring also gets `silence`.
- `ringOnce(h)`: the time of the ring is `t`, the later of `now` and `start + RING_INTERVAL * rung`. The action needs `left > 0` and `t < start + RING_FOR` (the check of the ring task, sub-phase 19.10). It sets `now := t`. Each other ring with `start + RING_FOR <= t` gets `left := 0`, not `stopped`: its ring task ends at its next check. This replaces the last two sentences of the third item of the model of sub-phase 19.10.
- `TimerDesk` gives `plan` the rings in the `order` of their entries. Thus `meant` (sub-phase 19.4c) takes the newest ring with the label, also when two rings have the same name. Before, the rings came in entity ID order.
- The driver of sub-phase 19.12 changes:
  - `silence(h)`: `TimerDesk::decide` with `timer_cancel` and the label of the ring of `h`.
  - `ringOnce(h)`: after the ring, the driver ends each ring of `TimerDesk` whose check (`TimerDesk::rings`) fails, as its ring task does.
- The invariants do not change. P-19.1-2 still holds: the end at `RING_FOR` only sets `left := 0`.

**Acceptance criteria.**

- AC-19.12b-1 [Quint]: The test `ac_19_12b_1_cancel_meets_the_ring` passes: when a generic timer on `h1` rings and a new generic timer on `h1` is `active`, `cancel(h1)` is not enabled. After `silence(h1)`, the ring is `stopped` and `cancel(h1)` is enabled.
- AC-19.12b-2 [Quint]: The test `ac_19_12b_2_newest_ring` passes: when "chicken" with the order 1 rings on `h2` and "chicken" with the order 2 rings on `h1`, `silence(h2)` is not enabled, and `silence(h1)` is. After `silence(h1)`, `silence(h2)` is enabled.
- AC-19.12b-3 [Quint]: The test `ac_19_12b_3_ring_for` passes: both timers finish at the time 0, `h2` rings 8 times, a new timer on `h2` finishes and rings 2 times, and then `ringOnce(h1)` is not enabled and the ring of `h1` has `left = 0`.
- AC-19.12b-4: When "chicken" with the order 1 rings on `timer.jarvis_2` and "chicken" with the order 2 rings on `timer.jarvis_1`, "cancel the chicken timer" removes the ring of `timer.jarvis_1`, and the ring of `timer.jarvis_2` stays.
- AC-19.12b-5: When `quint-connect` replays the test `ac_19_12b_3_ring_for` against the driver, the timer table, the helper states and the rings match the model after each step.

**Proofs.**

- P-19.12b-4. Argument: `TimerDesk` gives `plan` the rings in increasing `order`, and the loop of `meant` runs from the last candidate and keeps the first one with the label that `fill` gives. Thus `Silence` is on the ring with the highest `order`.
- P-19.12b-1 to P-19.12b-3. Evidence: model check.
- P-19.12b-5. Evidence: test.

## Field Log

These steps are not tests and gate nothing. Use the Office satellite.

1. In HA, create the timer helpers "Jarvis 1" to "Jarvis 5" with "Restore" on. Make sure that their entity IDs are `timer.jarvis_1` to `timer.jarvis_5`.
2. Say "Set a timer for one minute for the eggs." Make sure that `timer.jarvis_1` is `active` in HA. After one minute, make sure that the satellite says "Your eggs timer is done." about every 15 s. Say "Hey Jarvis, stop." Make sure that the ring stops. Record whether the wake word works while the ring plays.
3. Say "Set a timer for ten minutes" and "Set a timer for five minutes for the chicken". Ask "How long is left on the timer?". Make sure that the answer names both timers. Say "Cancel that timer". Make sure that the question names both timers. Answer "The chicken", and make sure that HA cancels that helper.
4. Say "Set a timer" and answer "two minutes". Say "Pause the timer" and "Resume the timer". Make sure that HA shows `paused` and then `active`. Set a one-minute timer and do not say "stop". Make sure that the satellite says it 8 times, and the last one within 2 minutes.
5. Set a two-minute timer and restart Jarvis. Make sure that the satellite rings at the end of the timer.
