# Phase 2: Home and Hot Cache

This phase adds the `Home` port, its HA adapter, the hot cache, its Quint model and the sync use case.

---

### 2.1 Home Port and Reference House

**Goal:** Add `src/ports.rs` with the `Home` port, its data types in the domain, and the reference house snapshot. Later sub-phases can now build the HA adapter, the cache and the fakes against one fixed contract.

- **Implements:** G8, G10. **Depends on:** 1.2.
- **Files:** create `src/ports.rs`, `src/domain/state.rs`, `src/testing/house.json`. Modify `src/lib.rs`.

**Model.**

- `Home` has `snapshot() -> Result<Snapshot>`, `events() -> Stream<Event>` and `call(ServiceCall) -> Result<Option<Value>>`.
- The data types are in `src/domain/state.rs`, because the domain cache uses them (`01-architecture.md` section 1).
- `Snapshot`: `states`, `areas`, `devices`, `entities`, `config` (`time_zone`, `location_name`), `user_id` (the HA user of the token). `State`: `entity_id`, `state`, `attributes`, `last_updated`, `context`. `Context`: `id`, `user_id`, `parent_id`.
- An entity entry has `entity_id`, `name`, `aliases`, `area_id`, `device_id`, `entity_category`, `hidden_by` and `disabled_by`. The device class of an entity is the `device_class` attribute of its state.
- `Event`: `StateChanged { entity_id, old, new, context }` (`new` can be absent), `RegistryUpdated`, `Reconnected`.
- `ServiceCall`: `domain`, `service`, `target` (entity IDs), `data`, `return_response`.
- `house.json` is a `Snapshot` of the reference house (`01-architecture.md` section 11).

**Acceptance criteria.**

- AC-2.1-1: When `house.json` is parsed, it gives a `Snapshot` without error.
- AC-2.1-2: When the snapshot is read, it has the 7 areas, 2 `person` entities, 4 satellites and their 4 presence sensors without a device class.
- AC-2.1-3: When the snapshot is read, "Garage Light" is a `switch`, "Living Room Fan" is a `switch`, "Office Fan" is a `fan`, and the only `climate` entity is in the Kitchen.
- AC-2.1-4: When the snapshot is read, the Office has two temperature sensors with the states `64.0` and `76.6`.

**Proofs.**

- P-2.1-1 to P-2.1-4. Evidence: test.

---

### 2.2 HA Connection and Authentication

**Goal:** Add `HaHome::connect`, which opens the HA WebSocket and authenticates with the token. Add the fake HA server to the adapter tests, so later HA sub-phases test without a real HA.

