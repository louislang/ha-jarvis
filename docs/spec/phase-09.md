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

**Goal:** Add the end-to-end latency tests for commands, fetches and System Two fillers. The `gpu-test` job runs them on the GPU host with `device = cuda`, the Nemotron first pass and the Parakeet second pass, against the 300 ms goal and the targets of AC-9.2-3, AC-9.2-4 and AC-9.2-5. Set `vad_threshold = 0.65`.

- **Implements:** G1, G2, G6. **Depends on:** 9.1b.
- **Files:** modify `src/testing/e2e.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** The tests run the harness of sub-phase 9.1 on the GPU host with `device = cuda`, the Nemotron first pass and the Parakeet second pass, at real-time pace. Each phrase runs once as a warm-up, then 20 times. The response latency of a run is the sum of three segments: from the last voiced sample to `transcript`, from `transcript` to `handled`, and from `synthesize` to the first audio chunk. The harness sends each next event at once, so the sum equals the time from the last voiced sample to the first audio chunk. The p95 of 20 runs is the 19th smallest value. The latency to a filler uses the same segments with the filler text.

- The last voiced sample is the last sample of the last 20 ms frame of `speech(text)` with an RMS above -40 dBFS. The TTS audio has a long, quiet tail, and a person does not hear it as speech. This replaces the last sample that is not zero of sub-phase 9.1.
- `start_app` gives the handle of the startup pre-render task, so the harness waits for the pre-render before the warm-up. Thus no measured run misses the audio cache or shares the TTS with the pre-render.
- The reference house has no `weather.forecast_home` entity, so the harness adds a `weather.forecast_home` state to its copy of the house. "What is the weather" then gives the weather fetch and its filler.
- Each test prints the p95 of each phrase, so a run that passes also gives the numbers.
- `jarvis.toml` sets `vad_threshold = 0.65`. With 0.5, the commands had a p95 of 273 ms to 435 ms. With 0.65, they have 213 ms to 378 ms, and each transcript keeps its words. 0.7 cut "on" from "office lights on". The tests that read `jarvis.toml` change with it.
- AC-9.2-3, AC-9.2-4 and AC-9.2-5 have the measured targets of `01-architecture.md` section 6. AC-9.3c-1 of sub-phase 9.3c replaces AC-9.2-4 with a target of 400 ms.

**Acceptance criteria.**

Each measured command and fetch phrase has a top intent at or above `model.commit_threshold` with the fixture model, and a response in the audio cache.

- AC-9.2-1 [GPU host]: For "turn on the office fan" and "start the office fan", the response latency has a p95 of 300 ms or less over 20 runs.
- AC-9.2-2 [GPU host]: For "what is the weather", the latency to the filler audio has a p95 of 300 ms or less.
- AC-9.2-3 [GPU host]: For an `unknown` phrase, with `FakeReasoner` delayed by 2 s, the latency to the filler audio has a p95 of `endpoint_ms` + 150 ms or less. An `unknown` prediction is not confident, so the commit waits `endpoint_ms`.
- AC-9.2-4 [GPU host]: For a command whose response is not in the audio cache, the response latency has a p95 of 700 ms or less.
- AC-9.2-5 [GPU host]: For "turn on the office lights", "office lights on" and "turn the office lights on", the response latency has a p95 of 400 ms or less over 20 runs.

**Proofs.**

- P-9.2-1 to P-9.2-5. Evidence: test.

---

### 9.3 Supertonic TTS Adapter

**Goal:** Add `SupertonicTts`, a `TextToSpeech` adapter that runs the four Supertonic-3 graphs in ONNX Runtime through `ort`, with the fp32 vector estimator and vocoder. Jarvis can then control the input shapes of each graph, which sub-phase 9.3b fixes.

- **Implements:** G1, G18. **Depends on:** 9.2.
- **Files:** create `src/adapters/supertonic.rs`. Modify `src/lib.rs` (the module declaration only), `Cargo.toml`, `justfile`, `xtask/src/gates.rs`.

**Model.**

- Reason: ONNX Runtime keeps the cuDNN state of a `Conv` node only for the last input shape of that node (`onnxruntime/core/providers/cuda/nn/conv.cc`, `Conv::UpdateState`). A new input shape builds new cuDNN plans, and no cache keeps the plans of earlier shapes. Supertonic-3 has 192 `Conv` nodes: the text length sets the shapes of the duration predictor and the text encoder, and the latent length sets the shapes of the vector estimator and the vocoder. Thus each new reply length adds about 230 ms on the GPU host. `cudnn_conv_algo_search` does not change this. sherpa-onnx does not let Jarvis set the input shapes, so Jarvis runs the graphs itself.
- `SupertonicTts::try_new(config, device)` loads `[tts].model_dir`: `duration_predictor.int8.onnx`, `text_encoder.int8.onnx`, `vector_estimator.onnx`, `vocoder.onnx`, `tts.json`, `unicode_indexer.bin` and `voice.bin`. The two `int8` names are the files of the sherpa-onnx folder. They are not quantized: their bytes equal the fp32 files of Hugging Face `Supertone/supertonic-3`. Only the vector estimator of the sherpa-onnx folder is quantized.
- The fp32 vector estimator and vocoder come from Hugging Face `Supertone/supertonic-3`, folder `onnx/`, at the revision `3cadd1ee6394adea1bd021217a0e650ede09a323` (license OpenRAIL-M). `just models` downloads them into the folder of the sherpa-onnx Supertonic-3 files. It checks the SHA-256 of each file and downloads only a missing or wrong file. The SHA-256 values are `883ac868ea0275ef0e991524dc64f16b3c0376efd7c320af6b53f5b780d7c61c` (`vector_estimator.onnx`) and `085de76dd8e8d5836d6ca66826601f615939218f90e519f70ee8a36ed2a4c4ba` (`vocoder.onnx`).
- Measurements on the GPU host give the reason for fp32. ONNX Runtime has no CUDA kernels for the int8 operators `MatMulInteger` and `DynamicQuantizeLinear`, so they run on the CPU. With fixed input lengths and 4 steps, int8 has a p95 of 76.4 ms and fp32 has 53.0 ms. The int8 audio also changes with the input length (sub-phase 9.3b).
- Each graph is one `ort` session. The session builder is the builder of `OnnxClassifier`: `device` selects the CUDA or the CPU execution provider with its default options, and a provider that does not register gives an error. The libonnxruntime is the one of decision 27.
- `tts.json` gives the sample rate (44.1 kHz), `base_chunk_size`, `chunk_compress_factor` and `latent_dim`. One latent frame holds `base_chunk_size` × `chunk_compress_factor` = 3072 samples and has `latent_dim` × `chunk_compress_factor` = 144 channels.
- `voice.bin` has six `i64` dimensions, then the `style_ttl` values, then the `style_dp` values, all little-endian. The first dimension of each style is the number of voices. `[tts].voice` selects one voice. A voice outside the file gives an error.
- `unicode_indexer.bin` holds one little-endian `i32` for each Unicode code point.
- The text ids keep the text rules of sherpa-onnx 2.0.1 (`SupertonicUnicodeProcessor`), so a text gives the same ids as before. In this order:
  1. Replace "–", "‑" and "—" with "-". Replace "“" and "”" with `"`. Replace "‘", "’", "´" and the backtick with "'". Replace "_", "[", "]", "|", "/", "#", "→" and "←" with a space. Remove "♥", "☆", "♡", "©" and "\". Replace "@" with " at ", "e.g.," with "for example, " and "i.e.," with "that is, ".
  2. Remove each character from U+1F000 to U+1FFFF.
  3. Remove a space before ",", ".", "!", "?", ";", ":" and "'".
  4. Replace `""` with `"` and `''` with `'`.
  5. Replace each run of whitespace with one space, and remove the spaces at the start and the end.
  6. Add "." when the text is not empty and does not end with one of `. ! ? ; : , ' " ) ] } > … 。 」 』 】 〉 》 › » “ ” ‘ ’`.
  7. Put the text between `<en>` and `</en>`.
  8. Decompose the text with NFKD (`unicode-normalization`). Thus "…" becomes "...", and "é" becomes "e" and a combining accent. Remove each character above U+FFFF.
  9. The id of each character is the indexer value at its code point, also when that value is -1. A code point after the end of the indexer gives 0.
