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
| `phase-00.md` to `phase-16.md` | One file for each phase. |

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
| System One model | A pre-trained text encoder that Jarvis fine-tunes. It gets the transcript, the satellite area and the habits. It gives a probability for each intent and for `unknown`. It also tags the slots. |
| System Two | The OpenAI path. It handles requests that System One cannot classify. |
| Intent | One action that Jarvis can take, for example `lights_on`. Each intent is one label of the System One model. |
| Unknown | The reserved label for a request that matches no intent. |
| Prediction | The output of the System One model for one transcript: the label probabilities and the slots. |
| Slot | A value in a transcript, for example `area = kitchen`. |
| Pool | A list of response templates for one intent or one filler type. |
| Filler | A response that Jarvis speaks immediately when the real answer needs more time. |
| Hot cache | The in-memory copy of HA entity states and registries. |
| Announce | The HA service `assist_satellite.announce`. It speaks text on one satellite. |
| Ask | The HA service `assist_satellite.start_conversation`. It speaks a question on one satellite, then listens without the wake word. |
| Dialog | The state of one open conversation on one satellite. |
| Response latency | The time that Jarvis adds to a request: from the last voiced audio sample to the `transcript` event, from `transcript` to `handled`, and from `synthesize` to the first audio chunk. The time that HA takes between these events is not part of it. |
| Habit | A learned pattern: an entity action that occurs at a specified time of the week. |
| Bucket | One time slot for habits: the day type (weekday or weekend) and the hour. |
| Established habit | A habit with a score of `habits.min_score` or more in one or more buckets. |
| Source | The cause of a state change: `manual`, `automation`, `voice` or `unknown`. |
| Speaker | The person who speaks the request. |
| Voice profile | The stored voice embeddings of one person. |
| Learned intent | An intent that Jarvis adds itself from a cluster of `unknown` requests. |
| Static intent | A learned intent with pre-generated responses and no service. |
| Prefetch intent | A learned intent whose answer depends on current data. Jarvis gets the answer shortly before the usual time. |
| Proposal | A learned intent that System Two writes for one cluster. |
| Approval | Your decision to accept or reject a proposal that calls an HA service. |
| Interaction | One record of one request: audio, transcript, model context, prediction, decision, service calls, responses, latency and marks. |
| Mark | Your rating of an interaction: `good` or `bad`, for the action and for the response. |
| Correction | The intent that Jarvis should have selected. You give it with a bad action mark. |
| Training set | All labeled examples for the System One model. |
| Held-out set | The examples that Jarvis uses only to measure accuracy. Training never uses them. |
| Training run | One fine-tune of the System One model, on the GPU host or on RunPod. |
| Change | One event that counts toward the next automatic training run: a new action mark, a new active learned intent or a new established habit. |
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
| G1 | The response latency for System One requests is 300 ms or less (p95) on the GPU host. A System One request here has a top intent at or above `model.commit_threshold` and a response in the audio cache. An intent with `needs_speaker` can wait `speaker.wait_ms` more (AC-15.7-9). |
| G2 | STT starts when the first audio chunk arrives. STT has two passes: a streaming first pass and an offline second pass at commit. On the GPU host, the first pass is NVIDIA Nemotron Speech Streaming EN 0.6B and the second pass is NVIDIA Parakeet Unified EN 0.6B. |
| G3 | The System One model classifies common requests locally. It gets the transcript, the satellite area and the habits. It gives a probability for each intent and for `unknown`, and tags the slots. The response path does not call a remote service. |
| G4 | Each intent has a pool of responses. Jarvis selects with variety and does not repeat the last response. |
| G5 | Jarvis adds new responses to a pool when the pool has too few unused responses. |
| G6 | For a request that needs external data, Jarvis speaks a filler immediately. Then Jarvis announces the real answer. |
| G7 | When the top label is `unknown`, or no intent has enough probability, Jarvis sends the request to OpenAI with the minimum possible latency. |
| G8 | Jarvis keeps a hot cache of HA entity states. The hot cache stays in sync with HA. |
| G9 | Jarvis knows the area of the satellite. "Turn on the lights" controls the lights in that area. |
| G10 | The code uses hexagonal architecture. It is modular: each change in the change map (`01-architecture.md` section 12) touches only the listed files. It obeys all rules in `02-rules.md`. |
| G11 | Jarvis learns the habits of the house from HA state changes. The habits are an input of the System One model. They also help Jarvis select the entity and missing slot values. |
| G12 | Jarvis identifies the speaker from who is home, phone or watch proximity to the satellite (through Bluetooth), the satellite presence sensor, and voice. Jarvis discovers these entities from HA. Person-specific intents act for that speaker. |
| G13 | When Jarvis does not understand, or needs more data, it asks the user. It continues to ask until the request is clear, the user cancels, or the turn limit occurs. |
| G14 | Jarvis clusters frequent `unknown` requests. For each cluster, System Two writes a new intent. Jarvis adds it automatically, except an intent that calls an HA service. That intent waits for your approval. |
| G15 | Jarvis logs each request with its audio (for the last `audio_days` days), the action and the response. You can mark the action and the response as good or bad, with an optional correction. Jarvis uses the marks in future decisions. |
| G16 | Marks, learned intents and established habits become training data. Jarvis starts a training run after `train.min_changes` changes, or when you ask by voice. The run uses the GPU host first and RunPod when local training fails. The new model replaces the old model only when its held-out accuracy is equal or better. |
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

