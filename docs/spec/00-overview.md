# Jarvis Specification: Overview

## 0. About This Specification

This specification uses ASD-STE100 Simplified Technical English (STE).

- Sentences are short. Procedures have a maximum of 20 words for each sentence.
- Sentences use the active voice and the present tense.
- "Must" identifies a requirement. "Can" identifies an option.
- Technical names (crate names, file names, event names) are in `code` format.

### 0.1 Files of the Specification

| File | Contents |
|---|---|
| `00-overview.md` | This file: the specification format, technical names, goals, examples and the phase index. |
| `01-architecture.md` | Layers, ports, the System One model, decisions, latency budget, crates, files, configuration, machines, the reference house and the change map. |
| `02-rules.md` | Code rules, Python rules, test rules, Quint rules, proof rules, PR rules and the required checks. |
| `phase-00.md` to `phase-18.md` | One file for each phase. |

`CLAUDE.md` at the repository root holds the working rules for code changes. It must agree with `02-rules.md`.

### 0.2 Format of a Sub-Phase

Each sub-phase is exactly one pull request (PR) with one concern. Each sub-phase has these parts, in this order:

| Part | Contents |
|---|---|
| Title | `### <phase>.<n> <Title>`. The PR title is `<phase>.<n> <Title>`. A split part adds a letter to `<n>` (`02-rules.md` section 7.1, rule 8). |
| Goal | One or two sentences: what the sub-phase must achieve and why. It is written before the code. It is not the PR summary. |
| Implements | The goals (section 1) that the sub-phase serves. |
| Depends on | The sub-phases that must be merged first. |
| Files | The files that the PR creates, modifies or deletes. No other files are permitted. A sub-phase that writes domain code can also modify `src/domain/contracts.rs` (`02-rules.md` section 5.1, rule 8). |
| Quint | The Quint model that the sub-phase writes or checks against, if any. |
| Model | A high-level description of the data, the functions and the invariants. The Verus spec functions of a domain sub-phase come from this part. |
| Acceptance criteria | Numbered, binary, testable statements. `AC-<phase>.<n>-<k>`. |
| Proofs | Plain-English proofs for each criterion about Jarvis logic. `P-<phase>.<n>-<k>`. A proof about a domain function is also a Verus proof in the same PR. `02-rules.md` section 5 gives the format. |
| Field log | Optional. Steps on real hardware or accounts that are not tests. |

Every acceptance criterion gets a test before any code of the sub-phase is written (`02-rules.md` section 6). Every domain function gets its Verus clauses before its body (`02-rules.md` section 6.1). A sub-phase is complete only when all of its acceptance tests pass. A phase is complete only when the acceptance tests of all of its sub-phases pass on `main`.

Tags on acceptance criteria:

- No tag: an automated, deterministic test in `rust-test` or `python`.
- `[Bench]`: a deterministic instruction budget in `benches/domain.rs`, checked by `bench`.
- `[GPU host]`: an automated test that runs on the GPU host in the `gpu-test` job.
- `[Quint]`: a check that `just quint` does.

A sub-phase can also have a **Field log**: steps on real hardware or accounts (satellites, phones, RunPod, GitHub settings). Field log steps are not tests and gate nothing. A phase can end with a field log section.

### 0.3 Technical Names

