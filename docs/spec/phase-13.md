# Phase 13: Training

This phase improves the System One model. Jarvis builds the training set, trains on the GPU host (RunPod when local training fails), mutes the satellites and pauses itself during a local run, and keeps a new model only when it is equal or better.

---

### 13.1 Mark Examples

**Goal:** Turn marks into labeled examples. Your marks become training data.

- **Implements:** G15, G16. **Depends on:** 12.9g.
- **Files:** modify `src/domain/training.rs`.

**Model.**

- `mark_examples(marked) -> Vec<Example>`. `marked` holds the marked interactions and the first interaction that each marked dialog answer links to (the caller gets it with `get`, sub-phase 12.5b).
  - A good action mark gives an `Intent` example for the selected intent, with the logged slots as spans.
  - A bad action mark with a correction gives an `Intent` example for the correction, with `spans = None`.
  - A bad action mark without a correction gives a `Not` example for the selected intent, with `spans = None`.
  - A bad action mark with the correction `unknown` gives an `Intent(unknown)` example.
  - A good action mark on a System Two interaction gives an `Intent(unknown)` example with `spans = None`, so System One keeps sending it to System Two.
  - An interaction without an action mark gives no example.
  - A mark example uses the normalized transcript and the logged model context.
  - A mark on a dialog answer (an interaction with `link`) gives the example with the normalized transcript and the context of the first interaction of the dialog, and `spans = None`. The answer alone ("the kitchen") is not a request. When `marked` does not hold the first interaction, the mark gives no example.
- Request habits are not a model input (`01-architecture.md` decision 9), so `sample_context` (sub-phase 3.3) does not change.

**Acceptance criteria.**

- AC-13.1-1: When an interaction has a good action mark for `lights_on`, `mark_examples` returns a `lights_on` example with the logged spans and context.
- AC-13.1-2: When an interaction has a bad action mark with the correction `tv_show`, `mark_examples` returns a `tv_show` example with `spans = None`.
- AC-13.1-3: When an interaction has a bad action mark without a correction, `mark_examples` returns a `Not` example for the selected intent.
- AC-13.1-4: When an interaction has no action mark, `mark_examples` returns no example.
- AC-13.1-6: When a bad action mark has the correction `unknown`, or a good action mark is on a System Two interaction, `mark_examples` returns an `Intent(unknown)` example.
- AC-13.1-7: When a good action mark is on a dialog answer, the example has the text and the context of the first interaction and `spans = None`.

AC-13.1-5 moves to sub-phase 13.1b as AC-13.1b-1.

**Proofs.**

- P-13.1-1 to P-13.1-4, P-13.1-6. Ensures: `mark_examples` is a total function of the action mark and the decision. Argument: it is a `filter_map` over the interactions with a `match` on `(value, correction, decision)`. The arms are `(Good, _, System Two)`, `(Good, _, _)`, `(Bad, Some(k), _)` (also for `k = unknown`), `(Bad, None, _)` and no mark. They cover every case once.
- P-13.1-7. Ensures: an example from a dialog answer has the text and the context of the first interaction and no spans. Argument: before the `match`, the builder replaces the text and the context with those of the interaction that `link` names, and sets `spans = None`.

---

### 13.1b Training Set

**Goal:** Build the full training set from seeds, generated examples and marks. Sub-phase 14.11 adds the active learned intents.

- **Implements:** G16. **Depends on:** 13.1.
- **Files:** modify `src/domain/training.rs`.

**Model.**

- `training_set(catalog, generated, marked, areas, rng, settings) -> TrainingSet`: the examples of `seed_set` (sub-phase 3.3b), `generated` (labeled examples) and `mark_examples(marked)`. The text of each example is normalized, so the builder compares the texts.
- Precedence: a mark is higher than a seed, and a seed is higher than a generated example. Of two marks, the newer mark is higher. Of two examples with the same source and time, the later one in the order generated, seeds, marks is higher.
- The set keeps an example only when no example with the same text and a different label is higher.

**Acceptance criteria.**

- AC-13.1b-1: When a mark and a seed have the same normalized text and different labels, the training set has only the mark example.

**Proofs.**

- P-13.1b-1 (AC-13.1b-1). Ensures: of the examples with the same normalized text and different labels, the set keeps only the one with the highest precedence: the newest mark, then a seed, then a generated example. Argument: the builder keeps an example only when no other example has the same text, a different label and a higher key `(source rank, mark time, position)`.

---

### 13.2 Held-Out Split and Acceptance

**Goal:** Add the accuracy measures and the accept rule on the held-out split of sub-phase 3.3. A new model can be compared fairly with the old one on the same examples.

- **Implements:** G16. **Depends on:** 13.1b.
- **Files:** modify `src/domain/training.rs`.

**Model.**

- `held_out` is in sub-phase 3.3.
- `metrics(predictions, examples, threshold) -> Metrics { intent, slots }`. `intent`: an `Intent(k)` example is correct when the top label is `k` with a probability of `threshold` or more. An `Intent(unknown)` example is also correct when the top probability is less than `threshold`. A `Not(k)` example is correct when the top label is not `k`, or its probability is less than `threshold`. `slots`: the share of examples with spans whose decoded slots equal the spans.
- `accept(new, old) -> bool = new.intent >= old.intent and new.slots >= old.slots`. Thus "never worse" means never worse on these two measures.

**Acceptance criteria.**

- AC-13.2-1: When the top label of an `Intent(k)` example is `k` with a probability less than `threshold`, `metrics` counts it as wrong.
- AC-13.2-2: When the decoded slots of an example differ from its spans, `metrics` counts it as wrong in `slots`.
- AC-13.2-3: When a `Not` example has a top label other than its label, `metrics` counts it as correct.
- AC-13.2-4: When both new measures equal the old ones, `accept` returns true. When the new intent accuracy is higher but the new slot accuracy is lower, it returns false.

