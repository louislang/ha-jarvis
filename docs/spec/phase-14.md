# Phase 14: Learned Intents

This phase makes System Two teach System One. Jarvis clusters frequent `unknown` requests, System Two proposes an intent for each cluster, and the next training run adds it to the model. Prefetch intents get their answers before the usual time.

---

### 14.1 Clusters

**Goal:** Group the OpenAI embeddings of recent `unknown` requests into clusters with DBSCAN, and skip clusters that a learned entry already covers. Jarvis can see which unknown requests are frequent.

- **Implements:** G14. **Depends on:** 13.11.
- **Files:** create `src/domain/learn.rs`. Modify `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.** The embeddings come from `Reasoner::embed` (sub-phase 14.4) on the transcripts. They do not change when System One retrains, and the context does not change them. `clusters(points, distance, min_points) -> Vec<Cluster { texts, centroid }>`: DBSCAN with `linfa-clustering` on the L2-normalized embeddings. Noise points are dropped. `covered(cluster, learned, distance) -> bool`: the cluster centroid is within `distance` of the stored centroid of one learned entry, at any status. Thus new wordings of a rejected topic stay covered when the window moves. This sub-phase adds `[learn]` (`window_days`, `min_cluster`, `cluster_distance`).

**Acceptance criteria.**

- AC-14.1-1: When six embeddings are close and one is far, `clusters` with `min_points = 5` returns one cluster of six.
- AC-14.1-2: When four embeddings are close, `clusters` with `min_points = 5` returns no cluster.
- AC-14.1-3: When a learned entry has a centroid within `cluster_distance` of the cluster centroid, `covered` returns true, also when the texts differ.
- AC-14.1-4: When no learned entry has a centroid within `cluster_distance`, `covered` returns false.

**Proofs.**

- P-14.1-3, P-14.1-4. Ensures: `covered` is true if and only if one learned entry has a centroid within `distance` of the cluster centroid. Argument: `covered` is `any(entry: dist(cluster.centroid, entry.centroid) <= distance)`.
- P-14.1-1, P-14.1-2. Evidence: test (the DBSCAN crate).

---

### 14.2 Proposal Rules

**Goal:** Add `validate_proposal` and `needs_approval`. A proposal is used only when it is well formed, and any proposal that calls an HA service waits for you.

- **Implements:** G14. **Depends on:** 14.1.
- **Files:** modify `src/domain/learn.rs`.

**Model.** `validate_proposal(proposal, catalog, learned) -> Result<LearnedIntent>`: the name is snake case, is new, and is not `unknown`. There are `min_examples` examples or more, and each passes `validate_example`. The responses render with the slots of the examples. The kind decides the other fields:

- `command`: `target` and `select` (or `entity`) are present. `service` is optional. The data templates use only the slots and the built-ins.
- `query`: `select` is present, and `service` is absent.
- `fetch`: `service` is present, and each response uses `response`.
- `prefetch`: `question` is present, `service` is absent, and the examples have no slots.
- `none`: no other check. The entry only marks the cluster as not suitable.

`needs_approval(intent) = intent.service.is_some()`.

**Acceptance criteria.**

- AC-14.2-1: When a proposal has fewer than `min_examples` examples, `validate_proposal` returns an error.
- AC-14.2-2: When a proposal example has a markup error, `validate_proposal` returns an error.
- AC-14.2-3: When a proposal name equals an existing intent name or `unknown`, `validate_proposal` returns an error.
- AC-14.2-4: When an intent has a `service`, `needs_approval` returns true. Without a `service`, it returns false.
- AC-14.2-5: When a `fetch` proposal has no `service`, or a `prefetch` proposal has a `service`, no `question` or a slot, `validate_proposal` returns an error that names the field.
- AC-14.2-6: When a `command` proposal has no `target`, `validate_proposal` returns an error.

**Proofs.**

- P-14.2-1 to P-14.2-3, P-14.2-5, P-14.2-6. Ensures: `validate_proposal` returns `Ok` if and only if the common checks and the checks of its kind pass, and else the first failure with the field. Argument: `validate_proposal` is a conjunction of checks that returns the first failure. The kind checks are one `match` on `kind`.
- P-14.2-4. Ensures: `needs_approval(i)` is true if and only if `i` has a `service`. Argument: `needs_approval` is `is_some()` on the service.

---

### 14.3 Learned Intent Quint Model

**Goal:** Add `quint/learned.qnt`, the model of the proposal lifecycle. Prove that no intent with a service becomes active without approval, that a cluster is proposed only once, and that each activation counts as exactly one change.

- **Implements:** G14, G16, G19. **Depends on:** 14.2.
- **Files:** create `quint/learned.qnt`.
- **Quint:** `learned.qnt`.

**Model.**

- Constants: `CLUSTERS = Set("c1", "c2")`, `HAS_SERVICE: CLUSTER -> bool`.
- State: `status: CLUSTER -> Absent | Pending | Active | Rejected | None`, `approved: Set[CLUSTER]`, `proposals: int`, `changes: int`.
- Actions:
  - `propose(c)` (only when `Absent`): `proposals += 1`. With a service: `Pending`. Without: `Active` and `changes += 1`. Unsuitable: `None`.
  - `approve(c)` (only when `Pending`): add to `approved`, `Active`, `changes += 1`.
  - `reject(c)` (only when `Pending`): `Rejected`.
- Invariants:
  - `ServiceNeedsApproval`: `status[c] = Active and HAS_SERVICE[c]` implies `c` in `approved`.
  - `NoReproposal`: `proposals` equals the number of clusters whose status is not `Absent`.
  - `ActivationCountsOnce`: `changes` equals the number of `Active` clusters.

**Acceptance criteria.**

- AC-14.3-1 [Quint]: `quint typecheck` passes.
- AC-14.3-2 [Quint]: All three invariants hold in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-14.3-2. `ServiceNeedsApproval`: `propose` makes a cluster with a service `Pending`, never `Active`. The only other path to `Active` is `approve`, which adds the cluster to `approved` in the same step. `NoReproposal`: `propose` is enabled only from `Absent` and moves the cluster out of `Absent`. No action moves a cluster back to `Absent`. So each cluster is proposed at most once. `ActivationCountsOnce`: each transition into `Active` adds 1 to `changes`, and no action leaves `Active`.
- P-14.3-1. Evidence: model check.

---

### 14.4 Proposal Generation

**Goal:** Add `propose` to the `Reasoner` port. System Two can now write a learned intent (name, kind, examples, responses, optional service) for a cluster of requests.

- **Implements:** G14. **Depends on:** 14.3.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `embed(texts) -> Result<Vec<Vec<f32>>>` calls the OpenAI embeddings API with `openai.embedding_model`. `propose(texts, intent names) -> Result<Proposal>` with structured output `{ name, kind, examples, responses, service, target, select, data, question }` and `bulk_max_output_tokens`. The prompt asks for `learn.responses` responses. This sub-phase adds `learn.responses`. `question` is the question that a `prefetch` intent asks System Two. `kind` is `command`, `query`, `fetch`, `prefetch` or `none`. `none` means the requests are not suitable for an intent.

**Acceptance criteria.**

- AC-14.4-1: When the mock returns a valid proposal, `propose` returns it.
- AC-14.4-2: When the prompt builder runs, the prompt contains the cluster texts and the existing intent names.
- AC-14.4-3: When the mock returns JSON that does not match the schema, `propose` returns an error.
- AC-14.4-4: When `embed` gets two texts, the mock receives one embeddings request with `embedding_model`, and `embed` returns two vectors.

**Proofs.**

- P-14.4-2. Argument: the prompt builder iterates both lists.
- P-14.4-1, P-14.4-3, P-14.4-4. Evidence: test.

---

### 14.5 Learned Intent Lifecycle

**Goal:** Add the learned intent use case that implements `learned.qnt`: find uncovered clusters, ask for a proposal, validate it and save it to `learned.toml` with its status. New intents appear without manual work, and device control waits for approval.

- **Implements:** G14, G19. **Depends on:** 14.4.
- **Files:** modify `src/app/background.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `learned.qnt` (conformance).

