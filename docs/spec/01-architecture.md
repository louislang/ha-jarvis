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

Verus verifies every domain function. The Verus proofs are in the domain files, and the trusted contracts are in `src/domain/contracts.rs` (`02-rules.md` section 5.1).

The composition root (`src/lib.rs`) is the only code that knows all layers. The binary `src/main.rs` only calls `jarvis::main`.

`xtask/` is a separate crate in the same Cargo workspace. It holds the developer tools: the complexity gate, the PR size gate, the module graph, the layer rules and the diagram. Jarvis does not depend on it.

The training script (`train/`) is outside the layers. It is the only Python code. It reads a training set file and writes ONNX model files. Rust code does not import it. The process adapter starts it as a process on the GPU host. The RunPod adapter starts it on RunPod. `just fixture-model` starts it on the CPU.

Dependency rules:

1. The domain does not use `crate::ports`, `crate::app`, `crate::adapters`, `tokio`, `std::fs`, `std::net`, `std::process`, `std::thread`, `std::env`, `std::time::SystemTime`, `std::time::Instant`, `std::collections::HashMap`, `std::collections::HashSet`, `jiff::Timestamp::now`, `jiff::Zoned::now`, `jiff::tz::TimeZone::system`, `rand::thread_rng`, `rand::rng`, `rand::random`, `rand::rngs::OsRng` or `rand::rngs::ThreadRng`. It gets time, randomness and the environment as parameters. It uses `BTreeMap` and `BTreeSet`, because the iteration order of a `HashMap` changes from run to run.
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
 Browser ──HTTP──────▶ adapters/   │  Review port                 │   TextToSpeech ───▶ adapters/supertonic.rs
                       web.rs ─────┼─▶ (list, mark, proposals)    │   VoiceEmbedder ──▶ adapters/sherpa.rs
                                   │        │                     │   VoiceProfiles ──▶ adapters/sherpa.rs
                                   │   ┌────▼──── app/ ─────────┐ │   Classifier ─────▶ adapters/onnx.rs
                                   │   │ conversation  speech   │─┼─▶ ModelLoader ────▶ adapters/onnx.rs
                                   │   │ background  training   │ │   Trainer ────────▶ adapters/process.rs, runpod.rs ─▶ RunPod
                                   │   │ review  plugins        │ │   Reasoner ───────▶ adapters/openai.rs ─▶ OpenAI
                                   │   └────┬───────────────────┘ │   InteractionLog ─▶ adapters/sqlite.rs
                                   │   ┌────▼──── domain/ ──────┐ │   Store<T> ───────▶ adapters/files.rs
                                   │   │ intent model training  │ │   Clock ──────────▶ SystemClock (lib.rs)
                                   │   │ decide responses state │ │   Plugin ─────────▶ adapters/open_meteo.rs ─▶ Open-Meteo
                                   │   │ dialog habits feedback │ │                     adapters/plex.rs ─▶ Plex
                                   │   │ learn speaker speech   │ │   Models ─────────▶ LiveModels (lib.rs)
                                   │   └────────────────────────┘ │   Issues ─────────▶ adapters/github.rs ─▶ GitHub
                                   └──────────────────────────────┘