**Proofs.**

- P-13.2-1, P-13.2-3. Ensures: an `Intent(k)` example is correct if and only if the top label is `k` with a probability of `threshold` or more (or, for `unknown`, the top probability is less than `threshold`). A `Not(k)` example is correct if and only if the top label is not `k` or its probability is less than `threshold`. Argument: each arm of `metrics` is the comparison in the model, with the threshold test.
- P-13.2-4. Ensures: `accept(new, old)` is true if and only if `new.intent >= old.intent` and `new.slots >= old.slots`. Argument: `accept` is the conjunction of two `>=`.
- P-13.2-2 (AC-13.2-2). Ensures: an example with spans is correct in `slots` if and only if its decoded slots equal its spans. Argument: the slot measure compares the two sets for each example with spans and counts the equal ones.

---

### 13.3 Change Count

**Goal:** Add the training state and the change count: new action marks since the last run. Jarvis knows when enough has changed to train. Sub-phase 14.11 adds the new active learned intents.

- **Implements:** G16. **Depends on:** 13.2.
- **Files:** modify `src/domain/training.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `TrainingState { current: Option<run_id>, last_time }`. `changes(state, marked) = |action marks with a mark time after last_time|`. Request habits are not training data (`01-architecture.md` decision 9), so they do not count. `should_train(count, min_changes) = count >= min_changes`. This sub-phase adds `train.state` and `train.min_changes`.

**Acceptance criteria.**

- AC-13.3-1: When 20 action marks occur after the last run and 3 before it, `changes` returns 20.
- AC-13.3-2: When the count is `min_changes - 1`, `should_train` returns false. At `min_changes`, it returns true.

**Proofs.**

- P-13.3-1. Ensures: `changes` is the number of action marks after `last_time`. Argument: `changes` is the size of the marks filtered by time.
- P-13.3-2. Ensures: `should_train(c, m)` is true if and only if `c >= m`. Argument: `should_train` is `>=`.

---

### 13.4 Training Quint Model

**Goal:** Add `quint/training.qnt`, the model of the trigger, the run, the local and RunPod attempts, the pause, the accept rule and the swap. Prove that only one run occurs at a time, that Jarvis pauses only for a local attempt, and that a swap never installs a worse model.

- **Implements:** G16, G19. **Depends on:** 13.3.
- **Files:** create `quint/training.qnt`.
- **Quint:** `training.qnt`.

**Model.**

- Constants: `MIN = 2`, `ACC = Set(0, 1, 2)`, `RUNPOD: bool`.
- State: `changes`, `during` (changes since the run started), `run: Idle | Local | RunPod`, `paused` (Jarvis has released its models for the local attempt), `starts`, `ends`, `modelAcc`, `prevAcc`, `swaps: List[(old, new)]`, `attempts`, `lastAction`.
- Actions: `change` (`changes += 1`, and `during += 1` when `run != Idle`), `autoStart` (if `Idle` and `changes >= MIN`: `run := Local`, `paused := true`, `starts += 1`, `during := 0`), `voiceStart` (if `Idle`: the same), `localDone(a)`, `localFail` (if `RUNPOD`: `run := RunPod`, else end with failure), `runpodDone(a)`, `runpodFail`. `localDone` and `localFail` also set `paused := false`. Each action first sets `prevAcc := modelAcc` and `lastAction`.
- A done action with accuracy `a`: if `a >= modelAcc`, append `(modelAcc, a)` to `swaps` and set `modelAcc := a`. Then `run := Idle`, `ends += 1` and `changes := during`.
- A final failure: `run := Idle`, `ends += 1`, and `changes` and `modelAcc` do not change.
- Invariants:
  - `OneRun`: `starts - ends <= 1`.
  - `SwapNeverWorse`: for every `(old, new)` in `swaps`, `new >= old`.
  - `CountResetsAfterRun`: when `lastAction` is a done action, `changes = during`. Changes that arrive during a run stay counted.
  - `FailureKeepsModel`: when `lastAction` is a final failure, `modelAcc = prevAcc`.
  - `FallbackOnce`: one run has at most one RunPod attempt, and only after a local failure.
  - `PauseOnlyLocal`: `paused` if and only if `run = Local`.

**Acceptance criteria.**

- AC-13.4-1 [Quint]: `quint typecheck` passes.
- AC-13.4-2 [Quint]: All six invariants hold in simulation and to 10 steps in `quint verify`, with `RUNPOD` true and false.
- AC-13.4-3 [Quint]: The test `ac_13_4_3_rejected_run_resets_count` passes: a run with a lower accuracy keeps the model and sets `changes` to the changes made during the run.

**Proofs.**

- P-13.4-2. `OneRun`: both start actions are guarded by `run = Idle` and increment `starts`. Every action that sets `run := Idle` from a run increments `ends`, so `starts - ends` is 1 during a run and 0 otherwise. `SwapNeverWorse`: the only append to `swaps` is guarded by `a >= modelAcc` and appends `(modelAcc, a)`. `CountResetsAfterRun`: both done actions end with `changes := during`. `FailureKeepsModel`: each action sets `prevAcc := modelAcc` first, and the failure actions write neither `modelAcc` nor `swaps`. `FallbackOnce`: `run := RunPod` occurs only in `localFail`, which is enabled only when `run = Local`, and no action sets `run := Local` from `RunPod`. `PauseOnlyLocal`: the two start actions are the only actions that set `run := Local`, and they set `paused := true`. `localDone` and `localFail` are the only actions that move `run` away from `Local`, and they set `paused := false`. No other action writes `paused`.
- P-13.4-1, P-13.4-3. Evidence: model check.

---

### 13.5 Paraphrase Generation

**Goal:** Add `paraphrase` to the `Reasoner` port and `validate_example`. Each intent gets many reworded examples, and `unknown` gets requests that match no intent.

- **Implements:** G16. **Depends on:** 13.4.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/domain/intent.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `paraphrase(target, seeds, slots, n) -> Result<Vec<String>>` with structured output `{ "examples": [string] }`. For an intent, the prompt has the seed examples that are not held out and the slot names, and asks for the same markup. For `unknown`, the prompt has the intent names and examples, and asks for requests that match none. `validate_example(markup, intent, seeds)`: parses, has normalized text (sub-phase 3.2, check 5), uses only the intent's slots, and is not equal to a seed after `normalize`. Both calls use `bulk_max_output_tokens`. This sub-phase adds `train.paraphrases` and `train.unknown_paraphrases`.

**Acceptance criteria.**

- AC-13.5-1: When the prompt builder runs for `lights_on`, the prompt contains the seed examples and the slot name `area`.
- AC-13.5-2: When the mock returns `{"examples": ["turn the [den](area) lights on"]}`, `paraphrase` returns that example.
- AC-13.5-3: When a generated example uses a slot that the intent does not have, `validate_example` rejects it.
- AC-13.5-4: When a generated example equals a seed after `normalize`, `validate_example` rejects it.
- AC-13.5-5: When a generated example has a markup error, `validate_example` rejects it.
- AC-13.5-6: When a seed is held out, the paraphrase prompt does not contain it.
- AC-13.5-7: When a generated example has a capital letter or punctuation, `validate_example` rejects it.

**Proofs.**

- P-13.5-3 to P-13.5-5, P-13.5-7. Ensures: `validate_example` returns `Ok` if and only if the markup parses, the text is normalized, the slots are the intent's slots and the text is not a seed. Argument: `validate_example` is the conjunction of `parse_example` (P-3.1b-3), a comparison of the text with its `normalize` result, a subset check on the slot names, and a set membership check on normalized seeds.
- P-13.5-1, P-13.5-2, P-13.5-6. Evidence: test.

---

### 13.6 Generated Example Store

**Goal:** Store generated examples in `generated.json` and regenerate only for changed seeds or a changed intent list. Training does not call OpenAI again for examples it already has.

- **Implements:** G16. **Depends on:** 13.5.
- **Files:** create `src/app/training.rs`. Modify `src/lib.rs`, `jarvis.toml`.

**Model.** `Generated { intents: Map<name, { seed_hash, examples }>, unknown: { intents_hash, examples } }`. `refresh(catalog, generated)`: for each intent whose `seed_hash` differs from the hash of its seeds, call `paraphrase` and keep the valid results. If the hash of the intent list differs, regenerate `unknown`. Remove the entries of intents that are not in the catalog. Sub-phase 14.11 adds the active learned intents. Save through `Store`. This sub-phase adds `train.generated`.

**Acceptance criteria.**

- AC-13.6-1: When the seeds of `lights_on` did not change, `FakeReasoner` records no `paraphrase` call for it.
- AC-13.6-2: When the seeds of `lights_on` change, `FakeReasoner` records one `paraphrase` call for `lights_on` and none for other intents.
- AC-13.6-3: When the intent list changes, `FakeReasoner` records one `paraphrase` call for `unknown`.
- AC-13.6-4: When `paraphrase` returns an error for one intent, the stored examples of that intent do not change.
- AC-13.6-5: When an intent is removed from `intents.toml`, `refresh` removes its generated examples.

**Proofs.**

- P-13.6-1 to P-13.6-3. Argument: `refresh` calls `paraphrase` for an intent only inside `if hash(seeds) != stored.seed_hash`, and for `unknown` only inside `if hash(names) != stored.intents_hash`.
- P-13.6-4. Argument: the store entry is replaced only in the `Ok` branch.
- P-13.6-5. Argument: `refresh` keeps only the keys that are in the label list.

---

### 13.7 Trainer Port and Process Adapter

**Goal:** Add the `Trainer` port and `ProcessTrainer`, which runs `train/train.py` on the GPU host. Jarvis can train a model locally from a training set.

- **Implements:** G16, G18. **Depends on:** 13.6.
- **Files:** create `src/adapters/process.rs`. Modify `src/ports.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `jarvis.toml`.

