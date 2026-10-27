# Jarvis Specification: Architecture

This file uses the technical names in `00-overview.md` section 0.3.

## 1. Hexagonal Architecture

Jarvis uses hexagonal architecture (ports and adapters). There are four layers.

| Layer | Folder | Contents | Can depend on |
|---|---|---|---|
| Domain | `src/domain/` | Pure rules: catalog, model input and output, training set, decide, select, dialogs, habits, feedback, learned intents, speaker identification, speech policy. | Pure crates only (see section 7). |
| Ports | `src/ports.rs` | Traits for the driving ports and the driven ports. | Domain, and the crates of section 7 for ports. |
| Application | `src/app/` | Use cases. They connect the domain to the ports. | Domain, ports, and the crates of section 7 for the application. |
| Adapters | `src/adapters/` | Port implementations for one technology each. | Ports, domain, technology crates. |

A data type that the domain uses is in the domain, and the ports use it from there (for example `Snapshot`, `ServiceCall`, `Audio`, `Question`, `Interaction`). Thus the domain never needs `crate::ports`.

The composition root (`src/lib.rs`) is the only code that knows all layers. The binary `src/main.rs` only calls `jarvis::main`.

`xtask/` is a separate crate in the same Cargo workspace. It holds the developer tools: the complexity gate, the PR size gate, the module graph, the layer rules and the diagram. Jarvis does not depend on it.

The training script (`train/`) is outside the layers. It is the only Python code. It reads a training set file and writes ONNX model files. Rust code does not import it. The process adapter starts it as a process on the GPU host. The RunPod adapter starts it on RunPod. `just fixture-model` starts it on the CPU.

Dependency rules:

1. The domain does not use `crate::ports`, `crate::app`, `crate::adapters`, `tokio`, `std::fs`, `std::net`, `std::process`, `std::thread`, `std::env`, `std::time::SystemTime`, `std::time::Instant`, `rand::thread_rng` or `rand::rng`. It gets time, randomness and the environment as parameters.
2. The application does not use `crate::adapters`.
3. An adapter does not use `crate::app`.
4. Only `src/lib.rs` names concrete adapter types. Use cases and other adapters see only port traits.
5. `cargo run -p xtask -- rules` builds the module graph and makes sure that rules 1 to 4 are true (sub-phase 0.8).

```
                    DRIVING SIDE                                   DRIVEN SIDE
                                   ┌──────────────────────────────┐
 HA Assist ──Wyoming──▶ adapters/  │  Assistant port              │
 pipeline              wyoming.rs ─┼─▶ (listen, handle, speak)    │   Home port ──────▶ adapters/ha.rs ─────▶ HA API
                                   │        │                     │   SpeechToText ───▶ adapters/sherpa.rs
 Browser ──HTTP──────▶ adapters/   │  Review port                 │   TextToSpeech ───▶ adapters/sherpa.rs
                       web.rs ─────┼─▶ (list, mark, proposals)    │   VoiceEmbedder ──▶ adapters/sherpa.rs
                                   │        │                     │   VoiceProfiles ──▶ adapters/sherpa.rs
                                   │   ┌────▼──── app/ ─────────┐ │   Classifier ─────▶ adapters/onnx.rs
                                   │   │ conversation  speech   │─┼─▶ ModelLoader ────▶ adapters/onnx.rs
                                   │   │ background  training   │ │   Trainer ────────▶ adapters/process.rs, runpod.rs ─▶ RunPod
                                   │   │ review                 │ │   Reasoner ───────▶ adapters/openai.rs ─▶ OpenAI
                                   │   └────┬───────────────────┘ │   InteractionLog ─▶ adapters/sqlite.rs
                                   │   ┌────▼──── domain/ ──────┐ │   Store<T> ───────▶ adapters/files.rs
                                   │   │ intent model training  │ │   Clock ──────────▶ SystemClock (lib.rs)
                                   │   │ decide responses state │ │
                                   │   │ dialog habits feedback │ │
                                   │   │ learn speaker speech   │ │
                                   │   └────────────────────────┘ │
                                   └──────────────────────────────┘
```

## 2. Ports

All ports are in `src/ports.rs`. A port gets a method only in the phase that uses it.

| Port | Side | Methods (final form) | Adapter |
|---|---|---|---|
| `Assistant` | Driving | `info`, `listen` (gives a `Listening` session: `push`, `finish`), `handle`, `speak` | Implemented by `App` in `src/lib.rs`. Called by `adapters/wyoming.rs`. |
| `Review` | Driving | `list`, `audio`, `mark`, `intents`, `proposals`, `resolve` | Implemented by `app/review.rs`. Called by `adapters/web.rs`. |
| `Home` | Driven | `snapshot`, `events`, `call`, `announce`, `ask`, `history` | `adapters/ha.rs` |
| `SpeechToText` | Driven | `stream` (first pass: gives `accept`, `partial`, `reset`), `transcribe` (second pass) | `adapters/sherpa.rs` |
| `TextToSpeech` | Driven | `synth` | `adapters/sherpa.rs` |
| `VoiceEmbedder` | Driven | `embed` | `adapters/sherpa.rs` |
| `VoiceProfiles` | Driven | `enroll`, `similarities` | `adapters/sherpa.rs` |
| `Classifier` | Driven | `predict` | `adapters/onnx.rs` |
| `ModelLoader` | Driven | `load` (gives a `Classifier` for a model folder) | `adapters/onnx.rs` |
| `Trainer` | Driven | `train` (gives a model folder for a training set) | `adapters/process.rs` (local GPU) and `adapters/runpod.rs` (fallback) |
| `Reasoner` | Driven | `warm`, `answer` (gives steps: sentence, call, ask, done), `generate`, `paraphrase`, `propose` | `adapters/openai.rs` |
| `InteractionLog` | Driven | `append`, `update`, `list`, `audio`, `mark`, `marked`, `unknowns`, `prune` | `adapters/sqlite.rs` |
| `Store<T>` | Driven | `load`, `save` | `adapters/files.rs` (TOML or JSON from the file extension) |
| `Clock` | Driven | `now` | `SystemClock` in `src/lib.rs` |

