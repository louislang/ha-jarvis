# Phase 13: Training

This phase improves the System One model. Jarvis builds the training set, trains on the GPU host (RunPod when local training fails), and keeps a new model only when it is equal or better.

---

### 13.1 Mark Examples and Training Set

**Goal:** Turn marks into labeled examples and build the full training set from seeds, generated examples, active learned intents and marks. Your marks become training data.

- **Implements:** G15, G16. **Depends on:** 12.11.
- **Files:** modify `src/domain/training.rs`.

**Model.**

- `mark_examples(marked) -> Vec<Example>`:
  - A good action mark gives an `Intent` example for the selected intent, with the logged slots as spans.
  - A bad action mark with a correction gives an `Intent` example for the correction, with `spans = None`.
  - A bad action mark without a correction gives a `Not` example for the selected intent.
  - An interaction without an action mark gives no example.
  - A mark example uses the logged model context.
- `sample_context(areas, habits, rng)` now adds the habit summary of the sampled area at a random time.
- `training_set(catalog, learned, generated, marked, areas, habits, rng, settings) -> TrainingSet`. The labels and tags include the active learned intents and their slots.

**Acceptance criteria.**

- AC-13.1-1: When an interaction has a good action mark for `lights_on`, `mark_examples` returns a `lights_on` example with the logged spans and context.
- AC-13.1-2: When an interaction has a bad action mark with the correction `tv_show`, `mark_examples` returns a `tv_show` example with `spans = None`.
- AC-13.1-3: When an interaction has a bad action mark without a correction, `mark_examples` returns a `Not` example for the selected intent.
- AC-13.1-4: When an interaction has no action mark, `mark_examples` returns no example.
- AC-13.1-5: When `sample_context` gets a habit model, the context has the habit summary of the sampled area and time.
- AC-13.1-6: When an active learned intent exists, the training set labels contain it and the tags contain its slots.

**Proofs.**

- P-13.1-1 to P-13.1-4. Ensures: `mark_examples` is a total function of the action mark with four disjoint cases. Argument: it is a `filter_map` over the interactions with a `match` on `(value, correction)`. The arms are `(Good, _)`, `(Bad, Some)`, `(Bad, None)` and no mark. They cover every case once.
- P-13.1-6. Argument: `labels` and `tags` run over `catalog.intents ++ learned.active()` (P-3.2-7, P-3.2-8).
- P-13.1-5. Evidence: test.

---

### 13.2 Held-Out Split and Acceptance

**Goal:** Add the stable held-out split, the accuracy measure and the accept rule. A new model can be compared fairly with the old one on the same examples.

