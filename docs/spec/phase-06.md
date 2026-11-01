# Phase 6: Speech

This phase adds TTS, the shared ONNX Runtime, the audio cache, the commit policy with its Quint model, two-pass STT and the Wyoming speech events. Jarvis then serves all three Wyoming services.

---

### 6.1 TTS Adapter

**Goal:** Add the `TextToSpeech` port and `SherpaTts` with the Piper voice, and the `models` recipe. Jarvis can now turn text into audio.

- **Implements:** G1. **Depends on:** 5.4.
- **Files:** create `src/adapters/sherpa.rs`, `src/domain/speech.rs`. Modify `src/ports.rs`, `src/lib.rs`, `justfile`, `.github/workflows/ci.yml`, `jarvis.toml`, `Cargo.toml`.

**Model.** `TextToSpeech::synth(text) -> Result<Audio>`. `Audio { samples: Vec<i16>, rate }` is in `src/domain/speech.rs`. `SherpaTts` wraps `OfflineTts` with `[tts].model_dir`. `just models` downloads the 20M zipformer and the Piper voice into `models/`. It checks the SHA-256 of each file and downloads only a missing or wrong file. `models` is a setup recipe: `check` and the `rust-test` job run it before the tests, and tests do not download. CI caches `models/`.

**Acceptance criteria.**

- AC-6.1-1: When `just models` has run, the STT and TTS model folders exist in `models/`, and each file has the SHA-256 that the recipe lists. When `ci.yml` is parsed, `rust-test` runs `just models` before `just cov`.
- AC-6.1-2: When `SherpaTts::synth("Hello")` runs, it returns 16-bit mono audio with more than 0 samples at the model rate.
- AC-6.1-3: When `synth("")` runs, it returns zero samples and no error.

**Proofs.**

- P-6.1-1, P-6.1-2. Evidence: test.
- P-6.1-3. Argument: `synth` returns empty audio for empty text before it calls the engine.

---

### 6.2 Shared ONNX Runtime and GPU Recipes

**Goal:** Load one shared ONNX Runtime library for `sherpa-onnx` and `ort`, and add the GPU model and GPU test recipes. Both crates now work in one process, and the GPU host runs every model on CUDA.

- **Implements:** G18. **Depends on:** 6.1.
- **Files:** modify `Cargo.toml`, `src/adapters/onnx.rs`, `src/adapters/sherpa.rs`, `justfile`, `.github/workflows/release.yml`, `xtask/src/gates.rs`.

**Model.**

- `sherpa-onnx` uses feature `shared`. `ort` uses feature `load-dynamic`. `SHERPA_ONNX_LIB_DIR` and `ORT_DYLIB_PATH` point to one folder with `libonnxruntime` (decision 27).
- `gpu-models` downloads the Nemotron 160 ms model, the Parakeet Unified offline model and the sherpa-onnx `cuda-12.x-cudnn-9.x` `linux-x64-gpu` libraries.
- `gpu-test` (sub-phase 0.13) now runs `gpu-models` first and sets `SHERPA_ONNX_LIB_DIR` and `ORT_DYLIB_PATH` to the CUDA libraries.
- `models` also downloads the sherpa-onnx CPU shared libraries for the host system. `test` and `cov` set `SHERPA_ONNX_LIB_DIR` and `ORT_DYLIB_PATH` to them.
- `device` selects the provider for `SherpaTts` and `OnnxClassifier`.
- sherpa-onnx falls back to the CPU with only a log line when the CUDA provider does not load. Thus a test checks the CUDA provider of a sherpa-onnx model by the GPU memory of the test process (`nvidia-smi`): the memory grows when the model loads.
- The Linux release job builds against the CUDA libraries and puts them in the archive. The macOS archive has the CPU libraries. Both release builds set the run path to the folder of the binary (`$ORIGIN` on Linux, `@executable_path` on macOS) through `RUSTFLAGS`, so the binary finds the shared libraries in its archive. The `jarvis` unit sets `ORT_DYLIB_PATH` (`01-architecture.md` section 10).

