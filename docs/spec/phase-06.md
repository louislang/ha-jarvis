# Phase 6: Speech

This phase adds the shared ONNX Runtime, TTS, the audio cache, the commit policy with its Quint model, two-pass STT and the Wyoming speech events. Jarvis then serves all three Wyoming services.

---

### 6.1 Shared ONNX Runtime and GPU Recipes

**Goal:** Load the ONNX Runtime 1.28.2 shared library of sherpa-onnx 2.0.1 in `ort`, and add the library, GPU model and GPU test recipes. The classifier now uses the runtime that sherpa-onnx uses, and the GPU host runs it on CUDA.

- **Implements:** G18. **Depends on:** 5.4c.
- **Files:** modify `Cargo.toml`, `src/adapters/onnx.rs`, `justfile`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `xtask/src/gates.rs`.

**Model.**

- `ort` uses the features `load-dynamic` and `cuda`. It loads the `libonnxruntime` that `ORT_DYLIB_PATH` names (decision 27).
- `just models` downloads the sherpa-onnx 2.0.1 CPU shared libraries for the host system into one library folder in `models/`. The recipe lists one SHA-256 for each archive that it downloads, and one for each `.onnx` file and each `tokens.txt` file in it. It checks each listed file and downloads only a missing or wrong file. `models` is a setup recipe: `check` and the `rust-test` job run it before the tests, and tests do not download. CI caches `models/`.
- `gpu-models` downloads the Parakeet Unified offline model and the sherpa-onnx 2.0.1 `cuda-12.x-cudnn-9.x-onnxruntime1.28.2` `linux-x64-gpu` libraries.
- `test` and `cov` set `ORT_DYLIB_PATH` to the `libonnxruntime` in the CPU library folder, and `LD_LIBRARY_PATH` (`DYLD_LIBRARY_PATH` on macOS) to the folder. CI sets them in the same way.
- `gpu-test` (sub-phase 0.13) now runs `gpu-models` first and sets `ORT_DYLIB_PATH` and `LD_LIBRARY_PATH` to the CUDA library folder.
- The Linux release archive has the CUDA libraries. The macOS archive has the CPU libraries. The verification step sets `ORT_DYLIB_PATH` and `LD_LIBRARY_PATH` (`DYLD_LIBRARY_PATH` on macOS) to the extracted folder. The `jarvis` unit sets them in its environment file (`01-architecture.md` section 10).

**Acceptance criteria.**

- AC-6.1-1: When `just models` has run, the library folder exists in `models/`, and each file that the recipe lists has its SHA-256. When `ci.yml` is parsed, `rust-test` runs `just models` before `just cov`.
- AC-6.1-2: When the tests run, `ORT_DYLIB_PATH` names the `libonnxruntime` in the library folder, and the fixture classifier predicts without error.
- AC-6.1-3 [GPU host]: With `device = cuda`, `OnnxClassifier` runs on the CUDA execution provider of the shared library.
- AC-6.1-4: When `release.yml` is parsed, the Linux build job has a verification step that fails unless the archive contains `libonnxruntime` and the CUDA provider library, and `ldd jarvis` in the extracted archive, with `LD_LIBRARY_PATH` set to the extracted folder, reports no missing library.

**Proofs.**

- P-6.1-1, P-6.1-2, P-6.1-3. Evidence: test.
- P-6.1-4. Evidence: test (a structure test of `release.yml`).

**Field log.** Push a tag. Record that the Linux archive passes its verification step. On the Intel MacBook, run `just models` and `just test`. Record that the sherpa-onnx macOS x86_64 libraries load ONNX Runtime 1.28.2.

---

### 6.2 TTS Adapter

**Goal:** Add the `TextToSpeech` port and `SherpaTts` with the Supertonic-3 voice, and the STT and TTS models in the `models` recipe. Jarvis can now turn text into audio, and `sherpa-onnx` and `ort` work in one process.