Tests in the application layer use the fakes in `src/testing/fakes.rs`. Adapter tests use local servers (`wiremock`, a fake HA WebSocket server), in-memory object stores or real models.

## 3. System One Model

The System One model is one ONNX model with two heads: intent classification and slot tagging.

Input: two text segments.

- Segment A is the context. `context_text` makes it from the satellite area and the habit summary.
- Segment B is the transcript.

```
User: "Turn on the lights"
Context: "area: living room | habits: living room lamp on 18:00 weekdays 4.2; tv play bluey 20:00 weekdays 3.1"
```

Output:

```json
{
  "intents": { "lights_on": 0.89, "fan_on": 0.03, "close_garage": 0.01, "unknown": 0.02 },
  "slots": []
}
```

For "What is the weather like?" before Jarvis learns `what_is_weather`:

```json
{ "intents": { "lights_on": 0.01, "unknown": 0.76 }, "slots": [] }
```

Rules:

1. The ONNX outputs are `intent_logits` (one for each label) and `slot_logits` (one for each token and tag).
2. Slot tags use the BIO scheme (`O`, `B-area`, `I-area`, ...). Only segment B tokens get tags.
3. `labels.json` in the model folder lists the labels and the tags in output order.
4. When the top label is `unknown`, or the top intent probability is less than `model.threshold`, the request goes to System Two.
5. The base model is `train.base_model`. The default is `microsoft/MiniLM-L12-H384-uncased`. It predicts in less than 10 ms on the CPU and in less than 5 ms on the GPU host.
6. Real training runs use the GPU of the GPU host. When local training fails, they use a RunPod GPU. The training script runs on the CPU only for the fixture model and the script tests.

## 4. Request Flow

```
Satellite ──audio──▶ HA Assist pipeline
                       │ 1. AudioChunk...         wyoming.rs ─▶ Assistant.listen ─▶ app/speech.rs
                       │◀── Transcript (early) ── speculative prediction (Classifier) + commit policy (domain/speech.rs)
                       │ 2. Transcript + context  wyoming.rs ─▶ Assistant.handle ─▶ app/conversation.rs
                       │◀── Handled(text) ─────── Classifier.predict, then domain/decide.rs (pure), then dispatch in the background
                       │ 3. Synthesize(text)      wyoming.rs ─▶ Assistant.speak ─▶ app/speech.rs (audio cache)
Satellite ◀──audio─────┘◀── AudioChunk... ──────
```

## 5. Key Design Decisions

