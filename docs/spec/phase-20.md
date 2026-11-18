# Phase 20: Stop and Mute

This phase adds "stop" and mute (decision 36). "Stop" or "never mind" ends all work of Jarvis on the satellite: the ring of a timer, the open question, the queued announces and the System Two and fetch jobs. Jarvis says "OK." and is quiet. Only a bare "stop" when Jarvis does nothing on the satellite pauses the music in the room. "Be quiet" or "mute" turns on the mute switch of the satellite through HA, as the training pause does (decision 12), for a time or until you unmute it. The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 19.

---

### 20.1 Quiet Quint Model

**Goal:** Add `quint/quiet.qnt`, the model of stop and mute on two satellites. Prove that no work that a stop ended acts after it, that a bare stop pauses the music only when Jarvis does nothing, and that Jarvis unmutes only its own timed mute.

- **Implements:** G19, G26. **Depends on:** 19.8.
- **Files:** create `quint/quiet.qnt`.
- **Quint:** `quiet.qnt`.

**Model.**

- Constants: `SATELLITES = Set("s1", "s2")`, `REQUESTS = 1.to(3)`.
- State:
  - For each satellite: `gen`, `dialog`, `ringing`, `interrupted` (the wake word cut a reply), `muted` (the state of the mute switch), `mutedBy` (`jarvis`, `user` or `none`) and `timed` (an unmute task of Jarvis waits).
  - For each request: `satellite`, `running`, `jobGen`, `acts` (its announces, questions and calls), `stopped` and `actsAtStop`.
  - `nextRequest`, `pauses: List[bool]` (for each music pause, whether Jarvis was busy) and `foreignUnmutes` (the unmutes of Jarvis while `mutedBy` was not `jarvis`).
- Actions:
  - `request(s)` (when `not muted[s]` and `nextRequest` is in `REQUESTS`): a request `r` with a deferred job. `gen[s] += 1`, `running[r] := true`, `jobGen[r] := gen[s]`, `dialog[s] := false`.
  - `jobAct(r)` (when `running[r]`): when `jobGen[r] = gen[satellite[r]]`, `acts[r] += 1`, else `running[r] := false`. `jobEnd(r)`: `running[r] := false`.
  - `ask(s)`, `ring(s)` and `interrupt(s)` set `dialog`, `ringing` and `interrupted` of `s`. `ringEnd(s)` and `speechEnd(s)` clear `ringing` and `interrupted`.
  - `stop(s)` (when `not muted[s]`): `busy` is `dialog[s]`, `ringing[s]`, `interrupted[s]`, or a running request of `s` with `jobGen = gen[s]`. When `busy`, each such request gets `stopped` and `actsAtStop := acts`, `gen[s] += 1`, and `dialog`, `ringing` and `interrupted` of `s` become false. Else `busy` joins `pauses`.
  - `mute(s, t)` (when `not muted[s]`): the abort of `stop` when `busy`, with no pause. `muted[s] := true`, `mutedBy[s] := jarvis`, `timed[s] := t`.
  - `unmuteDue(s)` (when `timed[s]`): `timed[s] := false`. When `muted[s]`: if `mutedBy[s] != jarvis`, `foreignUnmutes += 1`. `muted[s] := false`, `mutedBy[s] := none`.
  - `userUnmute(s)` (the button, the HA switch or the Jarvis tab): `muted[s] := false`, `mutedBy[s] := none`, `timed[s] := false`. The unmute task sees the switch `off` and ends. `userMute(s)`: `muted[s] := true`, `mutedBy[s] := user`.
- Invariants:
  - `NothingAfterStop`: each request with `stopped` has `acts = actsAtStop`.
  - `PauseOnlyWhenIdle`: no entry of `pauses` is true.
  - `UnmuteOnlyOwnMute`: `foreignUnmutes = 0`. Without the `timed[s] := false` of `userUnmute`, a timed unmute of Jarvis ends a later mute of the user.

**Acceptance criteria.**

- AC-20.1-1 [Quint]: `quint typecheck` passes.
- AC-20.1-2 [Quint]: The three invariants hold in simulation and to 10 steps in `quint verify`.
- AC-20.1-3 [Quint]: The test `ac_20_1_3_stop_ends_job` passes: after `request(s1)` and `stop(s1)`, `jobAct` of the request does not increase `acts`, and `pauses` is empty.
- AC-20.1-4 [Quint]: The test `ac_20_1_4_user_mute_kept` passes: after `mute(s1, true)`, `userUnmute(s1)` and `userMute(s1)`, `unmuteDue(s1)` is not enabled, and `muted[s1]` stays true.

