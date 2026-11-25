# Phase 20: Stop and Mute

This phase adds a general "stop" and a voice mute. "Stop" or "never mind" on a satellite ends what Jarvis does there, and Jarvis says "Okay.". A bare "stop" when Jarvis does nothing there pauses the music of the room. "Mute" stops Jarvis and turns on the microphone mute of that satellite through HA (`01-architecture.md` decision 37). The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 19.

You chose these rules (2026-11-12 and 2026-11-17):

- "Stop" or "never mind" ends everything that Jarvis does on that satellite: its speech, its queued announces, its pending System Two and fetch work, its open question and its ringing timer. Jarvis says a short "Okay." and goes quiet. The Satellite1 stops its own speech when it hears the wake word, so a barge-in already ends the speech.
- A bare "stop" first stops Jarvis. Only when Jarvis did nothing on that satellite does it pause the music in that room, as `music_pause` does.
- "Mute", "be quiet", "mute for ten minutes" and "mute until I unmute" stop Jarvis and turn on the mute switch of that satellite only, so it stops listening. You unmute with the button of the satellite, the switch in HA or the Jarvis tab. "Mute for ten minutes" also unmutes after ten minutes.
- Stop and mute are local: cancel phrases, an intent and state, not a plugin.

Each request on a satellite already increases its generation. Thus the request drops the queued announces of the satellite, stops its System Two and fetch jobs at their next step, and closes its dialog (sub-phases 10.5b and 16.1b). "Stop" adds the stop of the ring, the reply and the music rule. A stop is a request of `jarvis.qnt` and a cancel of `dialog.qnt`, so those models do not change. The mute state and its end get `quint/mute.qnt`.

---

### 20.1 Stop Route

**Goal:** Route each cancel phrase on a satellite locally, also with no dialog and no ring. "Never mind" then never goes to System One or System Two, and Jarvis says "Okay.".

- **Implements:** G26. **Depends on:** 19.12b.
- **Files:** modify `src/app/conversation.rs`, `responses.toml`, `README.md` ("stop" and "never mind", `CLAUDE.md` workflow item 15).

**Model.**

- For a request with a satellite, `route` checks `is_cancel(text, dialog.cancel)` (sub-phase 10.3) first. When it is true, `route` calls `TimerDesk::stop` for the satellite and replies from `cancelled` with no record. The dialog that `handle` took stays closed. `route` runs no prediction. This replaces step 1 of sub-phase 10.5 and the ring check of sub-phase 19.10b for a request with a satellite. A request without a satellite keeps step 1 of sub-phase 10.5.
- `handle` increased the generation of the satellite before `route` (sub-phase 16.1b). Thus the stop also drops the queued announces, questions and `failure` answers of the satellite, and its System Two and fetch jobs make no further announce, question or call. A call that already started is not undone.
- The pool `cancelled` becomes `["Okay."]`.

**Acceptance criteria.**

- AC-20.1-1: When the user says "never mind" on the Kitchen satellite with no dialog and no ring, `handle` returns "Okay.", the interaction log gets no record, and `FakeReasoner` and `FakeHome` record no call.
- AC-20.1-2: When a System Two job of the Kitchen satellite waits in `FakeReasoner` and the user says "stop" there, `handle` returns "Okay.", and `FakeHome` records no announce of the job after `FakeReasoner` answers.
- AC-20.1-3: When the user says "stop" on the Kitchen satellite after the second ring announce, `handle` returns "Okay.", the interaction log gets no record, and `FakeHome` records no further announce. This replaces AC-19.10b-1.
- AC-20.1-4: When the request has no satellite and no dialog, and the user types "never mind", the reply comes from the System One prediction, as before.

**Proofs.**

- P-20.1-1, P-20.1-3. Requires: `NoRingAfterStop` (P-19.1-2). Argument: the cancel branch of `route` returns before the prediction and before any dispatch, and it calls `TimerDesk::stop` (P-19.10b-1).
- P-20.1-2. Requires: `NoStaleAnnounce` (P-4.7-4). Argument: `handle` increased the generation before `route`, and the job compares its generation before each announce (P-10.5b-1).
- P-20.1-4. Argument: the cancel branch needs a satellite.