1. **Early commit.** HA reads the STT result while it sends audio. HA accepts the first `Transcript` event and stops the stream. Thus Jarvis decides the end of speech itself. It does not wait for the HA voice activity detection. (Source: `homeassistant/components/wyoming/stt.py`.)
2. **Speculative prediction.** The speech use case runs the System One model on each changed partial transcript. The audio stream has no satellite ID, so this prediction uses an empty context. When the top intent is not `unknown` and its probability is `model.commit_threshold` or more, Jarvis uses a short silence threshold (`early_commit_ms`). Other transcripts use a longer threshold (`endpoint_ms`). Silence is the time since the last voiced sample, which a Silero VAD in the first-pass adapter finds. Thus decoder delay does not add to the wait, and silence before speech does not commit.
3. **Reply first, act after.** Jarvis returns `Handled` before the HA service call completes. The service call runs in the background.
4. **Pre-rendered audio.** Jarvis synthesizes at startup each pool response whose only variable is `area`. A TTS request for such a response is a memory read. Other responses synthesize at request time and are not cached.
5. **Filler, then announce.** For external data and for System Two, Jarvis returns a filler in `Handled`. Then Jarvis speaks the answer with `assist_satellite.announce` on the same satellite. The first announce starts after the filler audio ends.
6. **Area from context.** HA sends `device_id` and `satellite_id` in the `Transcript` context. Jarvis finds the area of the device in the hot cache. (Source: `homeassistant/components/wyoming/conversation.py`.)
7. **One templating engine.** `minijinja` renders the response text and the service call data. All pools and intents use the same syntax.
8. **Time is an input.** Domain functions receive the current time as an argument. The application gets the time from the `Clock` port. Tests use `FakeClock`.
9. **Habits are a model input.** The application learns habits from the same event stream as the hot cache. The habit summary for the satellite area and the current time is part of the model context. Habits also rank entities and fill missing slots in `decide`.
10. **Source from the HA context.** Each `state_changed` event has a context. A `user_id` that is not the Jarvis user means `manual`. A `parent_id` means `automation`. No `user_id` and no `parent_id` means a physical device change, which is `manual`. The Jarvis user means `voice`.
11. **One joint model.** One model gives the intent probabilities and the slot tags. Thus one CPU pass per transcript gives all System One data. Clusters of `unknown` requests use OpenAI embeddings (sub-phase 14.1). These do not change when System One retrains, and the context does not change them.
12. **Train locally first.** The process adapter runs `train/train.py` on the GPU of the GPU host. When that run fails (for example, CUDA out of memory), the training use case sends the same training set to the RunPod adapter. Jarvis predicts with the ONNX model through `ort`. The Python script and the inline script of the review page are the only non-Rust code. The training set file is the contract between them.
13. **Accept only an equal or better model.** Jarvis assigns each example to the held-out set by a stable hash of its text. Jarvis measures the old model and the new model on the same held-out set. Jarvis keeps the new model only when its intent accuracy and its slot accuracy are both equal or higher.
14. **Training trigger.** Jarvis counts changes since the last training run. At `train.min_changes`, Jarvis starts a run. A voice request starts a run at any count. Only one run occurs at a time. After each run, accepted or rejected, the count starts again from 0.
15. **Speaker identification is not on the critical path.** Jarvis calculates the voice embedding in the background after commit. Only intents with `needs_speaker = true` wait for the result.
16. **Passive voice enrollment.** When presence or proximity identifies the speaker with high confidence, Jarvis adds the voice embedding to the profile of that person. No enrollment step is necessary.
17. **Ask through `start_conversation`.** The HA Wyoming conversation agent does not set `continue_conversation`. Thus `Handled` cannot open the microphone again. To ask a question, Jarvis returns `Handled` with empty text. Then Jarvis calls `assist_satellite.start_conversation` with the question and `preannounce = false`. The satellite speaks the question and listens. (Source: `homeassistant/components/wyoming/conversation.py`.)
18. **Dialogs by satellite.** `start_conversation` starts a new pipeline run with a new `conversation_id`. Thus Jarvis keys each dialog by `satellite_id`. When the context has no `satellite_id` (for example, text from the HA UI), Jarvis keys the dialog by `conversation_id` and puts the question in `Handled`. A dialog closes on completion, on cancel, after `dialog.max_turns` questions, or `dialog.timeout_s` after the last question.
19. **System Two asks with a tool.** The OpenAI request has an `ask_user` tool. When the model calls it, Jarvis asks the question and keeps the dialog open. The next turn sends the answer as the `function_call_output` of that call, with `previous_response_id`.
20. **System Two teaches System One.** Jarvis clusters the OpenAI embeddings of recent `unknown` requests. For each large cluster, System Two writes a proposal: name, kind, examples, responses and an optional service. Jarvis validates it and saves it to `learned.toml`. The next training run adds it to the model.
21. **Device control needs approval.** A proposal with a `service` waits for your approval on the review page. Other proposals become active immediately.
22. **Prefetch has no side effects.** A prefetch answer must not change the house. A `Call` step during a prefetch runs only when its service is in `learn.prefetch_services` (read-only services such as `weather.get_forecasts`). Any other `Call` discards that prefetch.
23. **Discovery from HA.** Jarvis reads the HA registries in the hot cache. Each `person` entity lists its `device_trackers`. The sensors on the same device as a tracker give its area and distance (for example from Bermuda with the satellites as Bluetooth proxies). The `binary_sensor` entities with device class `occupancy`, `presence` or `motion` on the satellite device are its presence sensors. Configuration can override discovery. In the reference house (section 11) the presence sensors have no device class. Set their device class to `occupancy` in HA, or list them in `[[satellites]]`.
24. **Audio correlation.** HA sends the audio (`asr`) and the transcript (`handle`) on different connections with no shared ID. The speech use case keeps each committed utterance (text, audio, time) for `log.correlate_s`. For a voice request (a context with a `satellite_id`), the conversation use case finds the one utterance with the same normalized text and removes it. An ambiguous match gives no audio. Speaker identification uses the same correlation.
25. **Logging is not on the critical path.** Jarvis writes the interaction after it returns `Handled`. Later announces and questions update the same record.
26. **Feedback rules.** A bad action blocks that decision (intent and targets) for that normalized text at once. `decide` then uses the next entity candidate, then the next intent with enough probability. Else it sends the request to System Two with the bad action as a correction. A bad response leaves its pool. The next training run learns from all marks and corrections.
27. **One ONNX Runtime, GPU or CPU.** `sherpa-onnx` (feature `shared`) and `ort` (feature `load-dynamic`) load the same ONNX Runtime 1.28 shared library. On the GPU host, it is the CUDA 12 and cuDNN 9 build from the sherpa-onnx `linux-x64-gpu` release. On other machines, it is the CPU build. Thus the two crates cannot conflict, and `device` selects the CUDA or the CPU execution provider for all models.
28. **Two-pass STT.** The first pass streams, so Jarvis knows the words while the user speaks. It uses NVIDIA Nemotron Speech Streaming EN 0.6B with 160 ms chunks on the GPU host. A 160 ms chunk is shorter than `early_commit_ms`, so the last words arrive inside the silence wait. At commit, the second pass decodes the whole utterance with NVIDIA Parakeet Unified EN 0.6B in offline mode. The second pass has full context, so it is more accurate. On the Open ASR Leaderboard sets, the average word error rate goes from 7.67% to 5.91%, and on LibriSpeech test-clean from 2.56% to 1.63%. HA receives the second-pass text. Both models are official sherpa-onnx exports. Cloud streaming APIs take about 250 ms to 300 ms to give the final text, so they cannot meet G1. CPU machines use the 20M zipformer for the first pass and have no second pass.

## 6. Latency Budget (System One)

| Step | Budget |
|---|---|
| Trailing silence before early commit (`early_commit_ms`), from the last voiced sample | 200 ms |
| Second pass of a 3 s utterance (GPU host). The first pass finishes inside the silence wait. | 40 ms |
| Wyoming `Transcript` → `Handled` (model prediction 10 ms, decide, select) | 20 ms |
| Wyoming `Synthesize` → first audio chunk (cache hit) | 10 ms |
| Margin | 30 ms |
| **Total response latency** | **300 ms** |

HA to satellite audio delivery is outside Jarvis. The Phase 9 field log records it on real hardware.

The budget applies to the GPU host. The `gpu-test` job runs the latency tests there (section 10).

## 7. Crates

