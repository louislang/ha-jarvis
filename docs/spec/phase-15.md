# Phase 15: Speaker Identification

This phase identifies the speaker from presence, Bluetooth proximity through the satellites, the satellite presence sensors and voice. The people of the house are the `person` entities of HA, so `jarvis.toml` lists no people. Replies can address each person in their own way. In the Jarvis tab, you can assign the voice of a request to a person.

---

### 15.1 Tracker Discovery

**Goal:** Find the trackers of each person from the HA registries, with an optional override in `jarvis.toml`. Speaker identification needs no manual tracker list.

- **Implements:** G12. **Depends on:** 14.13.
- **Files:** create `src/domain/speaker.rs`. Modify `src/lib.rs`, `jarvis.toml`.

**Model.**

- `trackers(cache, person, configured) -> Vec<Tracker { area_sensor, distance_sensor }>`: `configured` when it is `Some`. Else from the `device_trackers` attribute of the `person` entity. For each tracker device: the sensor whose state is an area name, and the sensor with the unit `m`. A device without both sensors gives no tracker. `configured` is the `trackers` of the `[[people]]` entry whose `person` is that entity.
- This sub-phase adds the optional `[[people]]` (`person` and `trackers`) with `#[serde(default)]`. An entry is only an override of the trackers of one person. The people of the house are the `person` entities of HA (sub-phase 15.2).

**Acceptance criteria.**

- AC-15.1-1: When `person.alex` has `device_trackers = [device_tracker.alex_watch]`, `trackers` returns the area and distance sensors on the watch device.
- AC-15.1-2: When a sensor on the tracker device has the unit `m`, `trackers` uses it as the distance sensor.
- AC-15.1-3: When the `[[people]]` entry of `person.alex` sets `trackers`, `trackers` returns them and does not use discovery.
- AC-15.1-4: When `jarvis.toml` has no `[[people]]`, `Config::parse` succeeds with no overrides.

**Proofs.**

- P-15.1-3. Ensures: with an override, the result is the override. Argument: `trackers` returns the override first when it is `Some`.
- P-15.1-1, P-15.1-2. Ensures: each tracker has the sensor of its device whose state is an area name and the sensor with the unit `m`. Argument: the tracker filter selects sensors on the same device by the two stated predicates.
- P-15.1-4. Evidence: test.

---

### 15.1b Presence Sensor Discovery

**Goal:** Find the presence sensors of each satellite from the HA registries, with an optional override in `jarvis.toml`. Speaker identification needs no manual sensor list.

- **Implements:** G12. **Depends on:** 15.1.
- **Files:** modify `src/domain/speaker.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- `presence_sensors(cache, satellite_device, configured) -> Vec<EntityId>`: `configured` when it is `Some`. Else the `binary_sensor` entities on the satellite device with device class `occupancy`, `presence` or `motion`. `configured` is the `presence` list of the `[[satellites]]` entry of the satellite.
- This sub-phase adds the optional `[[satellites]]` (`satellite` and `presence`).

**Acceptance criteria.**

- AC-15.1b-1: When the satellite device has a `binary_sensor` with device class `occupancy`, `presence_sensors` returns it.
- AC-15.1b-2: When a `binary_sensor` on the satellite device has no device class, `presence_sensors` does not return it.
- AC-15.1b-3: When `[[satellites]].presence` is set, `presence_sensors` returns it and does not use discovery.

**Proofs.**

- P-15.1b-3. Ensures: with an override, the result is the override. Argument: `presence_sensors` returns the override first when it is `Some`.
- P-15.1b-1, P-15.1b-2. Ensures: a `binary_sensor` of the satellite device is a presence sensor if and only if its device class is `occupancy`, `presence` or `motion`. Argument: the discovery filter requires a device class in the set of three. `None` is not in the set.

---

### 15.2 People and Presence

**Goal:** Read the people of the house from HA, who is home, and whether the satellite's room is occupied.

- **Implements:** G12. **Depends on:** 15.1b.
- **Files:** modify `src/domain/speaker.rs`.

**Model.**

- `Person { entity, name }`: a `person` entity of HA and its name. The name is the `friendly_name` attribute of the state, else the entity ID.
- `people(cache) -> Vec<Person>`: each state of the cache in the domain `person`, in entity ID order. `jarvis.toml` does not list people.
- `home(cache) -> Vec<Person>`: the people whose state is `home`.
- `occupied(cache, satellite_device, configured) -> Option<bool>`: `None` without a presence sensor, else true when one sensor is `on`. The presence sensors are `presence_sensors(cache, satellite_device, configured)`.

**Acceptance criteria.**

- AC-15.2-1: When two `person` entities are `home` and one is `not_home`, `home` returns the two people.
- AC-15.2-2: When the satellite device has no presence sensor, `occupied` returns `None`.
- AC-15.2-7: When HA has `person.alex` and `person.sam` and `jarvis.toml` has no `[[people]]`, `people` returns both with their friendly names.
- AC-15.2-8: When a `person` entity has no `friendly_name`, `people` gives its entity ID as its name.

**Proofs.**

- P-15.2-1, P-15.2-2. Ensures: `home` gives the people whose state is `home`, and `occupied` is `None` without a presence sensor, else whether one sensor is `on`. Argument: both functions are filters with the stated predicate.
- P-15.2-7, P-15.2-8. Ensures: `people` gives exactly the states in the domain `person`, in key order, and the name of each is its `friendly_name` text, else its entity ID. Argument: `people` maps the filtered states of the ordered state map, and the name falls back to the key when the attribute is not a string.

---

### 15.2b Discovery Report

**Goal:** Log a discovery report at startup, so you can check what Jarvis found.

- **Implements:** G12. **Depends on:** 15.2.
- **Files:** modify `src/domain/speaker.rs`, `src/lib.rs`.

**Model.**

- `Satellite`: a `[[satellites]]` entry (`satellite` and `presence`). `PersonTrackers`: a `[[people]]` entry (`person` and `trackers`). Both types move from `src/lib.rs` to `src/domain/speaker.rs`.
- `discovery_report(cache, overrides, satellites) -> Vec<String>`: one line for each `assist_satellite` entity with its presence sensors, and one for each person of `people(cache)` with their trackers. The `presence` of the `[[satellites]]` entry of a satellite and the `trackers` of the `[[people]]` entry of a person replace discovery. `run` logs it.
- `trackers` reads the current cache at each identification, so an area sensor that shows no area at startup is found when it shows one. The line of a person also names each `device_tracker` of the person that gives no tracker at that time.

**Acceptance criteria.**

- AC-15.2b-1: When `discovery_report` runs, it names each satellite with its presence sensors and each `person` entity of HA with its trackers.
- AC-15.2b-2: When the area sensor of a `device_tracker` of a person shows no area, `discovery_report` names that `device_tracker` in the line of the person.

**Proofs.**

- P-15.2b-1, P-15.2b-2. Evidence: test. The report is log text for the user, and its tests check each line.

---

### 15.2c Tracker Proximity

**Goal:** Read how close each person's phone or watch is to the satellite.

- **Implements:** G12. **Depends on:** 15.2b.
- **Files:** modify `src/domain/speaker.rs`.

**Model.**

- `proximity(cache, trackers, area) -> f64`: 0 when no available tracker is in the area, else `1 / (1 + distance)` of the nearest tracker in the area.

**Acceptance criteria.**

- AC-15.2c-1: When a tracker area is the satellite area and the distance is 1 m, `proximity` returns 0.5.
- AC-15.2c-2: When no tracker is in the satellite area, `proximity` returns 0.
- AC-15.2c-3: When a person has two trackers in the area, `proximity` uses the nearer one.
- AC-15.2c-4: When a tracker state is `unavailable`, `proximity` ignores it.

**Proofs.**

- P-15.2c-1 to P-15.2c-4. Ensures: `proximity` is in `[0, 1]` and equals `1 / (1 + min distance)` over available trackers in the area. Argument: the function filters by availability and area, takes the minimum distance, and maps it with `1 / (1 + d)`. An empty filter gives 0. For `d >= 0`, `1 / (1 + d)` is in `(0, 1]`, and `d = 1` gives 0.5.

---

### 15.3 Voice Embedder

**Goal:** Add the `VoiceEmbedder` port and its sherpa-onnx adapter. Jarvis can turn an utterance into a voice embedding.

- **Implements:** G12. **Depends on:** 15.2c.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`, `justfile`, `src/lib.rs`, `jarvis.toml`, `xtask/src/gates.rs`.

