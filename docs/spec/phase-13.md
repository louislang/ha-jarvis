# Phase 13: Training

This phase improves the System One model. Jarvis builds the training set, trains on the GPU host (RunPod when local training fails), and keeps a new model only when it is equal or better.

---

### 13.1 Mark Examples and Training Set

**Goal:** Turn marks into labeled examples and build the full training set from seeds, generated examples and marks. Your marks become training data. Sub-phase 14.11 adds the active learned intents.

- **Implements:** G15, G16. **Depends on:** 12.11.
- **Files:** modify `src/domain/training.rs`.

**Model.**

- `mark_examples(marked) -> Vec<Example>`:
  - A good action mark gives an `Intent` example for the selected intent, with the logged slots as spans.
  - A bad action mark with a correction gives an `Intent` example for the correction, with `spans = None`.
  - A bad action mark without a correction gives a `Not` example for the selected intent.
  - A bad action mark with the correction `unknown` gives an `Intent(unknown)` example.
  - A good action mark on a System Two interaction gives an `Intent(unknown)` example, so System One keeps sending it to System Two.
  - An interaction without an action mark gives no example.
  - A mark example uses the normalized transcript and the logged model context.
  - A mark on a dialog answer (an interaction with `link`) gives the example with the normalized transcript and the context of the first interaction of the dialog, and `spans = None`. The answer alone ("the kitchen") is not a request.
- `sample_context(areas, habits, rng)` now adds the habit summary of the sampled area at a random time.
- `training_set(catalog, generated, marked, areas, habits, rng, settings) -> TrainingSet`. When two examples have the same normalized text and different labels, the newest mark wins, then a seed, then a generated example.

**Acceptance criteria.**

- AC-13.1-1: When an interaction has a good action mark for `lights_on`, `mark_examples` returns a `lights_on` example with the logged spans and context.
- AC-13.1-2: When an interaction has a bad action mark with the correction `tv_show`, `mark_examples` returns a `tv_show` example with `spans = None`.
- AC-13.1-3: When an interaction has a bad action mark without a correction, `mark_examples` returns a `Not` example for the selected intent.
- AC-13.1-4: When an interaction has no action mark, `mark_examples` returns no example.
- AC-13.1-5: When `sample_context` gets a habit model, the context has the habit summary of the sampled area and time.
- AC-13.1-6: When a mark and a seed have the same normalized text and different labels, the training set has only the mark example.
- AC-13.1-7: When a bad action mark has the correction `unknown`, or a good action mark is on a System Two interaction, `mark_examples` returns an `Intent(unknown)` example.
- AC-13.1-8: When a good action mark is on a dialog answer, the example has the text and the context of the first interaction and `spans = None`.

**Proofs.**

- P-13.1-1 to P-13.1-4, P-13.1-7. Ensures: `mark_examples` is a total function of the action mark and the decision. Argument: it is a `filter_map` over the interactions with a `match` on `(value, correction, decision)`. The arms are `(Good, _, System Two)`, `(Good, _, _)`, `(Bad, Some(unknown), _)`, `(Bad, Some(k), _)`, `(Bad, None, _)` and no mark. They cover every case once.
- P-13.1-5 (AC-13.1-5). Ensures: the context is `context_text(area, summary(area, time, n))` for the sampled area and time. Argument: `sample_context` draws an area and a time from `rng`, then calls `context_text` with the summary (P-3.3-2).
- P-13.1-6 (AC-13.1-6). Ensures: of the examples with the same normalized text and different labels, the set keeps only the one with the highest precedence: the newest mark, then a seed, then a generated example. Argument: the builder groups the examples by normalized text and keeps the maximum of each group by the key `(source rank, mark time)`.
- P-13.1-8. Ensures: an example from a dialog answer has the text and the context of the first interaction and no spans. Argument: before the `match`, the builder replaces the text and the context with those of the interaction that `link` names, and sets `spans = None`.

---

### 13.2 Held-Out Split and Acceptance

**Goal:** Add the accuracy measures and the accept rule on the held-out split of sub-phase 3.3. A new model can be compared fairly with the old one on the same examples.

- **Implements:** G16. **Depends on:** 13.1.
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

**Goal:** Add the training state and the change count: new action marks and new established habits since the last run. Jarvis knows when enough has changed to train. Sub-phase 14.11 adds the new active learned intents.