---

### 20.2 Satellite Work

**Goal:** Let Jarvis know when it does something on a satellite: a live job, an item in its queue, or speech in the last 10 s. Sub-phase 20.3 then pauses the music only when Jarvis does nothing there.

- **Implements:** G26. **Depends on:** 20.1.
- **Files:** modify `src/app/conversation.rs`.

**Model.**

- `Queues` counts the live work of each satellite by generation. `Queues::work(satellite, generation) -> Work` adds one, and the drop of `Work` removes it. The `after` work of a `Defer` decision holds a `Work` from `dispatch` to its end. Each item of `Queues::queued` holds one from its call to its end, also a ring.
- `Queues` keeps the virtual time (`tokio::time::Instant`) of the last speech of each satellite: when `handle` returns a reply with text for a request with the satellite, and when an announce or a question of its queue ends, with success or an error. The `cancelled` reply of sub-phase 20.1 is not speech. Thus a second "stop" after "Okay." can pause the music.
- `Queues::busy(satellite) -> bool`: a `Work` with the current generation of the satellite exists, or the last speech is less than `SPOKE_WITHIN` (10 s, a constant in `src/app/conversation.rs`) ago. When the wake word stops an announce, either HA ends the announce call, which writes the time, or the item stays live. Thus `busy` is true in both cases. A reply plays after `handle` returns, so 10 s also covers a reply of a few seconds.

**Acceptance criteria.**

- AC-20.2-1: When a System Two job of the Kitchen satellite waits in `FakeReasoner` 30 s after its filler, `busy` gives true for the Kitchen satellite and false for the Office satellite. After the job ends and 10 s pass, `busy` gives false.
- AC-20.2-2: When a `failure` answer waits in the queue of the Kitchen satellite for `idle` 30 s after the reply, `busy` gives true.
- AC-20.2-3: When a reply with text returned 9 s ago, `busy` gives true. At 10 s, it gives false. After the `cancelled` reply, `busy` gives false.
- AC-20.2-4: When a newer request increased the generation of the satellite and a job of the older request still waits, `busy` gives false 10 s after the newer reply.

**Proofs.**

- P-20.2-1 to P-20.2-4. Argument: `Work` adds and removes its count in `new` and `drop`, `busy` reads only the count of the current generation, and only the two kinds of speech write the time.

---

### 20.3 Stop Pauses the Music

**Goal:** Let a bare "stop" pause the music of the room when Jarvis does nothing on the satellite. "Stop" then still works when the only sound is the music.

- **Implements:** G26. **Depends on:** 20.2.
- **Files:** modify `src/app/conversation.rs`, `README.md` ("stop" pauses the music, `CLAUDE.md` workflow item 15).

**Model.**

- `handle` reads `busy` for the satellite before it increases the generation, and keeps it in `heard`.
- The cancel branch of sub-phase 20.1 changes for the normalized text "stop": when `busy` was false, `handle` took no dialog, and `TimerDesk::stop` gives false, Jarvis decides the prediction `music_pause` with the probability 1 and no slots, in the area of the request. This is the decision of a filled dialog (sub-phase 10.5). A `Reply` goes through `act` and `dispatch`, and Jarvis records it. Any other decision, for example `Defer` for an area without a speaker, gives the `cancelled` reply with no record. Thus "stop" never goes to System Two.
- The other cancel phrases ("never mind", "cancel", "forget it") never pause the music.

**Acceptance criteria.**

- AC-20.3-1: When the Kitchen satellite has no work, no dialog and no ring, and no speech in the last 10 s, "stop" there returns "Pausing the music.", and `FakeHome` records `media_player.media_pause` on `media_player.carson_kitchen` after the reply.
- AC-20.3-2: When a reply on the Kitchen satellite returned 5 s ago, "stop" there returns "Okay.", and `FakeHome` records no call.
- AC-20.3-3: When the Kitchen satellite has no work, "never mind" there returns "Okay.", and `FakeHome` records no call.
- AC-20.3-4: When the area of the satellite has no speaker, "stop" returns "Okay.", and `FakeReasoner` records no call.

**Proofs.**