**Model.** `VoiceEmbedder::embed(audio) -> Result<Vec<f32>>` with `SpeakerEmbeddingExtractor`. `just models` also downloads the speaker model and the multi-speaker test voice `vits-piper-en_US-libritts_r-medium`. This sub-phase adds `[speaker]` with `model`. `LiveModels` (sub-phase 13.10d) also keeps the voice embedder session, so the training pause releases it and loads it again.

**Acceptance criteria.**

- AC-15.3-1: When `just models` has run, the speaker model and the multi-speaker test voice exist in `models/` with the SHA-256 that the recipe lists.
- AC-15.3-6 [GPU host]: With `device = cpu`, one embedding of 3 s of audio has a p95 of less than 80 ms.
- AC-15.3-7 [GPU host]: With `device = cuda`, the voice embedder runs on the CUDA execution provider.
- AC-15.3-8: When `LiveModels::release` returns, no reference to the `VoiceEmbedder` fake exists. After `restore`, `embed` returns an embedding.

**Proofs.**

- P-15.3-1, P-15.3-6 to P-15.3-8. Evidence: test.

---

### 15.3b Voice Profiles

**Goal:** Add the `VoiceProfiles` port and its sherpa-onnx adapter. Jarvis can compare a voice embedding with stored profiles.

- **Implements:** G12. **Depends on:** 15.3.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `VoiceProfiles::enroll(person, id, embedding)` and `similarities(embedding) -> Map<person, f32>` with `SpeakerEmbeddingManager`, saved to `speaker.voices`. `id` is the interaction ID of the request (sub-phase 12.6). The store keys each sample by its interaction ID, so one request gives at most one sample. `enroll` with an interaction ID that the store has does not change the store. A profile keeps the samples of the newest `max_samples` interaction IDs: after each change, the store removes the samples of the person with the oldest IDs until `max_samples` are left. Sub-phase 15.8 adds `assign` and the pick record. This sub-phase adds `voices`, `max_samples` and `voice_threshold` to `[speaker]`. `voice_threshold` is a voice similarity. Sub-phase 15.6c adds `threshold`, which is a combined score.

**Acceptance criteria.**

- AC-15.3b-1: When one test voice speaker ID speaks two different phrases, the similarity is more than `speaker.voice_threshold`.
- AC-15.3b-2: When two different test voice speaker IDs speak, the similarity is less than `speaker.voice_threshold`.
- AC-15.3b-3: When `enroll` adds more than `max_samples` embeddings for a person, the profile keeps the newest `max_samples`.
- AC-15.3b-4: When the adapter starts, it loads all profiles from the voice store.
- AC-15.3b-5: When `enroll` gets an interaction ID that the store has, the store does not change.

**Proofs.**

- P-15.3b-3. Argument: `enroll` adds the sample, and then removes the samples of the person with the oldest interaction IDs until `max_samples` are left.
- P-15.3b-5. Argument: `enroll` returns before any change when the store has a sample of the interaction ID, or (from sub-phase 15.8) a pick of it.
- P-15.3b-1, P-15.3b-2, P-15.3b-4. Evidence: test.

---

### 15.4 Voice Embedding at Commit

**Goal:** Compute the voice embedding in the background when an utterance commits, and attach the speaker to the interaction. Voice identification adds no time to the transcript.