- `synth(text)` runs on a blocking thread:
  1. Empty text gives zero samples, before any graph runs.
  2. The duration predictor gets `text_ids`, `style_dp` and `text_mask` (all ones). It gives the duration in seconds. The sample count is the duration × the sample rate, rounded down, and 1 or more. The frame count is the sample count / 3072, rounded up.
  3. The text encoder gets `text_ids`, `style_ttl` and `text_mask`. It gives `text_emb`.
  4. The noise is 144 × frames values from `rand_distr::StandardNormal` with a `StdRng` from the seed 1, in channel order. Thus the same text gives bit-identical samples on every run. The samples are not those of sherpa-onnx, which has a different noise generator.
  5. The vector estimator runs `[tts].num_steps` times with `noisy_latent`, `text_emb`, `style_ttl`, `latent_mask` (all ones), `text_mask`, `current_step` (0, 1, ...) and `total_step`. Each output is the next `noisy_latent`.
  6. The vocoder gets the last latent. The audio is its first samples, up to the sample count, as 16-bit samples.
- The adapter does not split a text into parts. sherpa-onnx split a text of more than 300 characters, and Jarvis speaks the replies of System Two one sentence at a time (sub-phase 7.3).
- This sub-phase adds the crates `rand_distr` and `unicode-normalization` to `Cargo.toml`. `SherpaTts` stays the production adapter of `TextToSpeech` until sub-phase 9.3c: `run` and `speech(text)` keep it, and only the tests of this sub-phase use `SupertonicTts`.