- P-20.3-1 to P-20.3-4. Argument: the music branch needs the text "stop", no work, no dialog and no stopped ring, all read before the generation increase. Only a `Reply` decision of `music_pause` gets dispatched. Each other decision gives `cancelled`.

---

### 20.4 Mute Quint Model

**Goal:** Add `quint/mute.qnt`, the model of the mute switches, the voice mutes and their end. Prove that Jarvis never unmutes a switch that a person changed after the mute, never before the end, and never during a local training run, before the Rust code exists.

- **Implements:** G19, G26. **Depends on:** 20.3.
- **Files:** create `quint/mute.qnt`.
- **Quint:** `mute.qnt`.

**Model.**

- Constants: `SATELLITES = Set("s1", "s2")`, `LENGTH = 2` (the time of a timed mute, in ticks).
- State: each variable is a map with a fixed domain or a value.
  - `on: SAT -> bool` (the switch is `on`, muted) and `version: SAT -> int` (its `last_updated`: each change of the state adds 1).
  - `calling: SAT -> { live: bool, timed: bool, end: int }`: the `switch.turn_on` call of a voice mute runs.
  - `mute: SAT -> { live: bool, end: int, version: int }`: the timed voice mute of the desk.
  - `pressed: SAT -> bool`: a person changed the switch after the last mute call. `trainMuted: SAT -> bool` and `training: bool`. `now: int`.
  - History of the last step: `unmuted: SAT -> bool`, `unmutedEnd: SAT -> int`, `unmutedPressed: SAT -> bool`.
- Actions:
  - `voiceMute(s, timed)` (when not `on[s]`, not `calling[s].live` and not `training`): `calling[s] := { live: true, timed, end: now + LENGTH }`.
  - `callDone(s)` (when `calling[s].live`): `on[s] := true`, and `version[s]` adds 1 when `on[s]` was false. `pressed[s] := false`. A timed call sets `mute[s] := { live: true, end, version: version[s] }`. An untimed call sets `mute[s].live := false`. Then `due`, because the end of a timed mute can pass while its call waits: the end task of sub-phase 20.8 then unmutes at once. The desk reads the `on` state of its call milliseconds after the result, so the model makes the two one step.
  - `callFailed(s)`: `calling[s].live := false`. `press(s)`: a person changes the switch (the button, HA or the Jarvis tab). `on[s]` flips, `version[s]` adds 1 and `pressed[s] := true`.
  - `tick`: `now += 1`, then `due`. `trainStart` (when not `training`): each switch that is not `on` gets `on`, `version + 1` and `trainMuted`, and `training := true`. `trainEnd` (when `training`): each `trainMuted` switch gets `off` and `version + 1`, `training := false`, then `due`. `restart` (when not `training`): each `calling[s].live := false`, then `due`.
  - `due`: for each `s` with `mute[s].live`, `mute[s].end <= now` and not `training`: `mute[s].live := false`. When `on[s]` and `version[s] = mute[s].version`, also `on[s] := false`, `version[s] += 1`, `unmuted[s] := true`, `unmutedEnd[s] := mute[s].end` and `unmutedPressed[s] := pressed[s]`.
- `mute.qnt` is not in `composed.qnt` (sub-phase 16.3). A mute request is an ordinary request of the lifecycle.
- Invariants:
  - `NoUnmuteAfterPress`: no `s` has `unmuted[s]` and `unmutedPressed[s]`.
  - `NoEarlyUnmute`: each `s` with `unmuted[s]` has `unmutedEnd[s] <= now`.
  - `NoUnmuteInTraining`: no `s` has `unmuted[s]` while `training`.

**Acceptance criteria.**

- AC-20.4-1 [Quint]: `quint typecheck` passes.
- AC-20.4-2 [Quint]: The three invariants hold in simulation and to 10 steps in `quint verify`.
- AC-20.4-3 [Quint]: The test `ac_20_4_3_timed_mute_ends` passes: after `voiceMute("s1", true)`, `callDone("s1")` and two `tick`, `on["s1"]` is false.
- AC-20.4-4 [Quint]: The test `ac_20_4_4_person_wins` passes: after the timed mute, two `press("s1")` and two `tick`, `on["s1"]` is true. After an untimed mute and three `tick`, `on["s1"]` is true.
- AC-20.4-5 [Quint]: The test `ac_20_4_5_training_holds` passes: after the timed mute, `trainStart` and two `tick`, `on["s1"]` is true. After `trainEnd`, it is false.
- AC-20.4-6 [Quint]: The test `ac_20_4_6_restart_and_satellites` passes: after the timed mute, `restart` and two `tick`, `on["s1"]` is false. Actions on `s1` do not change `on["s2"]`.