- **Implements:** G12, G15. **Depends on:** 15.3b.
- **Files:** modify `src/app/speech.rs`, `src/app/conversation.rs`, `src/lib.rs`, `src/testing/e2e.rs`.

**Model.** At commit, the speech use case spawns `embed(audio)` in a blocking task and stores the future with the utterance. The conversation use case gets it with the utterance that `handle` correlates (sub-phases 12.1b and 12.6). Sub-phase 15.6e uses it to identify the speaker, and sub-phase 15.6b stores it with the interaction.

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

- Constants: `PEOPLE = Set("alex", "sam")`, `MAX_SAMPLES = 2`.
- State: `home: Option[Set[PERSON]]` (`None` when presence is not known), `occupied: Option[bool]`, `proximity: PERSON -> int`, `voice: PERSON -> int`, `result: Option[PERSON]`, `source: Presence | Proximity | Voice | NoSource`, `utterance: int` (the interaction ID of the current request), `samples: int -> (PERSON, source)` (the stored samples by interaction ID).
- Actions: `setSignals(...)` (a new request: `utterance += 1`), `identify` (rules 1 to 4 of sub-phase 15.6), `enroll` (rule 5). `enroll` is enabled only when `samples` has no entry for `utterance`. After it adds the sample, `enroll` trims the person: it removes the samples of that person with the lowest IDs until `MAX_SAMPLES` are left, as the store of sub-phase 15.3b does.
- Invariants:
  - `SpeakerIsCandidate`: `result = Some(p)` implies `p` is in `home`, or `home = None`.
  - `NoVoiceOnlyEnrollment`: no value in `samples` has source `Voice`.
  - `ProfilesBounded`: each person has at most `MAX_SAMPLES` samples in `samples`.

**Acceptance criteria.**

- AC-15.5-1 [Quint]: `quint typecheck` passes.
- AC-15.5-2 [Quint]: The three invariants hold in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-15.5-2. `SpeakerIsCandidate`: `identify` chooses only from the candidate set, which is `home` when presence is known (rule 1). `NoVoiceOnlyEnrollment`: `enroll` is enabled only when `source` is `Presence` or `Proximity`. `ProfilesBounded`: only `enroll` adds a sample, and it trims the person to `MAX_SAMPLES` in the same step.
- P-15.5-1. Evidence: model check.

---

### 15.6 Speaker Signals

**Goal:** Read the signals of one voice request from the hot cache: the candidates, the occupancy of the satellite room, and the proximity and the voice similarity of each person. Sub-phases 15.6c to 15.6e use them to identify the speaker and to enroll the voice.

- **Implements:** G12. **Depends on:** 15.5.
- **Files:** modify `src/domain/speaker.rs`.

**Model.** Sub-phases 15.6 to 15.6e implement `speaker.qnt` with these rules:

1. The people are `people(cache)` (sub-phase 15.2). Only people who are `home` are candidates. Presence is known when each person has the state `home` or `not_home`. If presence is not known, all people are candidates, and no presence identification occurs.
2. When presence is known and one person is `home`, that person is the speaker with confidence 1.0 and source `Presence`.
3. If `occupied` is `Some(false)`, proximity counts 0.
4. Without a presence identification, the score of each candidate is proximity + voice similarity. The candidate with the top score is the speaker when the top score is more than `threshold` and more than each other score plus `margin`. Else no speaker is identified. The confidence is the top score. The source is `Proximity` when proximity alone is at least `enroll_threshold` (`0.5` is 1 m or less), else `Voice`.
5. Jarvis enrolls the embedding under the interaction ID only when all of these are true: the source is `Presence` or `Proximity`; the utterance has `min_enroll_ms` of voice or more; and, when the person has `min_profile` samples or more, the voice similarity to that person is at least `voice_threshold` and higher than to each other person. Thus a guest, a TV or stale presence cannot change a profile that has samples.

This sub-phase implements rule 1 and reads the signals:

- `Signals`: `home` (the entity IDs of `home(cache)` when presence is known, else `None`), `people` (the entity IDs of `people(cache)`), `occupied`, and the proximity and the voice similarity of each person by entity ID.
- `signals(cache, device, area, overrides, presence, voice) -> Signals`. `device` and `area` are the device and the area of the satellite. `overrides` are the `[[people]]` entries. `presence` is the `presence` list of the `[[satellites]]` entry of the satellite. `voice` is the result of `VoiceProfiles::similarities` for the embedding, or `None` without an embedding.
- `occupied` is `occupied(cache, device, presence)` (sub-phase 15.2). The proximity of a person is `proximity(cache, trackers(cache, person, configured), area)` (sub-phases 15.1 and 15.2c). `configured` is the `trackers` of the `[[people]]` entry of the person. Without an area, each proximity is 0.
- The voice similarity of a person is the value of the person in `voice`. A person without a value has 0. When `voice` is `None`, each person has 0.

**Acceptance criteria.**

- AC-15.6-11: When one `person` state is `unavailable`, `signals` gives `home = None`.
- AC-15.6-13: When HA has `person.alex` and `person.sam`, both are `home`, and `jarvis.toml` has no `[[people]]`, `home` of `signals` has both.
- AC-15.6-14: When only the watch of `person.alex` is in the satellite area at 1 m, `signals` gives `person.alex` the proximity 0.5 and `person.sam` the proximity 0.
- AC-15.6-15: When `voice` is `None`, `signals` gives each person the voice similarity 0. When `voice` has only `person.alex`, `person.sam` has the voice similarity 0.

**Proofs.**