| Crate | Layer | Use |
|---|---|---|
| `minijinja` | Domain | Response and service data templates. |
| `rand` | Domain | Response selection and context samples. |
| `jiff` | Domain | Time zones, buckets and calendar dates. |
| `strsim` | Domain | Fuzzy match of a spoken answer to a list of choices. |
| `dashmap` | Application | Concurrent audio cache. |
| `ndarray`, `linfa`, `linfa-clustering` | Domain | DBSCAN clusters of `unknown` embeddings. |
| `xxhash-rust` | Domain | Stable hash for the held-out split. |
| `serde`, `serde_json` | All | Data types and messages. |
| `tokio` | Application, adapters | Async runtime, TCP, timers, channels. |
| `arc-swap` | Application | Atomic swap of the System One model. |
| `anyhow` | All except domain | Error propagation. Domain functions return typed errors. |
| `tracing`, `tracing-subscriber` | Application, adapters, root | Logs and latency spans. |
| `futures` | Ports, application, adapters | `Stream` for events, partial transcripts and sentences. |
| `async-trait` | Ports, application, adapters, root | Async methods on port traits behind `Arc<dyn Port>`. |
| `wyoming` | Adapter | Wyoming event types (sans-IO). |
| `tokio-tungstenite`, `futures-util` | Adapter | HA WebSocket API. |
| `sherpa-onnx` (feature `shared`) | Adapter | Streaming STT (`OnlineRecognizer`), TTS (`OfflineTts`, Piper VITS models), voice identification (`SpeakerEmbeddingExtractor`, `SpeakerEmbeddingManager`). CUDA or CPU provider. |
| `ort` (feature `load-dynamic`), `tokenizers` | Adapter | System One model prediction with ONNX Runtime (CUDA or CPU execution provider). Tokenizer from `tokenizer.json`. |
| `reqwest` | Adapter | RunPod serverless REST API. |
| `object_store` (feature `aws`) | Adapter | RunPod network volume through its S3-compatible API. |
| `async-openai` | Adapter | OpenAI Responses API: streaming, tools, structured output, `previous_response_id`. |
| `rusqlite` (feature `bundled`) | Adapter | Interaction log database. |
| `axum` | Adapter | Review web page and API. |
| `hound` | Adapter | WAV encode for audio playback. |
| `toml`, `tempfile` | Adapter, root | File stores with atomic write. Configuration. |
| `wiremock` (dev) | Adapter tests | Mock OpenAI and RunPod servers. |
| `cargo-llvm-cov` (tool) | — | Coverage gate. |
| `rust-code-analysis-cli` (tool) | — | Cyclomatic complexity, Halstead and Python function length metrics. `xtask complexity` reads its JSON output. |
| `pre-commit` (tool) | — | Git hook manager. |
| `jscpd` (tool) | — | Duplication gate for Rust and Python. |
| `gungraun` (dev), Valgrind (tool) | Benchmarks | Deterministic instruction budgets (`Callgrind::hard_limits`). Linux only. |
| `serde_yaml`, `tempfile` (dev) | `xtask` | Structure tests and gate test fixtures. |
| `actionlint` (tool) | — | Workflow lint. |
| `gh-stack` (GitHub CLI extension) | — | GitHub native stacked PRs. |
| Codex (GitHub app) | — | AI code review of each PR. |
| `quint-connect` (dev) | Domain and application tests | Conformance tests: replay Quint traces against the Rust code. |
| `syn`, `clap` | `xtask` | Parse the Rust source for the module graph. Command line of `xtask`. |
| Quint CLI (`@informalsystems/quint`, tool) | — | Type check, test, simulate and model check the Quint models. |
| Apalache (tool, through `quint verify`, needs Java 17) | — | Bounded model checking of the Quint invariants. |

Python packages for `train/` (pinned in `train/pyproject.toml`):

- Runtime: `torch`, `transformers`, `onnx`, `onnxruntime`, `runpod`.
- Development (`dev` extra): `pytest`, `pytest-cov`, `ruff`, `mypy`, `vulture`, `interrogate`.

If the `wyoming` crate does not have an event type that Jarvis needs, add only that type in `adapters/wyoming.rs` with `serde`.

`ort` and `sherpa-onnx` load one shared ONNX Runtime library (decision 27). `SHERPA_ONNX_LIB_DIR` points to the sherpa-onnx CUDA build on the GPU host and in the Linux release job. `ORT_DYLIB_PATH` points to the `libonnxruntime` in the same folder. Sub-phase 6.1 proves that both work in one process.

## 8. Files

Jarvis uses these files. No other files are permitted without a change to this section. `src/lib.rs` declares the folders as inline public modules (for example `pub mod domain { pub mod intent; }`). Thus no `mod.rs` files are necessary. Jarvis is a library with a thin binary. Thus `benches/` can import the domain, and a `pub` item that only a later sub-phase uses is not dead code.