**Acceptance criteria.**

- AC-6.2-1: When one test process loads the fixture classifier and `SherpaTts`, both predict or synthesize without error.
- AC-6.2-2 [GPU host]: With `device = cuda`, `SherpaTts` and `OnnxClassifier` run in one process on the CUDA execution provider.
- AC-6.2-3: When `release.yml` is parsed, the Linux build job has a verification step that fails unless the archive contains `libonnxruntime` and the CUDA provider library, and `ldd jarvis` in the extracted archive reports no missing library.

**Proofs.**

- P-6.2-1, P-6.2-2. Evidence: test.
- P-6.2-3. Evidence: test (a structure test of `release.yml`).

**Field log.** Push a tag. Record that the Linux archive passes its verification step. On the Intel MacBook, run `just models` and `just test`. Record that the sherpa-onnx macOS x86_64 libraries load ONNX Runtime 1.28.

---

### 6.3 Audio Cache and Speak

**Goal:** Add the speech use case with `speak` and the audio cache, and pre-render every pool response at startup. A TTS request for a known response is now a memory read.

- **Implements:** G1, G4. **Depends on:** 6.2.
- **Files:** create `src/app/speech.rs`. Modify `src/domain/speech.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `benches/domain.rs`.

**Model.**

- `AudioCache: DashMap<text, Audio>`. `speak(text)`: a hit returns the cached audio. A miss calls `synth` and returns the result without a store. Only `prerender` (and the prefetch of sub-phase 14.10, which replaces the old answer) adds audio, so the cache does not grow with dynamic texts.
- `prerender(pools, areas)`: for each template whose only variable is `area` (or no variable), render it for each area and synthesize it.
- `chunks(audio, size) -> Vec<&[i16]>` and `duration(audio) -> Duration` are in `domain/speech.rs`.
- `FakeTts` records each text.

**Acceptance criteria.**

- AC-6.3-1: When the text is in the cache, `speak` returns the cached audio and `FakeTts` records no call.
- AC-6.3-2: When startup completes, the cache contains each such pool text for each area name.
- AC-6.3-3: When a template uses a variable other than `area`, startup does not synthesize it.
- AC-6.3-4: When `chunks(audio, 1024)` runs, the joined chunks equal the original audio, and every chunk except the last has 1024 samples.
- AC-6.3-5: When `duration` gets 22050 samples at 22050 Hz, it returns 1 s.
- AC-6.3-6 [Bench]: A cache hit for a response of 2 s uses 100,000 instructions or fewer.
- AC-6.3-7: When `speak` gets a text that is not in the cache, `FakeTts` records one call and the cache size does not change.

**Proofs.**

- P-6.3-1. Argument: `speak` returns from the hit branch before the `synth` branch.
- P-6.3-3. Argument: `prerender` filters templates by their variable set, which must be a subset of `{area}`.
- P-6.3-4 (AC-6.3-4). Ensures: the concatenation of the chunks is the audio, and each chunk except the last has `size` samples. Argument: `chunks` is `samples.chunks(size)`, which partitions the slice in order into pieces of `size` with a shorter last piece. Concatenation of a partition gives the slice.
- P-6.3-5. Ensures: `duration(audio) = len(samples) / rate`. Argument: the function is this division. 22050 samples at 22050 Hz give 1 s.
- P-6.3-2, P-6.3-7. Evidence: test.
- P-6.3-6. Evidence: benchmark.

---

### 6.4 Commit Quint Model

**Goal:** Add `quint/commit.qnt`, the model of the commit policy over time, with a VAD that decides each chunk late. It proves that Jarvis commits only when a rule allows it, never waits past that point, and never counts undecided speech as silence.

- **Implements:** G1, G2, G19. **Depends on:** 6.3.
- **Files:** create `quint/commit.qnt`.
- **Quint:** `commit.qnt`.

**Model.**

- Constants (in 100 ms units): `EARLY = 2`, `ENDPOINT = 6`, `MAX = 20`, `LAG = 2` (the VAD decides a chunk at most `LAG` chunks late).
- State: `chunks: List[bool]` (true for a voiced chunk), `decided` (the number of chunks that the VAD decided), `voiceEnd` (the last decided voiced chunk, `-1` before any), `text: bool` (the partial is not empty), `confident: bool`, `committed: bool`, `reason`, `commitPos`.
- Actions, each followed by "commit if allowed": `tick(voiced)` (enabled when `len(chunks) - decided < LAG`: append a chunk), `decide` (enabled when `decided < len(chunks)`: `decided += 1`, and `voiceEnd := decided` when that chunk is voiced), `partial(nonEmpty, conf)` (`text := nonEmpty`, `confident := conf`). A commit records `reason` and `commitPos := decided`.
- `silence = decided - voiceEnd`. `allowed = (voiceEnd >= 0 and text and ((confident and silence >= EARLY) or silence >= ENDPOINT)) or len(chunks) >= MAX`. Silence counts from the last voiced chunk, not from the last partial change, so decoder delay does not add to the wait. Silence counts only decided chunks, so a late VAD decision cannot commit early. Before any speech, and with an empty partial, only `MAX` commits.
- Invariants:
  - `CommitOnlyWhenAllowed`: `committed` implies `allowed` was true at the commit step (kept in `reason`).
  - `NoLateCommit`: `not committed` implies `not allowed`.
  - `NoCommitDuringSpeech`: a commit for silence has `EARLY` unvoiced chunks in `chunks` right before `commitPos`.

**Acceptance criteria.**

- AC-6.4-1 [Quint]: `quint typecheck` passes.
- AC-6.4-2 [Quint]: `CommitOnlyWhenAllowed` holds in simulation and to 10 steps in `quint verify`.
- AC-6.4-3 [Quint]: `NoLateCommit` holds in simulation and to 10 steps in `quint verify`.
- AC-6.4-4 [Quint]: `NoCommitDuringSpeech` holds in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-6.4-2 (AC-6.4-2). Argument: `committed` becomes true only in the commit step that follows an action, under `if allowed`, and `reason` records which disjunct was true.
- P-6.4-3 (AC-6.4-3). Argument by induction: each action that can change an input of `allowed` evaluates `allowed` after its update and commits in the same step when it is true. So no reachable state has `allowed` true and `committed` false.
- P-6.4-4 (AC-6.4-4). Argument by induction: `decide` moves `voiceEnd` to each voiced chunk that it decides. So every decided chunk after `voiceEnd` is unvoiced. A commit for silence needs `decided - voiceEnd >= EARLY`, so the `EARLY` chunks before `commitPos = decided` are unvoiced. The chunks after `decided` do not count.
- P-6.4-1. Evidence: model check.

---

### 6.5 Commit Policy

**Goal:** Add `should_commit`, the pure commit rule of the model. Jarvis can now end listening 200 ms after the last voiced sample of confident speech and 600 ms after uncertain speech.

- **Implements:** G1, G2, G19. **Depends on:** 6.4.
- **Files:** modify `src/domain/speech.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `commit.qnt` (conformance).