**Proofs.**

- P-20.1-2. `NothingAfterStop`: `stop` and `mute` increase `gen` when they set `stopped`, and `jobAct` acts only with `jobGen = gen`. `gen` only grows, so the request never acts again. `PauseOnlyWhenIdle`: `stop` appends to `pauses` only in the branch where `busy` is false. `UnmuteOnlyOwnMute`: `mutedBy` becomes `user` only in `userMute`. `timed` becomes true only in `mute`, which sets `mutedBy := jarvis`, and `userUnmute` sets it to false. Thus `timed[s]` and `muted[s]` imply `mutedBy[s] = jarvis`.
- P-20.1-1, P-20.1-3, P-20.1-4. Evidence: model check.

---

### 20.2 Stop Rules

**Goal:** Add the pure stop rule and the interrupted speech check. Jarvis can then tell an abort from a music pause and from a quiet "OK".

- **Implements:** G26. **Depends on:** 20.1.
- **Files:** modify `src/domain/dialog.rs`.

**Model.**

- `Stop { Abort, Pause, Quiet }`. `stop(text, cancel, busy) -> Option<Stop>`: `None` when `is_cancel(text, cancel)` is false (sub-phase 10.3). Else `Abort` when `busy`, `Pause` when the normalized text is "stop", and `Quiet` for each other cancel phrase.
- `interrupted(old, new) -> bool`: `old` is `responding` and `new` is `listening`. An `assist_satellite` entity is `responding` while it plays a reply or an announce. The Satellite1 stops its speech at once when it hears the wake word again (a known fact, from you on 2026-11-17), and the new pipeline sets `listening`. Thus a stop does not stop the audio: it only ends the work of Jarvis.

**Acceptance criteria.**

- AC-20.2-1: `stop` gives `Abort` for "never mind" and "stop" when `busy`, `Pause` for "Stop." when not `busy`, `Quiet` for "never mind" when not `busy`, and `None` for "stop the music".
- AC-20.2-2: `interrupted` gives true for `responding` to `listening`, and false for `idle` to `listening` and for `responding` to `idle`.

**Proofs.**

- P-20.2-1. Requires: P-10.3-1. Ensures: `stop` is `None` if and only if `normalize(text)` is not a cancel phrase. Else it is `Abort` exactly when `busy`, and `Pause` exactly when `busy` is false and `normalize(text) = "stop"`. Argument: `stop` checks `is_cancel`, then `busy`, then the text.
- P-20.2-2. Ensures: `interrupted(old, new)` is `old.state = "responding" && new.state = "listening"`. Argument: one comparison.

---

### 20.3 Satellite Work

**Goal:** Know whether Jarvis does something on a satellite: an open dialog, a ring, a deferred job or a queued announce of the current generation, or a reply that the wake word cut.

- **Implements:** G26. **Depends on:** 20.2.
- **Files:** modify `src/app/conversation.rs`, `src/app/background.rs`, `src/app/timers.rs`, `src/lib.rs`.

**Model.**

- `Queues` counts the work of each satellite. Each deferred job and each queue item registers with its generation when it starts and leaves when it ends, also on an error. `live(satellite)` is true when registered work has the current generation of the satellite. Work of an older generation stops before its next step (sub-phase 10.5b), so it does not count.
- `sync` (sub-phase 2.9) gives each `StateChanged` of an `assist_satellite` entity to `Queues::observe(old, new)`. At each change to `listening`, it keeps `interrupted(old, new)` for the satellite (sub-phase 20.2).
- `Timers::ringing(satellite)` is true when a ring of the satellite is active (sub-phase 19.7).
- `Queues::busy(satellite, key, now)` is true when the key has an open dialog (`timeout_s` did not pass, sub-phase 10.2), `live` is true, `ringing` is true, or the satellite is `interrupted`.

**Acceptance criteria.**

- AC-20.3-1: With no work, `busy` is false. While a System Two job of the satellite waits for `FakeReasoner`, `busy` is true. After the job ends, `busy` is false.
- AC-20.3-2: While a fetch answer waits in the queue for `idle`, `busy` is true. After a second request on the satellite, the old job does not count.
- AC-20.3-3: With an open dialog, `busy` is true, and after `timeout_s`, it is false. During a ring, `busy` is true.
- AC-20.3-4: After `FakeHome` sends the satellite from `responding` to `listening`, `busy` is true. After `idle` to `listening`, it is false.

**Proofs.**