**Model.** Each `learn.check_interval_s`: read `unknowns(now - window_days)`, `embed` their texts, compute `clusters`, skip `covered` clusters, call `propose`, then `validate_proposal`. A valid proposal is saved with `status = pending` if `needs_approval`, else `active`. `kind = none` is saved with `status = none`. An invalid proposal is logged and saved with `status = none`, so the next check does not ask again. Each entry has the cluster `texts` and `centroid`. The pools task (sub-phase 4.8) owns `learned.toml` writes too. This sub-phase adds `learn.file` and `learn.check_interval_s`.

**Acceptance criteria.**

- AC-14.5-1: When `FakeLog` has a cluster that is not covered, `FakeReasoner` records one `propose` call with the cluster texts.
- AC-14.5-2: When the proposal has no service, `learned.toml` has an `active` entry.
- AC-14.5-3: When the proposal has a service, `learned.toml` has a `pending` entry.
- AC-14.5-4: When the proposal is `none`, `learned.toml` has a `none` entry, and the next check does not propose that cluster.
- AC-14.5-5: When the proposal is not valid, Jarvis logs the error and saves a `none` entry, and the next check does not propose that cluster.
- AC-14.5-6: When `quint-connect` replays 100 traces of `learned.qnt`, the statuses in `learned.toml` match the model after each step.