- P-15.6-11 (AC-15.6-11). Ensures: when a person state is not `home` or `not_home`, `home` is `None`. Argument: rule 1 checks that each state is `home` or `not_home` before it filters, and else gives `None`.
- P-15.6-13 (AC-15.6-13). Ensures: when presence is known, `home` is exactly the entity IDs of `home(cache)`, in order. Argument: `signals` maps `home(cache)` (P-15.2-1) and reads no configuration for it.
- P-15.6-14 (AC-15.6-14). Ensures: the proximity of each person is the result of `proximity` for the trackers of the person in `area`, or 0 without an area. Argument: `signals` calls `proximity` for each person (P-15.2c-1 to P-15.2c-4).
- P-15.6-15 (AC-15.6-15). Ensures: the voice similarity of a person is the value of the person in `voice`, else 0. Argument: `signals` looks up each person in `voice` and gives 0 when the lookup finds nothing.

---

### 15.6c Identification

**Goal:** Add `identify`, which chooses the speaker from the signals of a request by rules 2 to 4 of sub-phase 15.6.

- **Implements:** G12. **Depends on:** 15.6.
- **Files:** modify `src/domain/speaker.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- `Source` (`Presence`, `Proximity`, `Voice`) and `Speaker { person, confidence, source }` are in `src/domain/speaker.rs`. Sub-phase 15.6b stores the speaker and the source with the interaction.
- `identify(signals, threshold, margin, enroll_threshold) -> Option<Speaker>`: rules 2 to 4 of sub-phase 15.6. The candidates are `home` of the signals, or `people` when `home` is `None`.
- This sub-phase adds `speaker.threshold`, `speaker.margin` and `speaker.enroll_threshold`.

**Acceptance criteria.**

- AC-15.6c-1: When only one person is home, `identify` returns that person with confidence 1.0 and the source `Presence`.
- AC-15.6c-2: When two people are home and only one watch is in the satellite area, `identify` returns the owner of that watch.
- AC-15.6c-3: When the satellite presence sensor is `off`, proximity does not identify a speaker.
- AC-15.6c-4: When two people are home, no tracker is in the area, and the voice matches one profile, `identify` returns that person with the source `Voice`.
- AC-15.6c-5: When the top two scores differ by less than `margin`, `identify` returns no speaker.
- AC-15.6c-6: When one `person` state is `unavailable`, the other is `home`, and no tracker or voice matches, `identify` returns no speaker.

**Proofs.**

- P-15.6c-1. Ensures: when `home` has one element, the result is that person with confidence 1.0 and the source `Presence`. Argument: rule 2 applies when presence is known and the candidate set has one element.
- P-15.6c-3. Ensures: with `occupied = Some(false)`, proximity adds 0 to every score. Argument: rule 3 sets proximity to 0 before rule 4 scores.
- P-15.6c-5. Ensures: `identify` returns a speaker by rule 4 only when the score of the speaker is more than `threshold` and more than each other candidate score plus `margin`. Argument: rule 4 requires both comparisons for the chosen candidate.
- P-15.6c-6. Ensures: with `home = None`, the source is not `Presence`. Argument: rule 2 applies only when `home` is `Some`.
- P-15.6c-2, P-15.6c-4. Requires: `SpeakerIsCandidate`. Argument: the maximum of rule 4 ranges over candidates only. Evidence: test for the scores.

---

### 15.6d Enrollment Rule

**Goal:** Add `enrolls`, the passive enrollment rule, and the conformance test of `speaker.qnt`. A voice match alone never adds a sample to a profile.

- **Implements:** G12, G19. **Depends on:** 15.6c.
- **Files:** modify `src/domain/speaker.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `speaker.qnt` (conformance).

**Model.**

- `enrolls(signals, speaker, voice_ms, samples, min_enroll_ms, min_profile, voice_threshold) -> bool`: rule 5 of sub-phase 15.6. `voice_ms` is the voice length of the utterance. `samples` is the number of samples in the profile of the speaker. The voice similarities are those of `signals`.
- This sub-phase adds `speaker.min_enroll_ms` and `speaker.min_profile`.
- The conformance driver is in the test block of `src/domain/speaker.rs`. It maps `setSignals` to `Signals`, with each level in tenths divided by 10, `identify` to `identify`, and `enroll(p)` to `enrolls` with `voice_ms = min_enroll_ms`. The thresholds of the driver are the model constants divided by 10. `enroll(p)` fails the step when `enrolls` is false. Else the driver adds the sample of `utterance` and trims the person to `MAX_SAMPLES`, as the store of sub-phase 15.3b does. After each step, `result`, `source` and `samples` of the model match the driver.

**Acceptance criteria.**

- AC-15.6d-1: When `quint-connect` replays 100 traces of `speaker.qnt`, `identify` and `enrolls` match the model after each step.
- AC-15.6d-2: When the source is `Presence`, the voice length is `min_enroll_ms` and the profile has fewer than `min_profile` samples, `enrolls` returns true. When the source is `Voice`, `enrolls` returns false.

**Proofs.**

- P-15.6d-2 (AC-15.6d-2). Ensures: `enrolls` is true if and only if the source is `Presence` or `Proximity`, `voice_ms >= min_enroll_ms`, and, when `samples >= min_profile`, the similarity to the speaker is at least `voice_threshold` and more than each other similarity. Argument: `enrolls` is the conjunction of these guards, as the `enroll` action of `speaker.qnt` is (`NoVoiceOnlyEnrollment`).
- P-15.6d-1. Evidence: test.

---

### 15.6e Identification Use Case

**Goal:** Identify the speaker of each voice request when its embedding completes, and enroll the voice by rule 5 of sub-phase 15.6. Jarvis knows who is speaking without an enrollment step.

- **Implements:** G12, G19. **Depends on:** 15.6d.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/app/speech.rs`, `src/app/conversation.rs`, `src/app/background.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `README.md`.

**Model.**