- P-20.3-1, P-20.3-2. Argument: the guard of each job and item leaves on each path out of it, and `live` compares the registered generation with the current one.
- P-20.3-3, P-20.3-4. Argument: `busy` is the disjunction of the four checks. `take` gives no dialog after the timeout (P-10.2-2). `observe` uses `interrupted` (P-20.2-2).

---

### 20.4 Stop Routing

**Goal:** Make "stop" and "never mind" end all work of the satellite, and make a bare "stop" pause the music when Jarvis does nothing.

- **Implements:** G13, G26. **Depends on:** 20.3.
- **Files:** modify `src/app/conversation.rs`, `jarvis.toml`, `responses.toml`, `README.md` (the stop requests, `CLAUDE.md` workflow item 15).

**Model.**

- `handle` runs `stop(text, dialog.cancel, busy)` first, before the dialog routing. This replaces routing step 1 of sub-phase 10.5 and the cancel phrase check of sub-phase 19.7.
  - `Abort`: `Timers::stop(satellite)`, close the dialog of the key, and reply from `cancelled`. The dispatch of the reply increases the generation (sub-phase 4.9c). Thus queued announces drop, and deferred jobs stop before their next announce, question or call (sub-phase 10.5b). A call that started is not undone.
  - `Pause`: with a satellite, `decide` with the prediction `music_pause` 1.0 and no slots, in the area of the satellite, and dispatch the result. Thus the speaker of the area pauses (sub-phase 18.8). Without a satellite, `Quiet`.
  - `Quiet`: reply from `cancelled`, with no call.
- "Stop the music" is not a cancel phrase, so the System One model still gives `music_pause`. A `timer_cancel` with no name during a ring still stops the ring (sub-phase 19.7).
- The pool `cancelled` becomes "OK.". `dialog.cancel` gets "nevermind".

**Acceptance criteria.**

- AC-20.4-1: When the user says "stop" while a System Two job of the satellite waits, the reply is "OK.", and `FakeHome` records no announce and no call of that job after it.
- AC-20.4-2: When a fetch answer waits in the queue and the user says "never mind", `FakeHome` records no announce of it.
- AC-20.4-3: When a dialog is open and the user says "stop", the reply is "OK.", and the next answer "kitchen" is a new request.
- AC-20.4-4: When Jarvis does nothing on the Kitchen satellite and the user says "stop", `FakeHome` records `media_player.media_pause` on `media_player.carson_kitchen`. When the user says "never mind", the reply is "OK.", with no call.
- AC-20.4-5: When the satellite went from `responding` to `listening` and the user says "stop", the reply is "OK.", with no call.
- AC-20.4-6: When `jarvis.toml` and `responses.toml` are read, `dialog.cancel` has "nevermind", and the pool `cancelled` has only "OK.".

**Proofs.**

- P-20.4-1, P-20.4-2. Requires: `NothingAfterStop`, `NoCallAfterCancel` (P-10.1-2). Argument: the `Abort` arm closes the dialog and dispatches the reply, which increases the generation. Each job and item compares the generation before it acts (P-4.9c-3, P-10.5b-1).
- P-20.4-4, P-20.4-5. Requires: `PauseOnlyWhenIdle`. Argument: `handle` matches on `stop` (P-20.2-1), and only the `Pause` arm calls `decide`.
- P-20.4-3, P-20.4-6. Evidence: test.

---

### 20.5 Mute Intent

**Goal:** Add the `mute` intent with an optional duration, and the mute switch of a satellite. "Mute for 10 minutes" then gives a mute of 600 s for the satellite of the request.

- **Implements:** G3, G26. **Depends on:** 20.4.
- **Files:** modify `src/domain/catalog.rs`, `src/domain/decide.rs`, `src/domain/state_cache.rs`, `src/adapters/onnx.rs`, `intents.toml`, `responses.toml`.

**Model.**

- `Kind::Mute`. `validate_catalog` checks that a `mute` intent has no `service`. The intent `mute` goes at the end of `intents.toml`, with `optional = ["duration"]`. Examples: "be quiet", "mute", "mute yourself", "stop listening", "mute for [10 minutes](duration)", "be quiet for [an hour](duration)", "mute until i unmute you". Pool: `{% if duration %}Muting for {{ duration }}.{% else %}Muting until you unmute me.{% endif %}`
- `decide` gives `Decision::Mute { reply, seconds: Option<u32> }` for a confident `mute` intent, before the area rules. `seconds` is `seconds` of the `duration` slot (sub-phase 19.2). A `duration` slot that gives `None` gives `Defer` with `Job::Llm`.
- `mute_switch(cache, satellite) -> Option<EntityId>` in `src/domain/state_cache.rs`: the switch of the rule of sub-phase 13.10c on the device of the satellite entity, in any state.