```

## 2. Ports

All ports are in `src/ports.rs`. A port gets a method only in the phase that uses it.

| Port | Side | Methods (final form) | Adapter |
|---|---|---|---|
| `Assistant` | Driving | `info`, `listen` (gives a `Listening` session: `push`, `finish`), `handle`, `speak` | Implemented by `App` in `src/lib.rs`. Called by `adapters/wyoming.rs`. |
| `Review` | Driving | `list`, `audio`, `mark`, `intents`, `proposals`, `resolve` | Implemented by `app/review.rs`. Called by `adapters/web.rs`. |
| `Home` | Driven | `snapshot`, `events`, `call`, `announce`, `ask` | `adapters/ha.rs` |
| `SpeechToText` | Driven | `stream` (first pass: gives `accept`, `partial`, `reset`), `transcribe` (second pass) | `adapters/sherpa.rs` |
| `TextToSpeech` | Driven | `synth` | `adapters/supertonic.rs` |
| `VoiceEmbedder` | Driven | `embed` | `adapters/sherpa.rs` |
| `VoiceProfiles` | Driven | `enroll`, `similarities` | `adapters/sherpa.rs` |
| `Classifier` | Driven | `predict` | `adapters/onnx.rs` |
| `ModelLoader` | Driven | `load` (gives a `Classifier` for a model folder) | `adapters/onnx.rs` |
| `Trainer` | Driven | `train` (gives a model folder for a training set) | `adapters/process.rs` (local GPU) and `adapters/runpod.rs` (fallback) |
| `Reasoner` | Driven | `warm`, `answer` (gives steps: sentence, call, plugin, ask, done), `generate`, `paraphrase`, `embed`, `propose` | `adapters/openai.rs` |
| `Plugin` | Driven | `tool`, `fetch` | One adapter for each remote API: `adapters/open_meteo.rs` (weather), `adapters/plex.rs` (new shows) |
| `InteractionLog` | Driven | `append`, `update`, `list`, `audio`, `mark`, `marked`, `deferred`, `prune` | `adapters/sqlite.rs` |
| `Store<T>` | Driven | `load`, `save` | `adapters/files.rs` (TOML or JSON from the file extension) |
| `Clock` | Driven | `now` | `SystemClock` in `src/lib.rs` |
| `Models` | Driven | `release`, `restore` | `LiveModels` in `src/lib.rs` |
| `Issues` | Driven | `open` | `adapters/github.rs` |

Tests in the application layer use the fakes in `src/testing/fakes.rs`. Adapter tests use local servers (`wiremock`, a fake HA WebSocket server), in-memory object stores or real models.

## 3. System One Model

The System One model is one ONNX model with two heads: intent classification and slot tagging.

Input: two text segments.

- Segment A is the context. `context_text` makes it from the satellite area. No sub-phase gives habit lines, so the `habits` part is always `none` (decision 9).
- Segment B is the normalized transcript (sub-phase 3.1).

```
User: "Turn on the lights"
Context: "area: living room | habits: none"
```

Output:

```json
{
  "intents": { "lights_on": 0.89, "fan_on": 0.03, "close_garage": 0.01, "unknown": 0.02 },
  "slots": []
}
```

For "Tell me a joke" before Jarvis learns `tell_joke`:

```json
{ "intents": { "lights_on": 0.01, "unknown": 0.76 }, "slots": [] }
```

Rules:

1. The ONNX outputs are `intent_logits` (one for each label) and `slot_logits` (one for each token and tag).
2. Slot tags use the BIO scheme (`O`, `B-area`, `I-area`, ...). Only segment B tokens get tags.
3. `labels.json` in the model folder lists the labels and the tags in output order.
4. When the top label is `unknown`, the top intent probability is less than `model.threshold`, or System One cannot act on the top intent, the request goes to System Two.
5. The base model is `train.base_model`. The default is `sentence-transformers/all-MiniLM-L12-v2`, which has `model.safetensors`. The pinned `transformers` does not load a `pytorch_model.bin` with the pinned `torch` (CVE-2025-32434). It predicts in less than 15 ms on the CPU and in less than 5 ms on the GPU host.
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
2. **Speculative prediction.** The speech use case runs the System One model on each changed partial transcript. The audio stream has no satellite ID, so this prediction uses an empty context. When the top intent is not `unknown`, its probability is `model.commit_threshold` or more, and no `media`, `movie` or `show` slot value ends the transcript, Jarvis uses a short silence threshold (`early_commit_ms`). Other transcripts use a longer threshold (`endpoint_ms`). Such a name can continue, for example "play hip" of "play hip hop" (sub-phase 6.8). Silence is the time since the voice end, which a Silero VAD in the first-pass adapter finds. On the GPU host, `vad_threshold = 0.65` gives a lower latency than 0.5. 0.7 cut quiet word endings ("office lights on" gave "Office lights"), so 0.65 is the highest tested value that keeps them (sub-phase 9.2). In a VAD speech segment, the silence is 0, so the wait of the VAD for the segment end cannot commit early. After the segment end, the silence counts each accepted sample (sub-phase 9.1b). Thus decoder delay does not add to the wait, and silence before speech does not commit.
3. **Reply first, act after.** Jarvis returns `Handled` before the HA service call completes. The service call runs in the background.
4. **Pre-rendered audio.** Jarvis synthesizes at startup each pool response whose only variable is `area`. A TTS request for such a response is a memory read. Other responses synthesize at request time and are not cached.
5. **Filler, then announce.** For external data and for System Two, Jarvis returns a filler in `Handled`. Then Jarvis speaks the answer with `assist_satellite.announce` on the same satellite. When an announce or a question starts, HA cancels the running pipeline of the satellite. HA returns the result only after the satellite plays the audio. (Source: `homeassistant/components/assist_satellite/entity.py`, `homeassistant/components/esphome/assist_satellite.py`.) Thus each satellite has one queue of announces and questions. An item starts when the hot cache shows the satellite entity `idle`, which is after the filler ends. A new request on a satellite stops the older deferred jobs of that satellite. HA caches TTS audio by text, so the time of a `synthesize` event does not show when the filler plays.
6. **Area from context.** HA sends `device_id` and `satellite_id` in the `Transcript` context. Jarvis finds the area of the device in the hot cache. (Source: `homeassistant/components/wyoming/conversation.py`.)
7. **One templating engine.** `minijinja` renders the response text and the service call data. All pools and intents use the same syntax.
8. **Time is an input.** Domain functions receive the current time as an argument. The application gets the time from the `Clock` port. Tests use `FakeClock`.
9. **Request habits, not device habits.** Jarvis learns when people ask for what. Each spoken request counts in its bucket: the day type and the hour (phase 11). HA state changes and automations do not count, because native HA automations handle device schedules. Thus habits are not a model input, and they do not select entities or slot values. Request habits move work from System Two (slow and expensive) to System One (fast and cheap): before a usual time, Jarvis prepares the answer of the request (phase 14).
10. **A habit fades.** Each day counts once for each request and bucket. The score halves each `habits.half_life_days`. A bucket is a usual time while its score is `habits.min_score` or more. With 14 and 2.0, a request on three days within 14 days is a habit on the day of the third request. Requests on two days are never a habit.
11. **One joint model.** One model gives the intent probabilities and the slot tags. Thus one CPU pass per transcript gives all System One data. Clusters of the requests that System Two answers use OpenAI embeddings (sub-phase 14.1). These do not change when System One retrains, and the context does not change them.
12. **Train locally first, with the full GPU.** The process adapter runs `train/train.py` on the GPU of the GPU host. A local run and live Jarvis never share the GPU. Jarvis does not announce a run. Before a local run, Jarvis mutes the microphones of the satellites that are not muted, stops answering and releases its model sessions. After the run, with success or an error, Jarvis loads them again, unmutes only the microphones that it muted, and answers again. Then it measures the new model (decision 13). When the local run fails (for example, CUDA out of memory), the training use case sends the same training set to the RunPod adapter. A RunPod run does not pause Jarvis or mute the satellites. Jarvis predicts with the ONNX model through `ort`. The Python script and the inline script of the review page are the only non-Rust code. The training set file is the contract between them.
13. **Accept only an equal or better model.** Jarvis assigns each example to the held-out set by a stable hash of its text. Jarvis measures the old model and the new model on the same held-out set. Jarvis keeps the new model only when its intent accuracy and its slot accuracy are both equal or higher.
14. **Training trigger.** Jarvis counts changes since the last training run. At `train.min_changes`, Jarvis starts a run. A voice request starts a run at any count. Only one run occurs at a time. After each run, accepted or rejected, the count starts again from 0.
15. **Speaker identification is not on the critical path.** Jarvis calculates the voice embedding in the background after commit. Only intents with `needs_speaker = true` wait for the result.
16. **Passive voice enrollment.** When presence or proximity identifies the speaker with high confidence, Jarvis adds the voice embedding to the profile of that person. No enrollment step is necessary.
17. **Ask through `start_conversation`.** The HA Wyoming conversation agent does not set `continue_conversation`. Thus `Handled` cannot open the microphone again. To ask a question, Jarvis returns `Handled` with empty text. Then, when the satellite is idle (decision 5), Jarvis calls `assist_satellite.start_conversation` with the question and `preannounce = false`. The satellite speaks the question and listens. (Source: `homeassistant/components/wyoming/conversation.py`.)
18. **Dialogs by satellite.** `start_conversation` starts a new pipeline run with a new `conversation_id`. Thus Jarvis keys each dialog by `satellite_id`. When the context has no `satellite_id` (for example, text from the HA UI), Jarvis keys the dialog by `conversation_id` and puts the question in `Handled`. A dialog closes on completion, on cancel, after `dialog.max_turns` questions, or `dialog.timeout_s` after the last question.
19. **System Two asks with a tool.** The OpenAI request has an `ask_user` tool. When the model calls it, Jarvis asks the question and keeps the dialog open. The next turn sends the answer as the `function_call_output` of that call, with `previous_response_id`.
20. **System Two teaches System One.** Jarvis clusters the OpenAI embeddings of recent requests that System Two answered: `unknown` requests, requests under `model.threshold` and requests that System One cannot act on. For each large cluster, System Two writes a proposal for a new intent: name, kind, examples, responses and an optional service. Jarvis validates it and saves it to `learned.toml`. The next training run adds it to the model, so System One handles these requests.
21. **Device control needs approval.** A proposal with a `service` waits for your approval on the review page. Other proposals become active immediately.
22. **Prefetch has no side effects.** A prefetch answer must not change the house. A `Call` step during a prefetch runs only when its service is in `learn.prefetch_services` (read-only services such as `calendar.get_events`). Any other `Call` discards that prefetch. A `Plugin` step always runs, because a plugin only reads data (decision 30). A `States` step also always runs, because it only reads the hot cache (sub-phase 7.6).
23. **Discovery from HA.** Jarvis reads the HA registries in the hot cache. Each `person` entity lists its `device_trackers`. The sensors on the same device as a tracker give its area and distance (for example from Bermuda with the satellites as Bluetooth proxies). The `binary_sensor` entities with device class `occupancy`, `presence` or `motion` on the satellite device are its presence sensors. Configuration can override discovery. In the reference house (section 11) the presence sensors have no device class. Set their device class to `occupancy` in HA, or list them in `[[satellites]]`.
24. **Audio correlation.** HA sends the audio (`asr`) and the transcript (`handle`) on different connections with no shared ID. The speech use case keeps each committed utterance (text, audio, time) for `log.correlate_s`. For a voice request (a context with a `satellite_id`), the conversation use case finds the one utterance with the same normalized text and removes it. An ambiguous match gives no audio. Speaker identification uses the same correlation.
25. **Logging is not on the critical path.** Jarvis writes the interaction after it returns `Handled`. Later announces and questions update the same record.
26. **Feedback rules.** A bad action blocks that decision (intent and targets) for that normalized text at once. `decide` then uses the next entity candidate, then the next intent with enough probability. Else it sends the request to System Two with the bad action as a correction. A bad response leaves its pool. The next training run learns from all marks and corrections.
27. **One ONNX Runtime, GPU or CPU.** `sherpa-onnx` (feature `shared`) and `ort` (feature `load-dynamic`) load the same ONNX Runtime 1.28.2 shared library from sherpa-onnx 2.0.1. On the GPU host, it is the CUDA 12 and cuDNN 9 build from the sherpa-onnx `linux-x64-gpu` release. On other machines, it is the CPU build. Thus the two crates cannot conflict, and `device` selects the CUDA or the CPU execution provider for all models.
28. **Two-pass STT.** The first pass streams, so Jarvis knows the words while the user speaks. It uses NVIDIA Nemotron Speech Streaming EN 0.6B with 160 ms chunks on the GPU host. The first pass gives the last word only after the VAD ends the segment and flushes the model (sub-phase 6.6c). The VAD ends the segment `vad_min_silence_ms` after the voice end, and an early commit comes `early_commit_ms` after the voice end (sub-phase 9.1b). `vad_min_silence_ms < early_commit_ms` (sub-phase 2.2), so the flush comes before an early commit. At commit, the second pass decodes the whole utterance with NVIDIA Parakeet Unified EN 0.6B in offline mode. The second pass has full context, so it is more accurate. The second pass adds zeros to each utterance up to 4 s, or up to `max_utterance_ms` for a longer utterance, so each decode has one of two input lengths (sub-phase 6.7). On the Open ASR Leaderboard sets, the average word error rate goes from 7.67% to 5.91%, and on LibriSpeech test-clean from 2.56% to 1.63%. HA receives the second-pass text on the GPU host. Both models are official sherpa-onnx exports. The GPU host runs fp16 conversions of them (sub-phase 6.6c). Cloud streaming APIs take about 250 ms to 300 ms to give the final text, so they cannot meet G1. CPU machines use the same first pass and have no second pass.
29. **Blocking work off the async workers.** STT decoding, the second pass, `predict`, TTS synthesis, voice embedding and SQLite calls run in `tokio::task::spawn_blocking`. The async workers only move bytes and wait. Thus a decode for one satellite does not delay `handle` for another.
30. **Remote data through plugins.** Data from a remote server, for example a weather forecast, reaches Jarvis only through a `Plugin` adapter. Jarvis code builds the request and parses the reply into a typed result. Thus the result is the same for the same reply, and no model sees a raw API call. System Two gets each plugin as its own tool, for example `get_weather(when)`. A System One fetch intent names a plugin. The plugin registry keeps each result for `plugins.cache_s`. A plugin only reads data, so it has no `allowed_domains` check. System Two still calls HA services directly, within `openai.allowed_domains` (sub-phase 7.5). The weather plugin uses Open-Meteo, which needs no API key, with the location of the HA configuration. The Plex plugin reads the recently added items and the On Deck list of the Plex server with a token (phase 18). A new plugin uses a free data source without an API key when one exists. A source with a key needs your approval first, and the PR of the plugin then names the environment variable and the sign-up page.
31. **Fixed TTS input lengths.** ONNX Runtime keeps the cuDNN state of a `Conv` node only for its last input shape, so each new reply length added about 230 ms to the TTS on the GPU host. Jarvis runs the four Supertonic-3 graphs itself through `ort`, with the fp32 vector estimator and vocoder. It adds zeros and masks to each input up to one of two lengths: 64 text ids and 64 latent frames, or 160 text ids and 192 latent frames. Each length has its own set of sessions, so a change of length builds no new plans. A longer input keeps its length (sub-phases 9.3 and 9.3b).
32. **Plugin requests through GitHub.** When System Two finds that a cluster needs remote data that no plugin gives, Jarvis does not guess an answer source. It opens one GitHub issue for the cluster through the `Issues` port: the example requests, the usual times and the data needed (sub-phase 14.13). Claude writes a normal code PR with the plugin, the fetch intent and its `fresh_min`, and the tests. CI, the Codex review and your review gate it as any other PR, and the next release puts it on the GPU host. The issue has the label `github.label` and the assignee `github.assignee`. The app that starts Claude on such an issue is a setting of the repository (field log of sub-phase 14.13).

## 6. Latency Budget (System One)

| Step | Budget |
|---|---|
| From the VAD voice end to the early commit: `early_commit_ms` (100 ms) of silence | 200 ms |
| Second pass of an utterance with zeros up to 4 s (GPU host). | 40 ms |
| Wyoming `Transcript` → `Handled` (model prediction 10 ms, decide, select) | 20 ms |
| Wyoming `Synthesize` → first audio chunk (cache hit) | 10 ms |
| Margin | 30 ms |
| **Total response latency** | **300 ms** |

The budget is the response latency (`00-overview.md` section 0.3). HA's own time between the events, and HA to satellite audio delivery, are outside Jarvis. The Phase 9 field log records the end-to-end time on real hardware.

The budget applies to the GPU host. The `gpu-test` job runs the latency tests there (section 10).

With `vad_threshold = 0.65`, the GPU host measures a p95 of 213 ms for the fan commands and 353 ms to 378 ms for the light commands. Thus AC-9.2-5 gives the light commands 400 ms. A response that is not in the audio cache waits for the synthesis. With sherpa-onnx, the TTS alone took about 315 ms to the first audio chunk, and the p95 was 654 ms, so AC-9.2-4 gave it 700 ms. With fixed input lengths (decision 31), the TTS of a reply in `responses.toml` has a p95 of 53.0 ms on the GPU host. AC-9.3c-1 gives an uncached response 400 ms, the target of the light commands. The `unknown` filler measures 697 ms, so AC-9.2-3 gives it `endpoint_ms` + 150 ms.

## 7. Crates

| Crate | Layer | Use |
|---|---|---|
| `minijinja` | Domain | Response and service data templates. |
| `rand` | Domain | Response selection and context samples. |
| `jiff` | Domain | Time zones, buckets and calendar dates. |
| `text2num` | Domain | Number words to digits in the template filter `number` (sub-phase 18.9). |
| `strsim` | Domain, adapter tests | Fuzzy match of a spoken answer to a list of choices. Word edit distance for the second-pass word error rate (AC-6.7-3). |
| `dashmap` | Application | Concurrent audio cache. |
| `ndarray`, `linfa`, `linfa-clustering` | Domain | DBSCAN clusters of the embeddings of System Two requests. |
| `xxhash-rust` | Domain | Stable hash for the held-out split. |
| `vstd` | Domain | Verus: `#[verus_spec]`, `#[verus_verify]`, `verus!`, spec types and the `vstd` specifications of `std`. A normal build erases the proof code. |
| `serde`, `serde_json` | All | Data types and messages. |
| `tokio` | Application, adapters | Async runtime, TCP, timers, channels. |
| `arc-swap` | Application, root | Atomic swap of the System One model. |
| `tokio-util` | Adapter, root | `TaskTracker`: the connection tasks and the `after` work that a shutdown waits for. |
| `anyhow` | All except domain | Error propagation. Domain functions return typed errors. |
| `tracing`, `tracing-subscriber` | Application, adapters, root | Logs and latency spans. |
| `futures` | Ports, application, adapters | `Stream` for events, partial transcripts and sentences. |
| `async-trait` | Ports, application, adapters, root | Async methods on port traits behind `Arc<dyn Port>`. |
| `tokio-tungstenite`, `futures-util` | Adapter | HA WebSocket API. |
| `sherpa-onnx` (`=2.0.1`, feature `shared`) | Adapter | Streaming STT (`OnlineRecognizer`), voice identification (`SpeakerEmbeddingExtractor`, `SpeakerEmbeddingManager`). CUDA or CPU provider. |
| `text-processing-rs` | Adapter | Numbers as words in the second-pass text (`tn_normalize_sentence`). |
| `ort` (features `load-dynamic`, `cuda`), `tokenizers` | Adapter | System One model prediction and the Supertonic-3 TTS graphs with ONNX Runtime (CUDA or CPU execution provider). Tokenizer from `tokenizer.json`. |
| `rand_distr` | Adapter | Seeded normal noise of the Supertonic-3 latent (`StandardNormal`). |
| `unicode-normalization` | Adapter | NFKD of the TTS text before the Supertonic-3 indexer. |
| `reqwest` | Adapter | RunPod serverless REST API. Open-Meteo forecast API (features `json`, `query`). Plex server API. GitHub REST API. |
| `object_store` (feature `aws`) | Adapter | RunPod network volume through its S3-compatible API. |
| `async-openai` | Adapter | OpenAI Responses API: streaming, tools, structured output, `previous_response_id`. |
| `rusqlite` (feature `bundled`) | Adapter | Interaction log database. |
| `axum` | Adapter | Review web page and API. |
| `hound` | Adapter | WAV encode for audio playback. |
| `toml`, `tempfile` | Adapter, root | File stores with atomic write. Configuration. |
| `wiremock` (dev) | Adapter tests | Mock OpenAI, RunPod, Open-Meteo, Plex and GitHub servers. |
| `rubato` (dev) | Test | Resample TTS audio from 44.1 kHz to 16 kHz in `speech(text)`. |
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
| `syn`, `toml` | `xtask` | Parse the Rust source and the dependency names of `Cargo.toml` for the module graph. |
| `clap` | `xtask`, root | Command lines of `xtask` and `jarvis`. |
| Quint CLI (`@informalsystems/quint`, tool) | — | Type check, test, simulate and model check the Quint models. |
| Quint Rust evaluator (tool, through `quint run` and `quint test`) | — | Simulation and `run` tests. |
| Apalache (tool, through `quint verify`, needs Java 21 as `java` on `PATH`) | — | Bounded model checking of the Quint invariants. |
| Verus (tool, with `cargo-verus` and Z3) | — | Proves the Verus proofs of the domain (`cargo verus verify`). Its release sets the Rust version of `rust-toolchain.toml`. |

