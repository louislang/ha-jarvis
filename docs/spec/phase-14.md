# Phase 14: Learned Intents

This phase makes System Two teach System One. Jarvis clusters frequent requests that System Two answers, System Two proposes a new intent for each cluster, and the next training run adds it to the model. Then System One handles these requests. Fetch and prefetch intents with a usual time (phase 11) get their answers before it, and Jarvis refreshes the answers while the window of the usual time is open. When a cluster needs remote data that no plugin gives, Jarvis asks Claude for a new plugin through one plugin request file in a local folder (decision 32).

---

### 14.1 Clusters

**Goal:** Group the OpenAI embeddings of recent requests that System Two answered into clusters with DBSCAN, and skip clusters that a learned entry already covers. Jarvis can see which System Two requests are frequent.

- **Implements:** G14. **Depends on:** 13.11.
- **Files:** create `src/domain/learn.rs`. Modify `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.** The embeddings come from `Reasoner::embed` (sub-phase 14.4) on the transcripts. They do not change when System One retrains, and the context does not change them. A point has the text, the time, the `spoken` flag (sub-phase 12.5) and the embedding of one request. `clusters(points, distance, min_points) -> Vec<Cluster { texts, times, centroid }>`: DBSCAN with `linfa-clustering` on the L2-normalized embeddings. Noise points are dropped. `times` are the times of the spoken points of the cluster, because only spoken requests are request habits (sub-phase 11.4). `covering(cluster, learned, distance) -> Option<name>`: the name of the first learned entry, at any status, whose stored centroid is within `distance` of the cluster centroid. Thus new wordings of a rejected topic stay covered when the window moves. This sub-phase adds `[learn]` (`window_days`, `min_cluster`, `cluster_distance`).

**Acceptance criteria.**

- AC-14.1-1: When six embeddings are close and one is far, `clusters` with `min_points = 5` returns one cluster of six.
- AC-14.1-2: When four embeddings are close, `clusters` with `min_points = 5` returns no cluster.
- AC-14.1-3: When a learned entry has a centroid within `cluster_distance` of the cluster centroid, `covering` returns its name, also when the texts differ.
- AC-14.1-4: When no learned entry has a centroid within `cluster_distance`, `covering` returns none.
- AC-14.1-5: When six close points have six request times and one of them is not spoken, the cluster has the five times of the spoken points.

**Proofs.**

- P-14.1-3, P-14.1-4. Ensures: `covering` gives `Some(name)` if and only if a learned entry has a centroid within `distance` of the cluster centroid, and `name` is the first such entry. Argument: `covering` is `find(entry: dist(cluster.centroid, entry.centroid) <= distance)` over the entries in file order, mapped to the name.
- P-14.1-5. Ensures: `times` of a cluster are the times of its points with `spoken`. Argument: the cluster builder filters its points by `spoken` before it collects the times.
- P-14.1-1, P-14.1-2. Evidence: test (the DBSCAN crate).

---

### 14.2 Proposal Rules

**Goal:** Add `validate_proposal` and `needs_approval`. A proposal is used only when it is well formed, and any proposal that calls an HA service waits for you.

- **Implements:** G11, G14. **Depends on:** 14.1.
- **Files:** modify `src/domain/learn.rs`, `src/domain/catalog.rs`, `intents.toml`.

**Model.** `Intent` gets `fresh_min`: how long the answer of a fetch or prefetch intent stays fresh, in minutes (sub-phase 14.9). `validate_catalog` (sub-phase 3.2) checks that each `fetch` and `prefetch` intent has `fresh_min` of 1 or more, and that no other kind of intent has it. The `weather` intent of `intents.toml` gets `fresh_min = 30`. The author of a new fetch intent sets its `fresh_min`, and you can change it.

`validate_proposal(proposal, catalog, learned, plugins) -> Result<LearnedIntent>`. `plugins` are the tool names of the plugin registry (sub-phase 17.1). The name is snake case, is new, and is not `unknown`. There are `min_examples` examples or more, and each passes `validate_example`. The responses render with the slots of the examples. The kind decides the other fields:

- `command`: `target` and `select` (or `entity`) are present. `service` is optional. The data templates use only the slots and the built-ins.
- `query`: `select` is present, and `service` is absent.
- `fetch`: `plugin` is one of `plugins`, `service` is absent, `fresh_min` is 1 or more, and each response uses `response` (decision 30).
- `prefetch`: `question` is present, `service` is absent, `fresh_min` is 1 or more, and the examples have no slots.
- `new_plugin`: `needs` is present and not empty, and `service`, `plugin` and `question` are absent. The responses are not checked, because the author of the plugin writes the pool (sub-phase 14.13).
- `none`: no other check. The entry only marks the cluster as not suitable.

`needs_approval(intent) = intent.service.is_some()`.

**Acceptance criteria.**

- AC-14.2-1: When a proposal has fewer than `min_examples` examples, `validate_proposal` returns an error.
- AC-14.2-2: When a proposal example has a markup error, `validate_proposal` returns an error.
- AC-14.2-3: When a proposal name equals an existing intent name or `unknown`, `validate_proposal` returns an error.
- AC-14.2-4: When an intent has a `service`, `needs_approval` returns true. Without a `service`, it returns false.
- AC-14.2-5: When a `fetch` proposal has no `plugin`, a `plugin` that is not in `plugins` or a `service`, or a `prefetch` proposal has a `service`, no `question` or a slot, `validate_proposal` returns an error that names the field.
- AC-14.2-6: When a `command` proposal has no `target`, `validate_proposal` returns an error.
- AC-14.2-7: When a `fetch` or `prefetch` proposal has no `fresh_min` or `fresh_min = 0`, `validate_proposal` returns an error that names `fresh_min`.
- AC-14.2-8: When a `fetch` intent of the catalog has no `fresh_min`, or a `command` intent has one, `validate_catalog` returns an error that names the intent and `fresh_min`.
- AC-14.2-9: When `intents.toml` is loaded, `weather` has `fresh_min = 30`, and the catalog is valid.
- AC-14.2-10: When a `new_plugin` proposal has no `needs`, an empty `needs`, a `service`, a `plugin` or a `question`, `validate_proposal` returns an error that names the field. With `needs` and none of the others, it returns `Ok`, also when a response uses `response`.

**Proofs.**

- P-14.2-8. Ensures: `validate_catalog` returns an error for an intent if `fresh_min` is absent or 0 on a `fetch` or `prefetch` intent, or present on another kind. Argument: the check is one `match` on `(kind, fresh_min)`.
- P-14.2-9. Evidence: test.
- P-14.2-1 to P-14.2-3, P-14.2-5 to P-14.2-7, P-14.2-10. Ensures: `validate_proposal` returns `Ok` if and only if the common checks and the checks of its kind pass, and else the first failure with the field. Argument: `validate_proposal` is a conjunction of checks that returns the first failure. The kind checks are one `match` on `kind`.
- P-14.2-4. Ensures: `needs_approval(i)` is true if and only if `i` has a `service`. Argument: `needs_approval` is `is_some()` on the service.

---

### 14.3 Learned Intent Quint Model

**Goal:** Add `quint/learned.qnt`, the model of the proposal lifecycle. Prove that no intent with a service becomes active without approval, that a cluster is proposed only once, that each activation counts as exactly one change, and that a cluster gets at most one plugin request.

- **Implements:** G14, G16, G19. **Depends on:** 14.2.
- **Files:** create `quint/learned.qnt`.
- **Quint:** `learned.qnt`.

**Model.**

- Constants: `CLUSTERS = Set("c1", "c2")`, `HAS_SERVICE: CLUSTER -> bool`, `NEEDS_PLUGIN: CLUSTER -> bool`.
- State: `status: CLUSTER -> Absent | Pending | Active | Rejected | None | Requested`, `approved: Set[CLUSTER]`, `proposals: int`, `changes: int`, `requests: CLUSTER -> int`.
- Actions:
  - `propose(c)` (only when `Absent`): `proposals += 1`. With `NEEDS_PLUGIN`: `Requested`. Else with a service: `Pending`. Else: `Active` and `changes += 1`. Unsuitable: `None`.
  - `approve(c)` (only when `Pending`): add to `approved`, `Active`, `changes += 1`.
  - `reject(c)` (only when `Pending`): `Rejected`.
  - `writeRequest(c)` (only when `Requested` and `requests[c] = 0`): `requests[c] += 1`.
  - `failRequest(c)` (only when `Requested` and `requests[c] = 0`): nothing changes.
- Invariants:
  - `ServiceNeedsApproval`: `status[c] = Active and HAS_SERVICE[c]` implies `c` in `approved`.
  - `NoReproposal`: `proposals` equals the number of clusters whose status is not `Absent`.
  - `ActivationCountsOnce`: `changes` equals the number of `Active` clusters.
  - `OneRequestPerCluster`: `requests[c] <= 1`, and `requests[c] = 1` implies `status[c] = Requested`.

**Acceptance criteria.**

- AC-14.3-1 [Quint]: `quint typecheck` passes.
- AC-14.3-2 [Quint]: All four invariants hold in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-14.3-2. `ServiceNeedsApproval`: `propose` makes a cluster with a service `Pending`, never `Active`. The only other path to `Active` is `approve`, which adds the cluster to `approved` in the same step. `NoReproposal`: `propose` is enabled only from `Absent` and moves the cluster out of `Absent`. No action moves a cluster back to `Absent`. So each cluster is proposed at most once. `ActivationCountsOnce`: each transition into `Active` adds 1 to `changes`, and no action leaves `Active`. `Requested` adds no change. `OneRequestPerCluster`: `writeRequest` is the only action that writes `requests`, and its guard needs `Requested` and `requests[c] = 0`. No action leaves `Requested`.
- P-14.3-1. Evidence: model check.

---

### 14.4 Proposal Generation

**Goal:** Add `propose` to the `Reasoner` port. System Two can now write a learned intent (name, kind, examples, responses, optional service) for a cluster of requests.

- **Implements:** G14. **Depends on:** 14.3.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.** `embed(texts) -> Result<Vec<Vec<f32>>>` calls the OpenAI embeddings API with `openai.embedding_model`. `propose(texts, intent names, tools) -> Result<Proposal>` with structured output `{ name, kind, examples, responses, service, plugin, target, select, data, question, fresh_min, needs }` and `bulk_max_output_tokens`. The prompt asks for `learn.responses` responses. This sub-phase adds `learn.responses`. `question` is the question that a `prefetch` intent asks System Two. `fresh_min` is how long the answer of a `fetch` or `prefetch` intent stays fresh. You can change it in `learned.toml`. `kind` is `command`, `query`, `fetch`, `prefetch`, `new_plugin` or `none`. `none` means the requests are not suitable for an intent. `tools` are the plugin tools, so a `fetch` proposal can name one in `plugin`. `new_plugin` means that the requests need remote data that no tool in `tools` gives. Then `needs` states that data in one line, and `name` is the name of the future fetch intent, for example `get_traffic`. The prompt states this rule.

**Acceptance criteria.**

- AC-14.4-1: When the mock returns a valid proposal, `propose` returns it.
- AC-14.4-2: When the prompt builder runs, the prompt contains the cluster texts, the existing intent names, the names and descriptions of the plugin tools, and the rule for `new_plugin`.
- AC-14.4-3: When the mock returns JSON that does not match the schema, `propose` returns an error.
- AC-14.4-4: When `embed` gets two texts, the mock receives one embeddings request with `embedding_model`, and `embed` returns two vectors.

**Proofs.**

- P-14.4-2. Argument: the prompt builder iterates the three lists and adds the constant rule text.
- P-14.4-1, P-14.4-3, P-14.4-4. Evidence: test.

---

### 14.5 Learned Intent Lifecycle

**Goal:** Add the learned intent use case that implements `learned.qnt`: find uncovered clusters, ask for a proposal, validate it and save it to `learned.toml` with its status. New intents appear without manual work, and device control waits for approval.

- **Implements:** G14, G19. **Depends on:** 14.4.
- **Files:** modify `src/app/background.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `learned.qnt` (conformance).