- **Implements:** G1, G18. **Depends on:** 6.1.
- **Files:** create `src/adapters/sherpa.rs`, `src/domain/speech.rs`. Modify `src/ports.rs`, `src/lib.rs`, `justfile`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `xtask/src/gates.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `TextToSpeech::synth(text) -> Result<Audio>`. `Audio { samples: Vec<i16>, rate }` is in `src/domain/speech.rs`.
- `SherpaTts` wraps `OfflineTts` with the Supertonic-3 model in `[tts].model_dir`, the speaker ID `[tts].voice` and `[tts].num_steps`. The adapter fixes `lang = "en"` and `seed = 1`. Thus the same text gives bit-identical samples on every run. The model rate is 44.1 kHz.
- `sherpa-onnx` is `=2.0.1` with feature `shared`. Each recipe and CI job that builds `jarvis` sets `SHERPA_ONNX_LIB_DIR` to the library folder of sub-phase 6.1, so no build downloads. `gpu-test` and the Linux release job set it to the CUDA library folder.
- `just models` also downloads the Nemotron 160 ms model (sub-phase 6.6) and the Supertonic-3 model `sherpa-onnx-supertonic-3-tts-int8-2026-05-11` into `models/`. `gpu-test` runs `models` and `gpu-models`, so the GPU host has the Supertonic-3 model too.
- `device` selects the provider for `SherpaTts`.
- sherpa-onnx falls back to the CPU with only a log line when the CUDA provider does not load. Thus a test checks the CUDA provider of a sherpa-onnx model by the GPU memory of the test process (`nvidia-smi`): the memory grows when the model loads.

**Acceptance criteria.**

- AC-6.2-1: When `just models` has run, the STT and TTS model folders exist in `models/`, and each file that the recipe lists has its SHA-256.
- AC-6.2-2: When `SherpaTts::synth("Hello")` runs, it returns 16-bit mono audio with more than 0 samples at the model rate.
- AC-6.2-3: When `synth("")` runs, it returns zero samples and no error.
- AC-6.2-4: When one test process loads the fixture classifier and `SherpaTts`, both predict or synthesize without error.
- AC-6.2-5 [GPU host]: With `device = cuda`, `SherpaTts` and `OnnxClassifier` run in one process on the CUDA execution provider.

**Proofs.**

- P-6.2-1, P-6.2-2, P-6.2-4, P-6.2-5. Evidence: test.
- P-6.2-3. Argument: `synth` returns empty audio for empty text before it calls the engine.

---

### 6.3 Audio Chunks and Duration

**Goal:** Add `chunks` and `duration` to the speech domain. The Wyoming adapter can later send audio in chunks of a fixed size.

- **Implements:** G1. **Depends on:** 6.2.
- **Files:** modify `src/domain/speech.rs`, `src/domain/contracts.rs`.

**Model.**

- `chunks(audio, size) -> Vec<&[i16]>` and `duration(audio) -> Duration` are in `domain/speech.rs`.

**Acceptance criteria.**

- AC-6.3-4: When `chunks(audio, 1024)` runs, the joined chunks equal the original audio, and every chunk except the last has 1024 samples.
- AC-6.3-5: When `duration` gets 22050 samples at 22050 Hz, it returns 1 s.

**Proofs.**

- P-6.3-4 (AC-6.3-4). Requires: `size > 0`, because `slice::chunks` panics at 0. Ensures: the concatenation of the chunks is the audio, and each chunk except the last has `size` samples. Argument: `chunks` is `samples.chunks(size)`, which partitions the slice in order into pieces of `size` with a shorter last piece. Concatenation of a partition gives the slice.
- P-6.3-5. Requires: `rate > 0`. Ensures: `duration(audio) = len(samples) / rate`. Argument: the function is this division. 22050 samples at 22050 Hz give 1 s.

---

### 6.3b Audio Cache and Speak

**Goal:** Add the speech use case with `speak`, the audio cache and `prerender`. A TTS request for a known response is now a memory read.

- **Implements:** G1, G4. **Depends on:** 6.3.
- **Files:** create `src/app/speech.rs`. Modify `src/lib.rs` (the module declaration only), `src/testing/fakes.rs`, `benches/domain.rs`, `Cargo.toml`.

**Model.**

- `AudioCache: DashMap<text, Arc<Audio>>`. `speak(text)`: a hit returns the cached audio without a copy of the samples. A miss calls `synth` and returns the result without a store. Only `prerender` (and the prefetch of sub-phase 14.10, which replaces the old answer) adds audio, so the cache does not grow with dynamic texts.
- `prerender(pools, areas)`: for each template whose only variable is `area` (or no variable), render it for each area and synthesize it. The `area` variable is the area name in lowercase, as in the replies of `decide`, so the cache keys match the reply texts.
- This sub-phase adds the use case and its tests only. Sub-phase 6.10 connects it to `run`.
- This sub-phase adds the crate `dashmap` to `Cargo.toml`.
- `FakeTts` records each text.

**Acceptance criteria.**

- AC-6.3b-1: When the text is in the cache, `speak` returns the cached audio and `FakeTts` records no call.
- AC-6.3b-2: When `prerender` runs with the pools and the area names, the cache contains each such pool text for each area name.
- AC-6.3b-3: When a template uses a variable other than `area`, `prerender` does not synthesize it.
- AC-6.3b-4 [Bench]: A cache hit for a response of 2 s uses 100,000 instructions or fewer.
- AC-6.3b-5: When `speak` gets a text that is not in the cache, `FakeTts` records one call and the cache size does not change.

**Proofs.**

- P-6.3b-1. Argument: `speak` returns from the hit branch before the `synth` branch.
- P-6.3b-3. Argument: `prerender` filters templates by their variable set, which must be a subset of `{area}`.
- P-6.3b-2, P-6.3b-5. Evidence: test.
- P-6.3b-4. Evidence: benchmark.

---

### 6.4 Commit Quint Model

**Goal:** Add `quint/commit.qnt`, the model of the commit policy over time, with a VAD that decides each chunk late. It proves that Jarvis commits only when a rule allows it, never waits past that point, and never counts undecided speech as silence.

- **Implements:** G1, G2, G19. **Depends on:** 6.3b.
- **Files:** create `quint/commit.qnt`.
- **Quint:** `commit.qnt`.

**Model.**

- Constants (in 100 ms units): `EARLY = 2`, `ENDPOINT = 6`, `MAX = 8`, `LAG = 2` (the VAD decides a chunk at most `LAG` chunks late). `MAX` is small so that a simulation run of 30 steps reaches the `MAX` commit. The real limit is `max_utterance_ms` in the configuration (sub-phase 6.5).
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
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `justfile`, `jarvis.toml`, `Cargo.toml`, `xtask/src/gates.rs`.

**Model.**

- `SpeechToText::stream() -> Box<dyn SttStream>`. `SttStream`: `accept(samples)`, `partial() -> Partial { text }`, `reset()`.
- `SherpaStt` wraps `OnlineRecognizer` with the streaming transducer in `[asr].model_dir`. This sub-phase adds `asr.model_dir`.
- CPU machines and the GPU host use the same first-pass model: Nemotron 160 ms. CPU machines load its int8 files. The GPU host loads its fp32 files (sub-phase 6.6c). The 20M zipformer does not recognize the first word of `speech(text)`. `just models` downloads the int8 Nemotron model. `gpu-models` downloads the fp32 files of the same model. `just models` does not download the 20M zipformer. The structure test of AC-6.2-1 checks the Nemotron folder.
- `FakeStt` returns scripted partials.
- `speech(text)` in `fakes.rs` synthesizes text with `SherpaTts`, resamples it from 44.1 kHz to 16 kHz with `rubato`, and adds 500 ms of silence. The fixed TTS seed makes `speech(text)` bit-identical on every run.

**Acceptance criteria.**

- AC-6.6-1: When `SherpaStt` gets `speech("turn on the kitchen lights")` in 20 ms chunks, the normalized final partial is "turn on the kitchen lights".
- AC-6.6-2: When `SherpaStt` gets the audio in 20 ms chunks, it gives a non-empty partial before the last chunk.
- AC-6.6-3: When `reset` runs, `partial` returns an empty text.

**Proofs.**

- P-6.6-3. Argument: `reset` creates a new recognizer stream, which has no decoded tokens.
- P-6.6-1, P-6.6-2. Evidence: test.

---

### 6.6b First-Pass Voice Activity

**Goal:** Add the Silero VAD to the first pass, with the voice end and the decided audio in each partial. The commit policy can now measure silence.

- **Implements:** G2, G18. **Depends on:** 6.6.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `justfile`, `jarvis.toml`.

**Model.**

- `Partial { text, voice_end_ms: Option, vad_ms }`. `SherpaStt` runs the sherpa-onnx Silero VAD with `[asr].vad_model`, `vad_threshold` and `vad_min_silence_ms` on the same samples. Each stream has its own VAD, and the VAD can fail to load. Thus `stream()` returns `Result<Box<dyn SttStream>>`.
- `vad_ms` is the end of the audio that the VAD has decided: audio that the VAD still holds to decide the start or the end of a segment is not decided. `voice_end_ms` is the audio position of the end of the last voiced frame, and `None` before any speech. While the VAD is in a speech segment, `voice_end_ms = vad_ms`.
- This sub-phase adds `asr.vad_model`, `asr.vad_threshold` and `asr.vad_min_silence_ms`.
- `just models` also downloads the Silero VAD model `silero_vad.onnx`. It is one file, not an archive. Thus `fetch` extracts only `.tar.bz2` archives and checks a single file by its SHA-256.
- `FakeStt` returns scripted partials with voice end times.
- The first pass uses 4 threads.
- This sub-phase adds the `Config::validate` checks of sub-phase 2.2 for the new `asr` fields.

**Acceptance criteria.**

- AC-6.6b-1: When `SherpaStt` gets 500 ms of silence and then `speech("lights on")`, `voice_end_ms` is `None` during the silence and within 100 ms of the last voiced sample at the end.
- AC-6.6b-2: When `SherpaStt` gets `speech("turn on the kitchen lights")`, `vad_ms - voice_end_ms` stays less than `early_commit_ms` until the last voiced sample.
- AC-6.6b-3: When `reset` runs, `partial` returns no voice end.

**Proofs.**

- P-6.6b-3. Argument: `reset` resets the VAD and sets the voice end to `None`.
- P-6.6b-1, P-6.6b-2. Evidence: test.

---

### 6.6c First Pass on the GPU Host

**Goal:** Load the fp32 first pass on the GPU host, flush the last word at the speech end, and check the text, the latency and the speed on CUDA and on the CPU. The first pass is now fast enough for the early commit.

- **Implements:** G2, G18. **Depends on:** 6.6b.
- **Files:** modify `src/adapters/sherpa.rs`, `src/lib.rs`, `justfile`, `jarvis.toml`.

**Model.**

- Every device uses the same 160 ms Nemotron export and the same code. This sub-phase adds `asr.encoder`, `asr.decoder` and `asr.joiner`: the file names of the first-pass model in `model_dir`. `jarvis.toml` names the fp32 files (`encoder.onnx`, `decoder.onnx`, `joiner.onnx`). `jarvis.local.toml` and the tests on the CPU name the int8 files.
- Measurements on the GPU host (4 threads, 20 ms chunks, `speech("turn on the kitchen lights")`) give the reasons. ONNX Runtime runs the int8 operators `MatMulInteger`, `ConvInteger` and `DynamicQuantizeLinear` on the CPU, also with the CUDA execution provider. Thus int8 on CUDA has a real-time factor of 0.622, and int8 on the CPU has 0.558. fp32 on CUDA has 0.130 and uses about 3.37 GB more GPU memory. No fp16 export exists. On the CPU, 6 and 8 threads are slower (0.716 and 0.800), so the first pass keeps 4 threads.
- `gpu-models` downloads the fp32 files (about 2.5 GB) from the Hugging Face repository `csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-2026-04-25`, with one SHA-256 for each file. The encoder keeps its weights in `encoder.data`. The fp32 export has the same `tokens.txt`, so its files go in the folder of the int8 files.
- The model emits the last token about 410 ms of audio after the last voiced sample. Thus streaming alone gives the last word too late: 721 ms after the last voiced sample with fp32 on CUDA, 790 ms with int8 on the CPU. `input_finished()` does not flush the model.
- Flush: when the VAD ends a speech segment, the stream feeds 640 ms of zeros to the recognizer and decodes them. 480 ms of zeros is not enough. The zeros go only to the recognizer, not to the VAD or the audio positions. sherpa-onnx 2.0.1 cannot clone a stream. Thus the zeros go into the live stream, and the flush runs only at the end of a VAD speech segment, where a commit and `reset` can follow.
- The flush takes 88 ms with fp32 on CUDA and 376 ms with int8 on the CPU. Thus the last word comes about 220 ms after the last voiced sample on CUDA and about 510 ms on the CPU.
- The Silero VAD runs on the CPU on every device. Its real-time factor is 0.004 on the CPU and 0.017 on CUDA.
- On the CPU, a real-time factor of less than 0.6 lets each 160 ms chunk decode in less than 96 ms, so the decode keeps up with the audio.
- `gpu-test` runs its tests one at a time (`--test-threads=1`), because timing tests that run in parallel slow each other down.

**Acceptance criteria.**

- AC-6.6c-1 [GPU host]: With `device = cuda`, the normalized final partial of `speech("turn on the kitchen lights")` is "turn on the kitchen lights".
- AC-6.6c-2 [GPU host]: With `device = cuda`, the last word is in the partial less than 250 ms after the last voiced sample.
- AC-6.6c-3 [GPU host]: With `device = cuda`, the real-time factor of the first pass is less than 0.15.
- AC-6.6c-4 [GPU host]: With `device = cuda`, the first pass runs on the CUDA execution provider.
- AC-6.6c-5 [GPU host]: With `device = cpu` and 4 threads, the real-time factor of the first pass is less than 0.6.

**Proofs.**

- P-6.6c-1 to P-6.6c-5. Evidence: test.

---

### 6.7 Second-Pass STT

**Goal:** Add `transcribe`, the offline second pass with Parakeet Unified. At commit, Jarvis can decode the whole utterance again for a more accurate transcript.

- **Implements:** G2, G18. **Depends on:** 6.6b.
- **Files:** modify `src/ports.rs`, `src/adapters/sherpa.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `SpeechToText::transcribe(samples) -> Result<Option<String>>`. `SherpaStt` holds an optional `OfflineRecognizer` from `[asr].second_pass_dir`. Without it, `transcribe` returns `Ok(None)`. `FakeStt` returns a scripted second-pass text.
- Parakeet writes numbers as digits and symbols ("10 minutes", "30%", "7.30", "3 p.m."). Nemotron writes the same speech as words ("ten minutes", "thirty percent", "seven thirty", "three pm"). `normalize` keeps digits. Thus the two passes give different text for the same speech, and digits would reach System One.
- `transcribe` returns the second-pass text with its numbers as words, in three steps:
  1. A time with a dot becomes a time with a colon: "H.MM" becomes "H:MM", for an hour from 0 to 23 and two minute digits from 00 to 59.
  2. `text_processing_rs::tn_normalize_sentence` writes the numbers as words. It is the Rust port of the NVIDIA NeMo text normalization. Examples: "10" becomes "ten", "30%" becomes "thirty percent", "7:30" becomes "seven thirty", "7:05" becomes "seven oh five", "72.5" becomes "seventy two point five", "1st" becomes "first".
  3. "a.m." and "p.m." become "am" and "pm". The crate writes the period of a colon time as "a m" or "p m", so these become "am" and "pm" too.