**Acceptance criteria.**

- AC-9.3-1: When `just models` has run, `vector_estimator.onnx` and `vocoder.onnx` are in the Supertonic-3 folder of `models/` with their SHA-256.
- AC-9.3-2: When `SupertonicTts::synth("Hello")` runs, it returns 16-bit mono audio with more than 0 samples at 44100 Hz.
- AC-9.3-3: When `synth("")` runs, it returns zero samples and no error.
- AC-9.3-4: When `synth` runs two times with "I am turning the lights on.", the two audios have bit-identical samples.
- AC-9.3-5: The text ids of `It’s  “on” — now` equal the indexer values of `<en>It's "on" - now.</en>`, and the text ids of `café…` equal the indexer values of `<en>cafe`, U+0301 and `...</en>`.
- AC-9.3-6 [GPU host]: With `device = cuda`, `SupertonicTts::try_new` gives four sessions with the CUDA execution provider, and `synth("Hello")` returns audio.

**Proofs.**

- P-9.3-3. Argument: `synth` returns empty audio for empty text before it runs a graph (step 1).
- P-9.3-1, P-9.3-2, P-9.3-4 to P-9.3-6. Evidence: test.

---

### 9.3b Fixed TTS Input Lengths

**Goal:** Pad the TTS inputs to one of two fixed lengths, with one set of sessions for each length. ONNX Runtime then builds the cuDNN plans of each graph only once, so a reply of a new length does not add about 230 ms.

- **Implements:** G1. **Depends on:** 9.3.
- **Files:** modify `src/adapters/supertonic.rs`.

**Model.**

- Two input lengths: the short lengths are 64 text ids and 64 latent frames (`SHORT_TEXT`, `SHORT_FRAMES`). The long lengths are 160 text ids and 192 latent frames (`LONG_TEXT`, `LONG_FRAMES`). Every reply of `responses.toml` and every filler has 57 text ids or fewer and 55 frames or fewer. A reply of `backfill.max_chars` (80) characters has about 91 text ids and 80 frames. A sentence of System Two with 140 characters has about 150 text ids and 135 frames. Numbers make the speech longer: a sentence of 114 characters with times has 163 frames.
- The adapter holds one set of the four sessions for each length. ONNX Runtime keeps the cuDNN state of a node only for the last shape (sub-phase 9.3), so one set for both lengths builds new plans at each change of length. On the GPU host, one set gives a p95 of 237.8 ms for a short reply after a long reply. Two sets give 46.5 ms.
- A text with `n` ids and a speech with `f` frames use these input lengths:
  - The duration predictor and the text encoder: 64 text ids in the short set when `n` ≤ 64. Else the larger of `n` and 160 in the long set.
  - The vector estimator and the vocoder: 64 text ids and 64 frames in the short set when `n` ≤ 64 and `f` ≤ 64. Else the larger of `n` and 160 text ids, and the larger of `f` and 192 frames, in the long set.
- The added text ids are zeros, and the text mask is 0 for each of them. When the text encoder ran in the short set and the vector estimator runs in the long set, `text_emb` and the text mask get zeros up to the long length. The text encoder gives zeros for the masked positions, so the result is the same. A text or a speech longer than the long lengths keeps its real length, so it builds new plans.
- The noise has the values of sub-phase 9.3 for the real frames, and zeros for the added frames. The latent mask is 1 for each real frame and 0 for each added frame. The audio is the first samples of the vocoder output, up to the sample count of the duration.
- The audio of a padded input is not the audio of the same input with no zeros. Each `Pad` node of the four graphs has `mode=edge`. With no zeros, the edge repeats the last real frame. With zeros, it repeats a masked zero. The padded duration is 0.3% to 2% shorter. Two padded inputs give the same audio: on the CPU, the short and the long lengths give a maximum difference of 29 between the 16-bit samples, and 34 on the GPU host. With the int8 vector estimator, the difference is 14301, so the int8 audio changes with the length.
- Measurements on the GPU host, fp32, p95 over 20 runs: a short reply with no zeros takes 263.7 ms with 4 steps (each reply has a new length), 53.0 ms with the short lengths, and 98.0 ms with the long lengths. With 3 steps, 250.1 ms, 36.9 ms and 78.2 ms. With 2 steps, 233.3 ms, 30.8 ms and 58.1 ms. `jarvis.toml` keeps `num_steps = 4`.