**Model.** `should_commit(silence_ms: Option, has_text, audio_ms, confident, policy) -> bool` is the `allowed` formula of `commit.qnt` with `early_commit_ms`, `endpoint_ms` and `max_utterance_ms`. `silence_ms` is `vad_ms - voice_end_ms` (sub-phase 6.6): the decided audio after the last voiced sample, and `None` before any speech. A pause of `early_commit_ms` or more after a confident command ends the utterance. This is the cost of G1. `confident = top label is not unknown and p(top) >= model.commit_threshold`. This sub-phase adds `[asr]` (`early_commit_ms`, `endpoint_ms`, `max_utterance_ms`) and `model.commit_threshold`.

**Acceptance criteria.**

- AC-6.5-1: When `confident` is true and `silence_ms = early_commit_ms`, `should_commit` returns true.
- AC-6.5-2: When `confident` is false and `silence_ms = endpoint_ms - 1`, `should_commit` returns false. At `endpoint_ms`, it returns true.
- AC-6.5-3: When `audio_ms = max_utterance_ms`, `should_commit` returns true for any other input.
- AC-6.5-4: When `quint-connect` replays 100 traces of `commit.qnt`, `should_commit` equals the model `allowed` after each step.
- AC-6.5-5: When `silence_ms` is `None` or `has_text` is false, and `audio_ms < max_utterance_ms`, `should_commit` returns false for any `confident`.
- AC-6.5-6: When `endpoint_ms <= early_commit_ms`, or `model.commit_threshold < model.threshold`, `Config::parse` returns an error that names both fields.