- **Implements:** G16. **Depends on:** 13.1.
- **Files:** modify `src/domain/training.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `held_out(text, holdout) -> bool = xxh3(normalize(text)) mod 10000 < holdout * 10000`.
- `accuracy(predictions, examples) -> f64`: an `Intent(k)` example is correct when the top label is `k`. A `Not(k)` example is correct when the top label is not `k`.
- `accept(new, old) -> bool = new >= old`.
- This sub-phase adds `train.holdout`.

**Acceptance criteria.**

- AC-13.2-1: When `held_out` runs two times for the same text, it returns the same value.
- AC-13.2-2: When `held_out` runs for 1,000 different texts with `holdout = 0.1`, between 70 and 130 are held out.
- AC-13.2-3: When a `Not` example has a top label other than its label, `accuracy` counts it as correct.
- AC-13.2-4: When the new accuracy is 0.90 and the old is 0.90, `accept` returns true. When the new is 0.89, it returns false.

**Proofs.**

- P-13.2-1. Argument: `xxh3` with a fixed seed is a pure function, and `normalize` is pure.
- P-13.2-3. Argument: the `Not(k)` arm of `accuracy` tests `top != k`.
- P-13.2-4. Argument: `accept` is `>=`.
- P-13.2-2. Evidence: test (statistical property of the hash on this input set).

---

### 13.3 Change Count

**Goal:** Add the training state and the change count: new marks, new active learned intents and new established habits since the last run. Jarvis knows when enough has changed to train.

- **Implements:** G16. **Depends on:** 13.2.
- **Files:** modify `src/domain/training.rs`, `jarvis.toml`.

**Model.** `TrainingState { current: Option<run_id>, last_time, habit_keys: BTreeSet<HabitKey> }`. `changes(state, marked, learned, established) = |marks after last_time| + |learned activated after last_time| + |established \ state.habit_keys|`. `should_train(count, min_changes) = count >= min_changes`. This sub-phase adds `train.state` and `train.min_changes`.

**Acceptance criteria.**

- AC-13.3-1: When 12 marks, 2 learned intents and 6 new established habits occur after the last run, `changes` returns 20.
- AC-13.3-2: When an established habit was in `habit_keys`, it does not count.
- AC-13.3-3: When the count is `min_changes - 1`, `should_train` returns false. At `min_changes`, it returns true.

**Proofs.**

- P-13.3-1, P-13.3-2. Argument: `changes` is the sum of three set sizes. The third is a set difference, so keys in `habit_keys` are excluded.
- P-13.3-3. Argument: `should_train` is `>=`.

---

### 13.4 Training Quint Model

**Goal:** Add `quint/training.qnt`, the model of the trigger, the run, the local and RunPod attempts, the accept rule and the swap. Prove that only one run occurs at a time and that a swap never installs a worse model.

- **Implements:** G16, G19. **Depends on:** 13.3.
- **Files:** create `quint/training.qnt`.
- **Quint:** `training.qnt`.

**Model.**

- Constants: `MIN = 2`, `ACC = Set(0, 1, 2)`, `RUNPOD: bool`.
- State: `changes`, `run: Idle | Local | RunPod`, `modelAcc`, `swaps: List[(old, new)]`, `attempts`.
- Actions: `change` (`changes += 1`), `autoStart` (if `Idle` and `changes >= MIN`: `run := Local`), `voiceStart` (if `Idle`: `run := Local`), `localDone(a)`, `localFail` (if `RUNPOD`: `run := RunPod`, else end with failure), `runpodDone(a)`, `runpodFail`.
- A done action with accuracy `a`: if `a >= modelAcc`, append `(modelAcc, a)` to `swaps` and set `modelAcc := a`. Then `run := Idle` and `changes := 0`.
- A final failure: `run := Idle`, and `changes` and `modelAcc` do not change.
- Invariants:
  - `OneRun`: `run` has one value, and a start is enabled only when `run = Idle`.
  - `SwapNeverWorse`: for every `(old, new)` in `swaps`, `new >= old`.
  - `CountResetsAfterRun`: after a done action, `changes = 0`.
  - `FailureKeepsModel`: a final failure does not change `modelAcc`.
  - `FallbackOnce`: one run has at most one RunPod attempt, and only after a local failure.

**Acceptance criteria.**

- AC-13.4-1 [Quint]: `quint typecheck` passes.
- AC-13.4-2 [Quint]: All five invariants hold in simulation and to 10 steps in `quint verify`, with `RUNPOD` true and false.
- AC-13.4-3 [Quint]: The test `rejectedRunResetsCount` passes: a run with a lower accuracy keeps the model and sets `changes` to 0.

**Proofs.**

- P-13.4-2. `OneRun`: both start actions are guarded by `run = Idle`. `SwapNeverWorse`: the only append to `swaps` is guarded by `a >= modelAcc` and appends `(modelAcc, a)`. `CountResetsAfterRun`: both done actions end with `changes := 0`. `FailureKeepsModel`: the failure actions write neither `modelAcc` nor `swaps`. `FallbackOnce`: `run := RunPod` occurs only in `localFail`, which is enabled only when `run = Local`, and no action sets `run := Local` from `RunPod`.
- P-13.4-1, P-13.4-3. Evidence: model check.

---

### 13.5 Paraphrase Generation

**Goal:** Add `paraphrase` to the `Reasoner` port and `validate_example`. Each intent gets many reworded examples, and `unknown` gets requests that match no intent.

- **Implements:** G16. **Depends on:** 13.4.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/domain/intent.rs`, `src/testing/fakes.rs`, `jarvis.toml`.

**Model.** `paraphrase(target, seeds, slots, n) -> Result<Vec<String>>` with structured output `{ "examples": [string] }`. For an intent, the prompt has the seed examples and the slot names, and asks for the same markup. For `unknown`, the prompt has the intent names and examples, and asks for requests that match none. `validate_example(markup, intent, seeds)`: parses, uses only the intent's slots, and is not equal to a seed after `normalize`. This sub-phase adds `train.paraphrases` and `train.unknown_examples`.