| Name | Meaning |
|---|---|
| Jarvis | The Rust service that this specification describes. |
| HA | Home Assistant. |
| Satellite | A Future Proof Homes Satellite1 device. It runs microWakeWord on the device. HA does no wake word detection. After the wake word, HA streams the audio to the STT provider and decides the end of speech with its own voice activity detection (VAD). |
| Wyoming | The TCP protocol that HA uses to speak to STT, TTS and conversation services. It is a transport only. |
| STT | Speech to text. |
| TTS | Text to speech. |
| First pass | The streaming STT model. It gives partial text while the user speaks. |
| Second pass | The offline STT model. At commit, it decodes the whole utterance again and gives the final transcript. |
| Early commit | Jarvis sends the final transcript before HA stops the audio stream. |
| System One | The local, fast path. The System One model classifies the request. Jarvis acts without a remote call. |
| System One model | A pre-trained text encoder that Jarvis fine-tunes. It gets the transcript and the satellite area. It gives a probability for each intent and for `unknown`. It also tags the slots. |
| System Two | The OpenAI path. It handles requests that System One cannot classify or cannot act on. |
| Intent | One action that Jarvis can take, for example `lights_on`. Each intent is one label of the System One model. |
| Unknown | The reserved label for a request that matches no intent. |
| Prediction | The output of the System One model for one transcript: the label probabilities and the slots. |
| Slot | A value in a transcript, for example `area = kitchen`. |
| Pool | A list of response templates for one intent or one filler type. |
| Filler | A response that Jarvis speaks immediately when the real answer needs more time. |
| Hot cache | The in-memory copy of HA entity states and registries. |
| Plugin | A Jarvis data source for one remote API, for example the weather forecast. System Two gets it as a tool, and a fetch intent names it. |
| Announce | The HA service `assist_satellite.announce`. It speaks text on one satellite. |
| Ask | The HA service `assist_satellite.start_conversation`. It speaks a question on one satellite, then listens without the wake word. |
| Dialog | The state of one open conversation on one satellite. |
| Response latency | The time that Jarvis adds to a request: from the voice end that the VAD detects to the `transcript` event, from `transcript` to `handled`, and from `synthesize` to the first audio chunk. The time that HA takes between these events is not part of it. |
| Request habit | A spoken request that people make in the same time slot on several days, for example "what's the traffic like today?" on weekdays at 08:00. Its key is the intent name, or the name of the learned entry of a cluster of System Two requests. HA state changes and automations are not request habits. |
| Bucket | One time slot for request habits: the day type (weekday or weekend) and the hour. |
| Usual time | A bucket in which the decayed score of a request is `habits.min_score` or more. |
| Speaker | The person who speaks the request. |
| Voice profile | The stored voice embeddings of one person. |
| Learned intent | An intent that Jarvis adds itself from a cluster of requests that System Two answered. |
| Static intent | A learned intent with pre-generated responses and no service. |
| Prefetch intent | A learned intent whose answer depends on current data. System Two gives the answer to its question. |
| Prefetch | Jarvis gets the answer of a fetch intent or a prefetch intent before its usual time, then refreshes it each `fresh_min` of the intent until the window ends. A request with a fresh answer gets System One speed. |
| Window | The time in which Jarvis prefetches for one usual time: from `learn.prefetch_lead_min` before the hour to the end of the hour. |
| Proposal | A learned intent that System Two writes for one cluster. |
| Plugin request | One GitHub issue in which Jarvis asks Claude for a new plugin and its fetch intent, for a cluster that needs remote data that no plugin gives. |
| Approval | Your decision to accept or reject a proposal that calls an HA service. |
| Interaction | One record of one request: audio, transcript, model context, prediction, decision, service calls, responses, latency and marks. |
| Mark | Your rating of an interaction: `good` or `bad`, for the action and for the response. |
| Correction | The intent that Jarvis should have selected. You give it with a bad action mark. |
| Training set | All labeled examples for the System One model. |
| Held-out set | The examples that Jarvis uses only to measure accuracy. Training never uses them. |
| Training run | One fine-tune of the System One model, on the GPU host or on RunPod. |
| Change | One event that counts toward the next automatic training run: a new action mark or a new active learned intent. |
| Fixture model | A System One model that `just fixture-model` trains on the CPU from the seed examples. |
| RunPod | The GPU cloud that runs a training run when local training fails. |
| ONNX | The model file format. ONNX Runtime runs the models. |
| GPU host | The Linux x86-64 machine with an NVIDIA RTX 2060 (6 GB VRAM) that runs Jarvis. |
| Test machine | The Intel MacBook Pro. It runs Jarvis on the CPU for tests. |
| Device | `cuda` or `cpu`. It selects where the models run and where local training runs. |
| Discovery | Jarvis finds the satellite sensors and the person trackers from the HA registries. |
| Domain | The layer with the rules of Jarvis. It has only pure code. |
| Port | A Rust trait at the edge of the application. |
| Adapter | A struct that implements a port with one technology. |
| Composition root | `src/lib.rs`. It makes the adapters and connects them to the application. |
| Quint model | A formal model of one stateful part of Jarvis in the Quint language, in `quint/`. |
| Conformance test | A Rust test that replays traces of a Quint model against the Rust code with `quint-connect`. |
| Module graph | The graph of Rust modules and their `use` dependencies. `xtask` builds it from the source. |
| Architecture diagram | The Mermaid drawing of the module graph in `docs/architecture.md`. |
| Required check | A CI job that must pass before a PR can merge. `02-rules.md` section 7 lists them. |