**Model.** Each `learn.check_interval_s`: read `deferred(now - window_days)` (sub-phase 12.5), `embed` their texts, compute `clusters`, skip the clusters that `covering` names, call `propose`, then `validate_proposal`. A valid proposal is saved with `status = requested` if its kind is `new_plugin` (sub-phase 14.13), else with `status = pending` if `needs_approval`, else `active`. `kind = none` is saved with `status = none`. An invalid proposal is logged and saved with `status = none`, so the next check does not ask again. Each entry has the cluster `texts` and `centroid`. The pools task (sub-phase 4.8) owns `learned.toml` writes too. This sub-phase adds `learn.file` and `learn.check_interval_s`.

The check also records the request habits of System Two requests (sub-phase 11.4 counts only the requests that System One acts on). The request key of a cluster is the name of its covering entry, or the name of the entry that this check saves for it. When that entry is `pending`, `active` or `requested`, the check observes each time of the cluster under that key, in time order (sub-phase 11.3). A `none` or `rejected` entry records nothing, because its name can be invalid or never become an intent. Each day counts once for each key and bucket, so the next check, which sees the same times again, changes nothing. When the entry later becomes an intent with the same name, the requests that System One acts on continue the same key.

**Acceptance criteria.**

- AC-14.5-1: When `FakeLog` has a cluster that is not covered, `FakeReasoner` records one `propose` call with the cluster texts.
- AC-14.5-2: When the proposal has no service, `learned.toml` has an `active` entry.
- AC-14.5-3: When the proposal has a service, `learned.toml` has a `pending` entry.
- AC-14.5-4: When the proposal is `none`, `learned.toml` has a `none` entry, and the next check does not propose that cluster.
- AC-14.5-5: When the proposal is not valid, Jarvis logs the error and saves a `none` entry, and the next check does not propose that cluster.
- AC-14.5-6: When `quint-connect` replays 100 traces of `learned.qnt`, the statuses in `learned.toml` match the model after each step.
- AC-14.5-7: When a cluster that the entry `family_calendar` covers has requests at 07:10 on Monday, Tuesday and Wednesday, `usual_times` of `family_calendar` returns `[(weekdays, 7)]` after the check.
- AC-14.5-8: When the next check finds the same cluster with the same times, the habit model does not change.
- AC-14.5-9: When the proposal is a valid `new_plugin`, `learned.toml` has a `requested` entry with `needs` and no `request_file`.