- A time without a separator ("645") and a year ("2026" becomes "two thousand twenty six") stay different from the Nemotron text. A decimal with two digits after the dot ("2.50") reads as a time.
- `normalize` and its Verus clauses do not change.
- The word error rate is the word edit distance (`strsim::generic_levenshtein` on the words) between the normalized text and the normalized phrase, divided by the number of words in the phrase.

**Acceptance criteria.**

- AC-6.7-1: When no second pass is configured, `transcribe` returns `None`.
- AC-6.7-2 [GPU host]: `transcribe` of `speech("add dentist to my calendar for monday")` gives the normalized text "add dentist to my calendar for monday".
- AC-6.7-3 [GPU host]: Over 50 different `speech` phrases, 10 of them with numbers, percents or times, the word error rate of the `transcribe` text is equal to or less than the word error rate of the first-pass text.
- AC-6.7-4 [GPU host]: `transcribe` of a 3 s utterance has a p95 of less than 40 ms.
- AC-6.7-5 [GPU host]: With `device = cuda`, the second pass runs on the CUDA execution provider.
- AC-6.7-6: When the second-pass text is "Set the lights to 30% at 7.30 p.m. and the heat to 72.5 at 3 p.m.", the normalized converted text is "set the lights to thirty percent at seven thirty pm and the heat to seventy two point five at three pm".

