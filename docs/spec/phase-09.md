# Phase 9: End-to-End Verification

This phase proves the latency goal for the full pipeline.

---

### 9.1 Pipeline Harness

**Goal:** Add a test harness that drives the real Wyoming adapter in HA's order: `asr`, then `handle`, then `tts`. Later latency tests measure the whole pipeline with real models.

- **Implements:** G10. **Depends on:** 8.4.
- **Files:** create `src/testing/e2e.rs`. Modify `src/lib.rs`.

**Model.** The harness sends `speech(text)` in 20 ms chunks, sends the transcript to `handle` on a second connection, then sends `synthesize` with the reply. `[GPU host]` tests send the chunks at real-time pace. The other tests send them with no pause, so they do not wait on the wall clock. The harness uses the real Wyoming adapter, the real application, the real sherpa-onnx adapters, the ONNX adapter with the fixture model, `FakeHome` and `FakeReasoner`. It records the time of the last voiced sample and of each event of the response latency (`00-overview.md` section 0.3).

**Acceptance criteria.**

- AC-9.1-1: When the harness runs "turn on the lights", it receives audio for a `lights_on` response.
- AC-9.1-2: When the harness runs "turn on the lights", `FakeHome` records `homeassistant.turn_on`.

**Proofs.**

- P-9.1-1, P-9.1-2. Evidence: test.

---

### 9.1b Exact Silence

**Goal:** Count the silence after a VAD speech segment to the sample, and set `early_commit_ms = 100` and `vad_min_silence_ms = 60`. The early commit then comes about 32 ms to 56 ms sooner, so the latency tests of sub-phase 9.2 can meet G1.

- **Implements:** G1, G2. **Depends on:** 9.1.
- **Files:** modify `src/adapters/sherpa.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- In a VAD speech segment, `voice_end_ms = vad_ms`, so the silence is 0 (sub-phase 6.6b). Thus an early commit can occur only after the VAD ends the segment. This does not change.
- Before this sub-phase, `vad_ms` counted only full 32 ms VAD frames. Out of a segment, it also held back the last full frame, which can start a new segment. Thus at the segment end the silence was `vad_min_silence_ms` - 32 ms, and it grew in 32 ms steps. The early commit came one held frame (32 ms) and up to one frame step late.
- Out of a speech segment, `vad_ms` is now the end of all audio that the stream accepted, to the sample. The adapter holds no frame there. Thus at the segment end the silence is `vad_min_silence_ms` plus the samples that the VAD has not run yet (less than one frame), and it grows with each accepted sample. This replaces the `vad_ms` of sub-phase 6.6b out of a segment.
- Speech that starts after a segment end counts as silence until the VAD detects it. `commit.qnt` does not change: its chunks are the decisions that the adapter reports in `vad_ms`.
- `jarvis.toml` sets `early_commit_ms = 100` and `vad_min_silence_ms = 60`. `vad_threshold` stays 0.5. The test of AC-6.5-6 replaces the old values in `jarvis.toml`, so it changes with them.

**Acceptance criteria.**

- AC-9.1b-1: When `SherpaStt` gets `speech("turn on the kitchen lights")` in 20 ms chunks, each partial with `vad_ms > voice_end_ms` has `vad_ms` equal to the length of the accepted audio.
- AC-9.1b-2: When `SherpaStt` gets `speech("turn on the kitchen lights")` in 20 ms chunks, the first partial with `vad_ms > voice_end_ms` has `vad_ms - voice_end_ms` of `vad_min_silence_ms` or more and less than `vad_min_silence_ms` + 32 ms.
- AC-9.1b-3: `Config::parse` of `jarvis.toml` gives `asr.early_commit_ms = 100` and `asr.vad_min_silence_ms = 60`.

**Proofs.**

- P-9.1b-1 to P-9.1b-3. Evidence: test.

---

### 9.2 Latency Tests

**Goal:** Add the end-to-end latency tests for commands, fetches and System Two fillers. The `gpu-test` job runs them on the GPU host with `device = cuda`, the Nemotron first pass and the Parakeet second pass, against the 300 ms goal and the targets of AC-9.2-4 and AC-9.2-5. Set `vad_threshold = 0.7`.

- **Implements:** G1, G2, G6. **Depends on:** 9.1b.
- **Files:** modify `src/testing/e2e.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** The tests run the harness of sub-phase 9.1 on the GPU host with `device = cuda`, the Nemotron first pass and the Parakeet second pass, at real-time pace. Each phrase runs once as a warm-up, then 20 times. The response latency of a run is the sum of three segments: from the last voiced sample to `transcript`, from `transcript` to `handled`, and from `synthesize` to the first audio chunk. The harness sends each next event at once, so the sum equals the time from the last voiced sample to the first audio chunk. The p95 of 20 runs is the 19th smallest value. The latency to a filler uses the same segments with the filler text.