**Proofs.**

- P-14.5-2, P-14.5-3. Requires: `ServiceNeedsApproval`. Argument: the status is `if needs_approval { pending } else { active }` (P-14.2-4).
- P-14.5-4. Requires: `NoReproposal`. Argument: the saved entry has the cluster centroid, so the next check finds the cluster `covered` (P-14.1-3).
- P-14.5-5. Requires: `NoReproposal`. Argument: the `Err` branch of `validate_proposal` saves the `none` entry with the centroid, so the next check finds the cluster `covered`.
- P-14.5-1, P-14.5-6. Evidence: test.

---

### 14.6 Proposal Review

**Goal:** Add the pending proposals to the `Review` port, the web API and the review page, with approve and reject. You decide which device-control intents Jarvis may learn.

- **Implements:** G14. **Depends on:** 14.5.
- **Files:** modify `src/ports.rs`, `src/app/review.rs`, `src/adapters/web.rs`, `src/adapters/review.html`, `src/testing/fakes.rs`.

**Model.** `Review::proposals() -> Vec<LearnedIntent>` (status `pending`). `Review::resolve(name, approve)`: `pending` to `active` or `rejected`. Any other status gives an error. API: `GET /api/proposals`, `POST /api/proposals/{name}` with `{ "approve": true | false }`.

**Acceptance criteria.**

- AC-14.6-1: When `GET /api/proposals` runs, it returns only `pending` entries.
- AC-14.6-2: When you approve a `pending` entry, it becomes `active`.
- AC-14.6-3: When you reject a `pending` entry, it becomes `rejected`, and the next check does not propose that cluster.
- AC-14.6-4: When you resolve an entry that is not `pending`, the API returns 409 and nothing changes.
- AC-14.6-5: When `GET /` runs, the HTML contains `template#proposal` with the examples, the service, and the buttons `.approve` and `.reject`.

**Proofs.**

- P-14.6-2 to P-14.6-4. Requires: the `approve` and `reject` guards of `learned.qnt`. Argument: `resolve` matches `(status, approve)`. Only `(Pending, true)` and `(Pending, false)` change the entry. Other statuses return the error that maps to 409.
- P-14.6-1. Argument: a filter on `status == pending`.
- P-14.6-5. Evidence: test.

---

### 14.7 Activation

**Goal:** When a learned intent becomes active, add its pool and audio, count one change, seed its habit times and add it to the next training set. After the next training run, the request gets System One speed.

- **Implements:** G1, G14, G16. **Depends on:** 14.6.
- **Files:** modify `src/app/background.rs`, `src/app/speech.rs`, `src/app/training.rs`, `src/domain/habits.rs`.

**Model.** On activation, in this order: save the pool through the pools task, pre-render it, add the intent to the live catalog (an `ArcSwap<Catalog>` of `intents.toml` and the active learned intents), set and save `activated := now` (so `changes` counts it once, sub-phase 14.11), then observe one `jarvis.intent` habit for each cluster interaction time. Each step before `activated` is idempotent. At startup, each `active` entry without `activated` runs activation again. A crash after `activated` loses only the seed habits, which normal use replaces, and never adds a duplicate. `Review::intents` now also returns the active learned intent names. The habit model also records each voice request as entity `jarvis.intent`, action = the intent name, source `voice`. `jarvis.intent` has no area and is not a device habit, so `summary` and `established` leave it out.

**Acceptance criteria.**

- AC-14.7-1: When an entry becomes `active`, the saved pools have its pool and the audio cache has its responses.
- AC-14.7-2: When an entry becomes `active`, the habit model has one `jarvis.intent` observation for each cluster interaction.
- AC-14.7-3: When `FakeClassifier` gives `tell_joke` 0.9 after a training run, `handle` replies from the `tell_joke` pool.
- AC-14.7-4: When a static intent pool has fewer than `min_fresh` unused responses, the Phase 8 backfill adds responses.
- AC-14.7-5 [GPU host]: In the Phase 9 harness with `FakeClassifier` that gives `tell_joke`, the response latency p95 is 300 ms or less.
- AC-14.7-6: When Jarvis restarts with an `active` entry without `activated`, the pool exists once and `activated` is set.
- AC-14.7-7: When an entry becomes `active`, `intents` returns its name.
- AC-14.7-8: When the habit model has `jarvis.intent` keys, `summary` and `established` do not contain them.