---

## 1. Goals

| ID | Goal |
|---|---|
| G1 | The response latency for System One requests is 300 ms or less (p95) on the GPU host. A System One request here has a top intent at or above `model.commit_threshold` and a response in the audio cache. An intent with `needs_speaker` can wait `speaker.wait_ms` more (AC-15.7-9). The light commands of AC-9.2-5 have a target of 400 ms. |
| G2 | STT starts when the first audio chunk arrives. STT has two passes: a streaming first pass and an offline second pass at commit. On the GPU host, the first pass is NVIDIA Nemotron Speech Streaming EN 0.6B and the second pass is NVIDIA Parakeet Unified EN 0.6B. |
| G3 | The System One model classifies common requests locally. It gets the transcript and the satellite area. It gives a probability for each intent and for `unknown`, and tags the slots. The response path does not call a remote service. |
| G4 | Each intent has a pool of responses. Jarvis selects with variety and does not repeat the last response. |
| G5 | Jarvis adds new responses to a pool when the pool has too few unused responses. |
| G6 | For a request that needs external data, Jarvis speaks a filler immediately. Then Jarvis announces the real answer. |
| G7 | When the top label is `unknown`, no intent has enough probability, or System One cannot act on the top intent, Jarvis sends the request to OpenAI with the minimum possible latency. |
| G8 | Jarvis keeps a hot cache of HA entity states. The hot cache stays in sync with HA. |
| G9 | Jarvis knows the area of the satellite. "Turn on the lights" controls the lights in that area. |
| G10 | The code uses hexagonal architecture. It is modular: each change in the change map (`01-architecture.md` section 12) touches only the listed files. It obeys all rules in `02-rules.md`. |
| G11 | Jarvis learns the request habits of the house: which spoken request people make at which time of the week. Only spoken requests count, never HA state changes or automations. Before the usual time of a request whose answer needs current data, Jarvis prepares the answer. Thus the request moves from System Two speed to System One speed. |
| G12 | Jarvis identifies the speaker from who is home, phone or watch proximity to the satellite (through Bluetooth), the satellite presence sensor, and voice. Jarvis discovers these entities from HA. Person-specific intents act for that speaker. |
| G13 | When Jarvis does not understand, or needs more data, it asks the user. It continues to ask until the request is clear, the user cancels, or the turn limit occurs. |
| G14 | Jarvis clusters frequent requests that System Two answers, not only `unknown` requests. For each cluster, System Two writes a new intent, so System One handles these requests after the next training run. Jarvis adds it automatically, except an intent that calls an HA service. That intent waits for your approval. When the requests of a cluster need remote data that no plugin gives, Jarvis opens one GitHub issue instead, and Claude writes the plugin and its intent in a normal PR. |
| G15 | Jarvis logs each request with its audio (for the last `audio_days` days), the action and the response. You can mark the action and the response as good or bad, with an optional correction. Jarvis uses the marks in future decisions. |
| G16 | Marks and learned intents become training data. Jarvis starts a training run after `train.min_changes` changes, or when you ask by voice. The run uses the GPU host first and RunPod when local training fails. During a local run, Jarvis mutes the satellites and pauses: it does not answer, and the run gets the full GPU. The new model replaces the old model only when its held-out accuracy is equal or better. |
| G17 | The repository is on GitHub. The pre-commit hook and CI run the required checks. `just merge` merges a PR only when they all pass. A `v*` tag publishes release binaries for Linux x86-64 and macOS x86-64 (Intel), and the training image. |
| G18 | On the GPU host, STT, the System One model, TTS and voice identification run on the GPU. Other machines run the same code on the CPU. |
| G19 | Each stateful part of Jarvis with concurrency or a life cycle (the parts in `01-architecture.md` section 13) has a Quint model with named invariants. CI checks each model, and conformance tests replay its traces against the Rust code. A system model composes the parts of the request life cycle. Simple stores (use counts, the correlation buffer, interaction updates, voice profiles, habit storage) have unit tests instead. A model check to a step bound is bounded evidence, not a proof for all executions. |
| G20 | CI draws the module graph as a Mermaid diagram. On a PR, new modules are green, updated modules are orange and deleted modules are red. Each PR commits the full diagram in `docs/architecture.md`. The pre-commit hook writes it, and CI fails when it is stale. |
| G21 | Each PR has one concern, starts in a fresh context, is one layer of the phase's stack of PRs, has at most 400 changed lines, a two- or three-sentence summary written after the code, short bullet points and a check box for each required check, and gets a Codex code review with all threads resolved. |
| G22 | Every test is deterministic. Every check is a gated check in the pre-commit hook and in CI, not only a rule in `CLAUDE.md`. |
| G23 | Each proof about a domain function is a Verus proof, written in the same PR as the function. Verus also proves that no domain function overflows an integer, indexes out of bounds or fails an `expect`. The only trusted code is the contracts in `src/domain/contracts.rs`. |