- **Implements:** G16. **Depends on:** 13.2.
- **Files:** modify `src/domain/training.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `TrainingState { current: Option<run_id>, last_time, habit_keys: BTreeSet<HabitKey> }`. `changes(state, marked, established) = |action marks with a mark time after last_time| + |established \ state.habit_keys|`. Only action marks count, because a response mark gives no training example. `should_train(count, min_changes) = count >= min_changes`. This sub-phase adds `train.state` and `train.min_changes`.

**Acceptance criteria.**

- AC-13.3-1: When 12 action marks and 8 new established habits occur after the last run, `changes` returns 20.
- AC-13.3-2: When an established habit was in `habit_keys`, it does not count.
- AC-13.3-3: When the count is `min_changes - 1`, `should_train` returns false. At `min_changes`, it returns true.
- AC-13.3-4: When a response mark occurs after the last run, `changes` does not count it.

**Proofs.**

- P-13.3-1, P-13.3-2, P-13.3-4. Ensures: `changes` is the number of action marks after `last_time` plus the number of established keys that are not in `habit_keys`. Argument: `changes` is the sum of two set sizes. The first filters the marks by target and time. The second is a set difference, so keys in `habit_keys` are excluded.
- P-13.3-3. Ensures: `should_train(c, m)` is true if and only if `c >= m`. Argument: `should_train` is `>=`.

---

### 13.4 Training Quint Model

**Goal:** Add `quint/training.qnt`, the model of the trigger, the run, the local and RunPod attempts, the accept rule and the swap. Prove that only one run occurs at a time and that a swap never installs a worse model.

- **Implements:** G16, G19. **Depends on:** 13.3.
- **Files:** create `quint/training.qnt`.
- **Quint:** `training.qnt`.

**Model.**

- Constants: `MIN = 2`, `ACC = Set(0, 1, 2)`, `RUNPOD: bool`.
- State: `changes`, `during` (changes since the run started), `run: Idle | Local | RunPod`, `starts`, `ends`, `modelAcc`, `prevAcc`, `swaps: List[(old, new)]`, `attempts`, `lastAction`.
- Actions: `change` (`changes += 1`, and `during += 1` when `run != Idle`), `autoStart` (if `Idle` and `changes >= MIN`: `run := Local`, `starts += 1`, `during := 0`), `voiceStart` (if `Idle`: the same), `localDone(a)`, `localFail` (if `RUNPOD`: `run := RunPod`, else end with failure), `runpodDone(a)`, `runpodFail`. Each action first sets `prevAcc := modelAcc` and `lastAction`.
- A done action with accuracy `a`: if `a >= modelAcc`, append `(modelAcc, a)` to `swaps` and set `modelAcc := a`. Then `run := Idle`, `ends += 1` and `changes := during`.
- A final failure: `run := Idle`, `ends += 1`, and `changes` and `modelAcc` do not change.
- Invariants:
  - `OneRun`: `starts - ends <= 1`.
  - `SwapNeverWorse`: for every `(old, new)` in `swaps`, `new >= old`.
  - `CountResetsAfterRun`: when `lastAction` is a done action, `changes = during`. Changes that arrive during a run stay counted.
  - `FailureKeepsModel`: when `lastAction` is a final failure, `modelAcc = prevAcc`.
  - `FallbackOnce`: one run has at most one RunPod attempt, and only after a local failure.

**Acceptance criteria.**

- AC-13.4-1 [Quint]: `quint typecheck` passes.
- AC-13.4-2 [Quint]: All five invariants hold in simulation and to 10 steps in `quint verify`, with `RUNPOD` true and false.
- AC-13.4-3 [Quint]: The test `ac_13_4_3_rejected_run_resets_count` passes: a run with a lower accuracy keeps the model and sets `changes` to the changes made during the run.

**Proofs.**

- P-13.4-2. `OneRun`: both start actions are guarded by `run = Idle` and increment `starts`. Every action that sets `run := Idle` from a run increments `ends`, so `starts - ends` is 1 during a run and 0 otherwise. `SwapNeverWorse`: the only append to `swaps` is guarded by `a >= modelAcc` and appends `(modelAcc, a)`. `CountResetsAfterRun`: both done actions end with `changes := during`. `FailureKeepsModel`: each action sets `prevAcc := modelAcc` first, and the failure actions write neither `modelAcc` nor `swaps`. `FallbackOnce`: `run := RunPod` occurs only in `localFail`, which is enabled only when `run = Local`, and no action sets `run := Local` from `RunPod`.
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
- **Files:** create `src/adapters/runpod.rs`. Modify `src/lib.rs`, `src/testing/fakes.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `RunPodTrainer::train(run_id, set)`:
  1. Upload the set to `jarvis/runs/<run_id>/train.json` on the volume.
  2. `POST /v2/<endpoint_id>/run` with `{ "input": { "run_id": "<run_id>" } }`.
  3. Poll `GET /v2/<endpoint_id>/status/<job_id>` each `poll_s`.
  4. On `COMPLETED`, download `jarvis/runs/<run_id>/model/` to `<model.dir>/<run_id>/`.
  5. After `timeout_min`, send `POST /v2/<endpoint_id>/cancel/<job_id>` and return a timeout error.
  6. At the end, success or error, delete `jarvis/runs/<run_id>/` from the volume.
- The adapter gets an `object_store` instance. Tests use `InMemory`. Production uses `AmazonS3` with the RunPod endpoint and region, and the bucket `volume_id`.
- The poll loop checks the deadline between requests and sleeps only between them. Thus no timer runs while a request waits for the socket, and the tests can pause the clock (`02-rules.md` section 6.2).
- This sub-phase adds `[train.runpod]` and resolves its secrets.

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