**Proofs.**

- P-14.5-2, P-14.5-3, P-14.5-9. Requires: `ServiceNeedsApproval`. Argument: the status is `requested` for `new_plugin`, else `if needs_approval { pending } else { active }` (P-14.2-4).
- P-14.5-4. Requires: `NoReproposal`. Argument: the saved entry has the cluster centroid, so `covering` names it at the next check (P-14.1-3).
- P-14.5-5. Requires: `NoReproposal`. Argument: the `Err` branch of `validate_proposal` saves the `none` entry with the centroid, so `covering` names it at the next check.
- P-14.5-8. Argument: each time of the cluster has a date on or before the `last` of its key and bucket, so each `observe` returns the model unchanged (P-11.3-2).
- P-14.5-1, P-14.5-6, P-14.5-7. Evidence: test.

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

**Goal:** When a learned intent becomes active, add its pool and audio, count one change and add it to the next training set. After the next training run, the request gets System One speed.

- **Implements:** G1, G14, G16. **Depends on:** 14.6.
- **Files:** modify `src/app/background.rs`, `src/app/speech.rs`, `src/app/training.rs`.

**Model.** On activation, in this order: save the pool through the pools task, pre-render it, add the intent to the live catalog (an `ArcSwap<Catalog>` of `intents.toml` and the active learned intents), then set and save `activated := now` (so `changes` counts it once, sub-phase 14.11). Each step before `activated` is idempotent. At startup, each `active` entry without `activated` runs activation again. Thus a crash never adds a duplicate. `Review::intents` now also returns the active learned intent names. The request habits of the entry already have its name as their key (sub-phase 14.5), so activation does not change them.