**Model.** `Trainer::train(run_id, set) -> Result<PathBuf>`. `ProcessTrainer` writes the set to `<model.dir>/<run_id>/train.json` and runs `<train.python> train/train.py --data <file> --out <model.dir>/<run_id> --device <device> --max-vram-gb <train.max_vram_gb>`. A non-zero exit gives an error with the last 20 lines of the error output. After `train.timeout_min`, the adapter kills the process and returns a timeout error. On any error, it deletes `<model.dir>/<run_id>/`. This sub-phase adds `train.python`, `train.timeout_min` and `train.max_vram_gb`.

**Acceptance criteria.**

- AC-13.7-1: When the script exits with code 0, `train` returns `<model.dir>/<run_id>/`.
- AC-13.7-2: When the script exits with code 1 and prints "CUDA out of memory", `train` returns an error that contains that text.
- AC-13.7-3: When `train` runs, the script receives `--device` with the configured device and `--max-vram-gb` with `train.max_vram_gb`.
- AC-13.7-4: When the script runs longer than `timeout_min`, `train` kills it and returns a timeout error.
- AC-13.7-5: When `train` returns an error, `<model.dir>/<run_id>/` does not exist.

**Proofs.**

- P-13.7-1, P-13.7-2. Argument: `train` matches on the exit status, with `Ok(dir)` for success and `Err(tail of error output)` otherwise.
- P-13.7-3 to P-13.7-5. Evidence: test.

---

### 13.8 RunPod Handler

**Goal:** Add the RunPod handler to `train.py` and make it the command of the training image. A training set on the network volume can now run on a RunPod GPU.