| File | Layer | Responsibility | Created in |
|---|---|---|---|
| `CLAUDE.md` | — | Working rules for code changes. | — |
| `docs/spec/*.md` | — | This specification. | — |
| `docs/architecture.md` | — | The full architecture diagram. CI writes it on each commit to `main`. | 0.9 |
| `.gitignore` | — | Ignore `target/`, `models/`, `.venv/` and the data files. | 0.1 |
| `rust-toolchain.toml` | — | The fixed Rust version and its components. | 0.1 |
| `Cargo.toml` | — | Workspace (`jarvis`, `xtask`), crates and lint levels. | 0.1 |
| `Cargo.lock` | — | Locked crate versions. | 0.1 |
| `clippy.toml` | — | Clippy thresholds. | 0.2 |
| `justfile` | — | All developer commands. | 0.1 |
| `.pre-commit-config.yaml` | — | The pre-commit hook and the pre-push guard. | 0.1 |
| `.github/workflows/ci.yml` | — | The required checks for each push and PR. | 0.1 |
| `.github/pull_request_template.md` | — | The PR description template. | 0.5 |
| `.github/workflows/architecture.yml` | — | Diagram regeneration on each commit to `main`. | 0.11 |
| `.github/workflows/release.yml` | — | Release binaries and the training image for each `v*` tag. | 0.14 |
| `xtask/Cargo.toml` | Tools | The `xtask` crate. | 0.1 |
| `xtask/src/main.rs` | Tools | Command line: `complexity`, `pr-size`, `rules`, `diagram`, `merge`, `guard-push`. | 0.1 |
| `xtask/src/gates.rs` | Tools | `#[cfg(test)]` only. Gate tests (real tools on fixtures) and structure tests (hook, `justfile`, workflows, PR template). | 0.1 |
| `xtask/src/complexity.rs` | Tools | Reads `rust-code-analysis-cli` JSON and applies the limits. | 0.3 |
| `xtask/src/pr_size.rs` | Tools | Counts the changed lines of a PR without the excluded files. | 0.6 |
| `xtask/src/graph.rs` | Tools | Builds the module graph from the source with `syn`. | 0.7 |
| `xtask/src/rules.rs` | Tools | Checks the layer and modularity rules on the module graph. | 0.8 |
| `xtask/src/mermaid.rs` | Tools | Draws the module graph in Mermaid, with change colors. | 0.9 |
| `xtask/src/merge.rs` | Tools | The merge gate and the pre-push guard. | 0.15 |
| `benches/domain.rs` | Test | Gungraun benchmarks with absolute instruction budgets. | 2.8 |
| `jarvis.toml` | — | Runtime configuration. | 1.1 |
| `intents.toml` | — | Intent catalog with seed examples. | 3.2 |
| `responses.toml` | — | The source response pools. You edit it. Jarvis reads it and does not write it. | 4.1 |
| `train/pyproject.toml` | Training | Pinned Python packages and the configuration of `ruff`, `mypy`, `pytest`, `coverage`, `vulture` and `interrogate`. | 3.5 |
| `train/train.py` | Training | Training set load, fine-tune, ONNX export, RunPod handler. | 3.5 |
| `train/test_train.py` | Training | `pytest` tests for `train.py`. | 3.5 |
| `train/Dockerfile` | Training | The RunPod worker image. | 3.10 |
| `quint/cache_sync.qnt` | Model | Hot cache and HA state. | 2.6 |
| `quint/announce.qnt` | Model | Filler and announce order. | 4.7 |
| `quint/commit.qnt` | Model | Commit policy. | 6.4 |
| `quint/backfill.qnt` | Model | Response backfill. | 8.3 |
| `quint/dialog.qnt` | Model | Dialogs. | 10.1 |
| `quint/feedback.qnt` | Model | Marks and blocked decisions. | 12.2 |
| `quint/training.qnt` | Model | Training trigger, run, accept and fallback. | 13.4 |
| `quint/learned.qnt` | Model | Learned intent lifecycle and approval. | 14.3 |
| `quint/prefetch.qnt` | Model | Prefetch freshness and side effects. | 14.8 |
| `quint/speaker.qnt` | Model | Speaker identification and enrollment. | 15.5 |
| `quint/jarvis.qnt` | Model | The system model. It composes the other models. | 16.1 |
| `src/lib.rs` | Root | Module tree, `Config`, `SystemClock`, `App` (implements `Assistant` by delegation), `run`, the `dataset` command, `main`. | 0.1 |
| `src/main.rs` | Root | Binary entry point. `main` only calls `jarvis::main`. | 0.1 |
| `src/ports.rs` | Ports | All port traits and their data types. | 2.1 |
| `src/domain/state.rs` | Domain | Hot cache data and queries. | 2.7 |
| `src/domain/intent.rs` | Domain | Normalize, example markup, catalog parse and validation, tag list. | 3.1 |
| `src/domain/training.rs` | Domain | Training set, context samples, held-out split, accuracy, accept rule, change count. | 3.3 |
| `src/domain/model.rs` | Domain | Softmax, BIO decode, prediction ranking. | 3.11 |
| `src/domain/responses.rs` | Domain | Pools: select, render, record use, backfill policy, validation. | 4.1 |
| `src/domain/select.rs` | Domain | Entity selection for an intent and an area. | 4.4 |
| `src/domain/decide.rs` | Domain | `decide`: prediction and context to `Decision`. | 4.5 |
| `src/domain/speech.rs` | Domain | Announce delay, commit policy, audio chunks, sentence split, audio correlation. | 4.9 |
| `src/domain/dialog.rs` | Domain | Dialog state, answer fill, cancel, turn limit. | 10.2 |
| `src/domain/habits.rs` | Domain | Habit model: source, buckets, decay, scores, summary. | 11.1 |
| `src/domain/feedback.rs` | Domain | Marks, corrections, feedback rules. | 12.3 |
| `src/domain/learn.rs` | Domain | Clusters, proposal validation, approval rule, prefetch schedule and freshness. | 14.1 |
| `src/domain/speaker.rs` | Domain | Discovery, presence, proximity and identification. | 15.1 |
| `src/app/background.rs` | Application | Hot cache sync, habit learning, backfill, learned intents, prefetch. | 2.9 |
| `src/app/conversation.rs` | Application | `handle` use case, dispatch, deferred jobs, dialog map. | 4.8 |
| `src/app/speech.rs` | Application | `listen` and `speak` use cases, audio cache, pre-render. | 6.3 |
| `src/app/review.rs` | Application | `Review` use case. | 12.7 |
| `src/app/training.rs` | Application | Generated examples, training trigger, training run, accept and swap. | 13.6 |
| `src/adapters/ha.rs` | Adapter | `Home` over the HA WebSocket API. | 2.2 |
| `src/adapters/onnx.rs` | Adapter | `Classifier` and `ModelLoader` with ONNX Runtime. | 3.12 |
| `src/adapters/files.rs` | Adapter | `Store<T>` with TOML or JSON files and atomic write. | 4.3 |
| `src/adapters/wyoming.rs` | Driving adapter | Wyoming TCP server. It calls `Assistant`. | 5.1 |
| `src/adapters/sherpa.rs` | Adapter | `SpeechToText` (first and second pass), `TextToSpeech`, `VoiceEmbedder`, `VoiceProfiles` with sherpa-onnx. | 6.1 |
| `src/adapters/openai.rs` | Adapter | `Reasoner` with OpenAI. Prompt builders. | 7.1 |
| `src/adapters/sqlite.rs` | Adapter | `InteractionLog` with SQLite. | 12.4 |
| `src/adapters/web.rs` | Driving adapter | Review web server with token auth. It calls `Review`. | 12.8 |
| `src/adapters/review.html` | Driving adapter | The review page. `web.rs` embeds it with `include_str!`. | 12.9 |
| `src/adapters/process.rs` | Adapter | `Trainer` that runs `train/train.py` as a local process. | 13.7 |
| `src/adapters/runpod.rs` | Adapter | `Trainer` with the RunPod REST API and network volume (fallback). | 13.8 |
| `src/testing/house.json` | Test | The reference house snapshot (section 11). | 2.1 |
| `src/testing/fakes.rs` | Test | `#[cfg(test)]` only. A fake for each port, `FakeWyomingClient`, `speech(text)`. | 2.9 |
| `src/testing/e2e.rs` | Test | `#[cfg(test)]` only. Pipeline harness and end-to-end latency tests. | 9.1 |

Jarvis also writes these data files: `habits.json`, `voices.json`, `jarvis.db`, `generated.json`, `training.json`, `pools.json` (use counts and generated responses), `learned.toml` (learned intents) and the model folders in `models/system-one/`. They are not source files, and Git ignores them. Edit `learned.toml` only while Jarvis is stopped.