Python packages for `train/` (pinned in `train/pyproject.toml`):

- Runtime: `torch`, `transformers`, `onnx`, `onnxruntime`, `runpod`.
- Development (`dev` extra): `pytest`, `pytest-cov`, `ruff`, `mypy`, `vulture`, `interrogate`.

Jarvis does not use the `wyoming` crate. Version 0.2.1 turns on the `serde_json` feature `arbitrary_precision` for the whole build, and then a template no longer reads an HA number as a number (AC-4.5d-1). `adapters/wyoming.rs` reads the event type as a string and defines the data of each event that Jarvis uses with `serde`.

`ort` and `sherpa-onnx` load one shared ONNX Runtime library (decision 27). One library folder holds `libonnxruntime` and the sherpa-onnx libraries: the CPU build from `just models`, or the CUDA build from `just cuda-libraries`, which `just gpu-models` runs on the GPU host and the Linux release job runs alone. Every build sets `SHERPA_ONNX_LIB_DIR` to this folder, so no build downloads. At run time, `ORT_DYLIB_PATH` points to the `libonnxruntime` in the folder, and `LD_LIBRARY_PATH` (`DYLD_LIBRARY_PATH` on macOS) points to the folder. The recipes `test`, `cov` and `gpu-test`, CI and the environment file of the `jarvis` unit set these variables. Sub-phase 6.1 adds the folder. Sub-phase 6.2 proves that both crates work in one process.