**Acceptance criteria.**

- AC-9.3b-1: For a text with `n` ids and a speech with `f` frames, the input lengths are: 64 text ids and 64 frames for (`n`, `f`) = (20, 20) and (64, 64); 64 text ids for the duration predictor and the text encoder, and 160 text ids and 192 frames for the vector estimator and the vocoder, for (64, 65); 160 text ids and 192 frames for (65, 64) and (160, 192); 200 text ids and 192 frames for (200, 150); 160 text ids and 250 frames for (100, 250).
- AC-9.3b-2: When "I am turning the lights on." is synthesized at the short lengths and at the long lengths, the two audios have the same number of samples, and each pair of samples differs by 64 or less.
- AC-9.3b-3 [GPU host]: With `device = cuda`, over 40 runs of `synth` after a warm-up, the runs alternate the short replies "Playing jeopardy.", "Putting jeopardy on the TV.", "One moment." and "I am turning the lights on." with the 142-character sentence "Tomorrow will be mostly sunny with a light breeze from the west, and the evening should stay clear and mild, so it is a good night for a walk." The short replies have a p95 of 80 ms or less, and the sentence has a p95 of 150 ms or less.

**Proofs.**

- P-9.3b-1. Ensures: the lengths of the rule in the model. Argument: the length function compares `n` and `f` with the short lengths, and else takes the maximum with the long lengths. Each case of the criterion is one branch.
- P-9.3b-2, P-9.3b-3. Evidence: test.

---

### 9.3c TTS Adapter Switch

**Goal:** Use `SupertonicTts` in `run` and in `speech(text)`, and delete `SherpaTts`. A response that is not in the audio cache then has a p95 of 400 ms or less.

- **Implements:** G1. **Depends on:** 9.3b.
- **Files:** modify `src/lib.rs`, `src/testing/fakes.rs`, `src/adapters/sherpa.rs`, `src/testing/e2e.rs`.

**Model.**

- `run` builds `SupertonicTts`. `speech(text)` synthesizes with `SupertonicTts`. `sherpa.rs` no longer has `SherpaTts`, its engine configuration or its tests. Thus `TextToSpeech` has one production adapter.
- AC-9.3-2 and AC-9.3-3 replace AC-6.2-2 and AC-6.2-3, so this sub-phase deletes the tests of AC-6.2-2 and AC-6.2-3. The tests of AC-6.2-4 and AC-6.2-5 load `SupertonicTts`.
- `speech(text)` gives new audio, so the STT tests get new input. Their criteria do not change.
- AC-9.3c-1 replaces AC-9.2-4, so this sub-phase deletes the test of AC-9.2-4. The command is "turn the tv to jeopardy", whose `tv_show` replies are not in the audio cache.

**Acceptance criteria.**

- AC-9.3c-1 [GPU host]: For a command whose response is not in the audio cache, the response latency has a p95 of 400 ms or less over 20 runs. This replaces AC-9.2-4.
- AC-9.3c-2 [GPU host]: With `device = cuda`, when one process loads both STT passes, the fixture classifier and `SupertonicTts`, and synthesizes "One moment." and the 142-character sentence of AC-9.3b-3, the peak GPU memory of the process is 5.5 GB or less.

**Proofs.**

- P-9.3c-1, P-9.3c-2. Evidence: test.

## Field Log

These steps are not tests and gate nothing. Record the results in the field log of the PR that closes the phase stack.

1. In HA, add the Wyoming integration with the Jarvis host and port.
2. Make an Assist pipeline with Jarvis for STT, conversation agent and TTS. Set "Prefer handling commands locally" to off. Assign it to each satellite.
3. Say each phrase in `00-overview.md` section 1.1 ten times. Record the time from the end of speech to the first sound from the satellite.
4. Ask a question that gets a filler. Make sure the satellite entity state becomes `idle` only after the satellite has played the filler (`01-architecture.md` decision 5).

The fixture model knows only the seed examples. The Phase 13 field log repeats this with a trained model.