- **Implements:** G8. **Depends on:** 2.1.
- **Files:** create `src/adapters/ha.rs`. Modify `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- Authentication states: `AwaitRequired -> SentAuth -> Ready | Rejected`. HA sends `auth_required`. Jarvis sends `auth` with the token. HA sends `auth_ok` or `auth_invalid`.
- The handshake has the deadline `ha.request_timeout_ms`. A timeout closes the socket and returns a timeout error. This sub-phase adds `ha.request_timeout_ms`.
- This sub-phase adds `Config::validate`. After the parse, `validate` checks the range of each field. `model.threshold`, `model.commit_threshold`, `model.next_threshold`, `train.holdout`, `dialog.choice_similarity`, `speaker.voice_threshold` and `speaker.enroll_threshold` are in `[0, 1]`. Every other number is more than 0. `validate` also checks these relations: `asr.vad_min_silence_ms < asr.early_commit_ms < asr.endpoint_ms < asr.max_utterance_ms`, `0.5 < model.threshold <= model.commit_threshold`, and `log.audio_days <= log.record_days`. Each later sub-phase adds the checks of the fields that it adds. `parse` calls `validate`.
- `HaHome` opens its transport through a connector. `src/lib.rs` passes a TCP connector for `ha.url`. Tests pass a connector that returns one end of a `tokio::io::duplex` pair. Thus the adapter tests pause the clock with no real socket (`02-rules.md` section 6.2).
- `FakeHaServer` (test block of `ha.rs`) serves the HA WebSocket protocol on the other end of the pair. It accepts one token, serves `house.json`, sends scripted events and records the messages it receives.

**Acceptance criteria.**

- AC-2.2-1: When the token is correct, `connect` returns a connected adapter.
- AC-2.2-2: When the token is incorrect, `connect` returns an authentication error.
- AC-2.2-3: When the server closes before `auth_required`, `connect` returns a connection error.
- AC-2.2-4: When the server sends a message other than `auth_ok` or `auth_invalid` after `auth`, `connect` returns a protocol error that contains the message type.
- AC-2.2-5: When the server keeps the socket open but sends no `auth_ok` within `request_timeout_ms`, `connect` returns a timeout error.
- AC-2.2-6: When a field is out of its range (for example `ha.request_timeout_ms = 0`), `parse` returns an error that contains the field name and the range.

**Proofs.**

- P-2.2-1, P-2.2-2, P-2.2-5. Evidence: test.
- P-2.2-3, P-2.2-4. Requires: the state is `AwaitRequired` or `SentAuth`. Ensures: only `auth_ok` reaches `Ready`. Argument: the handshake is a match on `(state, message type)`. `Ready` has one incoming transition, from `SentAuth` on `auth_ok`. Every other pair, and the end of the stream, returns an error.
- P-2.2-6. Ensures: a field out of its range gives `Err` with the field name and the range. Argument: `parse` returns the first `Err` of `validate`, which compares each field with its range.

---

### 2.3 HA Requests and Snapshot

**Goal:** Add request IDs to `HaHome`, so concurrent callers each receive their own result. Add `snapshot`, which loads the states and the area, device and entity registries.

- **Implements:** G8. **Depends on:** 2.2.
- **Files:** modify `src/adapters/ha.rs`, `src/ports.rs`.

**Model.** `HaHome` keeps `next_id: u64` and `pending: Map<u64, reply sender>`. A request takes `next_id`, then `next_id` increases by 1. A `result` message with ID `n` goes to `pending[n]`, and `n` is removed. Each request has the deadline `ha.request_timeout_ms`. A timeout removes `n` from `pending` and returns a timeout error. `snapshot` sends `get_states`, `config/area_registry/list`, `config/device_registry/list`, `config/entity_registry/list`, `get_config` and `auth/current_user`. The list of the entity registry has no aliases, so `snapshot` also sends `config/entity_registry/get_entries` with all entity IDs and takes the aliases from it.

**Acceptance criteria.**

- AC-2.3-1: When 100 requests are sent at the same time, each caller receives the result with its own ID.
- AC-2.3-2: When `snapshot` runs, it returns the states, the three registries, the configuration and the user ID of `house.json`.
- AC-2.3-3: When a result has an ID that is not pending, the adapter ignores it and other requests continue.
- AC-2.3-4: When HA returns `success = false`, the caller receives an error with the HA error message.
- AC-2.3-5: When no result arrives within `request_timeout_ms`, the caller receives a timeout error and the ID is no longer pending.
- AC-2.3-6: When `snapshot` runs, each entity has the aliases of its entry in `config/entity_registry/get_entries`.

**Proofs.**

- P-2.3-6. Evidence: test.
- P-2.3-1 (AC-2.3-1). Requires: `next_id` is only read and increased under one lock. Ensures: no two pending requests have the same ID, and each result reaches exactly one caller. Argument: `next_id` strictly increases, so each request gets a new value. `pending` is a map, so one ID has one sender. Removal on delivery means a sender receives at most one result.
- P-2.3-2. Evidence: test.
- P-2.3-3 (AC-2.3-3). Argument: delivery looks up the ID. A missing ID is a no-op, and the map does not change.
- P-2.3-4, P-2.3-5. Evidence: test.

---

### 2.4 HA Events and Reconnect

**Goal:** Add `events`, which subscribes to state changes and registry updates. Add reconnect after a disconnect, with a `Reconnected` event, so the cache can load a new snapshot.

- **Implements:** G8. **Depends on:** 2.3.
- **Files:** modify `src/adapters/ha.rs`, `jarvis.toml`.

**Model.**

- After `Ready`, the adapter sends `subscribe_events` for `state_changed`, `area_registry_updated`, `device_registry_updated` and `entity_registry_updated`.
- On a disconnect: every pending request gets a disconnect error, then the adapter connects again with backoff (100 ms, doubled, at most 2 s). After `Ready` and the new subscriptions, it gives `Reconnected`.
- Every `ha.ping_interval_s`, the adapter sends `ping`. No `pong` within `ha.request_timeout_ms` is a disconnect. Thus a dead TCP connection does not leave the cache stale. This sub-phase adds `ha.ping_interval_s`.
- `HaHome::start(connector, token, config)` returns at once. It makes the first connection in the background with the same backoff as a reconnect. A request before the first `Ready` gets a disconnect error. The first `Ready` also gives `Reconnected`.
- The connection has an epoch number. An event from an old epoch is dropped.
- `events` keeps the events from the first subscription in order, until the consumer reads them. A connection that reaches `Ready` resets the backoff attempt to 0.

**Acceptance criteria.**

- AC-2.4-1: When `FakeHaServer` sends `state_changed`, `events` gives `StateChanged` with its context.
- AC-2.4-2: When `FakeHaServer` sends `entity_registry_updated`, `events` gives `RegistryUpdated`.
- AC-2.4-3: When `FakeHaServer` closes the connection, the adapter connects again after the first backoff delay in virtual time and `events` gives `Reconnected`.
- AC-2.4-4: When the connection closes with a pending request, the caller receives a disconnect error.
- AC-2.4-5: When the backoff runs five times, the delays are 100, 200, 400, 800 and 1600 ms, and the sixth is 2000 ms.
- AC-2.4-6: When a reconnect reaches `Ready` after three attempts and the connection closes again, the next delay is 100 ms.
- AC-2.4-7: When `FakeHaServer` stops answering `ping`, the adapter connects again after the `pong` deadline in virtual time, and `events` gives `Reconnected`.
- AC-2.4-8: When no server is available at `start`, `start` returns an adapter. When the server becomes available, `events` gives `Reconnected`.

**Proofs.**

- P-2.4-1 to P-2.4-3, P-2.4-7, P-2.4-8. Evidence: test.
- P-2.4-4 (AC-2.4-4). Ensures: after a disconnect, `pending` is empty and each removed sender received an error. Argument: the disconnect handler drains the map and sends an error to each sender before it starts the reconnect.
- P-2.4-6. Argument: `Ready` sets the attempt to 0, and `delay(0) = 100`.
- P-2.4-5 (AC-2.4-5). Requires: attempt `k >= 0`. Ensures: `delay(k) = min(100 * 2^k, 2000)`. Argument: the function is this closed form. `100 * 2^5 = 3200 > 2000`, so the sixth delay is 2000.

---

### 2.5 HA Service Calls

**Goal:** Add `call`, which sends `call_service` with the target and data, and returns the response data when asked. Jarvis can now act on HA.

- **Implements:** G8. **Depends on:** 2.4.
- **Files:** modify `src/adapters/ha.rs`.

**Model.** `call(c)` sends `{ type: "call_service", domain, service, target: { entity_id }, service_data: data, return_response }` and returns `response` when `return_response` is true.

**Acceptance criteria.**

- AC-2.5-1: When `call` runs, `FakeHaServer` receives `call_service` with the domain, service, target and data.
- AC-2.5-2: When `call` runs with `return_response = true`, the caller receives the response data.
- AC-2.5-3: When `call` runs with `return_response = false`, the caller receives `None`.

**Proofs.**

- P-2.5-1, P-2.5-2. Evidence: test.
- P-2.5-3 (AC-2.5-3). Argument: the result is `Some(response)` only in the branch where `return_response` is true.

---

### 2.6 Cache Sync Quint Model

**Goal:** Add `quint/cache_sync.qnt`, the model of HA state, the event stream, disconnects and the hot cache. It proves that the cache equals HA whenever the connection is idle, before the Rust code exists.

- **Implements:** G8, G19. **Depends on:** 2.5.
- **Files:** create `quint/cache_sync.qnt`.
- **Quint:** `cache_sync.qnt`.

**Model.**

- Constants: `ENTITIES = Set("a", "b")`, `VALUES = Set(0, 1)`.
- State: `home: ENTITY -> VALUE`, `cache: ENTITY -> VALUE`, `queue: List[(ENTITY, VALUE, EPOCH)]`, `connected: bool`, `epoch: int`, `loading: bool`, `snap: Option[ENTITY -> VALUE]`.
- Actions:
  - `change(e, v)`: `home[e] := v`. If connected, append `(e, v, epoch)` to `queue`.
  - `deliver` (when `not loading`): pop the head. If its epoch equals `epoch`, set `cache[e] := v`.
  - `disconnect`: `connected := false`, `queue := []`, `loading := false`, `snap := None`.
  - `reconnect` (when `not connected`): `epoch := epoch + 1`, `connected := true`, `loading := true`. The subscription starts here, so later changes enter the queue.
  - `takeSnapshot` (when `loading` and `snap = None`): `snap := Some(home)`. Changes can still arrive before the result.
  - `applySnapshot` (when `snap = Some(s)`): `cache := s`, `snap := None`, `loading := false`.
- Invariants:
  - `CacheEqualsHomeWhenIdle`: `connected and not loading and queue == [] implies cache == home`.
  - `NoEventAfterDisconnect`: every queued event has the current epoch.

**Acceptance criteria.**

- AC-2.6-1 [Quint]: `quint typecheck` passes.
- AC-2.6-2 [Quint]: `CacheEqualsHomeWhenIdle` holds in simulation and in `quint verify` to 10 steps.
- AC-2.6-3 [Quint]: `NoEventAfterDisconnect` holds in simulation and in `quint verify` to 10 steps.
- AC-2.6-4 [Quint]: The test `ac_2_6_4_change_while_disconnected` passes: a change while disconnected is in the cache after `reconnect` and the snapshot.
- AC-2.6-5 [Quint]: The test `ac_2_6_5_change_during_load` passes: a change between `takeSnapshot` and `applySnapshot` is in the cache after the queue is delivered.

**Proofs.**

- P-2.6-2 (AC-2.6-2). Claim: `CacheEqualsHomeWhenIdle` is an inductive invariant together with `QueueReplaysToHome`: when connected and not loading, applying `queue` in order to `cache` gives `home`; after `takeSnapshot`, applying `queue` in order to `snap` gives `home`. Argument by induction on steps. Initially `connected`, `cache = home` and `queue = []`. `change` keeps the claim, because the new entry is the last write to `e`. `deliver` moves the head from the queue into the cache, so the replay result is the same. `disconnect` makes the claim vacuous. `reconnect` starts with an empty queue. `takeSnapshot` sets `snap = home`, and every later change enters the queue. A queued change from before `takeSnapshot` is already in `snap`, and replay in order ends with the last write of each entity, so the result is `home`. `applySnapshot` copies `snap` into `cache` with the same queue. When the queue is empty and loading is false, the claim gives `cache = home`.
- P-2.6-3 (AC-2.6-3). Argument by induction: `change` appends with the current epoch. `disconnect` empties the queue. `reconnect` requires `not connected`, so a disconnect came first and the queue is empty when the epoch increases. No action adds an old epoch.
- P-2.6-1, P-2.6-4, P-2.6-5. Evidence: model check.

---

### 2.7 State Cache Updates

**Goal:** Add `StateCache` in the domain with `load` and `apply`. The cache now follows HA events with pure, testable functions.

- **Implements:** G8. **Depends on:** 2.6.
- **Files:** modify `src/domain/state.rs`, `src/lib.rs`.

**Model.** `StateCache` holds `states: BTreeMap<Arc<str>, Arc<State>>`, `areas`, `devices`, `entities`. A copy of the cache copies pointers, not states, so a cache in an `ArcSwap` can change by one copy for each event. `load(snapshot) -> StateCache` replaces everything. `apply(self, event) -> StateCache`: `StateChanged` with `new` sets `states[id] := new`, unless the cached state has a newer `last_updated`. Without `new`, it removes `id`. `RegistryUpdated` and `Reconnected` do not change the cache (the use case loads a new snapshot).

**Acceptance criteria.**

- AC-2.7-1: When the cache loads `house.json`, `state("light.kitchen")` returns the state in the snapshot.
- AC-2.7-2: When the cache applies `StateChanged` with a new state, `state` returns the new state.
- AC-2.7-3: When the cache applies `StateChanged` without a new state, `state` returns none.
- AC-2.7-4: When the cache applies `StateChanged` for entity `a`, the state of every other entity does not change.
- AC-2.7-5: When the cache loads a second snapshot, no state from the first snapshot remains.
- AC-2.7-6: When `StateChanged` has a new state with an older `last_updated` than the cached state, `apply` keeps the cached state.

**Proofs.**

- P-2.7-1, P-2.7-5. Requires: a snapshot. Ensures: `states` equals the states of the snapshot. Argument: `load` builds a new map from the snapshot only.
- P-2.7-6. Ensures: when the cached state has a newer `last_updated`, `apply` keeps it. Argument: the insert is guarded by the `last_updated` comparison.
- P-2.7-2 to P-2.7-4. Ensures: `apply(c, StateChanged(a, n)).states = c.states[a := n]` (or `c.states \ {a}` when `n` is absent). Argument: `apply` does one insert or one remove on key `a`. A map insert or remove on key `a` keeps every other key.

---

### 2.8 State Cache Queries

**Goal:** Add the cache queries that selection and decisions need: area of a device, entities by area, domain and device class, and area by name or alias. The queries stay within an instruction budget with 2000 entities.

- **Implements:** G8, G9. **Depends on:** 2.7.
- **Files:** create `benches/domain.rs`. Modify `src/domain/state.rs`, `Cargo.toml`, `justfile`, `.github/workflows/ci.yml`, `.github/pull_request_template.md`, `xtask/src/gates.rs`.

**Model.**

- `area_of_device(device_id) -> Option<AreaId>`.
- `area_of_entity(entity_id)`: the entity area, else the area of its device.
- `entities(area, domain, device_class: Option) -> Vec<EntityId>`.
- `area_by_name(text) -> Option<AreaId>`: matches the name or an alias without case sensitivity.
- `benches/domain.rs` is the first Gungraun benchmark file. It measures `state`, `entities` and `apply` (sub-phase 2.7). Each benchmark sets `Callgrind::hard_limits` with the budget of its criterion. `just bench` runs it, and the `bench` CI job runs `just bench` on `ubuntu-latest`. `check` runs `bench` only on Linux, because Valgrind does not run on macOS.

**Acceptance criteria.**

- AC-2.8-1: When a device has an area, `area_of_device` returns that area.
- AC-2.8-2: When an entity has no area, `area_of_entity` returns the area of its device.
- AC-2.8-3: When `entities(living_room, "fan", None)` runs, it returns only fan entities in the Living Room.
- AC-2.8-4: When `entities` gets a device class, it returns only entities with that device class.
- AC-2.8-5: When `area_by_name("living ROOM")` runs, it returns the Living Room. An alias also matches.
- AC-2.8-6: When no area has the name or alias, `area_by_name` returns none.
- AC-2.8-7 [Bench]: With 2000 entities, `state` uses 5,000 instructions or fewer, and `entities` and `apply` each use 1,000,000 instructions or fewer.
- AC-2.8-8: When `ci.yml` is parsed, the job `bench` runs `just bench` on `ubuntu-latest`. When the `justfile` is dumped, `check` runs `bench` only on Linux.

**Proofs.**

- P-2.8-2 (AC-2.8-2). Ensures: `area_of_entity(e) = entity.area.or(area_of_device(entity.device))`. Argument: the function is this expression.
- P-2.8-3, P-2.8-4. Ensures: every returned entity has the area, the domain and (if given) the device class. Argument: `entities` is a filter whose predicate is the conjunction of the three conditions.
- P-2.8-5, P-2.8-6. Ensures: `Some(a)` if and only if the lowercase text equals the lowercase name or a lowercase alias of `a`. Argument: `area_by_name` is a `find` with that predicate. `find` returns `None` when no element matches.
- P-2.8-1. Ensures: `area_of_device(d)` is the area of `d` in the device registry, or `None`. Argument: the function is one map lookup.
- P-2.8-7. Evidence: benchmark.
- P-2.8-8. Evidence: test.

---

### 2.9 Sync Use Case

**Goal:** Add the sync use case, which loads the snapshot, applies events and loads a new snapshot after a registry update or a reconnect. Add `FakeHome` and the conformance test against `cache_sync.qnt`.

- **Implements:** G8, G19. **Depends on:** 2.8.
- **Files:** create `src/app/background.rs`, `src/testing/fakes.rs`. Modify `src/lib.rs`, `Cargo.toml`.
- **Quint:** `cache_sync.qnt` (conformance).

**Model.** `sync(home, cache: ArcSwap<StateCache>)`: take the `events` stream first, then load the snapshot. When the first `snapshot` fails, sync waits for `Reconnected`. Events that arrive during a load wait in the stream and apply after it. For each event: `StateChanged` applies it, `RegistryUpdated` and `Reconnected` load a new snapshot. `FakeHome` returns `house.json`, sends scripted events, records calls and can delay calls.

**Acceptance criteria.**

- AC-2.9-1: When sync starts, the cache contains all states and registries from `FakeHome`.
- AC-2.9-2: When `FakeHome` sends a state change, the cache shows the new state before virtual time advances.
- AC-2.9-3: When `FakeHome` sends `RegistryUpdated`, `FakeHome` records a second `snapshot` call.
- AC-2.9-4: When `FakeHome` sends `Reconnected`, the cache replaces all old data with the new snapshot.
- AC-2.9-5: When `quint-connect` replays 100 traces of `cache_sync.qnt`, the cache equals the model `cache` after each step. `FakeHome` maps `takeSnapshot` to the start of a delayed `snapshot` call and `applySnapshot` to its result.
- AC-2.9-6: When `FakeHome` sends a state change while the `snapshot` call is delayed, the cache has the change after the load.
- AC-2.9-7: When the first `snapshot` fails, the cache loads the snapshot after `FakeHome` sends `Reconnected`.

**Proofs.**

- P-2.9-1, P-2.9-3, P-2.9-4. Argument: the event loop is a match on the event type. `RegistryUpdated` and `Reconnected` both call `load(snapshot())`, and `load` replaces all data (P-2.7-1).
- P-2.9-2. Argument: the event loop applies each event when it is received, with no timer between receive and apply.
- P-2.9-6. Argument: the stream exists before the load, so the event waits in it and applies after the load (P-2.6-2).
- P-2.9-7. Argument: the `Err` arm of the first load reads the stream until `Reconnected`, which takes the reload arm of the event loop.
- P-2.9-5. Evidence: test. With P-2.6-2, it gives `CacheEqualsHomeWhenIdle` for the Rust code on the replayed traces.