## 8. Files

Jarvis uses these files. No other files are permitted without a change to this section. `src/lib.rs` declares the folders as inline public modules (for example `pub mod domain { pub mod intent; }`). Thus no `mod.rs` files are necessary. Jarvis is a library with a thin binary. Thus `benches/` can import the domain, and a `pub` item that only a later sub-phase uses is not dead code.

| File | Layer | Responsibility | Created in |
|---|---|---|---|
| `CLAUDE.md` | — | Working rules for code changes. | — |
| `README.md` | — | Project overview, a table of the current capabilities and a quickstart. Each PR that adds or changes a capability or a setup step updates it. | — |
| `docs/spec/*.md` | — | This specification. | — |
| `docs/architecture.md` | — | The full architecture diagram. The pre-commit hook of the PR author writes it (sub-phase 0.11). | 0.9 |
| `.gitignore` | — | Ignore `target/`, `models/`, `.venv/`, `jarvis.local.toml`, the data files, and the Python caches and coverage data (`__pycache__/`, `train/.coverage`). | 0.1 |
| `rust-toolchain.toml` | — | The fixed Rust version and its components. The version is the one that the pinned Verus release requires (sub-phase 0.18). | 0.1 |
| `Cargo.toml` | — | Workspace (`jarvis`, `xtask`), crates, lint levels and the Verus settings. | 0.1 |
| `Cargo.lock` | — | Locked crate versions. | 0.1 |
| `clippy.toml` | — | Clippy thresholds. | 0.2 |
| `justfile` | — | All developer commands. | 0.1 |
| `.pre-commit-config.yaml` | — | The pre-commit hook. `setup` writes the pre-push guard to `.git/hooks/pre-push` (sub-phase 0.17). | 0.1 |
| `.github/workflows/ci.yml` | — | The required checks for each push and PR. | 0.1 |
| `.github/pull_request_template.md` | — | The PR description template. | 0.5 |
| `.github/actionlint.yaml` | — | The `actionlint` configuration: the label `gpu` of the self-hosted runner. | 0.13 |
| `.github/workflows/release.yml` | — | Release binaries and the training image for each `v*` tag. | 0.14 |
| `xtask/Cargo.toml` | Tools | The `xtask` crate. | 0.1 |
| `xtask/src/main.rs` | Tools | Command line: `complexity`, `pr-size`, `rules`, `diagram`, `merge`, `guard-push`. | 0.1 |
| `xtask/src/gates.rs` | Tools | `#[cfg(test)]` only. Gate tests (real tools on fixtures) and structure tests (hook, `justfile`, workflows, PR template, tool versions, the file list of this section, the current diagram, the Verus attributes and the trusted code of the domain). | 0.1 |
| `xtask/src/complexity.rs` | Tools | Reads `rust-code-analysis-cli` JSON and applies the limits. | 0.3 |
| `xtask/src/pr_size.rs` | Tools | Counts the changed lines of a PR without the excluded files. | 0.6 |
| `xtask/src/graph.rs` | Tools | Builds the module graph from the source with `syn`. | 0.7 |
| `xtask/src/rules.rs` | Tools | Checks the layer and modularity rules on the module graph. | 0.8 |
| `xtask/src/mermaid.rs` | Tools | Draws the module graph in Mermaid, with change colors. | 0.9 |
| `xtask/src/merge.rs` | Tools | The merge rules, the merge command and the pre-push guard. | 0.15 |
| `benches/domain.rs` | Test | Gungraun benchmarks with absolute instruction budgets. | 2.8c |
| `jarvis.toml` | — | Runtime configuration. | 1.1 |
| `sherpa-cuda.config` | — | The CUDA provider options of the STT sessions: `arena_extend_strategy=kSameAsRequested`. | 6.6c |
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
| `quint/training.qnt` | Model | Training trigger, run, pause, accept and fallback. | 13.4 |
| `quint/learned.qnt` | Model | Learned intent lifecycle, approval and plugin request issues. | 14.3 |
| `quint/prefetch.qnt` | Model | Prefetch window, refresh, freshness and side effects. | 14.8 |
| `quint/speaker.qnt` | Model | Speaker identification and enrollment. | 15.5 |
| `quint/jarvis.qnt` | Model | The system model. It composes the other models. | 16.1 |
| `src/lib.rs` | Root | Module tree, `Config`, `SystemClock`, `LiveModels`, `App` (implements `Assistant` by delegation), `run`, the `dataset` command, `main`. | 0.1 |
| `src/main.rs` | Root | Binary entry point. `main` only calls `jarvis::main`. | 0.1 |
| `src/ports.rs` | Ports | All port traits and their data types. | 2.1 |
| `src/domain/contracts.rs` | Domain | Trusted Verus contracts and axioms for the `std` and domain crate functions that the domain calls. | 0.18 |
| `src/domain/state.rs` | Domain | Home data: snapshot, states, registries, events and service calls. | 2.1 |
| `src/domain/state_cache.rs` | Domain | `StateCache`: load, apply and queries. | — |
| `src/domain/intent.rs` | Domain | Normalize and example markup. | 3.1 |
| `src/domain/catalog.rs` | Domain | Intent catalog types and validation, labels, tag list. | 3.2 |
| `src/domain/training.rs` | Domain | Training set, context samples, held-out split, accuracy, accept rule, change count. | 3.3 |
| `src/domain/model.rs` | Domain | Softmax, BIO decode, prediction ranking. | 3.11 |
| `src/domain/responses.rs` | Domain | Pools: select, render, record use, backfill policy, validation. | 4.1 |
| `src/domain/select.rs` | Domain | Entity selection for an intent and an area. | 4.4 |
| `src/domain/decide.rs` | Domain | `decide`: prediction and context to `Decision`. | 4.5 |
| `src/domain/speech.rs` | Domain | Audio type, commit policy, audio chunks, sentence split, audio correlation. | 6.2 |
| `src/domain/dialog.rs` | Domain | Dialog state, answer fill, cancel, turn limit. | 10.2 |
| `src/domain/habits.rs` | Domain | Request habits: buckets, decayed day scores, usual times. | 11.1 |
| `src/domain/feedback.rs` | Domain | Marks, corrections, feedback rules. | 12.3 |
| `src/domain/learn.rs` | Domain | Clusters, proposal validation, approval rule, prefetch schedule and freshness. | 14.1 |
| `src/domain/speaker.rs` | Domain | Discovery, presence, proximity and identification. | 15.1 |
| `src/app/background.rs` | Application | Hot cache sync, request habit store, backfill, learned intents, prefetch. | 2.9 |
| `src/app/conversation.rs` | Application | `handle` use case, dispatch, deferred jobs, dialog map. | 4.8 |
| `src/app/speech.rs` | Application | `listen` and `speak` use cases, audio cache, pre-render. | 6.3b |
| `src/app/review.rs` | Application | `Review` use case. | 12.7 |
| `src/app/training.rs` | Application | Generated examples, training trigger, training run, accept and swap. | 13.6 |
| `src/app/plugins.rs` | Application | Plugin registry: lookup by tool name, result cache and deadline. | 17.1 |
| `src/adapters/ha.rs` | Adapter | `Home` over the HA WebSocket API. | 2.2 |
| `src/adapters/onnx.rs` | Adapter | `Classifier` and `ModelLoader` with ONNX Runtime. | 3.12 |
| `src/adapters/files.rs` | Adapter | `Store<T>` with TOML or JSON files and atomic write. | 4.3 |
| `src/adapters/wyoming.rs` | Driving adapter | Wyoming TCP server. It calls `Assistant`. | 5.1 |
| `src/adapters/sherpa.rs` | Adapter | `SpeechToText` (first and second pass), `VoiceEmbedder`, `VoiceProfiles` with sherpa-onnx. | 6.2 |
| `src/adapters/supertonic.rs` | Adapter | `TextToSpeech` with the Supertonic-3 graphs in ONNX Runtime and fixed input lengths. | 9.3 |
| `src/adapters/openai.rs` | Adapter | `Reasoner` with OpenAI. Prompt builders. | 7.1 |
| `src/adapters/sqlite.rs` | Adapter | `InteractionLog` with SQLite. | 12.4 |
| `src/adapters/web.rs` | Driving adapter | Review web server with token auth. It calls `Review`. | 12.8 |
| `src/adapters/review.html` | Driving adapter | The review page. `web.rs` embeds it with `include_str!`. | 12.9 |
| `src/adapters/process.rs` | Adapter | `Trainer` that runs `train/train.py` as a local process. | 13.7 |
| `src/adapters/runpod.rs` | Adapter | `Trainer` with the RunPod REST API and network volume (fallback). | 13.8b |
| `src/adapters/open_meteo.rs` | Adapter | `Plugin` `get_weather` with the Open-Meteo forecast API. | 17.3 |
| `src/adapters/plex.rs` | Adapter | `Plugin` `get_new_shows` with the Plex server API. | 18.11 |
| `src/adapters/github.rs` | Adapter | `Issues` with the GitHub REST API. | 14.12 |
| `src/testing/house.json` | Test | The reference house snapshot (section 11). | 2.1 |
| `src/testing/fakes.rs` | Test | `#[cfg(test)]` only. A fake for each port, `FakeWyomingClient`, `speech(text)`. | 2.9 |
| `src/testing/e2e.rs` | Test | `#[cfg(test)]` only. Pipeline harness and end-to-end latency tests. | 9.1 |