**Acceptance criteria.**

- AC-14.7-1: When an entry becomes `active`, the saved pools have its pool and the audio cache has its responses.
- AC-14.7-2: When `FakeClassifier` gives `tell_joke` 0.9 after a training run, `handle` replies from the `tell_joke` pool.
- AC-14.7-3: When a static intent pool has fewer than `min_fresh` unused responses, the Phase 8 backfill adds responses.
- AC-14.7-4 [GPU host]: In the Phase 9 harness with `FakeClassifier` that gives `tell_joke`, the response latency p95 is 300 ms or less.
- AC-14.7-5: When Jarvis restarts with an `active` entry without `activated`, the pool exists once and `activated` is set.
- AC-14.7-6: When an entry becomes `active`, `intents` returns its name.

**Proofs.**

- P-14.7-1 to P-14.7-6. Evidence: test.

---

### 14.8 Prefetch Quint Model

**Goal:** Add `quint/prefetch.qnt`, the model of answers fetched before the usual time and refreshed while its window is open. Prove that a prefetch never acts on the house, that it fetches only inside the window and not more often than the freshness interval, and that Jarvis serves only fresh answers.

- **Implements:** G11, G14, G19. **Depends on:** 14.7.
- **Files:** create `quint/prefetch.qnt`.
- **Quint:** `prefetch.qnt`.

**Model.**

- Constants: `FRESH = 2`, `LEAD = 1`, `USUAL = 4`, `END = 6` (the end of the hour of the usual time).
- State: `now`, `answer: Option[int]` (the fetch time), `attempt: Option[int]` (the time of the last prefetch), `fetches: List[int]` (the prefetch times), `writeCalls: int`, `readCalls: int`, `served: List[(int, int)]` (serve time, answer time).
- Actions:
  - `tick`.
  - `prefetch`: enabled when `USUAL - LEAD <= now < END` and `attempt` is none or `now - attempt >= FRESH`. It sets `attempt := now` and appends `now` to `fetches`.
  - `prefetchStep(step)`: `Read` runs the call (`readCalls += 1`). `Write` discards the answer. `Sentence` keeps it with `answer := now`. `Fail` keeps the old answer. The step is any of the four, so a wrong branch in the use case would increment `writeCalls`.
  - `request`: if `answer` is fresh, append `(now, answer)` to `served`, else System Two.
- Invariants:
  - `NoCallFromPrefetch`: `writeCalls` stays 0.
  - `ServedAnswerIsFresh`: for every `(s, a)` in `served`, `s - a <= FRESH`.
  - `PrefetchOnlyInWindow`: every time in `fetches` is in `[USUAL - LEAD, END)`.
  - `NoEarlyRefresh`: two neighbor times in `fetches` differ by `FRESH` or more.

**Acceptance criteria.**

- AC-14.8-1 [Quint]: `quint typecheck` passes.
- AC-14.8-2 [Quint]: All four invariants hold in simulation and to 10 steps in `quint verify`.

**Proofs.**

- P-14.8-2. `NoCallFromPrefetch`: the prefetch path runs only `Read` calls. A `Write` step clears `answer` and calls nothing. `ServedAnswerIsFresh`: `request` appends only under the guard `now - answer <= FRESH`. `PrefetchOnlyInWindow`: `prefetch` appends `now` only under the guard `USUAL - LEAD <= now < END`. `NoEarlyRefresh`: `prefetch` appends `now` only when `now - attempt >= FRESH`, and `attempt` is the last time in `fetches`, because `prefetch` sets both in one step and no other action writes them.
- P-14.8-1. Evidence: model check.

