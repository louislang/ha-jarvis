# Phase 15: Speaker Identification

This phase identifies the speaker from presence, Bluetooth proximity through the satellites, the satellite presence sensors and voice. Person-specific intents act for that speaker.

---

### 15.1 Discovery

**Goal:** Find each person's trackers and each satellite's presence sensors from the HA registries, with optional overrides in `jarvis.toml`. Speaker identification needs no manual entity list.

- **Implements:** G12. **Depends on:** 14.11.
- **Files:** create `src/domain/speaker.rs`. Modify `src/domain/state.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- `trackers(cache, person) -> Vec<Tracker { area_sensor, distance_sensor }>`: from the `device_trackers` attribute of the `person` entity. For each tracker device: the sensor whose state is an area name, and the sensor with the unit `m`. `[[people]].trackers` replaces discovery when set.
- `presence_sensors(cache, satellite_device) -> Vec<EntityId>`: the `binary_sensor` entities on the satellite device with device class `occupancy`, `presence` or `motion`. `[[satellites]].presence` replaces discovery when set.
- This sub-phase adds `[[people]]` and the optional `[[satellites]]`.

**Acceptance criteria.**

- AC-15.1-1: When `person.alex` has `device_trackers = [device_tracker.alex_watch]`, `trackers` returns the area and distance sensors on the watch device.
- AC-15.1-2: When a sensor on the tracker device has the unit `m`, `trackers` uses it as the distance sensor.
- AC-15.1-3: When `[[people]].trackers` is set, `trackers` returns it and does not use discovery.
- AC-15.1-4: When the satellite device has a `binary_sensor` with device class `occupancy`, `presence_sensors` returns it.
- AC-15.1-5: When a `binary_sensor` on the satellite device has no device class, `presence_sensors` does not return it.
- AC-15.1-6: When `[[satellites]].presence` is set, `presence_sensors` returns it and does not use discovery.

**Proofs.**

- P-15.1-3, P-15.1-6. Ensures: with an override, the result is the override. Argument: both functions return the override first when it is `Some`.
- P-15.1-4, P-15.1-5. Ensures: a `binary_sensor` of the satellite device is a presence sensor if and only if its device class is `occupancy`, `presence` or `motion`. Argument: the discovery filter requires a device class in the set of three. `None` is not in the set.
- P-15.1-1, P-15.1-2. Ensures: each tracker has the sensor of its device whose state is an area name and the sensor with the unit `m`. Argument: the tracker filter selects sensors on the same device by the two stated predicates.

---

### 15.2 Presence and Proximity Signals

**Goal:** Read who is home, whether the satellite's room is occupied, and how close each person's phone or watch is to the satellite. Log a discovery report at startup, so you can check what Jarvis found.

- **Implements:** G12. **Depends on:** 15.1.
- **Files:** modify `src/domain/speaker.rs`, `src/lib.rs`.

**Model.**

- `home(cache, people) -> Vec<Person>`: people whose `person` state is `home`.
- `occupied(cache, satellite_device) -> Option<bool>`: `None` without a presence sensor, else true when one sensor is `on`.
- `proximity(cache, trackers, area) -> f64`: 0 when no available tracker is in the area, else `1 / (1 + distance)` of the nearest tracker in the area.
- `discovery_report(cache, people) -> Vec<String>`: one line for each satellite with its presence sensors, and one for each person with their trackers. `run` logs it. `trackers` reads the current cache at each identification, so an area sensor that shows no area at startup is found when it shows one. The report names a tracker without an area sensor at that time.

**Acceptance criteria.**

- AC-15.2-1: When two people are `home` and one is `not_home`, `home` returns the two people.
- AC-15.2-2: When the satellite device has no presence sensor, `occupied` returns `None`.
- AC-15.2-3: When a tracker area is the satellite area and the distance is 1 m, `proximity` returns 0.5.
- AC-15.2-4: When no tracker is in the satellite area, `proximity` returns 0.
- AC-15.2-5: When a person has two trackers in the area, `proximity` uses the nearer one.
- AC-15.2-6: When a tracker state is `unavailable`, `proximity` ignores it.
- AC-15.2-7: When `discovery_report` runs, it names each satellite with its presence sensors and each person with their trackers.

**Proofs.**

- P-15.2-3 to P-15.2-6. Ensures: `proximity` is in `[0, 1]` and equals `1 / (1 + min distance)` over available trackers in the area. Argument: the function filters by availability and area, takes the minimum distance, and maps it with `1 / (1 + d)`. An empty filter gives 0. For `d >= 0`, `1 / (1 + d)` is in `(0, 1]`, and `d = 1` gives 0.5.
- P-15.2-1, P-15.2-2. Ensures: `home` gives the people whose state is `home`, and `occupied` is `None` without a presence sensor, else whether one sensor is `on`. Argument: both functions are filters with the stated predicate.
- P-15.2-7 (AC-15.2-7). Ensures: the report has one line for each satellite with its presence sensors and one line for each person with their trackers. Argument: `discovery_report` maps over the satellites and over the people, and formats each with the result of `presence_sensors` or `trackers`.

---

### 15.3 Voice Ports and Adapters

**Goal:** Add the `VoiceEmbedder` and `VoiceProfiles` ports and their sherpa-onnx adapters. Jarvis can turn an utterance into a voice embedding and compare it with stored profiles.

- **Implements:** G12. **Depends on:** 15.2.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`, `justfile`, `src/lib.rs`, `jarvis.toml`.