Jarvis also writes these data files: `habits.json`, `voices.json`, `jarvis.db`, `generated.json`, `training.json`, `pools.json` (use counts and generated responses), `learned.toml` (learned intents) and the model folders in `models/system-one/`. They are not source files, and Git ignores them. Edit `learned.toml` only while Jarvis is stopped. `jarvis.local.toml` is the configuration of a machine other than the GPU host (section 9). Git ignores it too.

Unit tests are in a `#[cfg(test)] mod tests` block at the end of each source file. The conformance driver for a Quint model is in the test block of the module that it checks. Shared test code is in `src/testing/` only. The fake HA WebSocket server is in the test block of `src/adapters/ha.rs`. `src/testing/house.json` is a data file: tests embed it with `include_str!`, and the `dataset` command reads it from its `--house` path.

## 9. Configuration Files

`jarvis.toml` is the configuration of the GPU host. Another machine copies it to `jarvis.local.toml`, sets `device = "cpu"`, names the int8 files, removes `second_pass_dir`, sets `early_commit_ms = 250`, and runs `jarvis serve jarvis.local.toml`. Git ignores `jarvis.local.toml`. Without a second pass, the transcript is the first-pass text. The value 250 ms comes from sub-phase 6.6c: with `vad_min_silence_ms = 100`, the VAD ended the segment and flushed the last word about 203 ms after the last voiced sample, so 250 ms let the flush finish before an early commit. The value needs a new measurement with the defaults of sub-phase 9.1b.