---

### 14.9 Prefetch Rules

**Goal:** Add the pure prefetch functions: whether a time is in the window of a usual time, when a prefetch or a refresh is due, and whether an answer is fresh.

- **Implements:** G11, G14. **Depends on:** 14.8.
- **Files:** modify `src/domain/learn.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- The usual times of an intent are `usual_times` of the request habits with the intent name as the key (sub-phase 11.3b).
- `in_window(usual, now, zone, lead) -> bool`: `usual` contains `bucket(now, zone)` or `bucket(now + lead, zone)`. Thus the window of a usual time starts `lead` before the hour and ends at the end of the hour: for weekdays 08:00 and a lead of 5 min, it is 07:55 to 09:00 on a weekday. The bucket of the shifted time gives the day type, so a window that starts before midnight works. A time change of the HA time zone moves the window with the local hour.
- `refresh_due(usual, attempt, now, zone, lead, fresh_min) -> bool`: `in_window(usual, now, zone, lead)`, and `attempt` is none or `now - attempt >= fresh_min`. `attempt` is the time of the last prefetch of the intent, with or without success. Thus the first prefetch is at the window start, a refresh follows each `fresh_min`, and a failed prefetch waits `fresh_min` before it tries again.
- `fresh(answer_time, now, fresh_min) = now - answer_time <= fresh_min`.
- The freshness interval of an intent is its `fresh_min` (sub-phase 14.2).
- This sub-phase adds `learn.prefetch_lead_min`. `validate` checks that it is from 1 to 60. With a lead of 60 min or less, `bucket(now + lead)` covers the time from `lead` before the hour to the hour, and `bucket(now)` covers the hour, so the two buckets cover the whole window.

**Acceptance criteria.**

- AC-14.9-1: When the usual time is weekdays 08:00 and the lead is 5 min, `in_window` is true at 07:55:00 and at 08:59:59 on a weekday, and false at 07:54:59, at 09:00:00 and at 07:55:00 on Saturday.
- AC-14.9-2: When the usual time is weekdays 00:00 and the lead is 5 min, `in_window` is true at Sunday 23:55.
- AC-14.9-3: When `in_window` is true and no prefetch occurred, `refresh_due` is true. With `fresh_min = 5` and the last prefetch at 07:55:23, it is false at 07:59:23 and true at 08:00:23.
- AC-14.9-4: When the answer is older than `fresh_min`, `fresh` returns false. At exactly `fresh_min`, it returns true.
- AC-14.9-5: When `prefetch_lead_min` is 0 or 61, `validate` returns an error that names it. At 60, the usual time weekdays 08:00 gives `in_window` true at 07:00:00 and at 08:59:59.

**Proofs.**

- P-14.9-1, P-14.9-2. Ensures: `in_window(u, now, z, l)` is true if and only if `u` contains `bucket(now, z)` or `bucket(now + l, z)`. Argument: `in_window` is that disjunction. The tests check the times (time zone data).
- P-14.9-3. Ensures: `refresh_due` is true if and only if `in_window` is true and `attempt` is none or `now - attempt >= fresh_min`. Argument: `refresh_due` is that conjunction, with one `match` on `attempt`.
- P-14.9-4. Ensures: `fresh(a, now, f)` is true if and only if `now - a <= f`. Argument: `fresh` is `<=`.
- P-14.9-5. Evidence: test.

---

### 14.10 Prefetch Use Case

**Goal:** Add the prefetch use case that implements `prefetch.qnt` for learned prefetch intents. A prefetch intent gets its answer from System Two before the usual time and again while the window is open, and the request in the window is answered at System One speed.

- **Implements:** G1, G11, G14, G19. **Depends on:** 14.9.
- **Files:** modify `src/app/background.rs`, `src/app/conversation.rs`, `src/app/speech.rs`, `src/app/plugins.rs`, `src/lib.rs`, `jarvis.toml`.
- **Quint:** `prefetch.qnt` (conformance).

**Model.** Each minute, for each active prefetch intent with `refresh_due`, set its attempt time and call `answer` with the intent `question`. The question has the area of the most recent interaction of the intent, so System Two gets its entities. A `Call` step with a service in `learn.prefetch_services` runs, and its result goes back to System Two. Any other `Call` step discards the prefetch and makes no call. A `Plugin` step runs through the plugin registry, because a plugin only reads data (decision 30). It uses `Plugins::refresh(call, home)`: `fetch` of sub-phase 17.1 without the cache read. `refresh` always calls the plugin, and its result replaces the cache entry. Thus a prefetched answer never has plugin data older than the prefetch, also when `plugins.cache_s` is longer than `fresh_min`. Only a stream that ends with `Done` and no error forms the answer, which is pre-rendered, stored with its time, and replaces the old answer. A failed prefetch keeps the old answer. Prefetch intents have no slots (sub-phase 14.2), so the intent and the area of the question are the full cache key. When the intent is predicted, a fresh answer with the area of the request is the reply. Else the request goes to System Two. This sub-phase adds `learn.prefetch_services`.

**Acceptance criteria.**

- AC-14.10-1: When `refresh_due` is true, `FakeReasoner` records one `answer` call with the intent question.
- AC-14.10-2: When the prefetch completes, the audio cache has the answer audio.
- AC-14.10-3: When the user asks within `fresh_min` of the answer, `handle` returns the prefetched answer and `FakeReasoner` records no new call.
- AC-14.10-4: When the answer is not fresh, `handle` returns a filler and the request goes to System Two.
- AC-14.10-5: When `FakeReasoner` gives a `Call` step for a service that is not in `prefetch_services` during a prefetch, Jarvis discards the answer and `FakeHome` records no call.
- AC-14.10-6: When `quint-connect` replays 100 traces of `prefetch.qnt`, the stored answer, the prefetch times and the `FakeHome` calls match the model after each step.
- AC-14.10-7 [GPU host]: A fresh prefetch answer has a response latency p95 of 300 ms or less in the Phase 9 harness.
- AC-14.10-8: When `FakeReasoner` gives a `Call` step for `calendar.get_events`, which is in `prefetch_services`, `FakeHome` records the call with `return_response: true` (`openai.read_services`, sub-phase 7.5b) and the prefetch continues.
- AC-14.10-9: When the prefetch stream ends with an error after one sentence, Jarvis stores no new answer, and the old answer stays.
- AC-14.10-10: When `FakeReasoner` gives a `Plugin` step during a prefetch, `FakePlugin` records the fetch, `FakeHome` records no call, and the prefetch continues.
- AC-14.10-11: When the window of weekdays 08:00 is open with `fresh_min = 30`, `FakeReasoner` records `answer` calls at 07:55, 08:25 and 08:55, and no call at 09:00 or later.
- AC-14.10-12: When the registry cache has a result of the call from less than `cache_s` ago, a `Plugin` step during a prefetch still makes `FakePlugin` record a new fetch, and the next `fetch` of the call returns the new result.
- AC-14.10-13: When the fresh answer has the area Kitchen and the request comes from the Office, `handle` returns a filler and the request goes to System Two.

**Proofs.**

- P-14.10-3, P-14.10-4, P-14.10-13. Requires: `ServedAnswerIsFresh`. Argument: the reply branch is guarded by `fresh` (P-14.9-4) and by the equality of the stored area and the area of the request.
- P-14.10-5. Requires: `NoCallFromPrefetch`. Argument: the `Call` arm of the prefetch consumer checks the service against `prefetch_services` before it calls `Home`. Any other service returns a discard.
- P-14.10-9. Argument: only the `Done` arm without an error replaces the stored answer.
- P-14.10-10. Argument: the `Plugin` arm of the prefetch consumer calls only `Plugins::refresh` and has no discard.
- P-14.10-12. Argument: `refresh` has no cache read before the plugin call, and its `Ok` arm stores the result under the key of the call.
- P-14.10-11. Requires: `PrefetchOnlyInWindow`, `NoEarlyRefresh`. Argument: the prefetch starts only under `refresh_due` (P-14.9-3).
- P-14.10-1, P-14.10-2, P-14.10-6 to P-14.10-8. Evidence: test.

---

### 14.10b Fetch Intent Prefetch

**Goal:** Prefetch the answer of each fetch intent with a usual time through its plugin, and refresh it at the `fresh_min` of the intent while the window is open. "What is the weather like?" at the usual time then gets System One speed with no filler.

- **Implements:** G1, G11. **Depends on:** 14.10.
- **Files:** modify `src/app/background.rs`, `src/app/conversation.rs`, `src/lib.rs`.

**Model.**

- The prefetch use case of sub-phase 14.10 also runs, each minute, each fetch intent of the live catalog with `refresh_due`. It sets the attempt time of the intent first.
- It renders the `data` of the intent with no slots, as in step 2 of sub-phase 17.4. A data value that needs a slot gives no prefetch for that intent.
- It runs `Plugins::refresh` (sub-phase 14.10) with the rendered call and the `HomeConfig` of the hot cache. A plugin only reads data (decision 30), so the prefetch acts on nothing.
- On success, it selects and renders one response of the intent pool with the result as `response` and the area of the most recent interaction of the intent, pre-renders it, and stores `{ call, area, text, time }` for the intent. `area` is the area of the render, or none. It replaces the old answer. An error keeps the old answer.
- When `decide` gives `Defer` with a filler and `Job::Fetch { call, pool }`, and the stored answer of `pool` is fresh, has the same `call` and has the same area as the request, `handle` replies with the stored text instead. A pool can use `area`, so a text rendered for one area never answers a request in another area. It speaks no filler and fetches nothing. Else the fetch runs as in sub-phase 17.4.
- `refresh` skips the registry cache, so each refresh gets new plugin data, also when `plugins.cache_s` is longer than `fresh_min`.

**Acceptance criteria.**

- AC-14.10b-1: When `weather` has `fresh_min = 30` and the usual time weekends 09:00, at Saturday 08:55:23 `FakePlugin` records one fetch of `get_weather` with `{ when: "today" }`, and the audio cache has the rendered answer.
- AC-14.10b-2: When the user asks "what is the weather like" at Saturday 09:10, `handle` returns the prefetched text, and `FakePlugin` records no new fetch.
- AC-14.10b-3: When the window stays open, `FakePlugin` records the next fetch at 09:25:23, and no fetch at 10:00:00 or later.
- AC-14.10b-4: When the stored answer is older than `fresh_min`, `handle` returns a filler and the fetch runs as in sub-phase 17.4.
- AC-14.10b-5: When the stored call has other arguments than the call of the request, or the stored area is the Kitchen and the request comes from the Office, `handle` returns a filler and the fetch runs.
- AC-14.10b-6: When the `data` of a fetch intent uses a slot, the intent gets no prefetch.
- AC-14.10b-7: When `FakePlugin` returns an error during a refresh, the old answer stays, and the next fetch is `fresh_min` later.
- AC-14.10b-8 [GPU host]: A fresh fetch intent answer has a response latency p95 of 300 ms or less in the Phase 9 harness.
- AC-14.10b-9: When an intent has `fresh_min = 1` and `plugins.cache_s = 300`, two refreshes one minute apart make `FakePlugin` record two fetches.

**Proofs.**

- P-14.10b-2, P-14.10b-4, P-14.10b-5. Requires: `ServedAnswerIsFresh`. Argument: the reply branch is guarded by `fresh` (P-14.9-4) and by the equality of the stored call and area and those of the request.
- P-14.10b-3. Requires: `PrefetchOnlyInWindow`, `NoEarlyRefresh`. Argument: the fetch starts only under `refresh_due` (P-14.9-3).
- P-14.10b-6. Argument: a render error of the data returns before `Plugins::refresh`.
- P-14.10b-7. Argument: only the `Ok` arm replaces the stored answer, and the attempt time is set before the fetch.
- P-14.10b-9. Argument: the fetch intent prefetch calls `Plugins::refresh`, which reads no cache (P-14.10-12).
- P-14.10b-1, P-14.10b-8. Evidence: test.

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

- P-14.11-1. Ensures: the labels and the tags of the set are those of `catalog.intents ++ learned.active()`. Argument: `labels` and `tags` run over that list (P-3.2c-1, P-3.2c-2).
- P-14.11-2. Requires: `ActivationCountsOnce` (P-14.3-2). Ensures: each activation adds 1 to the count once. Argument: `activated` is written once at activation (sub-phase 14.7), and `changes` counts the entries with `activated > last_time`.
- P-14.11-3. Argument: `refresh` takes the catalog intents and the active learned intents as one list, so the hash check of sub-phase 13.6 applies to both.

---

### 14.12 Plugin Request Port and File Adapter

**Goal:** Add the `PluginRequests` port and `RequestFiles`, which writes one Markdown file into a local folder. Jarvis can then ask Claude for a code change without a remote service.

- **Implements:** G14. **Depends on:** 14.11.
- **Files:** modify `src/ports.rs`, `src/adapters/files.rs`, `src/domain/learn.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`, `.gitignore`.

**Model.**

- `PluginRequest { name, text }` is in `src/domain/learn.rs`, because sub-phase 14.13 builds it in the domain. `name` is the name of the learned entry. `text` is the Markdown text.
- `PluginRequests::write(request) -> Result<PathBuf>` writes the request and gives the path of its file.
- `RequestFiles` implements `PluginRequests` in `src/adapters/files.rs`, because it writes local files as the file store does. It has the folder `learn.plugin_requests`. `write` makes the folder when it does not exist. Then it writes `text` to `<folder>/<name>.md` with an atomic write: a `tempfile` in the same folder, then a rename. Thus a reader never sees a part of a file, and a second write of the same name replaces the file. The name is snake case (sub-phase 14.2), so it is a safe file name. An I/O error gives an error with the path.
- `FakePluginRequests` records each request and gives a script of results.
- This sub-phase adds the optional key `learn.plugin_requests`: the folder. The key turns plugin requests on. Without it, `run` makes no `PluginRequests` adapter, and Jarvis writes no plugin requests. `jarvis.toml` sets it to `plugin-requests`, and `.gitignore` ignores that folder.

**Acceptance criteria.**

- AC-14.12-1: When `write` runs for the name `get_traffic`, the file `<folder>/get_traffic.md` has the text of the request, and `write` returns its path.
- AC-14.12-2: When the folder does not exist, `write` makes it and writes the file.
- AC-14.12-3: When `write` runs two times for the same name with different texts, the folder has one file, with the second text.
- AC-14.12-4: When the folder path is a file, `write` returns an error that contains the path.
- AC-14.12-5: When `jarvis.toml` has no `learn.plugin_requests`, `run` starts with no `PluginRequests` adapter.

**Proofs.**

- P-14.12-1 to P-14.12-5. Evidence: test.

---

### 14.13 Plugin Requests

**Goal:** When System Two finds that a cluster needs remote data that no plugin gives, write one plugin request file that asks Claude for the plugin and its fetch intent. The request then becomes a normal code PR, and after the next release it gets System One speed.

- **Implements:** G11, G14, G19. **Depends on:** 14.12.
- **Files:** modify `src/domain/learn.rs`, `src/app/background.rs`, `src/lib.rs`.
- **Quint:** `learned.qnt` (conformance).

**Model.**

- Sub-phase 14.5 saves a valid `new_plugin` proposal with `status = requested` and no `request_file`. The request habits of the cluster have the entry name as their key (sub-phase 14.5).
- After each check and at startup, for each `requested` entry without `request_file`, the use case builds `plugin_request(entry, usual)` with the usual times of the entry name, calls `PluginRequests::write`, and saves `request_file := path` in the entry. An error logs one `warn` event, and the next check tries again. Without a `PluginRequests` adapter, the entries wait. When you set `learn.plugin_requests` later, the next start writes them.
- `plugin_request(entry, usual) -> PluginRequest` is a pure function:
  - `name` is the entry name.
  - `text` starts with the title line `# Plugin request: <name>`. Then it has four parts: `## Requests` with each cluster text on its own line, `## Usual times` with one line `<weekdays|weekends> <HH:00>` for each bucket of `usual`, or `none`, `## Data needed` with `needs`, and `## Task`.
  - `## Task` says: write a plugin for this data in the pattern of phase 17 (one `Plugin` adapter, the change map row "Add a plugin" of `01-architecture.md` section 12), the fetch intent `<name>` with `plugin` and `fresh_min` in `intents.toml`, its pool in `responses.toml`, and tests with a local mock server. Use a free data source without an API key when one exists. Use a source with a key only after the owner agrees, so ask first. Then the PR names the environment variable and the sign-up page. Open a normal PR.