**Proofs.**

- P-6.5-1 to P-6.5-3, P-6.5-5. Ensures: `should_commit = (silence_ms = Some(s) and has_text and ((confident and s >= early_commit_ms) or s >= endpoint_ms)) or audio_ms >= max_utterance_ms`. Argument: `should_commit` is the disjunction of the `MAX` comparison and the two silence comparisons, which are guarded by `Some(silence)` and `has_text`. Each criterion makes one disjunct true, or all false.
- P-6.5-6. Ensures: a configuration that breaks a relation of sub-phase 2.2 gives `Err` with both field names. Argument: `validate` compares the two fields of each relation and returns the first failure.
- P-6.5-4. Evidence: test.

---

### 6.6 First-Pass STT Adapter

**Goal:** Add the `SpeechToText` port with the streaming first pass and its sherpa-onnx adapter. Jarvis now has partial text while the user speaks.

- **Implements:** G2. **Depends on:** 6.5.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `justfile`, `jarvis.toml`, `Cargo.toml`.

**Model.** `SpeechToText::stream() -> Box<dyn SttStream>`. `SttStream`: `accept(samples)`, `partial() -> Partial { text, voice_end_ms: Option, vad_ms }`, `reset()`. `SherpaStt` wraps `OnlineRecognizer` with `[asr].model_dir`, and the sherpa-onnx Silero VAD with `[asr].vad_model`, `vad_threshold` and `vad_min_silence_ms` on the same samples. `vad_ms` is the end of the audio that the VAD has decided: audio that the VAD still holds to decide the start or the end of a segment is not decided. `voice_end_ms` is the audio position of the end of the last voiced frame, and `None` before any speech. While the VAD is in a speech segment, `voice_end_ms = vad_ms`. This sub-phase adds `asr.model_dir`, `asr.vad_model`, `asr.vad_threshold` and `asr.vad_min_silence_ms`. `just models` also downloads the Silero VAD model. `FakeStt` returns scripted partials with voice end times. `speech(text)` in `fakes.rs` synthesizes text with `SherpaTts`, resamples it to 16 kHz with `rubato`, and adds 500 ms of silence. `FakeStt` returns scripted partial texts.

**Acceptance criteria.**

- AC-6.6-1: When `SherpaStt` gets `speech("turn on the kitchen lights")` in 20 ms chunks, the normalized final partial is "turn on the kitchen lights".
- AC-6.6-2: When `SherpaStt` gets the audio in 20 ms chunks, it gives a non-empty partial before the last chunk.
- AC-6.6-3: When `reset` runs, `partial` returns an empty text and no voice end.
- AC-6.6-4 [GPU host]: With `device = cpu`, the real-time factor of the 20M zipformer is less than 0.3.
- AC-6.6-5 [GPU host]: With the Nemotron model, the normalized final partial of `speech("turn on the kitchen lights")` is "turn on the kitchen lights".
- AC-6.6-6 [GPU host]: With the Nemotron model, the last word is in the partial less than 200 ms after the last voiced sample.
- AC-6.6-7 [GPU host]: With the Nemotron model, the real-time factor is less than 0.1.
- AC-6.6-8: When `SherpaStt` gets 500 ms of silence and then `speech("lights on")`, `voice_end_ms` is `None` during the silence and within 100 ms of the last voiced sample at the end.
- AC-6.6-9 [GPU host]: With `device = cuda`, the first pass runs on the CUDA execution provider.
- AC-6.6-10: When `SherpaStt` gets `speech("turn on the kitchen lights")`, `vad_ms - voice_end_ms` stays less than `early_commit_ms` until the last voiced sample.