- `VoiceProfiles::sample_count(person) -> usize`: the number of samples in the profile of `person`.
- `FakeVoiceProfiles` gives fixed similarities and sample counts, and records each `enroll` with its person and interaction ID. A test of `src/app/background.rs` builds the conversation use case, so it gets the fake too.
- After `handle` sends the append (sub-phase 12.6), it spawns a task for a voice request with an utterance. The task awaits the embedding (sub-phase 15.4). With an embedding, it gets `similarities`, reads `signals` from the hot cache for the device and the area of the satellite, and calls `identify`. When `identify` gives a speaker and `enrolls` is true, the task calls `enroll(person, id, embedding)` with the interaction ID. The voice length is the voiced time of the utterance: the speech session counts the chunks that the VAD decides as voiced (sub-phase 6.4), and `Utterances::append` keeps that count in milliseconds with the utterance. The trailing silence before the commit is not voice, so it does not count. An embedding error or a port error gives no enroll and a log line. The reply does not wait for the task.
- `run` builds `SherpaVoices` with a `FileStore` of `speaker.voices`.
- `README.md` describes passive voice enrollment and the voice store.

**Acceptance criteria.**

- AC-15.6e-1: When `identify` returns confidence 1.0 from presence, `FakeVoiceProfiles` records an enroll for that person with the interaction ID.
- AC-15.6e-2: When the identification comes from voice alone, `FakeVoiceProfiles` records no enroll.
- AC-15.6e-3: When the person has `min_profile` samples and the voice similarity to that person is less than `voice_threshold`, or higher to another person, `FakeVoiceProfiles` records no enroll.
- AC-15.6e-4: When the utterance has less than `min_enroll_ms` of voice, `FakeVoiceProfiles` records no enroll. This holds also when the audio with its trailing silence is longer than `min_enroll_ms`.
- AC-15.6e-5: When the voice store has two samples of `person.alex`, `sample_count(person.alex)` of `SherpaVoices` returns 2.

**Proofs.**

- P-15.6e-1 to P-15.6e-4. Requires: `NoVoiceOnlyEnrollment`. Argument: the task calls `enroll` only when `enrolls` is true (P-15.6d-2), and a voice-only identification has the source `Voice`.
- P-15.6e-5. Evidence: test.

---

### 15.6b Speaker Record

**Goal:** Store the speaker, the source and the voice embedding with each voice request. The embedding stays after the log deletes the audio, so you can assign the voice of a request later.

- **Implements:** G12, G15. **Depends on:** 15.6e.
- **Files:** modify `src/ports.rs`, `src/domain/feedback.rs`, `src/adapters/sqlite.rs`, `src/testing/fakes.rs`, `src/app/conversation.rs`.

**Model.**

- `Interaction` gets `speaker: Option<EntityId>` and `source: Option<Source>`. Sub-phase 15.8 adds the source `Assigned`.
- The next migration step of the schema (sub-phase 12.4) adds the columns `speaker`, `source` and `embedding` (a BLOB of little-endian `f32` values). `prune` deletes the audio after `log.audio_days`, but the embedding stays as long as the record.
- `update(id, change)` gets the change `Speaker { speaker, source, embedding }`. `embedding: None` keeps the stored embedding. `embedding(id) -> Option<Vec<f32>>` gives the stored embedding. `list` and `get` do not read the embedding.
- When the embedding of a voice request completes, the conversation use case identifies the speaker (sub-phase 15.6e) and sends the `Speaker` update with the embedding to the log writer. It sends the update also when `identify` gives no speaker, so you can assign the voice later. The update uses the channel of the append, so it comes after the append (P-12.6c-2).

**Acceptance criteria.**

- AC-15.6b-1: When `identify` returns a speaker, the interaction record has the speaker and the source.
- AC-15.6b-2: When `identify` returns no speaker, `embedding(id)` returns the embedding of the request.
- AC-15.6b-3: When `prune` deletes the audio of an interaction, `embedding(id)` still returns its embedding.
- AC-15.6b-4: When the database has the previous schema version with records, the adapter migrates it and keeps every record.

**Proofs.**

- P-15.6b-2. Argument: the use case sends the `Speaker` update with the embedding after `identify` in both branches.
- P-15.6b-3. Argument: the audio delete of `prune` does not change the `embedding` column.
- P-15.6b-1, P-15.6b-4. Evidence: test.

---

### 15.7 Person-Specific Intents (removed)

This sub-phase is removed. The calendar intent was only an example, so no intent acts for the speaker yet. Thus no intent sets `needs_speaker`, nothing fills the `person` template variable, and Jarvis has no `next_date` and no `speaker.wait_ms`. The `needs_speaker` field of the catalog and the `person` pool variable of sub-phases 3.2 and 4.2 already exist, and they stay unchanged. The form of address (sub-phase 15.9) uses the speaker without them.

---

### 15.8 Voice Sample Assignment

**Goal:** Add `VoiceProfiles::assign`. It puts the voice sample of one request into the profile of a person, moves it to another person, or removes it.