**Proofs.**

- P-20.4-2 (AC-20.4-2). `NoUnmuteAfterPress`: `callDone` records `version` and clears `pressed` in one step, and each later `press` adds 1 to `version`. `due` unmutes only with an equal `version`, so `pressed` is false then. Training changes `version` too, so a training change also wins. `NoEarlyUnmute`: `due` unmutes only with `end <= now`, and `now` only grows. `NoUnmuteInTraining`: `due` needs not `training`, and only `trainEnd` runs `due` after it sets `training := false`.
- P-20.4-1, P-20.4-3 to P-20.4-6. Evidence: model check.

---

### 20.5 Mute Plan

**Goal:** Add the pure plan of a mute request and the rule of its end. Jarvis then knows which switch a mute turns on, for how long, and when it can unmute.

- **Implements:** G26. **Depends on:** 20.4.
- **Files:** create `src/domain/mute.rs`. Modify `src/domain/state_cache.rs`, `src/lib.rs`.
- **Quint:** `mute.qnt` (the rule of `due`).

**Model.**

- `mute_switch(cache, satellite) -> Option<EntityId>` in `src/domain/state_cache.rs`: the first mute switch (sub-phase 13.10c) on the device of the `assist_satellite` entity `satellite`, in any state.
- `MuteRequest { intent, duration: Option<String>, transcript }`. `Plan`: `Mute { switch, seconds: Option<u32> }`, `Missing` and `Unclear`.
- `plan(request, cache, satellite: Option<&str>) -> Plan`: no satellite, or no mute switch, gives `Missing`. A duration slot that `timers::duration` (sub-phase 19.2) does not read gives `Unclear`. Else `Mute` with the switch and the seconds of the slot, or `None` without a slot.
- `Mute { satellite, switch, end: Timestamp, updated: Timestamp }`: a timed voice mute. `updated` is the `last_updated` of the `on` state of its call. `unmutes(mute, cache) -> bool`: the hot cache shows `switch` `on` with `last_updated = updated`. Thus a change by a person, by training or by an HA restart wins.

**Acceptance criteria.**

- AC-20.5-1: `mute_switch` gives `switch.carson_kitchen_mute_microphones` for the Kitchen satellite of the reference house, and `None` for a satellite whose device has no mute switch.
- AC-20.5-2: `plan` gives `Mute` with that switch and 600 s for the duration "ten minutes", `Mute` with no seconds without a duration, `Unclear` for "half an hour", and `Missing` with no satellite.
- AC-20.5-3: `unmutes` gives true when the switch is `on` with the `last_updated` of the mute. It gives false when the switch is `off`, or `on` with another `last_updated`.

**Proofs.**

- P-20.5-1 (AC-20.5-1). Ensures: `Some(id)` implies that `id` is a mute switch on the device of `satellite`. `None` implies that no such switch exists. Argument: a loop over the entities, with the predicates of P-13.10c-1. Its loop invariant states that no entity before `index` matches.
- P-20.5-2 (AC-20.5-2). Ensures: `Mute { switch, seconds }` implies `Some(switch) = mute_switch(cache, satellite)`, and `seconds = Some(s)` implies a duration slot and `1 <= s <= 86_400` (P-19.2-2). Argument: `plan` checks the satellite, the switch and the slot in this order.
- P-20.5-3 (AC-20.5-3). Ensures: `unmutes` is true if and only if the cache state of `switch` is `on` and has `last_updated = updated` (the guard of `due`). Argument: one lookup and two comparisons.

---

### 20.6 Mute Intent

**Goal:** Add the intent kind `mute`, the `Mute` decision, the intent `mute` with its seed examples, and its answer. System One then classifies "mute" and "mute for ten minutes", and Jarvis knows the reply and the call.