- **Implements:** G16. **Depends on:** 13.7.
- **Files:** modify `train/Dockerfile`, `train/train.py`, `train/test_train.py`.

**Model.** `train.py --serve` starts the handler. For a job with the input `run_id`, it reads `/runpod-volume/jarvis/runs/<run_id>/train.json` and writes the model files to `.../model/`. The root folder of the volume is a parameter, so tests use a temporary folder. The image command in `train/Dockerfile` becomes `python train.py --serve`.

**Acceptance criteria.**

- AC-13.8-1: When the handler runs with a temporary volume root, it reads `train.json` and writes the three model files.

**Proofs.**

- P-13.8-1. Evidence: test.

**Field log.** The training image is private. In RunPod, add a container registry credential for `ghcr.io` to the endpoint template. Record that a job pulls the image.

---

### 13.8b RunPod Adapter

**Goal:** Add `RunPodTrainer`, which sends a training set to the RunPod handler through the network volume and the RunPod REST API. The training run gets a second `Trainer` for the fallback.

- **Implements:** G16. **Depends on:** 13.8.
- **Files:** create `src/adapters/runpod.rs`. Modify `src/lib.rs`, `Cargo.toml`.

**Model.**

- `RunPodTrainer::train(run_id, set)`:
  1. Upload the set to `jarvis/runs/<run_id>/train.json` on the volume.
  2. `POST /v2/<endpoint_id>/run` with `{ "input": { "run_id": "<run_id>" } }`.
  3. Poll `GET /v2/<endpoint_id>/status/<job_id>` each `poll_s`.
  4. On `COMPLETED`, download `jarvis/runs/<run_id>/model/` to `<model.dir>/<run_id>/`.
  5. After `timeout_min`, send `POST /v2/<endpoint_id>/cancel/<job_id>` and return a timeout error.
  6. At the end, success or error, delete `jarvis/runs/<run_id>/` from the volume. A failed delete logs one `warn` event and does not change the result, because a left object costs only volume space.
- The adapter gets an `object_store` instance. Tests use `InMemory`. Sub-phase 13.8c adds the production instance.
- The download writes into a temporary folder in `<model.dir>` and renames it to `<model.dir>/<run_id>/`. Thus after an error, `<model.dir>/<run_id>/` does not exist (the `Trainer` contract).
- The poll loop checks the deadline between requests and sleeps only between them. Thus no timer runs while a request waits for the socket, and the tests can pause the clock (`02-rules.md` section 6.2).

**Acceptance criteria.**

- AC-13.8b-1: When `train` runs, the store contains `jarvis/runs/<run_id>/train.json` with the set.
- AC-13.8b-2: When `train` runs, the mock receives `POST /v2/<endpoint_id>/run` with the run ID and the bearer key.
- AC-13.8b-3: When the status is `IN_QUEUE`, then `IN_PROGRESS`, then `COMPLETED`, `train` downloads the model files into `<model.dir>/<run_id>/`.
- AC-13.8b-4: When the status is `FAILED`, `train` returns an error with the job error text.
- AC-13.8b-5: When the job runs longer than `timeout_min`, `train` sends `cancel` and returns a timeout error.
- AC-13.8b-6: When `train` ends, with success or an error, the store has no object under `jarvis/runs/<run_id>/`.

**Proofs.**

- P-13.8b-3 to P-13.8b-5. Argument: the poll loop matches on the status. `COMPLETED` downloads and returns. `FAILED`, `CANCELLED` and `TIMED_OUT` return `Err`. Other statuses sleep and loop, and the loop checks the deadline first.
- P-13.8b-1, P-13.8b-2, P-13.8b-6. Evidence: test.

---

### 13.8c RunPod Configuration

**Goal:** Add `[train.runpod]`, its secrets and the production `RunPodTrainer`. The training run can build the fallback trainer from `jarvis.toml`.

- **Implements:** G16. **Depends on:** 13.8b.
- **Files:** modify `src/adapters/runpod.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- This sub-phase adds the optional section `[train.runpod]` (`01-architecture.md` section 9). `Secrets` resolves `api_key_env`, `s3_key_env` and `s3_secret_env`, as it resolves `[mqtt]`. Without the section, Jarvis has no training fallback and needs no RunPod variable. With it, a missing variable stops the start with the variable name.
- In `jarvis.toml`, each line of the section starts with `# `, so Jarvis starts without a RunPod account. To use RunPod, remove the `# ` and set the variables.
- `RunPodTrainer::try_new(config, keys, dir)` gives the production trainer: the REST API `https://api.runpod.ai`, and `AmazonS3` with `s3_endpoint`, `s3_region`, the bucket `volume_id` and the S3 keys. `poll_s` is the poll time and `timeout_min` is the timeout.
- Nothing builds the trainer in `run` before sub-phase 13.10.

**Acceptance criteria.**

- AC-13.8c-1: When `jarvis.toml` has no `[train.runpod]` section and no RunPod variable is set, `Secrets::resolve` succeeds and gives no RunPod keys.
- AC-13.8c-2: When the section of `jarvis.toml` is set without `# ` and `RUNPOD_S3_SECRET` is not set, `Secrets::resolve` returns an error that names `RUNPOD_S3_SECRET`.
- AC-13.8c-3: When `try_new` gets the section of `jarvis.toml`, it returns a trainer with a poll time of 30 s and a timeout of 60 min.

**Proofs.**

- P-13.8c-1 to P-13.8c-3. Evidence: test.

---

### 13.9 Training Run

**Goal:** Add the first half of the training run of `training.qnt`: build and split the set, and train on the examples that are not held out. A run never trains on the examples that measure it.

- **Implements:** G3, G16. **Depends on:** 13.8c.
- **Files:** modify `src/app/training.rs`, `src/testing/fakes.rs`.