### 13.9 Training Run

**Goal:** Add the training run that implements `training.qnt`: check the change count, build and split the set, train, evaluate both models on the held-out set, and swap or reject. The model improves without manual steps and never gets worse.

- **Implements:** G3, G16, G19. **Depends on:** 13.8b.
- **Files:** modify `src/app/training.rs`, `src/app/conversation.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `training.qnt` (conformance).

**Model.** Each `check_interval_s`, if `should_train` and no run is active, start a run:

0. Keep `started := now` and the established habit keys at this time.
1. `refresh` the generated examples.
2. Build the training set. Upload only the examples that are not held out. When the held-out set is empty, stop with an error before training.
3. `train` of the local `Trainer`.
4. `ModelLoader::load` the new folder.
5. Predict the held-out set with the old and the new classifier.
6. If `accept`, swap the classifier, write `current` in the training state, and delete the model folders other than the current and the previous one. Else delete the new folder.
7. Log both measures. Set `last_time := started` and `habit_keys :=` the keys of step 0. Thus a change during the run counts for the next run.

On a final error, the old model stays and the training state does not change. The next automatic run waits `train.failure_backoff_s`. A voice trigger does not wait. `run` now loads `<model.dir>/<current>` from the training state when it exists (sub-phase 5.4 loads the fixture). This sub-phase adds `train.check_interval_s` and `train.failure_backoff_s`.

**Acceptance criteria.**

- AC-13.9-1: When the change count is less than `min_changes`, `FakeTrainer` records no call.
- AC-13.9-2: When the change count reaches `min_changes`, `FakeTrainer` records one call.
- AC-13.9-3: When a run is active, a second trigger starts no run.
- AC-13.9-4: When the uploaded set is checked, it contains no held-out example.
- AC-13.9-5: When both new measures are equal or higher, the next `handle` uses the new classifier and the training state names the new run.
- AC-13.9-6: When one new measure is lower, the old classifier stays and the new folder does not exist.
- AC-13.9-7: When a run ends, accepted or rejected, and no change occurred during the run, the change count is 0.
- AC-13.9-8: When the trainer returns an error, the old classifier stays and the change count does not change.
- AC-13.9-9: When `quint-connect` replays 100 traces of `training.qnt` with `RUNPOD = false`, the run state, the change count and the accuracy of the active model match the model after each step.
- AC-13.9-10: When `ModelLoader::load` is blocked during a run, `handle` returns before virtual time advances.
- AC-13.9-11: When a mark arrives during a run, the change count after the run is 1.
- AC-13.9-12: When a run fails, the next automatic run does not start before `failure_backoff_s`. A voice trigger starts a run at once.
- AC-13.9-13: When the held-out set is empty, the run stops with an error and `FakeTrainer` records no call.
- AC-13.9-14: When a training state names `current`, `run` loads that folder.

**Proofs.**

- P-13.9-3. Requires: `OneRun` (P-13.4-2). Argument: the start is guarded by an atomic compare-and-swap on the `active` flag from false to true.
- P-13.9-4. Argument: the upload filters `not held_out(text)` (P-3.3-8), and evaluation uses the complement.
- P-13.9-5, P-13.9-6. Requires: `SwapNeverWorse`. Argument: step 6 swaps only when `accept(new, old)`, which is `new >= old`.
- P-13.9-7, P-13.9-8. Requires: `CountResetsAfterRun`, `FailureKeepsModel`. Argument: step 7 runs after both branches of step 6. The error path returns before step 6.
- P-13.9-1, P-13.9-2. Argument: the start condition is `should_train` (P-13.3-3).
- P-13.9-10. Argument: `handle` reads the classifier with one `ArcSwap::load`, which never waits for a writer.
- P-13.9-9, P-13.9-11 to P-13.9-14. Evidence: test.

---

### 13.10 Training Fallback

**Goal:** Send the same training set to RunPod when the local run fails. A run that does not fit in 6 GB of VRAM still completes, and live STT never pauses for training.

- **Implements:** G16, G18, G19. **Depends on:** 13.9.
- **Files:** modify `src/app/training.rs`, `src/lib.rs`.
- **Quint:** `training.qnt` (conformance with `RUNPOD = true`).

**Model.** Step 3 of the run becomes: call the local `Trainer`. If it returns an error and `[train.runpod]` is set, call the RunPod `Trainer` with the same run ID and set.

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
3. In RunPod, make a network volume, an S3 API key and a serverless endpoint with `ghcr.io/<owner>/jarvis-train:<tag>`. Allow only GPU types with compute capability 9.0 or lower, because torch 2.2.2 has no kernels for newer GPUs (sub-phase 3.5). Set `[train.runpod]`.
4. Set a base model that is too large for 6 GB. Say "Start training". Record that the run moves to RunPod. Set `train.base_model` back.
5. Repeat the Phase 9 field log with the trained model.