- **Implements:** G3, G26. **Depends on:** 20.5.
- **Files:** modify `src/domain/catalog.rs`, `src/domain/decide.rs`, `src/domain/responses.rs`, `src/domain/mute.rs`, `src/app/conversation.rs`, `intents.toml`, `responses.toml`, `src/adapters/onnx.rs` (tests only).

**Model.**

- `Kind::Mute` (`kind = "mute"`). `validate_catalog` checks that a `mute` intent has no `service` and no `timer`. `decide`, step 2 of sub-phase 4.5: a confident `mute` intent gives `Decision::Mute(MuteRequest)` with its optional `duration` slot and the transcript. It needs no area.
- The intent `mute`, at the end of `intents.toml`, with `optional = ["duration"]` and the examples "mute", "be quiet", "mute for [ten minutes](duration)", "be quiet for [an hour](duration)", "mute until i unmute", "mute the microphone", "stop listening", "mute yourself for [thirty minutes](duration)".
- The pool `mute`: `{% if duration %}Muting for {{ duration }}.{% else %}Muting.{% endif %}`. The pool of a `mute` intent can use `duration` and no other variable (`pool_variables`, sub-phase 4.2).
- `answer(plan, request, pools, rng) -> Decision` in `src/domain/mute.rs`: `Mute { switch, .. }` gives `Reply` from the pool of the intent with the call `switch.turn_on` on `switch` and no data. `Missing`, `Unclear` and a pool that does not render give `Reply` from `failure` with no call. System Two cannot mute.
- Until sub-phase 20.7, `act` gives `Mute` the `Defer` with `Job::Llm`. `just fixture-model` trains the fixture model again, and the tests of AC-3.2-1 and AC-3.2c-2 get the name. When a light command of AC-9.2-5 falls under `model.commit_threshold`, the PR changes the mute examples (sub-phase 19.5).

**Acceptance criteria.**

- AC-20.6-1: When `intents.toml` and `responses.toml` are read, `validate_catalog` and `validate_pools` return `Ok`. When a `mute` intent has `service`, `validate_catalog` returns an error that names it. When the pool `mute` uses `area`, `validate_pools` returns an error with the pool and the variable.
- AC-20.6-2: When the prediction is `mute` 0.95 with `duration = ten minutes`, `decide` gives `Decision::Mute` with `Some("ten minutes")` and the transcript, also with no area.
- AC-20.6-3: When the plan is `Mute` with the Kitchen switch, `answer` gives `Reply` with "Muting for ten minutes." and the call `switch.turn_on` on the switch, or "Muting." with no duration. `Missing` and `Unclear` give "Sorry, I could not do that." with no call.
- AC-20.6-4: When the fixture model gets each example of `mute` with the context `area: none`, the top label is `mute`, and the slots are those of its markup.
- AC-20.6-5: When the fixture model gets "turn on the office lights", "office lights on" and "turn the office lights on" with the context `area: none`, the top label is `lights_on` with a probability of `model.commit_threshold` or more.

**Proofs.**

- P-20.6-2 (AC-20.6-2). Ensures: `Decision::Mute(r)` comes from a top `mute` intent with a probability of `threshold` or more. Argument: the `mute` arm of step 2 reads the optional slot (P-4.5d-3) and not the area.
- P-20.6-3 (AC-20.6-3). Ensures: a `Reply` with a call comes only from `Mute`, and its call is `switch.turn_on` on the switch of the plan. Argument: `answer` builds a call only in that arm. The texts are evidence by test (`minijinja`).
- P-20.6-1, P-20.6-4, P-20.6-5. Evidence: test.

---

### 20.7 Mute Desk

**Goal:** Add `MuteDesk` and give it the mute requests. "Mute" then stops Jarvis on the satellite and turns on its mute switch.