**Acceptance criteria.**

- AC-20.5-1: When the prediction is `mute` 0.9 with `duration = 10 minutes`, the result is `Mute` with `seconds = 600` and the reply "Muting for 10 minutes." With no slot, `seconds` is `None` and the reply is "Muting until you unmute me." With "10 bananas", the result is `Defer` with `Job::Llm`.
- AC-20.5-2: `mute_switch` of `assist_satellite.carson_kitchen` gives `switch.carson_kitchen_mute_microphones`. For a satellite whose device has no mute switch, it gives `None`.
- AC-20.5-3: When the fixture model gets "be quiet" and "mute for 10 minutes" with the context `area: none`, the top label is `mute` with a probability of `model.threshold` or more, and the second has `duration = 10 minutes`. `validate_catalog` and `validate_pools` return `Ok` for `intents.toml` and `responses.toml`.

**Proofs.**

- P-20.5-1. Ensures: a top `mute` intent with `p(top) >= threshold` gives `Mute` with `seconds = seconds(slot)` when the slot exists and parses, `None` with no slot, and `Defer` with `Job::Llm` when it does not parse. Argument: `decide` matches `Kind::Mute` before the area rules (P-19.2-1).
- P-20.5-2. Ensures: `mute_switch` is `Some(id)` if and only if `id` is a `switch` that ends in `_mute_microphones` on the device of the satellite. Argument: one pass over the entities, as `mute_switches` (P-13.10c-1).
- P-20.5-3. Evidence: test.

---

### 20.6 Mute Use Case

**Goal:** Mute the microphones of the satellite of the request through HA. "Mute for 10 minutes" unmutes itself after 10 minutes, unless you unmuted it before.

- **Implements:** G26. **Depends on:** 20.5.
- **Files:** modify `src/app/conversation.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `README.md` (the mute requests).

**Model.**

- `Mute` with a satellite and a mute switch: Jarvis ends the work of the satellite as the `Abort` of sub-phase 20.4, returns the reply, and the background work calls `switch.turn_on` on the switch.
- With `seconds`, the background work starts the unmute task of the satellite and ends an older one. The task waits `seconds`, or until the hot cache shows the switch `off`. After the wait, when the switch is `on`, it calls `switch.turn_off`. Thus a button or HA unmute ends the task (`UnmuteOnlyOwnMute`).
- Without a satellite, or without a mute switch, the reply is from `failure`, with no call.
- A local training run mutes only the switches that are `off` and unmutes only those (sub-phase 13.10e), so it keeps your mute. A timed unmute during a run makes the satellite listen, and Jarvis answers after the run.
- A ring still plays on a muted satellite, because the switch mutes only the microphones. The unmute task does not survive a restart of Jarvis: the satellite then stays muted until you unmute it.

**Acceptance criteria.**

- AC-20.6-1: When the request is `mute` with `seconds = 600` from the Kitchen satellite, `handle` returns "Muting for 10 minutes.", and `FakeHome` records `switch.turn_on` on `switch.carson_kitchen_mute_microphones`, then `switch.turn_off` 600 s later in virtual time, and not before.
- AC-20.6-2: When the hot cache shows the switch `off` 300 s after the mute and `on` again at 400 s, `FakeHome` records no `switch.turn_off`.
- AC-20.6-3: When the mute has no `seconds`, `FakeHome` records no `switch.turn_off` in one day of virtual time. When a second mute of 300 s comes 60 s after a mute of 600 s, `FakeHome` records one `switch.turn_off`, at 360 s.
- AC-20.6-4: When the satellite has no mute switch, the reply is from `failure`, with no call.
- AC-20.6-5: When the user says "mute" while a System Two job of the satellite waits, `FakeHome` records no announce of that job after it.

**Proofs.**

- P-20.6-2, P-20.6-3. Requires: `UnmuteOnlyOwnMute`. Argument: the task ends when the switch is `off`, and a new task ends the old one before it starts.
- P-20.6-4. Argument: the arm calls `Home` only inside the `Some` of both the satellite and `mute_switch`.
- P-20.6-5. Requires: `NothingAfterStop`. Argument: the mute uses the `Abort` path (P-20.4-1).
- P-20.6-1. Evidence: test.

---

### 20.7 Unmute in the Jarvis Tab

**Goal:** Show each satellite with its mute state in the Jarvis tab, with an unmute button. A muted satellite does not hear you, so the tab is one way to unmute it.

- **Implements:** G24, G26. **Depends on:** 20.6.
- **Files:** modify `src/ports.rs`, `src/lib.rs`, `src/adapters/web.rs`, `src/adapters/review.html`, `src/testing/fakes.rs`, `README.md`.

**Model.**

- `Control` gets `satellites() -> Vec<Satellite>` and `unmute(satellite) -> Result<bool>`. `Satellite { satellite, name, muted: Option<bool> }`. `muted` is `None` when the satellite has no mute switch or its state is not `on` or `off`.
- In `src/lib.rs`, `satellites` reads the `assist_satellite` entities and `mute_switch` (sub-phase 20.5) from the hot cache. `unmute` ends the unmute task of the satellite and calls `switch.turn_off`. A satellite without a mute switch gives `false`.
- `GET api/satellites` returns the list. `POST api/satellites/{satellite}/unmute` returns 204, or 404 for `false`.
- The page has `#satellites` with `template#satellite`, `.name`, `.muted` and `button.unmute`, which shows only for a muted satellite. The page reads `api/satellites` with `api/status` (sub-phase 12.9b).