**Model.** `VoiceEmbedder::embed(audio) -> Result<Vec<f32>>` with `SpeakerEmbeddingExtractor`. `VoiceProfiles::enroll(person, embedding)` and `similarities(embedding) -> Map<person, f32>` with `SpeakerEmbeddingManager`, saved to `speaker.voices`. A profile keeps the newest `max_samples` embeddings. `just models` also downloads the speaker model and the multi-speaker test voice `vits-piper-en_US-libritts_r-medium`. This sub-phase adds `[speaker]` (`model`, `voices`, `max_samples`, `voice_threshold`). `voice_threshold` is a voice similarity. Sub-phase 15.6 adds `threshold`, which is a combined score.

**Acceptance criteria.**

- AC-15.3-1: When `just models` has run, the speaker model and the multi-speaker test voice exist in `models/` with the SHA-256 that the recipe lists.
- AC-15.3-2: When one test voice speaker ID speaks two different phrases, the similarity is more than `speaker.voice_threshold`.
- AC-15.3-3: When two different test voice speaker IDs speak, the similarity is less than `speaker.voice_threshold`.
- AC-15.3-4: When `enroll` adds more than `max_samples` embeddings for a person, the profile keeps the newest `max_samples`.
- AC-15.3-5: When the adapter starts, it loads all profiles from the voice store.
- AC-15.3-6 [GPU host]: With `device = cpu`, one embedding of 3 s of audio has a p95 of less than 80 ms.
- AC-15.3-7 [GPU host]: With `device = cuda`, the voice embedder runs on the CUDA execution provider.

**Proofs.**

- P-15.3-4. Argument: `enroll` pushes to a bounded deque that drops the oldest element when it is full.
- P-15.3-1 to P-15.3-3, P-15.3-5 to P-15.3-7. Evidence: test.

---

### 15.4 Voice Embedding at Commit

**Goal:** Compute the voice embedding in the background when an utterance commits, and attach the speaker to the interaction. Voice identification adds no time to the transcript.

- **Implements:** G12, G15. **Depends on:** 15.3.
- **Files:** modify `src/app/speech.rs`, `src/app/conversation.rs`.

**Model.** At commit, the speech use case spawns `embed(audio)` in a blocking task and stores the future with the utterance. The conversation use case gets it with the utterance that `handle` correlates (sub-phases 12.1 and 12.6). Sub-phase 15.6 uses it to identify the speaker.

**Acceptance criteria.**

- AC-15.4-1: When an utterance commits, the transcript is returned before the embedding completes.
- AC-15.4-2: When the embedding completes, the conversation use case gets the embedding through `correlate`.
- AC-15.4-3 [GPU host]: With embeddings on, all Phase 9.2 latency tests pass.

**Proofs.**

- P-15.4-1. Argument: the embedding is spawned, and `push` returns the transcript without awaiting it.
- P-15.4-2, P-15.4-3. Evidence: test.

---

### 15.5 Speaker Quint Model

**Goal:** Add `quint/speaker.qnt`, the model of identification and passive enrollment. Prove that the identified speaker is always a candidate and that a voice match alone never enrolls a sample.

- **Implements:** G12, G19. **Depends on:** 15.4.
- **Files:** create `quint/speaker.qnt`.
- **Quint:** `speaker.qnt`.

**Model.**