**Proofs.**

- P-6.7-1. Argument: `transcribe` matches on the optional recognizer and returns `None` for `None`.
- P-6.7-2 to P-6.7-6. Evidence: test.

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

- `run` builds `SherpaTts` and the speech use case of sub-phase 6.3b. After the first snapshot, a background task runs `prerender` with the area names of the hot cache. `serve` does not wait for it.
- `synthesize` calls `speak(text)`. The adapter sends `audio-start`, one `audio-chunk` for each chunk of 1024 samples, and `audio-stop`. `info` lists `asr`, `handle` and `tts`.
- HA caches TTS audio by text, so HA does not send `synthesize` for each filler. Thus the announce queue waits for the satellite state, not for `synthesize` (sub-phase 4.9b).

**Acceptance criteria.**

- AC-6.10-1: When the client sends `synthesize`, the adapter sends `audio-start`, one or more `audio-chunk` and `audio-stop`, in that order.
- AC-6.10-2: When the client sends `describe`, `info` lists the `asr`, `handle` and `tts` programs.
- AC-6.10-3 [GPU host]: For a cached text, `synthesize` to the first `audio-chunk` has a p95 of less than 10 ms.

**Proofs.**

- P-6.10-1. Argument: the handler writes `audio-start`, then iterates `chunks` (P-6.3-4), then writes `audio-stop`, in one sequential function.
- P-6.10-2, P-6.10-3. Evidence: test.