**Model.** `Training::run` does steps 0 to 3 of one run. Sub-phase 13.9b adds steps 4 to 7.

0. Keep `started := now`.
1. `refresh` the generated examples.
2. Build the training set with `training_set` from the catalog, the generated examples, the marked interactions with the first interaction of each marked dialog answer (`get`), the area names of the hot cache and an RNG with the seed `started` in seconds. The generated examples are the seeds of a copy of the catalog whose intents have the generated examples (`seed_set`). Upload only the examples that are not held out. When the held-out set is empty, stop with an error before training.
3. `train` of the local `Trainer` with the run ID `started` in the format `%Y%m%d-%H%M%S`. Sub-phase 13.10e mutes the satellites and pauses Jarvis for this call.

- `FakeTrainer` records each call, waits while a test holds its gate, and gives the scripted results in order. A success creates `<dir>/<run_id>/`.

**Acceptance criteria.**

- AC-13.9-4: When the uploaded set is checked, it contains no held-out example.
- AC-13.9-13: When the held-out set is empty, the run stops with an error and `FakeTrainer` records no call.

AC-13.9-5 to AC-13.9-8, AC-13.9-10 and AC-13.9-11 move to sub-phase 13.9b as AC-13.9b-1 to AC-13.9b-6. AC-13.9-1 to AC-13.9-3, AC-13.9-12 and AC-13.9-9 move to sub-phase 13.9c as AC-13.9c-1 to AC-13.9c-5. AC-13.9-14 moves to sub-phase 13.9d as AC-13.9d-1.

**Proofs.**

- P-13.9-4. Argument: the upload filters `not held_out(text)` (P-3.3-8), and evaluation uses the complement.
- P-13.9-13. Evidence: test.

---

### 13.9b Training Swap

**Goal:** Add the second half of the training run: evaluate both models on the held-out set, and swap or reject. The model never gets worse.

- **Implements:** G3, G16, G19. **Depends on:** 13.9.
- **Files:** modify `src/app/training.rs`, `src/domain/training.rs`, `src/testing/fakes.rs`.

**Model.** `Training::run` gets steps 4 to 7:

4. `ModelLoader::load` the new folder.
5. Predict the held-out set with the old and the new classifier.
6. Log both measures and the decision. If `accept`, set `current :=` the new run. Else delete the new folder.
7. Save the training state with `last_time := started`. Thus a change during the run counts for the next run. After the save of an accepted run, swap the classifier and delete the model folders other than the current and the previous one. A failed delete logs one `warn` event.

- On a final error, the old model stays and the training state does not change.
- `Training::changes` gives the change count: `changes` (sub-phase 13.3) of the saved training state and the marked interactions.
- The `Store` of `train.state` holds `TrainingState` as JSON. Without the file, the state has no `current`, and `last_time` is the Unix epoch.
- `FakeModelLoader` gets a gate that blocks `load`.

**Acceptance criteria.**

- AC-13.9b-1: When both new measures are equal or higher, the next `predict` of the `Classifier` wrapper of sub-phase 5.4 uses the new classifier, and the training state names the new run. Thus the next `handle` uses the new classifier (AC-5.4-4).
- AC-13.9b-2: When one new measure is lower, the old classifier stays and the new folder does not exist.
- AC-13.9b-3: When a run ends, accepted or rejected, and no change occurred during the run, the change count is 0.
- AC-13.9b-4: When the trainer returns an error, the old classifier stays and the change count does not change.
- AC-13.9b-5: When `ModelLoader::load` is blocked during a run, `predict` of the `Classifier` wrapper returns before virtual time advances. `handle` reads the classifier only through it.
- AC-13.9b-6: When a mark arrives during a run, the change count after the run is 1.

**Proofs.**

- P-13.9b-1, P-13.9b-2. Requires: `SwapNeverWorse`. Argument: step 6 swaps only when `accept(new, old)`, which is `new >= old`.
- P-13.9b-3, P-13.9b-4. Requires: `CountResetsAfterRun`, `FailureKeepsModel`. Argument: step 7 runs after both branches of step 6. The error path returns before step 6.
- P-13.9b-5. Argument: `handle` reads the classifier through the `Classifier` wrapper of sub-phase 5.4, which calls `ArcSwap::load` once. `load` never waits for a writer.
- P-13.9b-6. Evidence: test.

---

### 13.9c Training Trigger

**Goal:** Start a training run when the change count reaches `min_changes`, one run at a time, and wait after a failed run. The model improves without manual steps.

- **Implements:** G16, G19. **Depends on:** 13.9b.
- **Files:** modify `src/app/training.rs`, `quint/training.qnt`.
- **Quint:** `training.qnt` (conformance).

**Model.**

- `Trigger` has the training run of sub-phases 13.9 and 13.9b, an `active` flag, the backoff and the end time of the last failed run.
- `check`: if no run is active, no failed run ended less than the backoff ago, and `should_train(changes, min_changes)`, start a run.
- `voice`: start a run at any change count and without the backoff. Sub-phase 13.11 calls it.
- A start changes `active` from false to true with an atomic compare-and-swap and runs `Training::run` in a new task. The end of the run sets `active` to false. A failed run logs one `warn` event with the error and keeps its end time.
- `start_trigger(trigger, interval)` calls `check` each interval. Sub-phase 13.9d gives the interval and the backoff from `jarvis.toml`.
- `training.qnt` gets `initWith(runpod)`. `init` is `initWith` of a nondeterministic value, and `initLocal` is `initWith(false)`. The conformance test replays the traces from `initLocal`. The model has no backoff, so the driver uses a backoff of 0. AC-13.9c-4 tests the backoff.

**Acceptance criteria.**