- Constants: `PEOPLE = Set("alex", "sam")`.
- State: `home: Option[Set[PERSON]]` (`None` when presence is not known), `occupied: Option[bool]`, `proximity: PERSON -> int`, `voice: PERSON -> int`, `result: Option[PERSON]`, `source: Presence | Proximity | Voice | Answer | NoSource`, `enrolled: List[(PERSON, source)]`.
- Actions: `setSignals(...)`, `identify` (rules 1 to 4 of sub-phase 15.6), `enroll` (rule 5), `answer(p)` (the user answers `ask_person`).
- Invariants:
  - `SpeakerIsCandidate`: `result = Some(p)` implies `p` is in `home`, or `home = None`, or `source = Answer`. The user can name a person who is not home.
  - `NoVoiceOnlyEnrollment`: no entry in `enrolled` has source `Voice`.

**Acceptance criteria.**

- AC-15.5-1 [Quint]: `quint typecheck` passes.
- AC-15.5-2 [Quint]: Both invariants hold in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-15.5-2. `SpeakerIsCandidate`: `identify` chooses only from the candidate set, which is `home` when presence is known (rule 1). `answer(p)` sets `source = Answer`. `NoVoiceOnlyEnrollment`: `enroll` is enabled only when `source` is `Presence`, `Proximity` or `Answer`.
- P-15.5-1. Evidence: model check.

---

### 15.6 Identification and Enrollment

**Goal:** Add `identify`, which implements `speaker.qnt` by combining presence, proximity and voice, and the passive enrollment rule. Jarvis knows who is speaking without an enrollment step.