- **Implements:** G26. **Depends on:** 20.6.
- **Files:** create `src/app/mute.rs`. Modify `src/app/conversation.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `src/testing/e2e.rs`, `README.md` (mute, `CLAUDE.md` workflow item 15).

**Model.**

- `MuteDesk` holds the `Home` port and the receiver of the hot cache. `MuteDesk::decide(request, satellite, pools, rng) -> Decision` runs `plan` with the current cache and gives `answer`.
- `Conversation::act` gives a `Mute` decision to `MuteDesk::decide` with the satellite of the request. First it calls `TimerDesk::stop` for the satellite. This replaces the `Defer` of sub-phase 20.6. The `Reply` goes through `dispatch`: the reply first, then the call (`ReplyBeforeCall`). `handle` already closed the dialog and increased the generation, so a mute stops Jarvis as "stop" does.
- `run` makes the `MuteDesk` and gives it to the conversation use case.

**Acceptance criteria.**

- AC-20.7-1: When `FakeClassifier` gives `mute` on the Kitchen satellite, `handle` returns "Muting.", and `FakeHome` records `switch.turn_on` on `switch.carson_kitchen_mute_microphones` after the reply.
- AC-20.7-2: When a timer rings on the Kitchen satellite and a System Two job of it waits, "mute" there gives no further ring announce and no announce of the job. A ring on the Office satellite continues.
- AC-20.7-3: When the request has no satellite, or the satellite has no mute switch, the reply is "Sorry, I could not do that.", and `FakeHome` records no call.

**Proofs.**

- P-20.7-1. Requires: `ReplyBeforeCall` (sub-phase 16.1). Argument: the mute decision is a `Reply` with a call, and `dispatch` gives the call as `after` work (P-4.8-1).
- P-20.7-2. Requires: `NoRingAfterStop`, `NoStaleAnnounce`. Argument: `act` calls `TimerDesk::stop` for the satellite of the request only (P-19.10b-2), and `handle` increased its generation.
- P-20.7-3. Argument: P-20.5-2 and P-20.6-3.

---

### 20.8 Timed Unmute

**Goal:** Unmute a satellite at the end of "mute for N minutes", only when the switch still has the state of the mute. A change by a person then always wins, and a local training run keeps its mute.

- **Implements:** G16, G26. **Depends on:** 20.7.
- **Files:** modify `src/app/mute.rs`, `src/app/conversation.rs`, `src/lib.rs`, `README.md` (the timed mute, `CLAUDE.md` workflow item 15).
- **Quint:** `mute.qnt` (the rules of `callDone`, `tick` and `trainEnd`).

**Model.**

- `MuteDesk` keeps the timed mutes by satellite in one async mutex. It also gets the `Clock` and the receiver of the status channel (sub-phase 12.9b).
- The `after` work of the `switch.turn_on` call of a `Mute` plan gives its result to `MuteDesk::started`, as the guard of a timer start call gets its result (sub-phase 19.9e). After a success, `started` waits until the hot cache shows the switch `on`, for at most `MUTE_WAIT` (5 s, a constant in `src/app/mute.rs`). Then it stores `Mute` with the time of the request plus the seconds and that `last_updated`, in place of the mute of the satellite, and starts its end task. A mute without seconds removes the mute of the satellite. When the wait ends with no `on`, `started` logs a `warn` event and stores nothing.
- The end task sleeps until `end`, then waits until the status is not `training`. Then, under the lock, when the satellite still has this mute, it removes it, and when `unmutes` gives true, it calls `switch.turn_off` on the switch. A failed call logs a `warn` event. HA has no conditional call, so a press in HA that the hot cache does not have yet can still meet the call.
- Training mutes only the switches that are `off`, and it unmutes only those (sub-phase 13.10e). Thus it does not touch a voice mute, and a timed end during a local run waits for `online`.

**Acceptance criteria.**

- AC-20.8-1: When "mute for ten minutes" on the Kitchen satellite returns "Muting for ten minutes." and the hot cache shows its switch `on`, `FakeHome` records `switch.turn_off` on the switch 600 s of virtual time after the request, and not before.
- AC-20.8-2: When a person turns the switch `off` and `on` again during the mute, `FakeHome` records no `switch.turn_off` at the end. After "mute" with no duration, `FakeHome` records no `switch.turn_off` within 24 hours.
- AC-20.8-3: When the status is `training` at the end, `FakeHome` records no `switch.turn_off` until the status is `online`, and then records it.
- AC-20.8-4: When the hot cache does not show the switch `on` within 5 s after the call, the log has a `warn` event, and `FakeHome` records no `switch.turn_off`.

**Proofs.**

- P-20.8-1 to P-20.8-3. Requires: `NoUnmuteAfterPress`, `NoEarlyUnmute`, `NoUnmuteInTraining` (P-20.4-2), P-20.5-3. Argument: the end task sleeps until `end`, waits for a status that is not `training`, and calls only when `unmutes` gives true under the lock.
- P-20.8-4. Evidence: test.

---

### 20.9 Mute Store

**Goal:** Add the `MuteStore` port and its SQLite adapter, which keep the timed mutes in `jarvis.db`. A restart of Jarvis then still unmutes at the end.

- **Implements:** G26. **Depends on:** 20.8.
- **Files:** modify `src/ports.rs`, `src/adapters/sqlite.rs`, `src/app/mute.rs`, `src/domain/mute.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `README.md` (the mute survives a restart, `CLAUDE.md` workflow item 15).
- **Quint:** `mute.qnt` (the rule of `restart`).