`jarvis.toml`:

```toml
listen = "0.0.0.0:10700"
device = "cuda"
intents = "intents.toml"
responses = "responses.toml"
pools = "pools.json"

[ha]
url = "ws://homeassistant.local:8123/api/websocket"
token_env = "HA_TOKEN"
request_timeout_ms = 30000  # music_assistant.play_media answers after its search (sub-phase 18.7)
announce_timeout_s = 60     # announce and ask: HA answers after the audio plays
ping_interval_s = 30

[model]
dir = "models/system-one"
threshold = 0.6
commit_threshold = 0.9
next_threshold = 0.2

[asr]
model_dir = "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25"
# The fp16 files of both passes. CPU machines name `encoder.int8.onnx`, `decoder.int8.onnx` and `joiner.int8.onnx`.
encoder = "encoder.fp16.onnx"
decoder = "decoder.fp16.onnx"
joiner = "joiner.fp16.onnx"
# Optional. Without it, the final transcript is the first-pass text (CPU machines).
second_pass_dir = "models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming"
vad_model = "models/silero_vad.onnx"
vad_threshold = 0.65
vad_min_silence_ms = 60
early_commit_ms = 100
endpoint_ms = 600
max_utterance_ms = 8000

[tts]
model_dir = "models/sherpa-onnx-supertonic-3-tts-int8-2026-05-11"
voice = 6     # speaker ID of the Supertonic voice style
num_steps = 4 # denoising steps

[openai]
api_key_env = "OPENAI_API_KEY"
model = "gpt-5.4-mini"
timeout_ms = 8000
warm_idle_s = 60
max_output_tokens = 150      # answers
bulk_max_output_tokens = 8000 # structured output: generate, paraphrase, propose
max_tool_calls = 5
embedding_model = "text-embedding-3-small"
allowed_domains = ["light", "switch", "fan", "media_player", "climate", "calendar", "todo"]
read_services = ["calendar.get_events", "todo.get_items"] # called with return_response = true

[plugins]
cache_s = 300      # a forecast changes slowly
timeout_ms = 5000

[plugins.weather]
url = "https://api.open-meteo.com/v1/forecast"

# Optional. Without this section, Jarvis has no Plex plugin.
[plugins.plex]
url = "http://plex.local:32400"
token_env = "PLEX_TOKEN"

[backfill]
min_fresh = 3
batch = 5
max_chars = 80
max_pool = 30

[announce]
margin_ms = 300
idle_wait_s = 30

[dialog]
max_turns = 3
timeout_s = 15
choice_similarity = 0.85
cancel = ["never mind", "cancel", "forget it", "stop"]

[habits]
file = "habits.json"
half_life_days = 14
min_score = 2.0 # three days within 14 days (decision 10)
save_interval_s = 300

[train]
state = "training.json"
generated = "generated.json"
base_model = "sentence-transformers/all-MiniLM-L12-v2"
epochs = 8
batch_size = 8
min_changes = 20
check_interval_s = 600
holdout = 0.1
failure_backoff_s = 21600
timeout_min = 60
max_vram_gb = 4.5 # PyTorch cap of a local run (section 10)
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
prefetch_services = ["calendar.get_events"]

# Optional. Without this section, Jarvis opens no plugin requests (sub-phase 14.13).
[github]
url = "https://api.github.com"
repo = "louislang/ha-jarvis"
token_env = "JARVIS_GITHUB_TOKEN" # fine-grained, issues of `repo` only
label = "claude"
assignee = "louislang" # the login that the Claude trigger watches (field log of sub-phase 14.13)
timeout_ms = 10000

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
plugin = "get_weather"
data = { when = "today" }
fresh_min = 30

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

- `examples` are seed examples. `[value](slot)` marks a slot value. Each intent has `min_examples` examples or more. The text of each example is normalized (sub-phase 3.1): lowercase, with no punctuation.
- `unknown_examples` are seed examples for the `unknown` label.
- `kind = "command"`: Jarvis replies from the pool and calls the service in the background. A command with no `service` only replies. Static intents use this form.
- `kind = "query"`: Jarvis replies with data from the hot cache. No remote call.
- `kind = "fetch"`: Jarvis replies with a filler. Then it fetches the result of the `plugin` of the intent, with the rendered `data` as the arguments, and announces the answer (decision 30). A fetch needs no area. When a fresh prefetched answer has the same call, Jarvis replies with it at once (sub-phase 14.10b).
- `kind = "prefetch"`: Jarvis replies with the prefetched answer when it is fresh. Else Jarvis treats the request as System Two. Only learned intents use this kind.
- `kind = "train"`: Jarvis replies from the pool and starts a training run (sub-phase 13.11).
- `select` is a list of entity filters. A filter has a `domain`, an optional `name` and an optional `device_class`. `name` matches whole words of the entity name or an alias, without case sensitivity (`fan` matches "Office Fan", not "Infant Monitor"). An entity matches when one filter matches. `select` skips an entity that has an entity category, that is hidden or disabled, or that is on a satellite device (a device with an `assist_satellite` entity) and is not a `media_player`.
- `exclude` is a list of entity IDs that `select` never matches. Use it for a sensor that gives bad values.
- `plugin` is the tool name of the plugin of a fetch intent, for example `get_weather`.
- `fresh_min` is how long the answer of a fetch or prefetch intent stays fresh, in minutes, for example 5 for traffic and 30 for the weather. Each fetch and prefetch intent has it, and no other kind has it. Before a usual time of the intent (phase 11), Jarvis prefetches the answer. Then it refreshes the answer each `fresh_min` until the window ends (sub-phases 14.9 to 14.10b).
- `search = true` makes Jarvis search Music Assistant for the `media_id` of the call and play the best match (sub-phase 18.13b). An artist match starts with one track of the artist (sub-phase 18.14). Only `play_music` has it.
- The area is the area slot. If no slot, it is the satellite area. The area slot matches an area name or alias without case sensitivity. For a command, an area slot that matches no area can name a Music Assistant speaker group (sub-phase 18.15).
- `target = "entities"` uses all matching entities in the area.
- `target = "entity"` uses `entity`, or the matching entity in the area with the lowest entity ID. `target` is `entity` when it is absent.
- `target = "house"` uses all matching entities of the house, in any area. A query with it needs no area.
- A list query is a query with `target = "entities"` or `target = "house"`. `state` is the state that it keeps, for example `on`. Its pool gets `entities`: the state, the attributes and the name of each selected entity with that state. An empty selection gives `Defer`. An empty `entities` list does not (sub-phase 18.1).
- When System One knows the intent but cannot act, the result is `Defer`: Jarvis speaks a filler, makes no call, and System Two answers the request (sub-phase 4.5). Examples: an empty selection, a missing slot, a missing `person`, or a template that does not render.
- When the area has no matching entity and the house has exactly one, Jarvis uses that entity. Example: one thermostat for the house.
- `needs_speaker = true` makes the `person` variable available. Jarvis waits up to `speaker.wait_ms` for it.
- `next_date(day, plus_days)` returns the ISO date of the next `day` plus `plus_days`. An empty `day` means today. A `day` that is not a day name, `today` or `tomorrow` gives an error.
- Template variables are the built-ins `area` (the area name in lowercase), `entity`, `person` and `response`, and the slots of the intent.
- A slot that the prediction does not have is missing, unless the intent lists it in `optional`. A missing optional slot is an empty string. Another missing slot gives `Defer`. Phase 10 makes Jarvis ask for a missing `area`. Before Phase 10, a missing `area` gives `Defer`.
- A data value that is one `{{ expression }}` keeps the type of the expression result. One `{{ expression }}` starts with `{{`, ends with `}}`, and has no other `}}`. Other data values render as strings.

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
responses = ["Today will be {{ response.condition }}, with a high of {{ response.high }} degrees."]

[pools.filler]
responses = [
  "I will check on that for you.",
  "Let me look into that.",
  "One moment.",
]

[pools.failure]
responses = ["Sorry, I could not do that."]

[pools.calendar_add]
responses = ["I added {{ summary }} to your calendar, {{ person.name }}."]

[pools.start_training]
responses = ["I will start training now."]

[pools.training_busy]
responses = ["Training is already running."]

[pools.ask_area]
responses = ["Which room?"]

[pools.cancelled]
responses = ["Okay, never mind."]
```