- **Implements:** G12. **Depends on:** 15.6b.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`.

**Model.** `assign(id, person: Option<EntityId>, embedding)`. The store keeps a pick record apart from the samples: the last pick of each interaction ID, a person or none. `assign` sets the pick of `id` and removes the sample of `id` from each profile. With `Some(person)`, it adds the sample to the profile of `person` and then trims that profile to the newest `max_samples` interaction IDs (sub-phase 15.3b), so the trim can remove the new sample at once. With `None`, it adds no sample. A later `enroll` with `id` changes nothing, because `id` has a pick (P-15.3b-5). Thus a pick always wins over passive enrollment, also after a trim.

**Acceptance criteria.**

- AC-15.8-1: When the sample of an interaction is in the profile of `person.alex` and `assign(id, Some(person.sam))` runs, the sample is only in the profile of `person.sam`.
- AC-15.8-2: When `assign(id, None)` runs, the sample of `id` is in no profile.
- AC-15.8-3: When `assign(id, None)` runs and then `enroll` runs with the same ID, the sample of `id` is in no profile.
- AC-15.8-4: When the profile of `person.sam` has `max_samples` samples and `assign` adds a newer one, the profile keeps the newest `max_samples`.
- AC-15.8-5: When the profile of `person.sam` has `max_samples` newer samples and `assign(id, Some(person.sam))` runs for an older interaction, the sample of `id` is in no profile, the pick of `id` is `person.sam`, and a later `enroll` with `id` changes nothing.

**Proofs.**

- P-15.8-1, P-15.8-2. Argument: the store keys each sample by its interaction ID, so one ID has one entry, and `assign` replaces the entry of `id`.
- P-15.8-3, P-15.8-5. Argument: `assign` always sets the pick of `id`, the trim removes samples but never picks, and `enroll` does not change an ID that has a pick (P-15.3b-5).
- P-15.8-4. Argument: `assign` uses the same trim as `enroll` (P-15.3b-3).

---

### 15.8b Voice Assignment Use Case

**Goal:** Add `people` and `assign` to the `Review` port. A pick for a logged request moves its voice sample and records the person as the speaker.

- **Implements:** G12, G15. **Depends on:** 15.8.
- **Files:** modify `src/ports.rs`, `src/domain/speaker.rs`, `src/app/review.rs`, `src/app/conversation.rs`, `src/adapters/sqlite.rs`, `src/testing/fakes.rs`, `src/lib.rs`.

**Model.**

- `Source` gets `Assigned`.
- `Review::people() -> Vec<Person>`: `people` of the hot cache (sub-phase 15.2).
- `Review::assign(id, person: Option<EntityId>)`: checks that `person` is `None` or the entity of one of `people()`, and gets the embedding with `embedding(id)` (sub-phase 15.6b). Then it calls `VoiceProfiles::assign` and writes the log update `Speaker { speaker: person, source: Assigned, embedding: None }`. A person that is not in `people()` gives an error, and no embedding gives `NotFound`. Both checks run before the writes. One `tokio::sync::Mutex` of the use case is held from `VoiceProfiles::assign` to the end of the log update. Thus two picks of one interaction at the same time do not interleave, and the last pick sets both the profile and the record, as the one `assign` action of `speaker.qnt` does (sub-phase 15.8d).
- A `Speaker` update of the log (sub-phase 15.6b) whose source is not `Assigned` does not replace a stored `Assigned` speaker and source. It only stores the embedding. Thus a passive identification that arrives after a pick does not change the record, and its enroll changes no profile (P-15.3b-5).
- `FakeVoiceProfiles` keeps the samples and the pick record by the rules of sub-phase 15.8, and its `assign` can wait at a gate that the test holds. `FakeReview` gives fixed people and records each `assign`. It gives the person error for a person that is not one of its people, and `NotFound` for an ID that is not one of its interactions. Sub-phase 15.8c uses it.

**Acceptance criteria.**

- AC-15.8b-1: When `people` runs, it returns each `person` entity of the hot cache with its friendly name.
- AC-15.8b-2: When `assign(id, Some(person.sam))` runs, `FakeVoiceProfiles` records the assign, and the interaction record has the speaker `person.sam` and the source `Assigned`.
- AC-15.8b-3: When `assign(id, None)` runs, the interaction record has no speaker and the source `Assigned`.
- AC-15.8b-4: When the person is not a `person` entity of the hot cache, `assign` returns an error, and `FakeVoiceProfiles` and `FakeLog` record no change.
- AC-15.8b-5: When the interaction has no embedding, `assign` returns `NotFound`.
- AC-15.8b-8: When `assign(id, Some(person.alex))` waits inside `FakeVoiceProfiles` and `assign(id, Some(person.sam))` starts during the wait, the pick of `FakeVoiceProfiles` and the speaker of the interaction record are the same person.
- AC-15.8b-9: When the record has the source `Assigned` and an identification update with the source `Presence` comes later, the record keeps the assigned speaker and the source `Assigned`, and `embedding(id)` gives the new embedding.

**Proofs.**

- P-15.8b-4, P-15.8b-5. Argument: the use case checks the person and the embedding before both writes and returns on failure.
- P-15.8b-8. Argument: the mutex is held across the profile write and the log update, so the second call does both writes after the first call ends, and its writes are the last ones in both stores.
- P-15.8b-9. Argument: the `Speaker` update keeps the stored speaker and source when they have the source `Assigned` and the change has another source.
- P-15.8b-1 to P-15.8b-3. Evidence: test.

---

### 15.8d Assignment Conformance

**Goal:** Add the pick record and `assignVoice` to `speaker.qnt`, and replay the model against `Review::assign`. Prove that a pick always wins over passive enrollment.

- **Implements:** G12, G19. **Depends on:** 15.8b.
- **Files:** modify `quint/speaker.qnt`, `src/domain/speaker.rs`, `src/app/review.rs`, `src/testing/fakes.rs`.
- **Quint:** `speaker.qnt` (`assignVoice`, conformance).

**Model.**

- `speaker.qnt`: the new state `picks: int -> Option[PERSON]` is the pick record. The new action `assignVoice(id, p)` sets `picks[id] = p`, removes `id` from `samples`, and with `p = Some(person)` adds `samples[id] = (person, Assigned)` and trims `person` as `enroll` does. `enroll` is also disabled when `picks` has `utterance`. The new invariant `PickWins`: for each ID in `picks`, a sample of that ID has the person of the pick and the source `Assigned`, and a pick of none has no sample. `NoVoiceOnlyEnrollment` and `ProfilesBounded` still hold.
- Quint has a built-in `assign`, so the action is named `assignVoice`. With `picks`, `quint verify` of `speaker.qnt` grows too slow past a few steps, so the model sets `VERIFY_STEPS` to the largest bound that ends in about 5 minutes (`02-rules.md` section 4).
- The conformance driver of sub-phase 15.6d moves to the test block of `src/app/review.rs`. It maps `enroll` to `enroll` of `FakeVoiceProfiles` and `assignVoice` to `Review::assign`. After each step, it compares the samples of `FakeVoiceProfiles` with `samples` and its pick record with `picks`.

**Acceptance criteria.**

- AC-15.8d-1 [Quint]: `PickWins`, `NoVoiceOnlyEnrollment` and `ProfilesBounded` hold in simulation and to `VERIFY_STEPS` steps in `quint verify`. This replaces AC-15.8b-6.
- AC-15.8d-2: When `quint-connect` replays 100 traces of `speaker.qnt` with `assignVoice`, the samples and the pick record of `FakeVoiceProfiles` match `samples` and `picks` after each step. This replaces AC-15.8b-7.

**Proofs.**

- P-15.8d-1. `PickWins`: `assignVoice` sets the pick of `id` and the sample of `id` in the same step. A later trim can only remove that sample, and `enroll` is disabled for an ID in `picks`, so no action adds a sample of a picked ID with another person or source. `NoVoiceOnlyEnrollment`: `assignVoice` sets the source `Assigned`, not `Voice`. `ProfilesBounded`: `assign` trims the person in the same step as it adds the sample. Evidence: model check.
- P-15.8d-2. Evidence: test.

---

### 15.8c Voice Assignment Page

**Goal:** Add a person list to each request in the review section of the Jarvis tab. You pick the speaker of a request, and Jarvis moves its voice sample at once.

- **Implements:** G12, G15, G24. **Depends on:** 15.8d.
- **Files:** modify `src/adapters/web.rs`, `src/adapters/review.html`, `README.md`.

**Model.**

- `GET api/people` gives `[{ "entity": "<person entity>", "name": "<friendly name>" }]` from `Review::people`.
- `POST api/interactions/{id}/speaker` with `{ "person": "<person entity>" | null }` calls `Review::assign`. Success gives 204, an unknown person gives 400, and `NotFound` gives 404.
- The items of `GET api/interactions` have `speaker` and `source`.
- The page loads `api/people` once. Each interaction with a `satellite` gets the list `select.speaker`: an empty choice, then each person by name. The list shows the speaker of the interaction, which is the identified or the picked person. A change of the list sends the `speaker` request at once.

**Acceptance criteria.**

- AC-15.8c-1: When `GET api/people` runs, it returns the people of `FakeReview`.
- AC-15.8c-2: When `POST api/interactions/{id}/speaker` has `{"person": "person.sam"}`, `FakeReview` receives `assign(id, Some(person.sam))` and the adapter returns 204. With `null`, `FakeReview` receives `assign(id, None)`.
- AC-15.8c-3: When the person is unknown, the adapter returns 400. When the interaction has no embedding, the adapter returns 404.
- AC-15.8c-4: When `GET /` runs, the template `template#interaction` has a `select.speaker` element.