- The last voiced sample is the last sample of the last 20 ms frame of `speech(text)` with an RMS above -40 dBFS. The TTS audio has a long, quiet tail, and a person does not hear it as speech. This replaces the last sample that is not zero of sub-phase 9.1.
- `start_app` gives the handle of the startup pre-render task, so the harness waits for the pre-render before the warm-up. Thus no measured run misses the audio cache or shares the TTS with the pre-render.
- The reference house has no `weather.home` entity, so the harness adds a `weather.home` state to its copy of the house. "What is the weather" then gives the weather fetch and its filler.
- Each test prints the p95 of each phrase, so a run that passes also gives the numbers.
- `jarvis.toml` sets `vad_threshold = 0.7`. With 0.5, the commands had a p95 of 273 ms to 435 ms. With 0.7, they have 216 ms to 351 ms, but "office lights on" gave "Office lights". The reply was still right. The tests that read `jarvis.toml` change with it.
- AC-9.2-4 and AC-9.2-5 have the measured targets of `01-architecture.md` section 6. Sub-phase 9.3 brings the target of AC-9.2-4 back to 400 ms.

**Acceptance criteria.**

Each measured command and fetch phrase has a top intent at or above `model.commit_threshold` with the fixture model, and a response in the audio cache.

- AC-9.2-1 [GPU host]: For "turn on the office fan" and "start the office fan", the response latency has a p95 of 300 ms or less over 20 runs.
- AC-9.2-2 [GPU host]: For "what is the weather", the latency to the filler audio has a p95 of 300 ms or less.
- AC-9.2-3 [GPU host]: For an `unknown` phrase, with `FakeReasoner` delayed by 2 s, the latency to the filler audio has a p95 of `endpoint_ms` + 100 ms or less. An `unknown` prediction is not confident, so the commit waits `endpoint_ms`.
- AC-9.2-4 [GPU host]: For a command whose response is not in the audio cache, the response latency has a p95 of 650 ms or less.
- AC-9.2-5 [GPU host]: For "turn on the office lights", "office lights on" and "turn the office lights on", the response latency has a p95 of 360 ms or less over 20 runs.

**Proofs.**

- P-9.2-1 to P-9.2-5. Evidence: test.

---

### 9.3 Streaming TTS

**Goal:** Send the first audio chunk of a response that is not in the audio cache before the TTS has synthesized the whole sentence. The uncached response latency then comes back to 400 ms.

- **Implements:** G1. **Depends on:** 9.2.
- **Files:** the spec PR of the mechanism lists them.

**Model.** Supertonic-3 synthesizes each text chunk as a whole. The sherpa-onnx callback gives the audio of one text chunk of up to `max_len` characters (default 300), with `silence_duration` (default 0.3 s) between chunks. Thus a short sentence gives one callback, after the whole synthesis. The mechanism is decided in a spec PR before the sub-phase starts.

**Acceptance criteria.**

- AC-9.3-1 [GPU host]: For a command whose response is not in the audio cache, the response latency has a p95 of 400 ms or less over 20 runs.

**Proofs.**

- P-9.3-1. Evidence: test.

## Field Log

These steps are not tests and gate nothing. Record the results in the field log of the PR that closes the phase stack.

1. In HA, add the Wyoming integration with the Jarvis host and port.
2. Make an Assist pipeline with Jarvis for STT, conversation agent and TTS. Set "Prefer handling commands locally" to off. Assign it to each satellite.
3. Say each phrase in `00-overview.md` section 1.1 ten times. Record the time from the end of speech to the first sound from the satellite.
4. Ask a question that gets a filler. Make sure the satellite entity state becomes `idle` only after the satellite has played the filler (`01-architecture.md` decision 5).

The fixture model knows only the seed examples. The Phase 13 field log repeats this with a trained model.