**Proofs.**

- P-14.7-8. Ensures: no line of `summary` and no key of `established` has the entity `jarvis.intent`. Argument: both functions filter out that entity before they score.
- P-14.7-1 to P-14.7-7. Evidence: test.

---

### 14.8 Prefetch Quint Model

**Goal:** Add `quint/prefetch.qnt`, the model of answers fetched before the usual time. Prove that a prefetch never acts on the house and that Jarvis serves only fresh answers.

- **Implements:** G14, G19. **Depends on:** 14.7.
- **Files:** create `quint/prefetch.qnt`.
- **Quint:** `prefetch.qnt`.

**Model.**

- Constants: `TTL = 3`, `LEAD = 1`, `USUAL = 4`.
- State: `now`, `answer: Option[int]` (the fetch time), `writeCalls: int`, `readCalls: int`, `served: List[(int, int)]` (serve time, answer time).
- Actions: `tick`, `prefetch` (enabled at `now = USUAL - LEAD`), `prefetchStep(step)`: `Read` runs the call (`readCalls += 1`); `Write` discards the answer; `Sentence` keeps it with `answer := now`. The step is any of the three, so a wrong branch in the use case would increment `writeCalls`. `request` (if `answer` is fresh, append `(now, answer)` to `served`, else System Two).
- Invariants:
  - `NoCallFromPrefetch`: `writeCalls` stays 0.
  - `ServedAnswerIsFresh`: for every `(s, a)` in `served`, `s - a <= TTL`.

**Acceptance criteria.**

- AC-14.8-1 [Quint]: `quint typecheck` passes.
- AC-14.8-2 [Quint]: Both invariants hold in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-14.8-2. `NoCallFromPrefetch`: the prefetch path runs only `Read` calls. A `Write` step clears `answer` and calls nothing. `ServedAnswerIsFresh`: `request` appends only under the guard `now - answer <= TTL`.
- P-14.8-1. Evidence: model check.

---

### 14.9 Prefetch Rules

**Goal:** Add the pure prefetch functions: usual times, when a prefetch is due, and whether an answer is fresh.

- **Implements:** G14. **Depends on:** 14.8.
- **Files:** modify `src/domain/learn.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `usual_times(habits, intent, min_score) -> Vec<Bucket>`: the buckets where the `jarvis.intent` score of the intent is at least `min_score`. `prefetch_due(usual, last_check, now, lead) -> bool`: a usual bucket starts at `s` in the local time of HA, and `last_check < s - lead <= now`. Thus a check at 07:55:23 finds the 08:00 bucket, and the next check does not find it again. After a restart, `last_check` is the start time, so Jarvis does not prefetch a missed window. A bucket start that does not exist on a day with a time change is skipped. `fresh(answer_time, now, ttl) = now - answer_time <= ttl`. This sub-phase adds `learn.prefetch_lead_min` and `learn.prefetch_ttl_min`.

**Acceptance criteria.**

- AC-14.9-1: When the usual time is weekdays 08:00 and the lead is 5 min, `prefetch_due` with `last_check` 07:54:23 is true at 07:55:23 on a weekday and false at 07:55:23 on Saturday. With `last_check` 07:55:23, it is false at 07:56:23.
- AC-14.9-2: When the answer is older than `prefetch_ttl_min`, `fresh` returns false. At exactly `prefetch_ttl_min`, it returns true.
- AC-14.9-3: When the score drops below `min_score`, `usual_times` no longer has the bucket.

**Proofs.**

- P-14.9-1. Ensures: `prefetch_due` is true if and only if a usual bucket start `s` has `last_check < s - lead <= now`. Argument: `s - lead = 07:55:00` is in `(07:54:23, 07:55:23]` only on a weekday, where `(weekday, 8)` is in `usual`. It is not in `(07:55:23, 07:56:23]`.
- P-14.9-2. Ensures: `fresh(a, now, ttl)` is true if and only if `now - a <= ttl`. Argument: `fresh` is `<=`.
- P-14.9-3. Ensures: `usual_times` has a bucket if and only if the score of the intent there is `min_score` or more. Argument: `usual_times` filters by `score >= min_score`.

---

### 14.10 Prefetch Use Case

**Goal:** Add the prefetch use case that implements `prefetch.qnt`. A prefetch intent gets its answer from System Two before the usual time, and the request at the usual time is answered at System One speed.

- **Implements:** G1, G14, G19. **Depends on:** 14.9.
- **Files:** modify `src/app/background.rs`, `src/app/conversation.rs`, `src/app/speech.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `prefetch.qnt` (conformance).