Unit tests are in a `#[cfg(test)] mod tests` block at the end of each source file. The conformance driver for a Quint model is in the test block of the module that it checks. Shared test code is in `src/testing/` only. The fake HA WebSocket server is in the test block of `src/adapters/ha.rs`. `src/testing/house.json` is a data file: tests embed it with `include_str!`, and the `dataset` command reads it from its `--house` path.

## 9. Configuration Files

`jarvis.toml`:

```toml
listen = "0.0.0.0:10700"
device = "cuda"    # "cpu" on the test machine and in CI
intents = "intents.toml"
responses = "responses.toml"
pools = "pools.json"

[ha]
url = "ws://homeassistant.local:8123/api/websocket"
token_env = "HA_TOKEN"
request_timeout_ms = 5000

[model]
dir = "models/system-one"
threshold = 0.6
commit_threshold = 0.9
next_threshold = 0.2
habits = 5

[asr]
# GPU host. CPU machines use "models/sherpa-onnx-streaming-zipformer-en-20M-2023-02-17".
model_dir = "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25"
# Optional. Without it, the final transcript is the first-pass text (CPU machines).
second_pass_dir = "models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming"
vad_model = "models/silero_vad.onnx"
early_commit_ms = 200
endpoint_ms = 600
max_utterance_ms = 8000

[tts]
model_dir = "models/vits-piper-en_US-lessac-medium"

[openai]
api_key_env = "OPENAI_API_KEY"
model = "gpt-4.1-nano"
timeout_ms = 8000
warm_idle_s = 60
max_output_tokens = 150      # answers
bulk_max_output_tokens = 8000 # structured output: generate, paraphrase, propose
embedding_model = "text-embedding-3-small"
allowed_domains = ["light", "switch", "fan", "media_player", "climate", "calendar", "weather"]

[backfill]
min_fresh = 3
batch = 5
max_chars = 80
max_pool = 30

[announce]
margin_ms = 300
tts_wait_ms = 1000

[dialog]
max_turns = 3
timeout_s = 15
choice_similarity = 0.85
cancel = ["never mind", "cancel", "forget it", "stop"]

[habits]
file = "habits.json"
domains = ["light", "fan", "switch", "media_player", "climate"]
half_life_days = 14
bootstrap_days = 30
save_interval_s = 300
min_score = 3.0

[train]
state = "training.json"
generated = "generated.json"
base_model = "microsoft/MiniLM-L12-H384-uncased"
epochs = 8
batch_size = 8
min_changes = 20
check_interval_s = 600
holdout = 0.1
failure_backoff_s = 21600
timeout_min = 60
paraphrases = 50
unknown_paraphrases = 300
python = ".venv/bin/python"

# Optional. Without this section, Jarvis has no training fallback.
[train.runpod]
api_key_env = "RUNPOD_API_KEY"
endpoint_id = "your-endpoint-id"
volume_id = "your-volume-id"
s3_endpoint = "https://s3api-eu-ro-1.runpod.io"
s3_region = "EU-RO-1"
s3_key_env = "RUNPOD_S3_KEY"
s3_secret_env = "RUNPOD_S3_SECRET"
poll_s = 30
timeout_min = 60

[learn]
file = "learned.toml"
window_days = 14
min_cluster = 5
cluster_distance = 0.3
check_interval_s = 3600
responses = 10
prefetch_lead_min = 5
prefetch_ttl_min = 60
prefetch_services = ["weather.get_forecasts", "calendar.get_events"]

[log]
database = "jarvis.db"
audio_days = 30
record_days = 90
correlate_s = 10

[review]
listen = "0.0.0.0:10701"
token_env = "JARVIS_REVIEW_TOKEN"
page_size = 50

[speaker]
model = "models/wespeaker_en_voxceleb_CAM++.onnx"
voices = "voices.json"
wait_ms = 100
voice_threshold = 0.6   # voice similarity
threshold = 0.6         # combined score of proximity and voice
margin = 0.15
enroll_threshold = 0.5  # proximity 1 / (1 + distance): 1 m or less
min_enroll_ms = 1500
min_profile = 5
max_samples = 20

[[people]]
person = "person.alex"
calendar = "calendar.alex"
# Optional. Discovery finds the trackers when this is not set.
# trackers = [{ area = "sensor.alex_watch_area", distance = "sensor.alex_watch_distance" }]

# Optional. Discovery finds the presence sensors when this is not set.
# [[satellites]]
# satellite = "assist_satellite.bedroom"
# presence = ["binary_sensor.bedroom_presence"]
```

Discovery (decision 23) finds the trackers and the satellite presence sensors. A tracker area sensor is a sensor on the tracker device whose state is an area name. A tracker distance sensor is a sensor on the tracker device with the unit `m`.

`intents.toml` (example entries):