- AC-13.9c-1: When the change count is less than `min_changes`, `FakeTrainer` records no call.
- AC-13.9c-2: When the change count reaches `min_changes`, `FakeTrainer` records one call.
- AC-13.9c-3: When a run is active, a second trigger starts no run.
- AC-13.9c-4: When a run fails, the next automatic run does not start before the backoff. A voice trigger starts a run at once.
- AC-13.9c-5: When `quint-connect` replays 100 traces of `training.qnt` from `initLocal`, the run state, the change count and the accuracy of the active model match the model after each step.

**Proofs.**

- P-13.9c-1, P-13.9c-2. Argument: the start condition is `should_train` (P-13.3-2).
- P-13.9c-3. Requires: `OneRun` (P-13.4-2). Argument: the start is guarded by an atomic compare-and-swap on the `active` flag from false to true.
- P-13.9c-4, P-13.9c-5. Evidence: test.

---

### 13.9d Training Start

**Goal:** Start the training trigger in `run`, and load the model of the last accepted run. Jarvis trains in production and keeps the trained model after a restart.

- **Implements:** G3, G16. **Depends on:** 13.9c.
- **Files:** modify `src/app/training.rs`, `src/app/conversation.rs`, `src/lib.rs`, `src/testing/e2e.rs`, `jarvis.toml`.

**Model.**

- `run` loads `<model.dir>/<current>` when the training state names `current`, else `<model.dir>/fixture` (sub-phase 5.4).
- `run` builds the training run with `ProcessTrainer`, `OnnxLoader`, the stores of `train.state` and `train.generated`, the log, the reasoner and the `Classifier` wrapper. It starts the trigger each `train.check_interval_s` with the backoff `train.failure_backoff_s`. This sub-phase adds `train.check_interval_s` and `train.failure_backoff_s`.
- The recorder logs the run name of the current classifier (`model`, sub-phase 12.4): the run of the training state at the start, and the new run after a swap.

**Acceptance criteria.**

- AC-13.9d-1: When a training state names `current`, `run` loads that folder.
- AC-13.9d-2: When a run swaps the classifier, the next logged interaction names the new run.

**Proofs.**

- P-13.9d-1, P-13.9d-2. Evidence: test.

---

### 13.10 Training Fallback

**Goal:** Send the same training set to RunPod when the local training run fails. A run that does not fit in the GPU of the GPU host still completes.

- **Implements:** G16, G19. **Depends on:** 13.9d.
- **Files:** modify `src/app/training.rs`, `src/lib.rs`, `src/testing/e2e.rs`, `quint/training.qnt`.
- **Quint:** `training.qnt` (conformance with `RUNPOD = true`).

**Model.**

- `Training` gets the optional RunPod `Trainer`. Step 3 of the run gets a fallback: if the local `Trainer` returns an error and the RunPod `Trainer` is set, log one `warn` event with the local error, and call the RunPod `Trainer` with the same run ID and set. Steps 4 to 7 then use its result. Without the RunPod `Trainer`, the run ends with the local error.
- `run` builds `RunPodTrainer::try_new` (sub-phase 13.8c) when `[train.runpod]` is set.
- `FakeRunPodTrainer` is a second `FakeTrainer`, which the tests give as the RunPod `Trainer`.
- `training.qnt` gets `initRunPod`, which is `initWith(true)`. The conformance driver replays the traces from `initLocal` (AC-13.9c-5) and from `initRunPod`. It gives the results of `runpodDone` and `runpodFail` to `FakeRunPodTrainer`. `runState` is `RunPod` while `FakeRunPodTrainer` has a call that waits. Sub-phase 13.10e adds `paused`.

**Acceptance criteria.**

- AC-13.10-1: When the process trainer returns an error, `FakeRunPodTrainer` records one call with the same run ID and set.
- AC-13.10-2: When the process trainer succeeds, `FakeRunPodTrainer` records no call.
- AC-13.10-3: When the process trainer returns an error and `[train.runpod]` is not set, the old classifier stays and the log names the local error.
- AC-13.10-4: When `quint-connect` replays 100 traces of `training.qnt` from `initRunPod`, the run state, the change count and the accuracy of the active model match the model after each step.

The mute switch parts of AC-13.10-6 and AC-13.10-9 move to sub-phase 13.10c as AC-13.10c-1 and AC-13.10c-2. AC-13.10-5 and AC-13.10-7 move to sub-phase 13.10d as AC-13.10d-1 and AC-13.10d-2. AC-13.10d-1 then moves to sub-phase 13.10f as AC-13.10f-1. AC-13.10-6, AC-13.10-8 and AC-13.10-9 move to sub-phase 13.10e as AC-13.10e-1 to AC-13.10e-3. The `paused` part of AC-13.10-4 moves to sub-phase 13.10e as AC-13.10e-4.

**Proofs.**

- P-13.10-1 to P-13.10-3. Requires: `FallbackOnce`. Argument: the RunPod call is in the `Err` arm of the local call, guarded by `runpod.is_some()`, and it is not in a loop.
- P-13.10-4. Evidence: test.

---

### 13.10c Mute Switches

**Goal:** Find the mute switch of each satellite in the hot cache. The training pause can then mute exactly the satellites that are not muted.

- **Implements:** G16. **Depends on:** 13.10.
- **Files:** modify `src/domain/state_cache.rs`, `src/domain/select.rs`, `src/testing/house.json`.

**Model.**

- The mute switch of a satellite is the `switch` entity whose entity ID ends in `_mute_microphones`, on the same device as the `assist_satellite` entity (FutureProofHomes Satellite1, ESPHome). `on` means muted.
- `mute_switches(cache) -> MuteSwitches { off, missing }` in `src/domain/state_cache.rs` gives the mute switches whose state is `off` and the satellites that have no mute switch. A switch that is `on` (muted by the user) or `unavailable` is in neither list.
- Section 11 of `01-architecture.md` gives the mute switches of the reference house. This sub-phase adds them to `src/testing/house.json`.