**Acceptance criteria.**

- AC-13.5-1: When the prompt builder runs for `lights_on`, the prompt contains the seed examples and the slot name `area`.
- AC-13.5-2: When the mock returns `{"examples": ["turn the [den](area) lights on"]}`, `paraphrase` returns that example.
- AC-13.5-3: When a generated example uses a slot that the intent does not have, `validate_example` rejects it.
- AC-13.5-4: When a generated example equals a seed after `normalize`, `validate_example` rejects it.
- AC-13.5-5: When a generated example has a markup error, `validate_example` rejects it.

**Proofs.**

- P-13.5-3 to P-13.5-5. Argument: `validate_example` is the conjunction of `parse_example` (P-3.1-5), a subset check on the slot names, and a set membership check on normalized seeds.
- P-13.5-1, P-13.5-2. Evidence: test.

---

### 13.6 Generated Example Store

**Goal:** Store generated examples in `generated.json` and regenerate only for changed seeds or a changed intent list. Training does not call OpenAI again for examples it already has.

- **Implements:** G16. **Depends on:** 13.5.
- **Files:** create `src/app/training.rs`. Modify `src/main.rs`, `jarvis.toml`.

**Model.** `Generated { intents: Map<name, { seed_hash, examples }>, unknown: { intents_hash, examples } }`. `refresh(catalog, generated)`: for each intent whose `seed_hash` differs from the hash of its seeds, call `paraphrase` and keep the valid results. If the hash of the intent list differs, regenerate `unknown`. Save through `Store`. This sub-phase adds `train.generated`.

**Acceptance criteria.**

- AC-13.6-1: When the seeds of `lights_on` did not change, `FakeReasoner` records no `paraphrase` call for it.
- AC-13.6-2: When the seeds of `lights_on` change, `FakeReasoner` records one `paraphrase` call for `lights_on` and none for other intents.
- AC-13.6-3: When the intent list changes, `FakeReasoner` records one `paraphrase` call for `unknown`.
- AC-13.6-4: When `paraphrase` returns an error for one intent, the stored examples of that intent do not change.

**Proofs.**

- P-13.6-1 to P-13.6-3. Argument: `refresh` calls `paraphrase` for an intent only inside `if hash(seeds) != stored.seed_hash`, and for `unknown` only inside `if hash(names) != stored.intents_hash`.
- P-13.6-4. Argument: the store entry is replaced only in the `Ok` branch.

---

### 13.7 Trainer Port and Process Adapter

**Goal:** Add the `Trainer` port and `ProcessTrainer`, which runs `train/train.py` on the GPU host. Jarvis can train a model locally from a training set.

- **Implements:** G16, G18. **Depends on:** 13.6.
- **Files:** create `src/adapters/process.rs`. Modify `src/ports.rs`, `src/main.rs`, `src/testing/fakes.rs`, `jarvis.toml`.

**Model.** `Trainer::train(run_id, set) -> Result<PathBuf>`. `ProcessTrainer` writes the set to `<model.dir>/<run_id>/train.json` and runs `<train.python> train/train.py --data <file> --out <model.dir>/<run_id> --device <device>`. A non-zero exit gives an error with the last 20 lines of the error output. This sub-phase adds `train.python`.

**Acceptance criteria.**

- AC-13.7-1: When the script exits with code 0, `train` returns `<model.dir>/<run_id>/`.
- AC-13.7-2: When the script exits with code 1 and prints "CUDA out of memory", `train` returns an error that contains that text.
- AC-13.7-3: When `train` runs, the script receives `--device` with the configured device.

**Proofs.**

- P-13.7-1, P-13.7-2. Argument: `train` matches on the exit status, with `Ok(dir)` for success and `Err(tail of error output)` otherwise.
- P-13.7-3. Evidence: test.

---

### 13.8 RunPod Adapter and Handler

**Goal:** Add `RunPodTrainer` and the RunPod handler in `train.py`. A training set can run on a RunPod GPU through the network volume.