```toml
min_examples = 3
unknown_examples = [
  "what is the meaning of life",
  "tell me about rome",
  "how far away is the moon",
  "write a poem about cats",
  "who won the game last night",
]

[[intent]]
name = "lights_on"
kind = "command"
examples = [
  "turn on the lights",
  "turn on the [kitchen](area) lights",
  "lights on",
  "switch the lights on in the [living room](area)",
  "can you turn the [office](area) lights on",
]
service = "homeassistant.turn_on"
target = "entities"
select = [{ domain = "light" }, { domain = "switch", name = "light" }]

[[intent]]
name = "fan_on"
kind = "command"
examples = ["turn on the fan", "turn on the [office](area) fan", "fan on please", "start the fan"]
service = "homeassistant.turn_on"
target = "entity"
select = [{ domain = "fan" }, { domain = "switch", name = "fan" }]

[[intent]]
name = "climate_down"
kind = "command"
examples = ["turn the air down", "turn the ac down", "make it cooler", "lower the temperature"]
service = "climate.set_temperature"
target = "entity"
select = [{ domain = "climate" }]
data = { temperature = "{{ entity.attributes.temperature - 2 }}" }

[[intent]]
name = "tv_show"
kind = "command"
examples = ["put the tv on [the office](show)", "play [bluey](show) on the tv", "turn the tv to [jeopardy](show)"]
service = "media_player.play_media"
target = "entity"
select = [{ domain = "media_player", name = "tv" }]
data = { media_content_type = "episode", media_content_id = 'plex://{"library_name": "TV Shows", "show_name": "{{ show }}", "shuffle": 1}' }

[[intent]]
name = "temperature_query"
kind = "query"
examples = ["what is the temperature", "what is the temperature in the [bedroom](area)", "how warm is it in here"]
select = [{ domain = "sensor", device_class = "temperature" }]

[[intent]]
name = "weather"
kind = "fetch"
examples = ["what is the weather like", "will it rain today", "what is the forecast"]
service = "weather.get_forecasts"
target = "entity"
entity = "weather.home"
data = { type = "daily" }

[[intent]]
name = "tv_on"
kind = "command"
examples = ["turn on the tv", "switch the tv on", "tv on"]
service = "media_player.play_media"
target = "entity"
select = [{ domain = "media_player", name = "tv" }]
habit_defaults = { show = "play" }
data = { media_content_type = "episode", media_content_id = 'plex://{"library_name": "TV Shows", "show_name": "{{ show }}", "shuffle": 1}' }

[[intent]]
name = "calendar_add"
kind = "command"
needs_speaker = true
examples = [
  "add [dentist](summary) to my calendar for [monday](day)",
  "put [soccer practice](summary) on my calendar [tomorrow](day)",
  "add [call mom](summary) to my calendar",
]
optional = ["day"]
service = "calendar.create_event"
target = "entity"
entity = "{{ person.calendar }}"
data = { summary = "{{ summary }}", start_date = "{{ next_date(day, 0) }}", end_date = "{{ next_date(day, 1) }}" }

[[intent]]
name = "start_training"
kind = "train"
examples = ["start training", "retrain yourself", "train your model", "start a new training run"]
```

- `examples` are seed examples. `[value](slot)` marks a slot value. Each intent has `min_examples` examples or more.
- `unknown_examples` are seed examples for the `unknown` label.
- `kind = "command"`: Jarvis replies from the pool and calls the service in the background. A command with no `service` only replies. Static intents use this form.
- `kind = "query"`: Jarvis replies with data from the hot cache. No remote call.
- `kind = "fetch"`: Jarvis replies with a filler. Then it calls the service with `return_response` and announces the answer.
- `kind = "prefetch"`: Jarvis replies with the prefetched answer when it is fresh. Else Jarvis treats the request as System Two. Only learned intents use this kind.
- `kind = "train"`: Jarvis replies from the pool and starts a training run (sub-phase 13.11).
- `select` is a list of entity filters. A filter has a `domain`, an optional `name` and an optional `device_class`. `name` matches whole words of the entity name or an alias, without case sensitivity (`fan` matches "Office Fan", not "Infant Monitor"). An entity matches when one filter matches.
- `exclude` is a list of entity IDs that `select` never matches. Use it for a sensor that gives bad values.
- The area is the area slot. If no slot, it is the satellite area. The area slot matches an area name or alias without case sensitivity.
- `target = "entities"` uses all matching entities in the area.
- `target = "entity"` uses `entity`, or the matching entity in the area with the lowest entity ID. Sub-phase 11.6 replaces "lowest entity ID" with "highest habit score".
- A command or query whose selection is empty gives `Fallback`: Jarvis replies from the pool `fallback` and makes no call.
- When the area has no matching entity and the house has exactly one, Jarvis uses that entity. Example: one thermostat for the house.
- `habit_defaults` maps a slot to a habit action. When the slot is empty, Jarvis uses the most frequent value of that action now.
- `needs_speaker = true` makes the `person` variable available. Jarvis waits up to `speaker.wait_ms` for it.
- `next_date(day, plus_days)` returns the ISO date of the next `day` plus `plus_days`. An empty `day` means today. A `day` that is not a day name, `today` or `tomorrow` gives an error.
- Template variables are the built-ins `area` (the area name in lowercase), `entity`, `person` and `response`, the slots of the intent, and the keys of `habit_defaults`.
- A slot that the prediction does not have is missing, unless the intent lists it in `optional`. A missing optional slot is an empty string. A missing `habit_defaults` key uses the habit value. Phase 10 makes Jarvis ask for any other missing slot, and for a missing `area` or `person`. Before Phase 10, a missing slot gives `Fallback`.
- A data value that is one `{{ expression }}` keeps the type of the expression result. Other data values render as strings.

`responses.toml` (example entries):

```toml
[pools.lights_on]
responses = [
  "I am turning the lights on.",
  "I will turn on the lights.",
  "Powering up the {{ area }} lights.",
]

[pools.temperature_query]
responses = ["It is {{ entity.state }} degrees in the {{ area }}."]

[pools.weather]
responses = ["Today will be {{ response['weather.home'].forecast[0].condition }}."]

[pools.filler]
responses = [
  "I will check on that for you.",
  "Let me look into that.",
  "One moment.",
]

[pools.failure]
responses = ["Sorry, I could not do that."]

[pools.fallback]
responses = ["Sorry, I could not find that."]

[pools.calendar_add]
responses = ["I added {{ summary }} to your calendar, {{ person.name }}."]

[pools.start_training]
responses = ["I will start training now."]

[pools.training_busy]
responses = ["Training is already running."]

[pools.ask_area]
responses = ["Which room?"]

[pools.ask_person]
responses = ["Who is asking?"]

[pools.ask_show]
responses = ["What would you like to watch?"]

[pools.cancelled]
responses = ["Okay, never mind."]
```

A pool named `ask_<slot>` holds the questions for one missing slot.

Template variables: `area` (area name), slot names, `entity` (state, attributes, name), `response` (fetch result), `person` (name and the fields of its `[[people]]` entry).

Training set file (the contract between Jarvis and `train/train.py`):

```json
{
  "base_model": "microsoft/MiniLM-L12-H384-uncased",
  "epochs": 8,
  "batch_size": 8,
  "labels": ["unknown", "lights_on", "fan_on"],
  "tags": ["O", "B-area", "I-area", "B-show", "I-show"],
  "examples": [
    { "context": "area: kitchen | habits: none", "text": "turn on the kitchen lights", "intent": "lights_on",
      "spans": [{ "slot": "area", "start": 12, "end": 19 }] },
    { "context": "area: office | habits: none", "text": "turn on the tv", "not": "tv_on", "spans": null }
  ]
}
```