- **Implements:** G12, G19. **Depends on:** 15.5.
- **Files:** modify `src/domain/speaker.rs`, `src/app/conversation.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `speaker.qnt` (conformance).

**Model.** Rules:

1. Only people who are `home` are candidates. Presence is known when each `[[people]]` person has the state `home` or `not_home`. If presence is not known, all people are candidates, and no presence identification occurs.
2. One candidate gives that person with confidence 1.0 and source `Presence`.
3. If `occupied` is `Some(false)`, proximity counts 0.
4. With more than one candidate, score = proximity + voice similarity. The top score must be more than `threshold` and more than the second score plus `margin`. The source is `Proximity` when proximity alone is at least `enroll_threshold` (`0.5` is 1 m or less), else `Voice`.
5. Jarvis enrolls the embedding only when all of these are true: the source is `Presence`, `Proximity` or `Answer`; the utterance has `min_enroll_ms` of voice or more; and, when the person has `min_profile` samples or more, the voice similarity to that person is at least `voice_threshold` and higher than to each other person. Thus a guest, a TV or stale presence cannot change a profile that has samples.

The interaction record gets the speaker and the source. This sub-phase adds `speaker.threshold`, `speaker.margin`, `speaker.enroll_threshold`, `speaker.min_enroll_ms` and `speaker.min_profile`.

**Acceptance criteria.**

- AC-15.6-1: When only one person is home, `identify` returns that person with confidence 1.0.
- AC-15.6-2: When two people are home and only one watch is in the satellite area, `identify` returns the owner of that watch.
- AC-15.6-3: When the satellite presence sensor is `off`, proximity does not identify a speaker.
- AC-15.6-4: When two people are home, no tracker is in the area, and the voice matches one profile, `identify` returns that person.
- AC-15.6-5: When the top two scores differ by less than `margin`, `identify` returns no speaker.
- AC-15.6-6: When `identify` returns confidence 1.0 from presence, `FakeVoiceProfiles` records an enroll for that person.
- AC-15.6-7: When the identification comes from voice alone, `FakeVoiceProfiles` records no enroll.
- AC-15.6-8: When `quint-connect` replays 100 traces of `speaker.qnt`, `identify` and the enrolls match the model after each step.
- AC-15.6-9: When the person has `min_profile` samples and the voice similarity to that person is less than `voice_threshold`, or higher to another person, `FakeVoiceProfiles` records no enroll.
- AC-15.6-10: When the utterance has less than `min_enroll_ms` of voice, `FakeVoiceProfiles` records no enroll.
- AC-15.6-11: When one `person` state is `unavailable`, all people are candidates, and presence alone identifies no speaker.
- AC-15.6-12: When `identify` returns a speaker, the interaction record has the speaker and the source.

**Proofs.**

- P-15.6-1. Ensures: one candidate gives that person with confidence 1.0 and source `Presence`. Argument: rule 2 applies when the candidate set has one element.
- P-15.6-3. Ensures: with `occupied = Some(false)`, proximity adds 0 to every score. Argument: rule 3 sets proximity to 0 before rule 4 scores.
- P-15.6-5. Ensures: `identify` returns a speaker only when the top score is more than the second score plus `margin`. Argument: rule 4 requires `top > second + margin`.
- P-15.6-6, P-15.6-7, P-15.6-9, P-15.6-10. Requires: `NoVoiceOnlyEnrollment`. Argument: rule 5 is a conjunction of guards on the source, the voice duration and the profile similarity, and voice-only identification has source `Voice`.
- P-15.6-11 (AC-15.6-11). Ensures: when a person state is not `home` or `not_home`, all people are candidates and rule 2 does not identify. Argument: rule 1 checks that each state is `home` or `not_home` before it filters, and else takes all people with no presence identification.
- P-15.6-12. Evidence: test.
- P-15.6-2, P-15.6-4. Requires: `SpeakerIsCandidate`. Argument: the maximum of rule 4 ranges over candidates only. Evidence: test for the scores.
- P-15.6-8. Evidence: test.

---

### 15.7 Person-Specific Intents

**Goal:** Add `needs_speaker`, the `person` template variable, `next_date` and the `ask_person` question. "Add dentist to my calendar for Monday" adds the event to the speaker's calendar.

- **Implements:** G12, G13. **Depends on:** 15.6.
- **Files:** modify `src/domain/intent.rs`, `src/domain/responses.rs`, `src/domain/decide.rs`, `src/app/conversation.rs`, `intents.toml`, `responses.toml`, `src/lib.rs`, `jarvis.toml`.

**Model.** An intent with `needs_speaker = true` waits up to `speaker.wait_ms` for the embedding, then calls `identify`. The `person` variable has the name and the fields of its `[[people]]` entry. No speaker gives `Ask` from `ask_person`. `next_date(day, plus_days)`: the ISO date of the next `day` (a day name, `today` or `tomorrow`) plus `plus_days`. An empty `day` is today. Another value is an error that gives `Fallback`. This sub-phase adds `speaker.wait_ms`.

**Acceptance criteria.**

- AC-15.7-1: When Alex says "add dentist to my calendar for monday", `FakeHome` records `calendar.create_event` for `calendar.alex` with `summary = dentist`.
- AC-15.7-2: When the date is Wednesday 2026-10-28, `next_date("monday", 0)` returns `2026-11-02` and `next_date("monday", 1)` returns `2026-11-03`.
- AC-15.7-3: When `day` is empty, `next_date` uses today.
- AC-15.7-4: When `day` is "someday", `next_date` returns an error and `decide` returns `Fallback`.
- AC-15.7-5: When `identify` returns no speaker, `decide` returns `Ask` from `ask_person` and `FakeHome` records no service call.
- AC-15.7-6: When the user answers "Alex" to `ask_person`, Jarvis acts for Alex and `FakeVoiceProfiles` records an enroll of the first utterance for Alex.
- AC-15.7-7: When the embedding does not complete within `speaker.wait_ms`, Jarvis uses presence and proximity only.
- AC-15.7-8: When the intent does not have `needs_speaker`, Jarvis does not wait for the speaker.
- AC-15.7-9 [GPU host]: For "add dentist to my calendar", the response latency has a p95 of 400 ms or less.

**Proofs.**

- P-15.7-2, P-15.7-3. Requires: `day` is a day name. Ensures: the result is the first date after today with that weekday, plus `plus_days`. Argument: `next_date` adds `(target - today_weekday + 6) mod 7 + 1` days, which is in `1..=7`. Wednesday to Monday is `(0 - 2 + 6) mod 7 + 1 = 5` days, so 2026-10-28 plus 5 days is 2026-11-02.
- P-15.7-4. Ensures: a `day` that is not a day name, `today` or `tomorrow` gives `Err`, and `decide` gives `Fallback`. Argument: the parser matches the allowed words and returns `Err` for others, and `decide` maps the error to `Fallback`.
- P-15.7-5. Ensures: no speaker gives `Ask` from `ask_person` and no call. Argument: `decide` returns `Ask` before it builds a call when the speaker is `None`.
- P-15.7-6. Requires: `NoVoiceOnlyEnrollment`. Argument: the answer sets the source to `Answer`, which rule 5 allows.
- P-15.7-8. Argument: the wait is inside `if intent.needs_speaker`.
- P-15.7-1, P-15.7-7, P-15.7-9. Evidence: test.

## Field Log

These steps are not tests and gate nothing.

1. In HA, open each satellite device page. Record its presence and Bluetooth entities.
2. If the satellite firmware has a Bluetooth proxy option, turn it on.
3. Install Bermuda. Add iPhones and Apple Watches with the Private BLE Device integration first.
4. Add each Bermuda `device_tracker` to the correct `person` entity.
5. Start Jarvis. Make sure the discovery report names each satellite presence sensor and each person tracker.
6. Walk to each satellite with a phone. Make sure the tracker area changes to the satellite area.
