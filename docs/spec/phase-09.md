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

### 9.2 Latency Tests

**Goal:** Add the end-to-end latency tests for commands, fetches and System Two fillers. The `gpu-test` job runs them on the GPU host with `device = cuda`, the Nemotron first pass and the Parakeet second pass, against the 300 ms goal.

- **Implements:** G1, G2, G6. **Depends on:** 9.1.
- **Files:** modify `src/testing/e2e.rs`.

**Model.** The tests run the harness of sub-phase 9.1 on the GPU host with `device = cuda`, the Nemotron first pass and the Parakeet second pass, at real-time pace. Each phrase runs once as a warm-up, then 20 times. The response latency of a run is the sum of three segments: from the last voiced sample to `transcript`, from `transcript` to `handled`, and from `synthesize` to the first audio chunk. The harness sends each next event at once, so the sum equals the time from the last voiced sample to the first audio chunk. The p95 of 20 runs is the 19th smallest value. The latency to a filler uses the same segments with the filler text.

**Acceptance criteria.**

Each measured command and fetch phrase has a top intent at or above `model.commit_threshold` with the fixture model, and a response in the audio cache.

- AC-9.2-1 [GPU host]: For five command phrases, the response latency has a p95 of 300 ms or less over 20 runs.
- AC-9.2-2 [GPU host]: For "what is the weather", the latency to the filler audio has a p95 of 300 ms or less.
- AC-9.2-3 [GPU host]: For an `unknown` phrase, with `FakeReasoner` delayed by 2 s, the latency to the filler audio has a p95 of `endpoint_ms` + 100 ms or less. An `unknown` prediction is not confident, so the commit waits `endpoint_ms`.
- AC-9.2-4 [GPU host]: For a command whose response is not in the audio cache, the response latency has a p95 of 400 ms or less.

**Proofs.**

- P-9.2-1 to P-9.2-4. Evidence: test.

## Field Log

These steps are not tests and gate nothing. Record the results in the field log of the PR that closes the phase stack.

1. In HA, add the Wyoming integration with the Jarvis host and port.
2. Make an Assist pipeline with Jarvis for STT, conversation agent and TTS. Set "Prefer handling commands locally" to off. Assign it to each satellite.
3. Say each phrase in `00-overview.md` section 1.1 ten times. Record the time from the end of speech to the first sound from the satellite.
4. Ask a question that gets a filler. Make sure the satellite entity state becomes `idle` only after the satellite has played the filler (`01-architecture.md` decision 5).

The fixture model knows only the seed examples. The Phase 13 field log repeats this with a trained model.