**Proofs.**

- P-6.6-3. Argument: `reset` creates a new recognizer stream, which has no decoded tokens.
- P-6.6-1, P-6.6-2, P-6.6-4 to P-6.6-10. Evidence: test.

---

### 6.7 Second-Pass STT

**Goal:** Add `transcribe`, the offline second pass with Parakeet Unified. At commit, Jarvis can decode the whole utterance again for a more accurate transcript.

- **Implements:** G2, G18. **Depends on:** 6.6.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `SpeechToText::transcribe(samples) -> Result<Option<String>>`. `SherpaStt` holds an optional `OfflineRecognizer` from `[asr].second_pass_dir`. Without it, `transcribe` returns `Ok(None)`. `FakeStt` returns a scripted second-pass text.

**Acceptance criteria.**

- AC-6.7-1: When no second pass is configured, `transcribe` returns `None`.
- AC-6.7-2 [GPU host]: `transcribe` of `speech("add dentist to my calendar for monday")` gives the normalized text "add dentist to my calendar for monday".
- AC-6.7-3 [GPU host]: Over 50 different `speech` phrases, the second-pass word error rate is equal to or less than the first-pass word error rate.
- AC-6.7-4 [GPU host]: `transcribe` of a 3 s utterance has a p95 of less than 40 ms.
- AC-6.7-5 [GPU host]: With `device = cuda`, the second pass runs on the CUDA execution provider.

**Proofs.**

- P-6.7-1. Argument: `transcribe` matches on the optional recognizer and returns `None` for `None`.
- P-6.7-2 to P-6.7-5. Evidence: test.

---

### 6.8 Listen Use Case

**Goal:** Add `listen`, which feeds audio to the first pass, predicts on each changed partial, commits by the policy and runs the second pass. Jarvis now produces a final transcript before the user's audio stream ends.

- **Implements:** G1, G2. **Depends on:** 6.7.
- **Files:** modify `src/app/speech.rs`, `src/ports.rs`, `src/lib.rs`.

**Model.**

- `Assistant::listen() -> Listening`. `Listening::push(chunk) -> Option<Transcript>` and `finish() -> Transcript`.
- `push`: accept the chunk in `spawn_blocking` (decision 29). If the partial text changed and is not empty, predict `normalize(partial)` with `context_text(None, [])` and keep `confident`. `silence_ms = vad_ms - voice_end_ms`. If `should_commit`, return the transcript.
- The transcript is `transcribe(utterance audio)` when it returns `Some(text)`, else the first-pass partial. A `transcribe` error is logged, and the first-pass text is used.

**Acceptance criteria.**

- AC-6.8-1: When `FakeStt` gives the same partial two times, `FakeClassifier` records one `predict` call.
- AC-6.8-2: When `FakeClassifier` gives `lights_on` 0.95, `push` returns the transcript `early_commit_ms` after the last voiced sample.
- AC-6.8-3: When a second pass is configured and `FakeStt` gives different first-pass and second-pass texts, `push` returns the second-pass text.
- AC-6.8-4: When no second pass is configured, `push` returns the first-pass text.
- AC-6.8-5: When `transcribe` returns an error, `push` returns the first-pass text.
- AC-6.8-6: When `finish` runs before a commit, it returns the transcript at once.
- AC-6.8-7: When `FakeStt` reports no voice for 1 s of audio, `push` returns none.
- AC-6.8-8: When a confident partial has 150 ms of silence and then voice again, `push` does not commit. It commits `early_commit_ms` after the new last voiced sample.
- AC-6.8-9: When the partial changes during the silence to a text that is not confident, `push` commits only at `endpoint_ms` of silence.
- AC-6.8-10: When `FakeStt` blocks inside `accept` on one session, `handle` on another connection returns before the block ends.
- AC-6.8-11: When the partial is "Turn on the", `FakeClassifier` records the text "turn on the".

**Proofs.**