**Model.** Each minute, for each active prefetch intent with `prefetch_due`, call `answer` with the intent `question`. The question has the area of the most recent interaction of the intent, so System Two gets its entities. A `Call` step with a service in `learn.prefetch_services` runs, and its result goes back to System Two. Any other `Call` step discards the prefetch and makes no call. Only a stream that ends with `Done` and no error forms the answer, which is pre-rendered, stored with its time, and replaces the old answer. Prefetch intents have no slots (sub-phase 14.2), so the intent is the full cache key. When the intent is predicted, a fresh answer is the reply. Else the request goes to System Two. This sub-phase adds `learn.prefetch_services`.

**Acceptance criteria.**

- AC-14.10-1: When `prefetch_due` is true, `FakeReasoner` records one `answer` call with the intent question.
- AC-14.10-2: When the prefetch completes, the audio cache has the answer audio.
- AC-14.10-3: When the user asks within `prefetch_ttl_min`, `handle` returns the prefetched answer and `FakeReasoner` records no new call.
- AC-14.10-4: When the answer is not fresh, `handle` returns a filler and the request goes to System Two.
- AC-14.10-5: When `FakeReasoner` gives a `Call` step for a service that is not in `prefetch_services` during a prefetch, Jarvis discards the answer and `FakeHome` records no call.
- AC-14.10-6: When `quint-connect` replays 100 traces of `prefetch.qnt`, the stored answer and the `FakeHome` calls match the model after each step.
- AC-14.10-7 [GPU host]: A fresh prefetch answer has a response latency p95 of 300 ms or less in the Phase 9 harness.
- AC-14.10-8: When `FakeReasoner` gives a `Call` step for `weather.get_forecasts`, which is in `prefetch_services`, `FakeHome` records the call and the prefetch continues.
- AC-14.10-9: When the prefetch stream ends with an error after one sentence, Jarvis stores no answer.

**Proofs.**

- P-14.10-3, P-14.10-4. Requires: `ServedAnswerIsFresh`. Argument: the reply branch is guarded by `fresh` (P-14.9-2).
- P-14.10-5. Requires: `NoCallFromPrefetch`. Argument: the `Call` arm of the prefetch consumer checks the service against `prefetch_services` before it calls `Home`. Any other service returns a discard.
- P-14.10-8, P-14.10-9. Evidence: test.
- P-14.10-1, P-14.10-2, P-14.10-6, P-14.10-7. Evidence: test.

---

### 14.11 Learned Intents in Training

**Goal:** Add the active learned intents to the training set, the change count and the generated examples. After the next training run, a learned intent gets System One speed.

- **Implements:** G14, G16. **Depends on:** 14.10.
- **Files:** modify `src/domain/training.rs`, `src/app/training.rs`, `src/lib.rs`.

**Model.**

- `training_set` (sub-phase 13.1) gets the active learned intents. Their labels, slots and examples join those of the catalog. The precedence rule counts their examples as seeds.
- `changes` (sub-phase 13.3) adds the number of learned intents activated after `last_time`.
- `refresh` (sub-phase 13.6) treats an active learned intent like a catalog intent, with its examples as seeds. It removes the entries of intents that are neither in the catalog nor active.

**Acceptance criteria.**

- AC-14.11-1: When an active learned intent exists, the training set labels contain it, the tags contain its slots, and the set has its examples.
- AC-14.11-2: When an entry becomes `active`, the change count increases by 1.
- AC-14.11-3: When a learned intent becomes active, `refresh` calls `paraphrase` for it once. When it is no longer active, `refresh` removes its generated examples.

**Proofs.**

- P-14.11-1. Ensures: the labels and the tags of the set are those of `catalog.intents ++ learned.active()`. Argument: `labels` and `tags` run over that list (P-3.2-7, P-3.2-8).
- P-14.11-2. Requires: `ActivationCountsOnce` (P-14.3-2). Ensures: each activation adds 1 to the count once. Argument: `activated` is written once at activation (sub-phase 14.7), and `changes` counts the entries with `activated > last_time`.
- P-14.11-3. Argument: `refresh` takes the catalog intents and the active learned intents as one list, so the hash check of sub-phase 13.6 applies to both.