### 1.3 Example Habits

- The living room lights turn on at about 18:00 on weekdays.
- The house watches TV in the living room at about 20:00.
- A person adjusts the thermostat manually in the evening.

### 1.4 Example Dialogs

- User: "Turn on the lights." (The satellite has no area.) Jarvis: "Which room?" User: "The kitchen." Jarvis: "Turning on the kitchen lights."
- User: "Add dentist to my calendar." (Jarvis cannot identify the speaker.) Jarvis: "Who is asking?" User: "Alex." Jarvis: "I added dentist to your calendar, Alex."
- User: "Plan dinner for tonight." Jarvis: "Let me look into that." Then System Two asks: "How many people?" User: "Four." Jarvis announces the answer.

### 1.5 Example Learned Intents

- Users often say "Tell me a joke". The model gives `unknown`, so System Two answers. Jarvis clusters these requests. System Two proposes `tell_joke`, a static intent with 10 jokes. It has no service, so Jarvis adds it automatically. After the next training run, "Tell me a joke" gets System One speed.
- Users often ask "What is the weather like?". System Two proposes `what_is_weather` with the service `weather.get_forecasts`. It calls an HA service, so it waits for your approval on the review page.
- A user says "How is traffic to work?" at about 08:00 on weekdays. System Two proposes a prefetch intent. At 07:55 Jarvis gets the answer. At 08:00 the answer is ready.

### 1.6 Example Training Triggers

- You mark the action of 12 interactions, Jarvis adds 2 learned intents and the house gets 6 new established habits. That is 20 changes. With `train.min_changes = 20`, Jarvis starts a training run.
- You say "Start training." Jarvis starts a training run now, with any number of changes.

### 1.7 Non-Goals

- Wake word detection. The satellite does this with microWakeWord.
- Custom satellite firmware.
- Languages other than English.

---

## 2. Phase Index

Each line is one PR.