### 1.1 Example System One Requests

- Turn on the fan.
- Turn on the lights. Turn off the lights.
- Put the TV on a specified show.
- Turn the air down.
- Play music. Pause music.

### 1.2 Example Person-Specific Request

- Add dentist to my calendar for Monday. Jarvis adds the event to the calendar of the speaker.

### 1.3 Example Request Habits

- On weekdays between 08:00 and 09:00, you ask "What's the traffic like today?". System Two answers slowly, or it cannot answer without a data source. After a few mornings, Jarvis notices the repeated request. When no plugin gives the data, Jarvis asks Claude for a new plugin (phase 14). When the new plugin is released, Jarvis knows the usual time: weekdays at 08:00. At about 07:55, Jarvis gets the traffic data and prepares the reply. It refreshes the reply until 09:00. At 08:10, the request gets System One speed.
- On weekends, the house asks "What is the weather like?" at about 09:00. At 08:55, Jarvis gets the forecast and prepares the reply.
- You ask for the traffic on two mornings only. That is not a request habit, so Jarvis prepares nothing.
- The living room lights turn on at 18:00. That is a device schedule, not a request habit. A native HA automation does it.

### 1.4 Example Dialogs

- User: "Turn on the lights." (The satellite has no area.) Jarvis: "Which room?" User: "The kitchen." Jarvis: "Turning on the kitchen lights."
- User: "Plan dinner for tonight." Jarvis: "Let me look into that." Then System Two asks: "How many people?" User: "Four." Jarvis announces the answer.

### 1.5 Example Learned Intents

- Users often say "Tell me a joke". The model gives `unknown`, so System Two answers. Jarvis clusters these requests. System Two proposes `tell_joke`, a static intent with 10 jokes. It has no service, so Jarvis adds it automatically. After the next training run, "Tell me a joke" gets System One speed.
- Users often ask "What is playing on the living room TV?". System One guesses a wrong intent and cannot act, so System Two answers. Jarvis clusters these requests, and System Two proposes a new intent. After the next training run, System One answers it.
- Users often say "Good night". System Two proposes `good_night` with the service `scene.turn_on`. It calls an HA service, so it waits for your approval on the review page.
- Users often ask "What is on the family calendar today?" at about 07:00 on weekdays. System Two answers with `calendar.get_events` and proposes a prefetch intent. At 06:55 Jarvis gets the answer. At 07:00 the answer is ready.