**Proofs.**

- P-15.8c-3. Argument: the handler maps the person error of `Review::assign` to 400 and `NotFound` to 404.
- P-15.8c-1, P-15.8c-2, P-15.8c-4. Evidence: test.

---

### 15.9 Form of Address

**Goal:** Add the form of address to response selection and pool validation. A reply uses `{{ address }}` only when the speaker has a form of address, and each pool keeps a reply for a request with no known speaker.

- **Implements:** G12. **Depends on:** 15.8c.
- **Files:** modify `src/domain/responses.rs`, `src/domain/decide.rs`, `src/app/conversation.rs`.

**Model.**

- `address` is a template variable of each catalog intent pool: the form of address of the speaker. A proposal does not act for the speaker, so it does not get `address` (sub-phase 14.2b).
- `addressed(pool) -> Vec<bool>`: for each response, true when its template uses `address` (`minijinja` `undeclared_variables`).
- Pool selection (sub-phase 4.1) gets `address: Option<&str>`. A response that uses `address` is eligible only when `address` is `Some` and not empty. A response without `address` is always eligible. The rule of sub-phase 4.1 then applies to the eligible responses: with two or more eligible responses, `last` is not allowed. With one eligible response, selection gives it, also when it is `last`. When no response is eligible, each response is eligible. Validation prevents this case (P-15.9-4).
- Pool validation (sub-phase 4.2): a pool with a response that uses `address` must also have a response without `address`. Else the error names the pool. Thus selection always has an eligible response.
- `Request` (sub-phase 4.5) gets `address: Option<String>`, the form of address of the speaker. When it is `Some`, `decide` adds it to the template context as `address`, and the reply of the intent selects with it.
- `handle` gives each request `address = None`. Sub-phase 15.9c gives it the form of address of the speaker.

**Acceptance criteria.**

- AC-15.9-1: When the request has the form of address `Lady Lang`, and selection chooses the response `I will turn on the lights for you, {{ address }}.`, `decide` gives the reply "I will turn on the lights for you, Lady Lang."
- AC-15.9-2: When `address` is `None`, selection never chooses a response that uses `address`.
- AC-15.9-3: When `address` is empty, selection never chooses a response that uses `address`.
- AC-15.9-4: When each response of a pool uses `address`, validation returns an error that names the pool.
- AC-15.9-7: When `address` is `None` and the pool has one response without `address` and one with it, two selections in a row, with `record_use` after each, both give the response without `address`.

**Proofs.**

- P-15.9-2, P-15.9-3. Ensures: when the pool has a response without `address`, a response that uses `address` is eligible only when `address` is `Some` and not empty, and selection gives an eligible response. Argument: selection filters the candidates by this predicate before it chooses. The flags of `addressed` come from `minijinja`, which Verus cannot see (P-4.2-2). Thus Verus proves the rule over the flags, and the flags are evidence: test.
- P-15.9-4. Ensures: the address check of a pool fails if and only if each flag of `addressed` is true. Argument: the check looks for one response without `address`.
- P-15.9-7. Requires: the pool has a response without `address` (P-15.9-4). Ensures: with one eligible response, selection gives it. Argument: the exclusion of `last` applies only to two or more eligible responses, so the one eligible response is the only candidate (P-4.1-5 on the eligible responses).
- P-15.9-1. Evidence: test.

---

### 15.9c Address Reply