**Model.**

- `MuteStore`: `load() -> Result<Vec<Mute>>` and `save(&[Mute]) -> Result<()>`. Its only production adapter is the SQLite adapter of the interaction log, as for `TimerStore` (sub-phase 19.8). Thus Jarvis has no new data file.
- The adapter adds the table `mutes (id INTEGER PRIMARY KEY, record TEXT NOT NULL)` with one row `id = 0`: the JSON of the mutes. `save` replaces the row in one statement. `load` gives no mutes when the row does not exist. The calls run in `spawn_blocking` (decision 29).
- `Mute` gets `Serialize` and `Deserialize`. `FakeMuteStore` in `src/testing/fakes.rs` keeps the last saved mutes and can fail `save`. `run` gives `MuteDesk` the SQLite adapter.
- Each change of the timed mutes saves them under the lock. A failed save logs a `warn` event, and the mutes in memory stay.
- `MuteDesk::new` loads the mutes. A failed load logs a `warn` event and starts with none. Each loaded mute starts its end task. The task first waits until the hot cache has a state of the switch, so the first load of the cache comes before `unmutes`. An end in the past ends at once.

**Acceptance criteria.**

- AC-20.9-1: When the adapter saves two mutes and a new adapter on the same database file loads them, they are equal. With no `mutes` row, `load` gives no mutes, and `save` and `load` do not change the interactions or the timers.
- AC-20.9-2: When "mute for ten minutes" is stored and a new `MuteDesk` starts on the same store 2 minutes later, `FakeHome` records `switch.turn_off` 8 minutes after that start.
- AC-20.9-3: When the store has a mute whose end passed and the hot cache shows its switch `on` with its `last_updated`, the new `MuteDesk` calls `switch.turn_off` after the first load of the cache.

**Proofs.**

- P-20.9-1 to P-20.9-3. Evidence: test.

---

### 20.10 Mute Conformance

**Goal:** Replay the traces of `mute.qnt` against `MuteDesk`. The switches and the timed mutes then match the model, so its invariants hold for the code on the replayed traces.

- **Implements:** G19, G26. **Depends on:** 20.9.
- **Files:** modify `src/app/mute.rs`.
- **Quint:** `mute.qnt` (conformance).

**Model.** The driver plays the part of the conversation use case, of HA and of training. It maps `s1` and `s2` to the Kitchen and Office satellites, whose switches start `off`. A tick is 60 s of virtual time, and `LENGTH` is the duration "two minutes".

- `voiceMute(s, timed)`: `MuteDesk::decide` with or without "two minutes", and `FakeHome` holds the call. `callDone(s)` gives the result and the `on` state, and `callFailed(s)` fails the call.
- `press(s)`: the switch flips with a new `last_updated`. `trainStart` and `trainEnd`: the status `training` with the `on` states of the switches that were `off`, then the `off` states and `online`. `restart`: a new `MuteDesk` on the same store. The held calls drop.
- `FakeHome` applies each `switch.turn_off` to the hot cache with a new `last_updated`. After each step, the driver waits until each end task waits, then compares the switches and the timed mutes (satellite, end) with the model.

**Acceptance criteria.**