**Acceptance criteria.**

- AC-13.10c-1: When `mute_switches` gets the reference house, `off` is exactly the mute switch whose state is `off`, and `missing` is empty.
- AC-13.10c-2: When a satellite device has no mute switch, `missing` is exactly that satellite, and `off` still has the mute switch of another satellite that is `off`.

**Proofs.**

- P-13.10c-1, P-13.10c-2. Ensures: `off` is each `switch` entity with the state `off` and an entity ID that ends in `_mute_microphones`, on the device of an `assist_satellite` entity. `missing` is each `assist_satellite` entity that has no device, or whose device has no such `switch` in any state. Argument: `mute_switches` checks these predicates for each entity of the cache.

---

### 13.10d Live Models

**Goal:** Release the model sessions of Jarvis and load them again on request. A local training run can then use the full GPU of the GPU host.

- **Implements:** G16, G18. **Depends on:** 13.10c.
- **Files:** modify `src/ports.rs`, `src/app/training.rs`, `src/lib.rs`.

**Model.**

- The `Models` port has `release()` and `restore()`. `release` drops the model sessions and returns when no use of them is in progress. `restore` loads them again.
- `LiveModels` in `src/lib.rs` implements `Models`. It keeps the sessions of the STT first and second pass, the System One model and TTS in one `tokio::sync::RwLock`. The `Classifier` wrapper of sub-phase 5.4 and new `SpeechToText` and `TextToSpeech` wrappers in `src/lib.rs` take a read guard with `try_read` for each use. A `Listening` session keeps its guard until it ends. When `try_read` fails, the use returns an error at once. `release` takes the write guard, so it waits for the uses in progress, and drops the sessions. `restore` loads them with the load function of `run` and releases the guard.
- During the pause, `listen` fails, and the Wyoming adapter sends `error` (sub-phase 6.9). `handle` gets the error from `predict` and returns `Err` with a `failure` text (AC-5.3-5). Jarvis does not answer, and it keeps no request for later.
- When `restore` fails, `run` returns the error, as when a load fails at startup (AC-5.4-3).
- The training run swaps the System One model through the `Classifier` wrapper.
- `run` builds `LiveModels`. Nothing calls `release` in `run` before sub-phase 13.10e.

**Acceptance criteria.**

- AC-13.10d-2: When `LiveModels::release` returns, no reference to the STT, TTS or classifier fakes exists, and `listen` and `handle` return an error. After `restore`, `handle` answers. When a load fails, `restore` returns the error.

AC-13.10d-1 moves to sub-phase 13.10f as AC-13.10f-1.

**Proofs.**

- P-13.10d-2. Evidence: test.

---

### 13.10f GPU Release

**Goal:** Measure the free GPU memory after a release and the latency after a restore on the GPU host. A local training run can then use the full GPU, and Jarvis answers as fast after the run.

- **Implements:** G16, G18. **Depends on:** 13.10d.
- **Files:** modify `src/lib.rs`, `src/testing/e2e.rs`.

**Model.**

- The start of the app also gives its `LiveModels`. The pipeline harness of `src/testing/e2e.rs` waits for the pre-render. Then on CUDA, it calls `release` and `restore`, and serves the app. Thus each GPU latency test runs after a pause.
- The test of AC-13.10f-1 runs alone in a child process, as AC-6.2-5 does, because the other GPU tests change the memory of their process.

**Acceptance criteria.**

- AC-13.10f-1 [GPU host]: After `LiveModels::release`, Jarvis has no GPU session, and the free GPU memory is `train.max_vram_gb` + 0.5 GB or more, so a local run can use the full GPU. After `restore`, Jarvis answers again, and all Phase 9.2 latency tests pass.

**Proofs.**

- P-13.10f-1. Evidence: test.

---

### 13.10e Training Pause

**Goal:** Mute the satellites and pause Jarvis for a local training run. A local run gets the full GPU of the GPU host, and nobody talks to a Jarvis that cannot answer.

- **Implements:** G16, G19. **Depends on:** 13.10f.
- **Files:** modify `src/app/training.rs`, `src/lib.rs`, `src/testing/fakes.rs`.
- **Quint:** `training.qnt` (conformance with `paused`).

**Model.**

- `FakeModels` implements `Models`. It records the calls and can fail the next `restore`.
- `Training` gets the `Home` port and the `Models` port. Step 3 of the run becomes:
  1. Log one `warn` event for each satellite in `missing` of `mute_switches` (sub-phase 13.10c). Call `switch.turn_on` for each switch in `off`. A failed call logs one `warn` event with the switch and the error. Keep the switches whose call succeeded.
  2. `Models::release`.
  3. Call the local `Trainer`.
  4. `Models::restore`, after a success or an error of the local `Trainer`.
  5. Call `switch.turn_off` for each switch that step 1 kept, also when `restore` failed. A failed call logs one `warn` event with the switch and the error.
  6. The RunPod fallback of sub-phase 13.10. Jarvis answers during the RunPod call, and the satellites are not muted.
- When `restore` fails, the training task ends with the error after step 5 (sub-phase 13.10d).
- `run` gives the training run the `Home` port and `LiveModels`.
- The conformance driver compares `paused` with the release state of `FakeModels`. After a start action, it waits until the local `FakeTrainer` has the call.

**Acceptance criteria.**