### 1.6 Example Training Triggers

- You mark the action of 18 interactions, and Jarvis adds 2 learned intents. That is 20 changes. With `train.min_changes = 20`, Jarvis starts a training run.
- You say "Start training." Jarvis starts a training run now, with any number of changes.

### 1.7 Non-Goals

- Wake word detection. The satellite does this with microWakeWord.
- Custom satellite firmware.
- Languages other than English.

---

## 2. Phase Index

Each line is one PR. The work order follows the rows of this table, not the phase numbers. Thus phases 17 and 18 come after phase 9 and before phase 10.

| Phase | Sub-phases |
|---|---|
| [0 Repository and Delivery](phase-00.md) | 0.1 Repository, coverage gate, hook and CI · 0.2 Lint gates · 0.3 Complexity gate · 0.4 Duplication gate · 0.5 PR template, stacks and review · 0.6 PR size gate · 0.7 Module graph · 0.7b Module graph paths · 0.8 Layer and modularity rules · 0.9 Architecture diagram · 0.10 PR diagram with change colors · 0.11 Diagram in each PR · 0.12 Quint gate · 0.13 GPU runner · 0.14 Release workflow · 0.15 Merge rules · 0.16 Merge command · 0.17 Pre-push guard · 0.18 Verus gate |
| [1 Configuration](phase-01.md) | 1.1 Configuration file · 1.2 Secrets from the environment |
| [2 Home and Hot Cache](phase-02.md) | 2.1 Home port and reference house · 2.2 HA connection and authentication · 2.3 HA requests and snapshot · 2.4 HA background connection · 2.4b HA events · 2.4c HA reconnect and ping · 2.5 HA service calls · 2.6 Cache sync Quint model · 2.7 State cache updates · 2.8 State cache queries · 2.8b Area names · 2.8c Domain benchmarks · 2.9 Sync use case |
| [3 System One Model](phase-03.md) | 3.1 Normalize · 3.1b Example markup · 3.2 Intent catalog · 3.2b Template slots and normalized examples · 3.2c Labels and tags · 3.3 Context samples and held-out split · 3.3b Seed training set · 3.3c Character offsets in the training set file · 3.4 Dataset command · 3.5 Python project and gates · 3.6 Training set loader and tag alignment · 3.7 Joint model and loss · 3.8 Training loop and ONNX export · 3.9 Fixture model · 3.10 Training image release · 3.11 Softmax · 3.11b BIO decoding · 3.11c Ranking · 3.12 Classifier port and ONNX adapter · 3.12b Execution providers and output checks · 3.12c Labels file checks |
| [4 System One Decisions](phase-04.md) | 4.1 Response selection · 4.2 Templates and pool validation · 4.3 File store · 4.4 Entity filters · 4.4b Name words and skipped entities · 4.4c Single entity and house fallback · 4.5 Decide commands · 4.5b Area rules · 4.5c Service call · 4.5d Slots and service data · 4.6 Decide queries and fetches · 4.7 Announce Quint model · 4.8 Dispatch replies · 4.9 Announce port · 4.9b Announce queue · 4.9c Failed reply announce · 4.9d Dispatch fetches |
| [5 Wyoming Conversation Agent](phase-05.md) | 5.1 Assistant port and Wyoming server · 5.2 Transcript handling in the adapter · 5.3 Handle use case · 5.4 Composition root · 5.4b HA unavailable at startup · 5.4c Graceful shutdown |
| [6 Speech](phase-06.md) | 6.1 Shared ONNX Runtime and GPU recipes · 6.2 TTS adapter · 6.3 Audio chunks and duration · 6.3b Audio cache and speak · 6.4 Commit Quint model · 6.5 Commit policy · 6.6 First-pass STT adapter · 6.6b First-pass voice activity · 6.6c First pass on the GPU host · 6.7 Second-pass STT · 6.8 Listen use case · 6.8b Second pass and wiring · 6.9 Wyoming STT events · 6.10 Wyoming TTS events · 6.11 Logs |
| [7 System Two](phase-07.md) | 7.1 Reasoner port and warm connection · 7.2 Prompt builder · 7.3 Sentence streaming · 7.4 Tool calls · 7.5 Deferred answers · 7.5b Tool calls through HA · 7.5c Answers without a satellite · 7.6 Area states tool · 7.7 Entities without an area |
| [8 Response Backfill](phase-08.md) | 8.1 Backfill policy · 8.2 Response generation · 8.3 Backfill Quint model · 8.4 Backfill use case · 8.4b Backfill pre-render and wiring |
| [9 End-to-End Verification](phase-09.md) | 9.1 Pipeline harness · 9.1b Exact silence · 9.2 Latency tests · 9.3 Supertonic TTS adapter · 9.3b Fixed TTS input lengths · 9.3c TTS adapter switch |
| [17 Plugins](phase-17.md) | 17.1 Plugin port and cache · 17.2 Plugin tools in System Two · 17.3 Open-Meteo weather plugin · 17.3b One plugin registry · 17.4 Weather intent through the plugin |
| [18 Queries and Media](phase-18.md) | 18.1 List queries · 18.2 Light queries · 18.3 Door, window and lock queries · 18.4 Climate and fan queries · 18.5 Presence and alarm queries · 18.6 Now playing query · 18.7 Music playback · 18.8 Music controls · 18.9 Plex playback · 18.10 TV controls · 18.11 Plex plugin · 18.12 New shows intent · 18.13 Music search data · 18.13b Music search playback · 18.14 Fast artist start · 18.15 Speaker groups · 18.15b Speaker group controls · 18.16 Opposite commands |
| [10 Multi-Turn Dialogs](phase-10.md) | 10.1 Dialog Quint model · 10.2 Dialog state · 10.3 Answer fill and cancel · 10.4 Ask decision and HA ask · 10.5 Dialog routing · 10.5b Dialog generations · 10.5c Dialog fill context · 10.6 System Two questions · 10.6b System Two answers · 10.7 Early commit for answers |
| [11 Request Habits](phase-11.md) | 11.1 Source and action · 11.1b Bucket · 11.2 Remove device habits · 11.3 Request habit scores · 11.4 Request recording and store |
| [12 Interaction Log and Feedback](phase-12.md) | 12.1 Audio correlation · 12.2 Feedback Quint model · 12.3 Feedback rules · 12.4 Interaction log port and SQLite storage · 12.5 Marks, deferred requests and pruning · 12.6 Record interactions · 12.7 Review port and use case · 12.8 Review web API · 12.9 Review page · 12.10 Feedback in decide · 12.11 Feedback in responses and System Two |
| [13 Training](phase-13.md) | 13.1 Mark examples and training set · 13.2 Held-out split and acceptance · 13.3 Change count · 13.4 Training Quint model · 13.5 Paraphrase generation · 13.6 Generated example store · 13.7 Trainer port and process adapter · 13.8 RunPod handler · 13.8b RunPod adapter · 13.9 Training run · 13.10 Training pause and fallback · 13.11 Voice trigger |
| [14 Learned Intents](phase-14.md) | 14.1 Clusters · 14.2 Proposal rules · 14.3 Learned intent Quint model · 14.4 Proposal generation · 14.5 Learned intent lifecycle · 14.6 Proposal review · 14.7 Activation · 14.8 Prefetch Quint model · 14.9 Prefetch rules · 14.10 Prefetch use case · 14.10b Fetch intent prefetch · 14.11 Learned intents in training · 14.12 Issues port and GitHub adapter · 14.13 Plugin requests |
| [15 Speaker Identification](phase-15.md) | 15.1 Discovery · 15.2 Presence and proximity signals · 15.3 Voice ports and adapters · 15.4 Voice embedding at commit · 15.5 Speaker Quint model · 15.6 Identification and enrollment · 15.7 Person-specific intents |
| [16 System Model](phase-16.md) | 16.1 System Quint model · 16.2 System conformance · 16.3 Composed Quint model · 16.4 Composed conformance |