- **Implements:** G16. **Depends on:** 13.7.
- **Files:** create `src/adapters/runpod.rs`. Modify `src/main.rs`, `src/testing/fakes.rs`, `train/train.py`, `train/test_train.py`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `RunPodTrainer::train(run_id, set)`:
  1. Upload the set to `jarvis/runs/<run_id>/train.json` on the volume.
  2. `POST /v2/<endpoint_id>/run` with `{ "input": { "run_id": "<run_id>" } }`.
  3. Poll `GET /v2/<endpoint_id>/status/<job_id>` each `poll_s`.
  4. On `COMPLETED`, download `jarvis/runs/<run_id>/model/` to `<model.dir>/<run_id>/`.
  5. After `timeout_min`, send `POST /v2/<endpoint_id>/cancel/<job_id>` and return a timeout error.
- The adapter gets an `object_store` instance. Tests use `InMemory`. Production uses `AmazonS3` with the RunPod endpoint and region.
- `train.py --serve` starts the handler. It reads `/runpod-volume/jarvis/runs/<run_id>/train.json` and writes the model files to `.../model/`.
- This sub-phase adds `[train.runpod]` and resolves its secrets.

**Acceptance criteria.**

- AC-13.8-1: When `train` runs, the store contains `jarvis/runs/<run_id>/train.json` with the set.
- AC-13.8-2: When `train` runs, the mock receives `POST /v2/<endpoint_id>/run` with the run ID and the bearer key.
- AC-13.8-3: When the status is `IN_QUEUE`, then `IN_PROGRESS`, then `COMPLETED`, `train` downloads the model files into `<model.dir>/<run_id>/`.
- AC-13.8-4: When the status is `FAILED`, `train` returns an error with the job error text.
- AC-13.8-5: When the job runs longer than `timeout_min`, `train` sends `cancel` and returns a timeout error.
- AC-13.8-6: When the handler runs with a temporary volume root, it reads `train.json` and writes the three model files.

**Proofs.**

- P-13.8-3 to P-13.8-5. Argument: the poll loop matches on the status. `COMPLETED` downloads and returns. `FAILED`, `CANCELLED` and `TIMED_OUT` return `Err`. Other statuses sleep and loop, and the loop checks the deadline first.
- P-13.8-1, P-13.8-2, P-13.8-6. Evidence: test.

---

### 13.9 Training Run

**Goal:** Add the training run that implements `training.qnt`: check the change count, build and split the set, train, evaluate both models on the held-out set, and swap or reject. The model improves without manual steps and never gets worse.

- **Implements:** G3, G16, G19. **Depends on:** 13.8.
- **Files:** modify `src/app/training.rs`, `src/app/conversation.rs`, `src/main.rs`, `jarvis.toml`.
- **Quint:** `training.qnt` (conformance).

**Model.** Each `check_interval_s`, if `should_train` and no run is active, start a run:

1. `refresh` the generated examples.
2. Build the training set. Upload only the examples that are not held out.
3. `ProcessTrainer::train`.
4. `ModelLoader::load` the new folder.
5. Predict the held-out set with the old and the new classifier.
6. If `accept`, swap the classifier and write `current` in the training state. Else delete the new folder.
7. Log both accuracies. Set `last_time := now` and `habit_keys := established`.

On a final error, the old model stays and the training state does not change. This sub-phase adds `train.check_interval_s`.

**Acceptance criteria.**

- AC-13.9-1: When the change count is less than `min_changes`, `FakeTrainer` records no call.
- AC-13.9-2: When the change count reaches `min_changes`, `FakeTrainer` records one call.
- AC-13.9-3: When a run is active, a second trigger starts no run.
- AC-13.9-4: When the uploaded set is checked, it contains no held-out example.
- AC-13.9-5: When the new accuracy is equal or higher, the next `handle` uses the new classifier and the training state names the new run.
- AC-13.9-6: When the new accuracy is lower, the old classifier stays and the new folder does not exist.
- AC-13.9-7: When a run ends, accepted or rejected, the change count is 0.
- AC-13.9-8: When the trainer returns an error, the old classifier stays and the change count does not change.
- AC-13.9-9: When `quint-connect` replays 100 traces of `training.qnt` with `RUNPOD = false`, the run state, the change count and the accuracy of the active model match the model after each step.
- AC-13.9-10: When `ModelLoader::load` is blocked during a run, `handle` returns before virtual time advances.