- AC-13.10e-1: When the process trainer runs, `FakeHome` has recorded `switch.turn_on` for exactly the mute switches of the reference house that were `off`, and no announce, and `FakeModels` is released. After the process trainer returns, with success or an error, `FakeModels` records one `restore`, and `FakeHome` records `switch.turn_off` for exactly the same switches, before any `FakeRunPodTrainer` call.
- AC-13.10e-2: When the `switch.turn_on` call of a mute switch fails, the log has one `warn` event that names the switch, the process trainer runs, and `FakeHome` records no `switch.turn_off` for that switch. When a `switch.turn_off` call fails, the log has one `warn` event that names the switch, and when both new measures are equal or higher, the next `handle` uses the new classifier.
- AC-13.10e-3: When a satellite device has no mute switch, the log has one `warn` event that names the satellite, and `FakeHome` still records `switch.turn_on` for the mute switch of another satellite that is `off`.
- AC-13.10e-4: When `quint-connect` replays 100 traces of `training.qnt` from `initLocal` and from `initRunPod`, `paused` matches the release state of `FakeModels` after each step.

**Proofs.**

- P-13.10e-1. Requires: `PauseOnlyLocal`. Argument: step 3 awaits the mute calls, `release`, the local `Trainer`, `restore` and the unmute calls in sequence. `restore` and the unmute calls run after both arms of the local result, and the RunPod call comes after them. The unmute calls use the switches of step 1, which are a subset of `off` (P-13.10c-1).
- P-13.10e-2. Argument: step 1 keeps only the switches whose call returned `Ok`. Each failed call logs and does not return early.
- P-13.10e-3. Argument: step 1 logs each satellite of `missing` and calls each switch of `off` (P-13.10c-2).
- P-13.10e-4. Evidence: test.

---

### 13.10b Training Status

**Goal:** Show `training` in the status of Jarvis during a local training run, and make a training run and a save of the settings exclude each other. You see the pause in the Jarvis tab and in `sensor.jarvis_status`, and a restart never stops a run.

- **Implements:** G16, G24. **Depends on:** 13.10e.
- **Files:** modify `src/ports.rs`, `src/app/training.rs`, `src/lib.rs`.

**Model.**

- The training use case gets the sender of the status channel (sub-phase 12.9b). In step 3 of sub-phase 13.10e, it sets `Training` before the mute calls of step 1, and `Online` after the unmute calls of step 5, also after an error. Both changes set only `state`, so a rollback message stays (sub-phase 12.9f). A RunPod run does not change the status, because Jarvis answers during it.
- `Activity` gets `Training`. The shared activity of sub-phase 12.9f replaces the `active` flag of the training trigger (P-13.9c-3). A run starts only when it changes the activity from `Idle` to `Training`, and the end of the run sets `Idle`. Sub-phase 13.11 uses the same change for the voice trigger. Thus a run cannot start while a save restarts Jarvis, and `save` returns `Busy` during a run.

**Acceptance criteria.**

- AC-13.10b-1: When the process trainer runs, the status is `training`. After it returns, with success or an error, the status is `online`.
- AC-13.10b-2: When the RunPod trainer runs after a local error, the status is `online`.
- AC-13.10b-3: When a run is active, `save` returns `Busy`, and the configuration file does not change.
- AC-13.10b-4: When a save has changed the activity to `Restarting`, a training trigger starts no run, and `FakeTrainer` records no call. When a training trigger and a save run at the same time, exactly one of them gets the activity.

**Proofs.**

- P-13.10b-1, P-13.10b-2. Requires: `PauseOnlyLocal`. Argument: step 3 sends `Training` before step 1 and `Online` after step 5, in sequence, and both arms of the local result reach step 5. The RunPod call comes after step 5.
- P-13.10b-3, P-13.10b-4. Requires: `OneRun`. Argument: the start of a run and `save` both change the activity only from `Idle`, each in one critical section of the same mutex. Thus the second of them sees `Training` or `Restarting` and does nothing.

---

### 13.11 Voice Trigger

**Goal:** Add the `train` intent kind and the `start_training` intent. You can say "Start training" to start a run at any change count.

- **Implements:** G16. **Depends on:** 13.10b.
- **Files:** modify `src/domain/decide.rs`, `src/app/conversation.rs`, `intents.toml`, `responses.toml`.

**Model.** `kind = "train"`: if no run is active, start one (the `voiceStart` action, `voice` of sub-phase 13.9c) and reply from `start_training`. Else reply from `training_busy`.

**Acceptance criteria.**

- AC-13.11-1: When `FakeClassifier` gives `start_training` 0.9 and the change count is 0, Jarvis replies from `start_training` and `FakeTrainer` records one call.
- AC-13.11-2: When a run is active, Jarvis replies from `training_busy` and `FakeTrainer` records no second call.
- AC-13.11-3: When a voice-started run has a lower accuracy, the old classifier stays.

**Proofs.**

- P-13.11-1, P-13.11-2. Requires: `OneRun`. Argument: the handler uses the same compare-and-swap as P-13.9c-3. Success replies from `start_training`, failure from `training_busy`.
- P-13.11-3. Argument: the voice run uses the same run function, so P-13.9b-2 applies.

## Field Log

These steps are not tests and gate nothing.

1. On the GPU host, make `.venv` with Python 3.12 and install `./train` with `uv` (`01-architecture.md` section 10).
2. Say "Start training". Record that the satellites that were not muted show the mute state during the run, that a satellite that you muted before the run stays muted after it, that the run uses the GPU, that the other satellites are not muted and Jarvis answers after the run, and that the log shows both accuracies and the decision. Record that `sensor.jarvis_status` shows `training` during the run and `online` after it.
3. In RunPod, make a network volume, an S3 API key and a serverless endpoint with `ghcr.io/<owner>/jarvis-train:<tag>`. Allow only GPU types with compute capability 9.0 or lower, because torch 2.2.2 has no kernels for newer GPUs (sub-phase 3.5). Set `[train.runpod]`.
4. Set a base model that is too large for 6 GB. Say "Start training". Record that the run moves to RunPod. Set `train.base_model` back.
5. Repeat the Phase 9 field log with the trained model.