**Acceptance criteria.**

- AC-20.7-1: When `satellites` reads the reference house, the four satellites have `muted` true, true, false and `None`, as their mute switches (`01-architecture.md` section 11).
- AC-20.7-2: When `unmute` runs for the Kitchen satellite during a timed mute, `FakeHome` records `switch.turn_off` on its switch once, and no later `switch.turn_off` of the task.
- AC-20.7-3: When `FakeControl` gives two satellites, `GET api/satellites` returns both. `POST api/satellites/assist_satellite.carson_kitchen/unmute` returns 204, and `FakeControl` records it. For `false`, the result is 404.
- AC-20.7-4: When `GET /` runs, the HTML contains `#satellites`, `template#satellite` and `button.unmute`. The checks of AC-12.9-2 and AC-12.9-3 pass.

**Proofs.**

- P-20.7-2. Requires: `UnmuteOnlyOwnMute`. Argument: `unmute` ends the task before the call.
- P-20.7-1, P-20.7-3, P-20.7-4. Evidence: test.

---

### 20.8 Quiet Conformance

**Goal:** Check stop and mute against `quiet.qnt`, so the Rust code ends the work of a satellite and unmutes as the model does.

- **Implements:** G19, G26. **Depends on:** 20.7.
- **Files:** modify `src/app/conversation.rs`.
- **Quint:** `quiet.qnt` (conformance).

**Model.** The driver maps the model to `handle` on two satellites with the port fakes:

- `request(s)`: a request with the top label `unknown`, whose System Two job waits. `jobAct(r)`: `FakeReasoner` gives one sentence. `jobEnd(r)`: it gives `Done`.
- `ask(s)`: a request that asks "Which room?". `ring(s)`: a timer of `s` finishes. `ringEnd(s)`: the ring reaches its limit. `interrupt(s)` and `speechEnd(s)`: `FakeHome` sends the satellite from `responding` to `listening`, or to `idle`.
- `stop(s)`: "stop". `mute(s, t)`: "mute for 1 minute" or "mute". `unmuteDue(s)`: virtual time advances 60 s.
- `userUnmute(s)` and `userMute(s)`: `FakeHome` sends the mute switch `off` or `on`.
- After each step, the driver compares the announces and calls of each request, the media pauses and the mute calls of `FakeHome` with the model.

**Acceptance criteria.**

- AC-20.8-1: When `quint-connect` replays 100 traces of `quiet.qnt`, the announces, the calls, the pauses and the mute state match the model after each step.

**Proofs.**

- P-20.8-1. Evidence: test. With P-20.1-2, it gives the three invariants for the Rust code on the replayed traces.

## Field Log

These steps are not tests and gate nothing.

1. Ask a question that System Two answers. During the answer, say "Hey Jarvis, stop". Confirm that the satellite stops the audio at once, and record that the satellite entity goes from `responding` to `listening`, that Jarvis says "OK.", and that no more answer follows.
2. Play music in the Office. Say "Hey Jarvis, stop". Record that the music pauses.
3. Say "Mute for 1 minute". Record that the mute switch of the satellite is `on`, that the wake word does nothing, and that the switch is `off` after 1 minute.
4. Say "Mute". Unmute in the Jarvis tab. Record that the satellite hears the wake word again.