- AC-20.10-1: When `quint-connect` replays 100 traces of `mute.qnt`, the switches and the timed mutes match the model after each step.

**Proofs.**

- P-20.10-1. Evidence: test. With P-20.4-2, it gives `NoUnmuteAfterPress`, `NoEarlyUnmute` and `NoUnmuteInTraining` for the Rust code on the replayed traces.

---

### 20.11 Unmute in the Jarvis Tab

**Goal:** Show the muted satellites in the Jarvis tab, each with an unmute button. You can then unmute a satellite without the button on the device.

- **Implements:** G24, G26. **Depends on:** 20.10.
- **Files:** modify `src/ports.rs`, `src/domain/state_cache.rs`, `src/app/mute.rs`, `src/lib.rs`, `src/adapters/web.rs`, `src/adapters/review.html`, `src/testing/fakes.rs`, `README.md` (the unmute in the tab, `CLAUDE.md` workflow item 15).

**Model.**

- `muted(cache) -> Vec<EntityId>` in `src/domain/state_cache.rs`: the satellites whose mute switch (`mute_switch`) is `on`.
- `Control` becomes an `#[async_trait]` trait and gets `mutes() -> Vec<Muted>` and `unmute(satellite) -> Result<(), Refusal>`. `Muted { satellite, name, until }` is in `src/ports.rs`, with the JSON form `{ "satellite": ..., "name": ..., "until": ... }`. `name` is the name of the satellite entity. `until` is the end of its timed voice mute (`MuteDesk::ends`), else `null`. `unmute` calls `switch.turn_off` on the mute switch of the satellite. During a training run, it returns `Busy`, as `save` does (sub-phase 13.10b). The changed `last_updated` then ends a timed mute without a second call (P-20.5-3).
- `GET api/mutes` returns the mutes. `POST api/unmute` with `{ "satellite": ... }` returns 204, or 409 for `Busy`, as `save`. A satellite with no mute switch gives 404.
- The page has a section `#mutes` with one `.mute` for each mute: the name, the end time when it has one, and a button "Unmute". The page reads `api/mutes` with `api/status` each 5 s.

**Acceptance criteria.**

- AC-20.11-1: When the Kitchen switch is `on` after "mute for ten minutes", `GET api/mutes` gives the Kitchen satellite with its name and its end, and each other satellite whose switch is `on` with `until = null`.
- AC-20.11-2: When `POST api/unmute` names the Kitchen satellite, `FakeHome` records `switch.turn_off` on its switch, and the response is 204. During a training run, the response is 409, and `FakeHome` records no call.
- AC-20.11-3: When `GET /` runs, the HTML contains `#mutes`, and the script reads `api/mutes` and posts `api/unmute`.

**Proofs.**

- P-20.11-1. Ensures: `muted` gives exactly the satellites whose mute switch is `on`. Argument: a filter over the satellites with `mute_switch` (P-20.5-1).
- P-20.11-2. Argument: `unmute` checks the activity first, as `save` does (P-13.10b-3).
- P-20.11-3. Evidence: test.

## Field Log

These steps are not tests and gate nothing. Use the Office satellite.

1. Ask "What is the weather like?" and say "Hey Jarvis, stop" during the answer. Make sure that the speech stops at the wake word, that Jarvis says "Okay.", and that no more answer comes.
2. Play music in the Office. Wait 15 s with no request. Say "Hey Jarvis, stop". Make sure that the music pauses. Say "Hey Jarvis, never mind" and make sure that Jarvis says "Okay." and does nothing else.
3. Set a one-minute timer. When it rings, say "Hey Jarvis, stop". Make sure that the ring stops, that Jarvis says "Okay.", and that the music does not change.
4. Say "Hey Jarvis, mute for two minutes". Make sure that the mute switch is `on` in HA, that the LED ring of the satellite shows the mute, and that the wake word does nothing. After two minutes, make sure that the switch is `off`.
5. Say "Hey Jarvis, mute". Make sure that the Jarvis tab lists the Office satellite with no end. Press "Unmute" and make sure that the switch is `off`.
6. Say "Hey Jarvis, mute for two minutes", press the button of the satellite two times within the two minutes, and make sure that the satellite stays muted after the end.