- One request for each cluster: the entry is saved before the write, `covering` names it at each later check (`NoReproposal`), and an entry with `request_file` writes no other request. A crash after `write` and before the save of `request_file` writes the request again at the next start. The file name is the entry name, so the second write replaces the first file (AC-14.12-3). A second file occurs only when the first file was moved or deleted before the restart.
- Jarvis only writes files to the folder. It does not read, move or delete them.
- The PR of Claude adds the intent `<name>` to `intents.toml`. After the release, System One acts on the requests under the same key, and sub-phase 14.10b prefetches the intent at its usual times. The `requested` entry stays, so `covering` still skips the cluster.

**Acceptance criteria.**

- AC-14.13-1: When the proposal for a cluster is `new_plugin` with the name `get_traffic`, `learned.toml` has a `requested` entry, and `FakePluginRequests` records one request with the name `get_traffic` and the title line `# Plugin request: get_traffic`.
- AC-14.13-2: When `plugin_request` runs for an entry with two texts, the usual time `(weekdays, 8)` and `needs = "current road traffic from home to work"`, the text has the title line, both texts, the line `weekdays 08:00`, the `needs` text, and the task text with the rule for API keys.
- AC-14.13-3: When the write succeeds, the entry has the path in `request_file`, and the next check and the next start write no request.
- AC-14.13-4: When `FakePluginRequests` returns an error, the entry has no `request_file`, and the next check records a new `write` call for it.
- AC-14.13-5: When Jarvis has no `PluginRequests` adapter, the entry stays `requested` without `request_file`.
- AC-14.13-6: When `usual` is empty, the `## Usual times` part has the line `none`.
- AC-14.13-7: When `quint-connect` replays 100 traces of `learned.qnt`, the statuses and the requests in `learned.toml` and `FakePluginRequests` match the model after each step.

**Proofs.**

- P-14.13-2, P-14.13-6. Ensures: the text is the title line and the four parts in order, with the texts of the entry, one line for each bucket of `usual` or `none`, and the `needs` text. Argument: `plugin_request` joins the parts with one `format!` for each line, and the empty case of `usual` has its own arm.
- P-14.13-3. Requires: `OneRequestPerCluster`, `NoReproposal`. Argument: the use case writes a request only for a `requested` entry with `request_file = None`, and it saves the path after `Ok`.
- P-14.13-4. Argument: the `Err` arm logs and does not save `request_file`.
- P-14.13-1, P-14.13-5, P-14.13-7. Evidence: test.

**Field log.**

1. In the `jarvis.toml` of the GPU host, set `learn.plugin_requests` to a folder that the `jarvis` unit can write and Claude can read. To turn plugin requests off, remove the key.
2. Set up Claude Code to watch the folder, for example with a scheduled routine that reads each new file and does its `## Task`. Record the method that you chose, and record that a test file starts Claude.