| Phase | Sub-phases |
|---|---|
| [0 Repository and Delivery](phase-00.md) | 0.1 Repository, coverage gate, hook and CI · 0.2 Lint gates · 0.3 Complexity gate · 0.4 Duplication gate · 0.5 PR template, stacks and review · 0.6 PR size gate · 0.7 Module graph · 0.7b Module graph paths · 0.8 Layer and modularity rules · 0.9 Architecture diagram · 0.10 PR diagram with change colors · 0.11 Diagram in each PR · 0.12 Quint gate · 0.13 GPU runner · 0.14 Release workflow · 0.15 Merge rules · 0.16 Merge command · 0.17 Pre-push guard · 0.18 Verus gate |
| [1 Configuration](phase-01.md) | 1.1 Configuration file · 1.2 Secrets from the environment |
| [2 Home and Hot Cache](phase-02.md) | 2.1 Home port and reference house · 2.2 HA connection and authentication · 2.3 HA requests and snapshot · 2.4 HA background connection · 2.4b HA events · 2.4c HA reconnect and ping · 2.5 HA service calls · 2.6 Cache sync Quint model · 2.7 State cache updates · 2.8 State cache queries · 2.8b Area names · 2.8c Domain benchmarks · 2.9 Sync use case |
| [3 System One Model](phase-03.md) | 3.1 Normalize · 3.1b Example markup · 3.2 Intent catalog · 3.2b Template slots and normalized examples · 3.2c Labels and tags · 3.3 Context samples and held-out split · 3.3b Seed training set · 3.3c Character offsets in the training set file · 3.4 Dataset command · 3.5 Python project and gates · 3.6 Training set loader and tag alignment · 3.7 Joint model and loss · 3.8 Training loop and ONNX export · 3.9 Fixture model · 3.10 Training image release · 3.11 Softmax · 3.11b BIO decoding · 3.11c Ranking · 3.12 Classifier port and ONNX adapter · 3.12b Execution providers and output checks · 3.12c Labels file checks |
| [4 System One Decisions](phase-04.md) | 4.1 Response selection · 4.2 Templates and pool validation · 4.3 File store · 4.4 Entity filters · 4.4b Name words and skipped entities · 4.4c Single entity and house fallback · 4.5 Decide commands · 4.5b Area rules · 4.5c Service call · 4.5d Slots and service data · 4.6 Decide queries and fetches · 4.7 Announce Quint model · 4.8 Dispatch replies · 4.9 Dispatch fetches and announce |
| [5 Wyoming Conversation Agent](phase-05.md) | 5.1 Assistant port and Wyoming server · 5.2 Transcript handling in the adapter · 5.3 Handle use case · 5.4 Composition root |
| [6 Speech](phase-06.md) | 6.1 TTS adapter · 6.2 Shared ONNX Runtime and GPU recipes · 6.3 Audio cache and speak · 6.4 Commit Quint model · 6.5 Commit policy · 6.6 First-pass STT adapter · 6.7 Second-pass STT · 6.8 Listen use case · 6.9 Wyoming STT events · 6.10 Wyoming TTS events |
| [7 System Two](phase-07.md) | 7.1 Reasoner port and warm connection · 7.2 Prompt builder · 7.3 Sentence streaming · 7.4 Tool calls · 7.5 Deferred answers |
| [8 Response Backfill](phase-08.md) | 8.1 Backfill policy · 8.2 Response generation · 8.3 Backfill Quint model · 8.4 Backfill use case |
| [9 End-to-End Verification](phase-09.md) | 9.1 Pipeline harness · 9.2 Latency tests |
| [10 Multi-Turn Dialogs](phase-10.md) | 10.1 Dialog Quint model · 10.2 Dialog state · 10.3 Answer fill and cancel · 10.4 Ask decision and HA ask · 10.5 Dialog routing · 10.6 System Two questions · 10.7 Early commit for answers |
| [11 Habit Learning](phase-11.md) | 11.1 Source, action and bucket · 11.2 Scores and summary · 11.3 Habit learning and store · 11.4 History bootstrap · 11.5 Habits in the model context · 11.6 Habits in entity and slot selection · 11.7 Habits in the System Two prompt |
| [12 Interaction Log and Feedback](phase-12.md) | 12.1 Audio correlation · 12.2 Feedback Quint model · 12.3 Feedback rules · 12.4 Interaction log port and SQLite storage · 12.5 Marks, unknowns and pruning · 12.6 Record interactions · 12.7 Review port and use case · 12.8 Review web API · 12.9 Review page · 12.10 Feedback in decide · 12.11 Feedback in responses and System Two |
| [13 Training](phase-13.md) | 13.1 Mark examples and training set · 13.2 Held-out split and acceptance · 13.3 Change count · 13.4 Training Quint model · 13.5 Paraphrase generation · 13.6 Generated example store · 13.7 Trainer port and process adapter · 13.8 RunPod handler · 13.8b RunPod adapter · 13.9 Training run · 13.10 Training fallback · 13.11 Voice trigger |
| [14 Learned Intents](phase-14.md) | 14.1 Clusters · 14.2 Proposal rules · 14.3 Learned intent Quint model · 14.4 Proposal generation · 14.5 Learned intent lifecycle · 14.6 Proposal review · 14.7 Activation · 14.8 Prefetch Quint model · 14.9 Prefetch rules · 14.10 Prefetch use case · 14.11 Learned intents in training |
| [15 Speaker Identification](phase-15.md) | 15.1 Discovery · 15.2 Presence and proximity signals · 15.3 Voice ports and adapters · 15.4 Voice embedding at commit · 15.5 Speaker Quint model · 15.6 Identification and enrollment · 15.7 Person-specific intents |
| [16 System Model](phase-16.md) | 16.1 System Quint model · 16.2 System conformance · 16.3 Composed Quint model · 16.4 Composed conformance |