- P-6.8-1 (AC-6.8-1). Argument: `predict` is called only inside `if partial != last_partial`, and `last_partial` is updated after the call.
- P-6.8-10. Argument: `accept` runs in `spawn_blocking`, so it does not hold the async worker that runs `handle`.
- P-6.8-11. Argument: `push` passes `normalize(partial)` to `predict` (P-3.1-1).
- P-6.8-3 to P-6.8-5. Argument: the final text is `match transcribe(audio) { Ok(Some(t)) => t, _ => first_pass }`.
- P-6.8-2. Requires: `should_commit` (P-6.5-1). Evidence: test for the timing.
- P-6.8-6. Argument: `finish` builds the transcript without checking `should_commit`.
- P-6.8-7 to P-6.8-9. Requires: P-6.5-1, P-6.5-2, P-6.5-5. Evidence: test.

---

### 6.9 Wyoming STT Events

**Goal:** Connect the Wyoming `transcribe` and audio events to `listen`. Jarvis now serves HA's STT step and sends the transcript before HA stops the audio.

- **Implements:** G1, G2. **Depends on:** 6.8.
- **Files:** modify `src/adapters/wyoming.rs`, `src/ports.rs`.

**Model.** `transcribe` starts a `Listening`. Each `transcribe` event on the same connection starts a new `Listening`. `audio-start` with a rate other than 16000 Hz, a width other than 2 bytes or more than one channel gives an `error` event and ends that session. Each `audio-chunk` goes to `push`. When `push` returns a transcript, the adapter sends `transcript` at once and ignores the rest of the audio. `audio-stop` before a commit calls `finish` and sends `transcript`. `info` lists the `asr` program.

**Acceptance criteria.**

- AC-6.9-1: When `push` returns a transcript, the adapter sends `transcript` before the client sends `audio-stop`.
- AC-6.9-2: When the client sends `audio-stop` first, the adapter calls `finish` and sends `transcript`.
- AC-6.9-3: When the transcript was sent, later `audio-chunk` events cause no second `transcript`.
- AC-6.9-4: When the client sends `describe`, `info` lists the `asr` and `handle` programs.
- AC-6.9-5: When `audio-start` has the rate 44100 Hz, the adapter sends `error` and calls no `push`.
- AC-6.9-6: When one connection sends two `transcribe` sessions one after the other, the adapter sends one `transcript` for each.

**Proofs.**

- P-6.9-3 (AC-6.9-3). Argument: the connection keeps `sent: bool`. Sending sets it, and both send paths check it first.
- P-6.9-1, P-6.9-2, P-6.9-4 to P-6.9-6. Evidence: test.

---

### 6.10 Wyoming TTS Events

**Goal:** Add `speak` to the `Assistant` port and connect the Wyoming `synthesize` event to it. Jarvis now serves all three Wyoming services with cached audio.

- **Implements:** G1. **Depends on:** 6.9.
- **Files:** modify `src/adapters/wyoming.rs`, `src/ports.rs`, `src/app/speech.rs`, `src/lib.rs`.

**Model.**

- `synthesize` calls `speak(text)`. The adapter sends `audio-start`, one `audio-chunk` for each chunk of 1024 samples, and `audio-stop`. `info` lists `asr`, `handle` and `tts`.
- HA caches TTS audio by text, so HA does not send `synthesize` for each filler. Thus the announce queue waits for the satellite state, not for `synthesize` (sub-phase 4.9b).

**Acceptance criteria.**

- AC-6.10-1: When the client sends `synthesize`, the adapter sends `audio-start`, one or more `audio-chunk` and `audio-stop`, in that order.
- AC-6.10-2: When the client sends `describe`, `info` lists the `asr`, `handle` and `tts` programs.
- AC-6.10-3 [GPU host]: For a cached text, `synthesize` to the first `audio-chunk` has a p95 of less than 10 ms.

**Proofs.**

- P-6.10-1. Argument: the handler writes `audio-start`, then iterates `chunks` (P-6.3-4), then writes `audio-stop`, in one sequential function.
- P-6.10-2, P-6.10-3. Evidence: test.