**Proofs.**

- P-13.9-3. Requires: `OneRun` (P-13.4-2). Argument: the start is guarded by an atomic compare-and-swap on the `active` flag from false to true.
- P-13.9-4. Argument: the upload filters `not held_out(text)` (P-13.2-1), and evaluation uses the complement.
- P-13.9-5, P-13.9-6. Requires: `SwapNeverWorse`. Argument: step 6 swaps only when `accept(new, old)`, which is `new >= old`.
- P-13.9-7, P-13.9-8. Requires: `CountResetsAfterRun`, `FailureKeepsModel`. Argument: step 7 runs after both branches of step 6. The error path returns before step 6.
- P-13.9-1, P-13.9-2. Argument: the start condition is `should_train` (P-13.3-3).
- P-13.9-10. Argument: `handle` reads the classifier with one `ArcSwap::load`, which never waits for a writer.
- P-13.9-9. Evidence: test.

---

### 13.10 Training Fallback

**Goal:** Send the same training set to RunPod when the local run fails. A run that does not fit in 6 GB of VRAM still completes, and live STT never pauses for training.

- **Implements:** G16, G18, G19. **Depends on:** 13.9.
- **Files:** modify `src/app/training.rs`, `src/main.rs`.
- **Quint:** `training.qnt` (conformance with `RUNPOD = true`).

**Model.** Step 3 of the run becomes: call the process trainer. If it returns an error and `[train.runpod]` is set, call the RunPod trainer with the same run ID and set.

**Acceptance criteria.**

- AC-13.10-1: When the process trainer returns an error, `FakeRunPodTrainer` records one call with the same set.
- AC-13.10-2: When the process trainer succeeds, `FakeRunPodTrainer` records no call.
- AC-13.10-3: When the process trainer returns an error and `[train.runpod]` is not set, the old classifier stays and the log names the local error.
- AC-13.10-4: When `quint-connect` replays 100 traces of `training.qnt` with `RUNPOD = true`, the state matches the model after each step.
- AC-13.10-5 [GPU host]: During a local training run, all Phase 9.2 latency tests pass and the peak VRAM use is 5.8 GB or less.

**Proofs.**

- P-13.10-1 to P-13.10-3. Requires: `FallbackOnce`. Argument: the RunPod call is in the `Err` arm of the process call, guarded by `runpod.is_some()`, and it is not in a loop.
- P-13.10-4, P-13.10-5. Evidence: test.

---

### 13.11 Voice Trigger

**Goal:** Add the `train` intent kind and the `start_training` intent. You can say "Start training" to start a run at any change count.

- **Implements:** G16. **Depends on:** 13.10.
- **Files:** modify `src/domain/decide.rs`, `src/app/conversation.rs`, `intents.toml`, `responses.toml`.

**Model.** `kind = "train"`: if no run is active, start one (the `voiceStart` action) and reply from `start_training`. Else reply from `training_busy`.

**Acceptance criteria.**

- AC-13.11-1: When `FakeClassifier` gives `start_training` 0.9 and the change count is 0, Jarvis replies from `start_training` and `FakeTrainer` records one call.
- AC-13.11-2: When a run is active, Jarvis replies from `training_busy` and `FakeTrainer` records no second call.
- AC-13.11-3: When a voice-started run has a lower accuracy, the old classifier stays.

**Proofs.**

- P-13.11-1, P-13.11-2. Requires: `OneRun`. Argument: the handler uses the same compare-and-swap as P-13.9-3. Success replies from `start_training`, failure from `training_busy`.
- P-13.11-3. Argument: the voice run uses the same run function, so P-13.9-6 applies.

## Field Log

These steps are not tests and gate nothing.

1. On the GPU host, make `.venv` with Python 3.12 and install `./train`.
2. Say "Start training". Record that the run uses the GPU and that the log shows both accuracies and the decision.
3. In RunPod, make a network volume, an S3 API key and a serverless endpoint with `ghcr.io/<owner>/jarvis-train:<tag>`. Set `[train.runpod]`.
4. Set a base model that is too large for 6 GB. Say "Start training". Record that the run moves to RunPod. Set `train.base_model` back.
5. Repeat the Phase 9 field log with the trained model.
