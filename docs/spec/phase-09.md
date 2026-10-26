# Phase 9: End-to-End Verification

This phase proves the latency goal for the full pipeline.

---

### 9.1 Pipeline Harness

**Goal:** Add a test harness that drives the real Wyoming adapter in HA's order: `asr`, then `handle`, then `tts`. Later latency tests measure the whole pipeline with real models.

- **Implements:** G10. **Depends on:** 8.4.
- **Files:** create `src/testing/e2e.rs`. Modify `src/main.rs`.

**Model.** The harness sends `speech(text)` at real-time pace in 20 ms chunks, sends the transcript to `handle` on a second connection, then sends `synthesize` with the reply. It uses the real Wyoming adapter, the real application, the real sherpa-onnx adapters, the ONNX adapter with the fixture model, `FakeHome` and `FakeReasoner`. It records the time of the last voiced sample and of the first audio byte.

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

**Acceptance criteria.**

- AC-9.2-1 [GPU host]: For five command phrases, the response latency has a p95 of 300 ms or less over 20 runs.
- AC-9.2-2 [GPU host]: For "what is the weather", the latency to the filler audio has a p95 of 300 ms or less.
- AC-9.2-3 [GPU host]: For an `unknown` phrase, with `FakeReasoner` delayed by 2 s, the latency to the filler audio has a p95 of 300 ms or less.

**Proofs.**

- P-9.2-1 to P-9.2-3. Evidence: test.

## Field Log

These steps are not tests and gate nothing. Record the results in the field log of the PR that closes the phase stack.

1. In HA, add the Wyoming integration with the Jarvis host and port.
2. Make an Assist pipeline with Jarvis for STT, conversation agent and TTS. Set "Prefer handling commands locally" to off. Assign it to each satellite.
3. Say each phrase in `00-overview.md` section 1.1 ten times. Record the time from the end of speech to the first sound from the satellite.

The fixture model knows only the seed examples. The Phase 13 field log repeats this with a trained model.