**Goal:** Address each person in their own way. With the form of address "Lady Lang" for Ali, a reply to Ali can say "I will turn on the lights for you, Lady Lang."

- **Implements:** G12. **Depends on:** 15.9.
- **Files:** modify `src/app/conversation.rs`, `src/app/speech.rs`, `src/app/background.rs`, `src/lib.rs`, `jarvis.toml`, `README.md`.

**Model.**

- This sub-phase adds the optional table `[address]` to `jarvis.toml`. A key is a `person` entity ID, and its value is the form of address, a free text. Example: `"person.ali" = "Lady Lang"`. An empty value is the same as no value. Sub-phase 15.9b sets it from the Jarvis tab.
- The reply does not wait for the voice embedding. `handle` calls `signals` with no embedding and then `identify`, so only presence and proximity identify the speaker (sub-phase 15.6). The `address` of the request (sub-phase 15.9) is the value of the speaker in `[address]`, or `None` without a speaker or a value.
- The answer of a fetch job (sub-phase 4.9d) and a prefetch reply (sub-phase 14.10d) select with `address = None`.
- `prerender` (sub-phase 6.3b) also synthesizes each template whose variables are in `{area, address}`, for each area and each non-empty value of `[address]`. The pre-render of backfill responses and of learned intents gets no values, because their pools do not use `address`. A change of `[address]` restarts Jarvis (sub-phase 12.9f), so the pre-render has the new values.
- A pool with a response that uses `address` gets no backfill, because a backfill pool uses only `area` (sub-phase 8.1).
- `responses.toml` does not change. `README.md` describes `[address]` and gives the example response `"I will turn on the lights for you, {{ address }}."`.

**Acceptance criteria.**

- AC-15.9c-1: When only `person.ali` is home with `"person.ali" = "Lady Lang"` in `[address]`, and selection chooses the response `I will turn on the lights for you, {{ address }}.`, `handle` replies "I will turn on the lights for you, Lady Lang."
- AC-15.9c-2: When `prerender` runs with `"person.ali" = "Lady Lang"`, the audio cache has each `address` template for each area name with `Lady Lang`. This replaces AC-15.9-5.
- AC-15.9c-3: When `handle` chooses a reply, it does not wait for the embedding, and it calls `signals` with no embedding. This replaces AC-15.9-6.

**Proofs.**

- P-15.9c-3. Argument: the `signals` call of the reply passes `None` for the embedding, and `handle` does not await the embedding future.
- P-15.9c-1, P-15.9c-2. Evidence: test.

---

### 15.9b Address Settings and System Two

**Goal:** Set the form of address of each person in the settings of the Jarvis tab, and give it to System Two. A System Two answer can then address the speaker too.

- **Implements:** G12, G24. **Depends on:** 15.9c.
- **Files:** modify `src/adapters/settings.rs`, `src/adapters/review.html`, `src/adapters/openai.rs`, `src/ports.rs`, `src/app/conversation.rs`, `README.md`.

**Model.**

- Settings `read` (sub-phase 12.9d) also gives one setting `address.<entity>` for each key of `[address]`, in the group `People`. `edit` accepts the name `address.<entity>` when the entity ID starts with `person.` and the value is a JSON string. It sets the key in `[address]`, and it adds the table when the file has no `[address]`. An empty string removes the key. Other text stays the same.
- The settings section of the page lists each person of `GET api/people` (sub-phase 15.8c) with an "Address as" text field (`input.address`). The field shows the value of the setting `address.<entity>`, or is empty. Save sends the changed fields with the other settings (sub-phase 12.9e).
- The request of `Reasoner::answer` gets the form of address of the speaker that the reply identified (P-15.9c-3). When it is not empty, the system prompt gets the line `Address the user as <address>.`.

**Acceptance criteria.**

- AC-15.9b-1: When `jarvis.toml` has `"person.ali" = "Lady Lang"` in `[address]`, `read` returns the setting `address.person.ali` with `Lady Lang` in the group `People`.
- AC-15.9b-2: When `edit` sets `address.person.ali` to `Mrs Lang`, the result has the new value and the other text stays. When the value is empty, the key is removed. When the file has no `[address]`, `edit` adds the table with the key.
- AC-15.9b-3: When `edit` gets `address.light.kitchen`, it returns an error that names the field.
- AC-15.9b-4: When `GET /` runs, the settings section has a `template#person` with an `input.address` element.
- AC-15.9b-5: When the speaker has the form of address `Lady Lang`, the OpenAI request has the line `Address the user as Lady Lang.`. With no speaker or an empty form of address, the request has no such line.

**Proofs.**

- P-15.9b-3. Argument: `edit` checks the prefix `address.person.` before it changes the document, and returns the error on the first name that fails.
- P-15.9b-5. Argument: the prompt builder adds the line only for a `Some` value that is not empty.
- P-15.9b-1, P-15.9b-2, P-15.9b-4. Evidence: test.

## Later

- Measure on the GPU host how long a reply takes when it waits for the voice embedding. Then decide if a reply with `{{ address }}` waits for the voice. This note has no criteria yet.

## Field Log

These steps are not tests and gate nothing.

1. In HA, open each satellite device page. Record its presence and Bluetooth entities.
2. If the satellite firmware has a Bluetooth proxy option, turn it on.
3. Install Bermuda. Add iPhones and Apple Watches with the Private BLE Device integration first.
4. Add each Bermuda `device_tracker` to the correct `person` entity.
5. Start Jarvis. Make sure the discovery report names each satellite presence sensor and each HA person with their trackers.
6. Walk to each satellite with a phone. Make sure the tracker area changes to the satellite area.
7. In the review section of the Jarvis tab, pick the speaker of some requests. Make sure that later requests of that person get the voice source more often.
8. In the settings of the Jarvis tab, set "Address as" for Ali to "Lady Lang". Add a response with `{{ address }}` to a pool. Ask Jarvis from the Office satellite while only Ali is home. Make sure the reply says "Lady Lang".