- An example has `intent` or `not`. `not` means "any label except this one".
- `spans` are character offsets in `text`. `null` means "the slots are not known". Such an example adds no slot loss.

## 10. Machines

| Machine | OS | Device | STT model | Use |
|---|---|---|---|---|
| GPU host (self-hosted runner `gpu`) | Linux x86-64, RTX 2060 6 GB | `cuda` | First pass: Nemotron Speech Streaming EN 0.6B, 160 ms, int8. Second pass: Parakeet Unified EN 0.6B offline, int8. | Runs Jarvis. Runs local training. |
| Test machine | macOS x86-64 (Intel MacBook Pro) | `cpu` | 20M zipformer | Development. Runs the pre-commit hook (without `bench` and `gpu-test`). |
| CI | `ubuntu-latest` | `cpu` | 20M zipformer | `just check`. |
| RunPod | RunPod serverless | GPU | — | Training fallback. |

VRAM budget on the GPU host (6 GB):

| Use | Budget |
|---|---|
| STT first pass (Nemotron 0.6B) | 1.5 GB |
| STT second pass (Parakeet Unified 0.6B) | 1.5 GB |
| System One model | 0.3 GB |
| TTS (Piper VITS) and voice identification (CAM++) | 0.4 GB |
| ONNX Runtime arenas and CUDA context | 0.8 GB |
| Local training run (MiniLM, fp16, `batch_size = 8`) | 1.0 GB |
| Reserve (not usable on a 6 GB card) | 0.5 GB |
| **Total** | **6.0 GB** |

The model budgets assume fp16 weights. The int8 models use less. The usable budget is 5.5 GB, below the 5.8 GB limit of AC-13.10-5. A GPU host test measures the real peak and checks that each model runs on the CUDA execution provider (AC-6.2-2). If a local run fails with "CUDA out of memory", the run moves to RunPod (decision 12). Live STT never stops for training.

The GPU host needs the NVIDIA driver, CUDA 12, cuDNN 9, and a Python 3.12 virtual environment in `.venv` with `pip install ./train`.

## 11. Reference House

The test fixtures (`FakeHome`, `FakeHaServer`) model this house. The data comes from the HA instance on 2026-10-25. `src/testing/house.json` holds it as a `Home` snapshot.

- Areas: Bedroom, Kitchen, Living Room, Office, Bathroom, Garage, Outside.
- People: two `person` entities.
- Satellites: four, in the Bedroom, Kitchen, Living Room and Office. Each area has a "<Area> Presence" `binary_sensor` with no device class and no entity area.
- Lights: many lights in each area. "Garage Light" is a `switch`.
- Fans: "Office Fan" is a `fan`. "Living Room Fan" is a `switch`.
- Climate: one thermostat, in the Kitchen.
- TV: one media player in the Living Room with the aliases "the TV" and "plex".
- Speakers: each satellite area has speaker media players.
- Office: two temperature sensors with different values (64.0 °F and 76.6 °F). The higher value is probably the satellite sensor, which the device makes warm.

---

## 12. Change Map

G10 requires that each change below touches only the listed files. Sub-phase 0.8 enforces the adapter rule that makes this true.

| Change | Files that change |
|---|---|
| Add or change an intent | `intents.toml`, `responses.toml`. The next training run adds it to the model. |
| Change a response | `responses.toml`. |
| Change a threshold or a time limit | `jarvis.toml`. |
| Change the STT, TTS or speaker model | `jarvis.toml`, and the download recipe in `justfile`. |
| Change the System One base model | `jarvis.toml` (`train.base_model`). |
| Replace the technology of a port (for example, a different LLM provider) | One new adapter file, `Cargo.toml`, `src/lib.rs`, and the port and file tables in this file. |
| Add a port | `src/ports.rs`, one adapter file, `src/testing/fakes.rs`, the use case that calls it, `src/lib.rs`. |
| Add a domain rule | One domain file, and the use case that calls it. |
| Change a stateful rule | The domain file, its Quint model, and its conformance driver. |

## 13. Quint Models

Each model has state variables, actions and named invariants. `02-rules.md` section 4 gives the rules for models.

| Model | Component | Rust module | Invariants |
|---|---|---|---|
| `cache_sync.qnt` | Hot cache sync | `domain/state.rs`, `app/background.rs` | `CacheEqualsHomeWhenIdle`, `NoEventAfterDisconnect` |
| `announce.qnt` | Filler and announces | `app/conversation.rs` | `AnnounceAfterFiller`, `SentencesInOrder` |
| `commit.qnt` | Commit policy | `domain/speech.rs` | `CommitOnlyWhenAllowed`, `NoLateCommit` |
| `backfill.qnt` | Backfill | `app/background.rs` | `OneBackfillPerPool`, `PoolsChangeOnlyOnSuccess` |
| `dialog.qnt` | Dialogs | `domain/dialog.rs` | `TurnLimit`, `NoCallAfterCancel` |
| `feedback.qnt` | Marks | `domain/feedback.rs` | `DecideAvoidsBlocked` |
| `training.qnt` | Training | `domain/training.rs`, `app/training.rs` | `OneRun`, `SwapNeverWorse`, `CountResetsAfterRun`, `FailureKeepsModel`, `FallbackOnce` |
| `learned.qnt` | Learned intents | `domain/learn.rs`, `app/background.rs` | `ServiceNeedsApproval`, `NoReproposal`, `ActivationCountsOnce` |
| `prefetch.qnt` | Prefetch | `domain/learn.rs`, `app/background.rs` | `NoCallFromPrefetch`, `ServedAnswerIsFresh` |
| `speaker.qnt` | Speaker identification | `domain/speaker.rs` | `SpeakerIsCandidate`, `NoVoiceOnlyEnrollment` |
| `jarvis.qnt` | System | All use cases | `OneHandledPerRequest`, `ReplyBeforeCall`, `FillerBeforeAnswer`, `ModelSwapAtomic` |