The pool `ask_area` holds the questions for a missing area. A pool that is not an intent pool uses no template variable.

Template variables of an intent pool: `area` (area name), slot names, `entity` (state, attributes, name), `entities` (a list of `entity` objects), `response` (the plugin result of a fetch), `person` (name and the fields of its `[[people]]` entry). `entity` exists only for `target = "entity"`, `entities` only for a list query, `response` only for `kind = "fetch"`, and `person` only for `needs_speaker = true`.

Training set file (the contract between Jarvis and `train/train.py`):

```json
{
  "base_model": "sentence-transformers/all-MiniLM-L12-v2",
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
| GPU host (self-hosted runner `gpu`) | Linux x86-64, RTX 2060 6 GB | `cuda` | First pass: Nemotron Speech Streaming EN 0.6B, 160 ms, fp16. Second pass: Parakeet Unified EN 0.6B offline, fp16. | Runs Jarvis. Runs local training while Jarvis pauses. |
| Test machine | macOS x86-64 (Intel MacBook Pro) | `cpu` | First pass: Nemotron Speech Streaming EN 0.6B, 160 ms, int8. No second pass. | Development. Runs the pre-commit hook (without `bench` and `gpu-test`). |
| CI | `ubuntu-latest` | `cpu` | First pass: Nemotron Speech Streaming EN 0.6B, 160 ms, int8. No second pass. | `just check`. |
| RunPod | RunPod serverless | GPU | — | Training fallback. |

The GPU host runs Jarvis as the systemd unit `jarvis`: `jarvis serve jarvis.toml` in a checkout of the release tag. An environment file of the unit holds the secrets, `ORT_DYLIB_PATH`, which points to the `libonnxruntime` in the release folder, and `LD_LIBRARY_PATH`, which points to the release folder (sub-phase 6.1). Port 10700 (Wyoming) has no authentication, so the host firewall accepts it only from the HA host.

VRAM budget of live Jarvis on the GPU host (6 GB). A local training run does not share the GPU with live Jarvis, so it has no row.

| Use | Budget |
|---|---|
| STT first pass (Nemotron 0.6B, fp16) | 1.25 GB |
| STT second pass (Parakeet Unified 0.6B, fp16) | 1.25 GB |
| System One model | 0.3 GB |
| TTS (Supertonic-3 fp32, one session set for each input length) and voice identification (CAM++) | 1.2 GB |
| ONNX Runtime arenas and CUDA context | 0.8 GB |
| Margin | 0.5 GB |
| Reserve (not usable on a 6 GB card) | 0.5 GB |
| **Total** | **5.8 GB** |

The model budgets assume fp16 weights. The GPU host does not run the int8 STT models, because ONNX Runtime runs their int8 operators on the CPU (sub-phase 6.6c). Each fp16 STT session takes about 1.2 GB with the CUDA option `arena_extend_strategy=kSameAsRequested`, and about 2.1 GB with the default arena. With this option on both STT sessions, the measured peak of both STT passes, the System One model and the int8 sherpa-onnx TTS is 3.57 GB (5.23 GB with the default arena). The fp32 TTS of sub-phases 9.3 and 9.3b, with one session set for each input length, takes 1694 MiB alone on the GPU host. That figure has its own CUDA context, which the context row counts, so the TTS row has 1.2 GB. The new TTS takes about 1.05 GB more than the int8 TTS, so the peak becomes about 4.6 GB to 4.7 GB. AC-9.3c-2 measures it. The usable budget is 5.5 GB. GPU host tests measure the real peak and check that each model runs on the CUDA execution provider (AC-3.12b-3, AC-6.1-3, AC-6.2-5, AC-6.6c-4, AC-6.7-5, AC-15.3-7). Before a local run, Jarvis releases its model sessions, and it loads them again after the run (decision 12, sub-phase 13.10). Thus during the run, the usable 5.5 GB has the CUDA context of the Jarvis process (0.5 GB), the CUDA context of the training process (0.5 GB) and the PyTorch memory of the run. The process trainer limits the PyTorch memory to `train.max_vram_gb` (`torch.cuda.set_per_process_memory_fraction`), 4.5 GB. If a local run fails with "CUDA out of memory", the run moves to RunPod (decision 12).

The GPU host needs the NVIDIA driver, CUDA 12, cuDNN 9, and a Python 3.12 virtual environment in `.venv`. The host has only Python 3.14, so `uv` installs Python 3.12:

```sh
curl -LsSf https://astral.sh/uv/install.sh | sh
uv python install 3.12
uv venv --python 3.12 .venv
uv pip install ./train
```

The fp16 STT files (sub-phase 6.6c) are not in a download. Copy them by hand, once, from the MacBook to the GPU host, with their license notice `NOTICE.md`. The NVIDIA Open Model License requires the notice on redistribution. The folder is outside the runner checkout, so a clean checkout does not remove it. Make the folder first, readable by the runner user and the `jarvis` unit.

```sh
scp <folder>/*.fp16.* <folder>/NOTICE.md <GPU host>:/opt/jarvis-models/fp16/
```

The checkout of the `jarvis` unit runs `just models` and `just gpu-models` too. Thus its model folders in `models/` get the same files as the runner.

## 11. Reference House

The test fixtures (`FakeHome`, `FakeHaServer`) model this house. The data comes from the HA instance on 2026-10-25. `src/testing/house.json` holds it as a `Home` snapshot.

- Areas: Bedroom, Kitchen, Living Room, Office, Bathroom, Garage, Outside.
- Location: the test location `41.88`, `-87.63`, not the location of the real house. The temperature unit is `°F`.
- People: two `person` entities.
- Satellites: four, in the Bedroom, Kitchen, Living Room and Office. Each area has a "<Area> Presence" `binary_sensor` with no device class and no entity area. Each satellite device has a mute switch, for example `switch.carson_kitchen_mute_microphones` ("Carson Kitchen Mute Microphones"). `on` means muted. Two mute switches are `on`, one is `off` and one is `unavailable` (from the HA instance on 2026-11-06, sub-phase 13.10).
- Lights: many lights in each area. "Garage Light" is a `switch`.
- Fans: "Office Fan" is a `fan`. "Living Room Fan" is a `switch`.
- Climate: one thermostat, in the Kitchen.
- TV: one media player in the Living Room with the aliases "the TV" and "plex".
- Speakers: each satellite area has speaker media players. Three Music Assistant group players have no area: "Downstairs Speakers" and two "Office Speakers". One "Office Speakers" has no members (sub-phase 18.15).
- Locks, garage doors and windows: none. The tests of sub-phase 18.3 add them to the hot cache.
- Office: two temperature sensors with different values (64.0 °F and 76.6 °F). The higher value is probably the satellite sensor, which the device makes warm. Entity selection skips the entities of a satellite device, except its media players (section 9).

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
| Add a plugin (a remote API) | One adapter file, `src/lib.rs`, `jarvis.toml`, `Cargo.toml` when it needs a crate, and the port and file tables in this file. |
| Add a domain rule | One domain file, and the use case that calls it. `src/domain/contracts.rs` when the rule calls a `std` or crate function that has no `vstd` specification and no contract yet. |
| Change a stateful rule | The domain file, its Quint model, and its conformance driver. `src/domain/contracts.rs` when the rule calls a `std` or crate function that has no `vstd` specification and no contract yet. |

## 13. Quint Models

Each model has state variables, actions and named invariants. `02-rules.md` section 4 gives the rules for models.

| Model | Component | Rust module | Invariants |
|---|---|---|---|
| `cache_sync.qnt` | Hot cache sync | `domain/state_cache.rs`, `app/background.rs` | `CacheEqualsHomeWhenIdle`, `NoEventAfterDisconnect` |
| `announce.qnt` | Announce queue of one satellite | `app/conversation.rs` | `AnnounceWhenIdle`, `NoStaleAnnounce`, `SentencesInOrder` |
| `commit.qnt` | Commit policy and VAD | `domain/speech.rs` | `CommitOnlyWhenAllowed`, `NoLateCommit`, `NoCommitDuringSpeech` |
| `backfill.qnt` | Backfill | `app/background.rs` | `OneBackfillPerPool`, `PoolsChangeOnlyOnSuccess` |
| `dialog.qnt` | Dialogs and deferred jobs | `domain/dialog.rs`, `app/conversation.rs` | `TurnLimit`, `NoCallAfterCancel` |
| `feedback.qnt` | Marks | `domain/feedback.rs` | `DecideAvoidsBlocked` |
| `training.qnt` | Training | `domain/training.rs`, `app/training.rs` | `OneRun`, `SwapNeverWorse`, `CountResetsAfterRun`, `FailureKeepsModel`, `FallbackOnce`, `PauseOnlyLocal` |
| `learned.qnt` | Learned intents | `domain/learn.rs`, `app/background.rs` | `ServiceNeedsApproval`, `NoReproposal`, `ActivationCountsOnce`, `OneIssuePerCluster` |
| `prefetch.qnt` | Prefetch | `domain/learn.rs`, `app/background.rs` | `NoCallFromPrefetch`, `ServedAnswerIsFresh`, `PrefetchOnlyInWindow`, `NoEarlyRefresh` |
| `speaker.qnt` | Speaker identification | `domain/speaker.rs` | `SpeakerIsCandidate`, `NoVoiceOnlyEnrollment` |
| `jarvis.qnt` | System | All use cases | `OneHandledPerRequest`, `ReplyBeforeCall`, `FillerBeforeAnswer`, `ModelSwapAtomic`, `NoTalkOver` |
