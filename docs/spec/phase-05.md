# Phase 5: Wyoming Conversation Agent

This phase adds the driving port and the Wyoming adapter. HA can use Jarvis as its conversation agent with text input.

---

### 5.1 Assistant Port and Wyoming Server

**Goal:** Add the `Assistant` driving port and the Wyoming TCP server that answers `describe` with `info`. HA can now discover Jarvis as a Wyoming service.

- **Implements:** G10. **Depends on:** 4.9.
- **Files:** create `src/adapters/wyoming.rs`. Modify `src/ports.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `Cargo.toml`.

**Model.**

- `Assistant`: `info() -> Info`, `handle(text, context) -> Result<Handled>`. `Handled` has the text and the `after` work (sub-phase 4.8). Later sub-phases add `listen` and `speak`.
- A Wyoming event is one JSON header line (`type`, `data_length`, `payload_length`), then `data_length` bytes of JSON data, then `payload_length` bytes of payload. Event types come from the `wyoming` crate.
- Each connection runs in its own task. An unknown event type is ignored. A header that is not valid JSON closes that connection only.
- `FakeWyomingClient` and `FakeAssistant` are in `fakes.rs`.

**Acceptance criteria.**

- AC-5.1-1: When the client sends `describe`, the adapter sends `info` with one `handle` program.
- AC-5.1-2: When the client sends an unknown event type, the adapter ignores it and the connection stays open.
- AC-5.1-3: When a client sends a header that is not valid JSON, the adapter closes that connection and the other connections continue.
- AC-5.1-4: When 10 clients connect at the same time, all 10 receive `info`.

**Proofs.**

- P-5.1-2 (AC-5.1-2). Argument: the event loop matches the type, and the default arm continues the loop.
- P-5.1-3 (AC-5.1-3). Argument: a parse error ends the loop of that task only. Tasks share no state other than the `Assistant`, which a parse error does not touch.
- P-5.1-1, P-5.1-4. Evidence: test.

---

### 5.2 Transcript Handling in the Adapter

**Goal:** Connect the Wyoming `transcript` event to `Assistant::handle` and send the result as `handled` or `not-handled`. HA's conversation step now reaches Jarvis.

- **Implements:** G10. **Depends on:** 5.1.
- **Files:** modify `src/adapters/wyoming.rs`.

**Model.** `transcript` with `text` and `context` (`conversation_id`, `device_id`, `satellite_id`) calls `handle`. `Ok(h)` sends `handled { text: h.text }`, flushes the socket, and then starts `h.after`. `Err(e)` sends `not-handled { text: e.text }`.

**Acceptance criteria.**

- AC-5.2-1: When the client sends `transcript`, `FakeAssistant` records the text and the three context values.
- AC-5.2-2: When `handle` returns `Ok`, the client receives `handled` with the text.
- AC-5.2-3: When `handle` returns `Err`, the client receives `not-handled` with the error text.
- AC-5.2-4: When the write of `handled` is blocked, the `after` work of `FakeAssistant` has not started. When the write completes, it starts.

**Proofs.**

- P-5.2-2, P-5.2-3. Argument: the adapter matches on the `Result` with one arm for each variant.
- P-5.2-4. Argument: the adapter awaits the write and the flush before it starts `after`.
- P-5.2-1. Evidence: test.

---

### 5.3 Handle Use Case

**Goal:** Implement `handle`: build the model context, predict, decide and dispatch. A transcript from HA now becomes a System One reply in under 20 ms.

- **Implements:** G1, G3, G9. **Depends on:** 5.2.
- **Files:** modify `src/app/conversation.rs`, `src/lib.rs`.

**Model.** `handle(text, ctx)`: the satellite area is `area_of_device(ctx.device_id)`. The context is `context_text(area, [])`. Then `predict` with `normalize(text)` (in `spawn_blocking`, decision 29), `decide` and dispatch. An error from `predict` or `decide` gives `Err` with a `failure` text. `App` in `src/lib.rs` delegates `Assistant::handle` to this use case.

**Acceptance criteria.**

- AC-5.3-1: When `handle` gets "turn on the lights" with a Kitchen `device_id` and `FakeClassifier` gives `lights_on` 0.9, it returns a `lights_on` response.
- AC-5.3-2: When `handle` runs with a Kitchen `device_id`, `FakeClassifier` records the context `"area: kitchen | habits: none"`.
- AC-5.3-3: When the context has no `device_id`, the recorded context has `area: none`.
- AC-5.3-4: When the decision is `Defer`, `handle` returns a filler text.
- AC-5.3-5: When `predict` returns an error, `handle` returns `Err` with a `failure` text.
- AC-5.3-6 [GPU host]: Through the real Wyoming adapter and the fixture model on localhost, `transcript` to `handled` has a p95 of less than 20 ms.
- AC-5.3-7: When `handle` gets "Turn on the Lights.", `FakeClassifier` records the text "turn on the lights".

**Proofs.**

- P-5.3-2, P-5.3-3. Argument: the context is `context_text(area_of_device(id), [])`. With no ID, the area is `None`, and `context_text` writes `none` (P-3.3-2).
- P-5.3-7. Argument: `handle` passes `normalize(text)` to `predict` (P-3.1-1).
- P-5.3-5. Argument: errors from `predict` and `decide` go through one `map_err` to the failure response.
- P-5.3-1, P-5.3-4, P-5.3-6. Evidence: test.

---

### 5.4 Composition Root

**Goal:** Add `run`, which loads the configuration, makes the adapters, starts the sync, loads the System One model and starts the Wyoming server. Jarvis now starts as one process that HA can use.

- **Implements:** G8, G10. **Depends on:** 5.3.
- **Files:** modify `src/lib.rs`, `jarvis.toml`.

**Model.**

- `run(config_path)`: `Config::load`, `Secrets::resolve`, then the adapters (`HaHome::start`, sub-phase 2.4), the sync task, the model and the Wyoming server.
- `main` calls `run` only for the command `jarvis serve <config>`. With no command or an unknown command, `main` prints the usage and returns. Thus `./jarvis` still exits with code 0 (sub-phase 0.14).
- The model folder is `<model.dir>/fixture`. Sub-phase 13.9 adds the training state.
- The classifier is in an `ArcSwap`. A swap affects the next `handle`.
- On SIGINT or SIGTERM, `run` stops accepting connections, waits up to 5 s for the running `handle` calls and `after` work, saves each store that has unsaved changes, and returns. Later sub-phases add their stores to this save (for example sub-phase 11.3).
- This sub-phase adds `model.dir` to `jarvis.toml`.

**Acceptance criteria.**

- AC-5.4-1: When the configuration is not valid, `run` returns an error before it opens a port.
- AC-5.4-2: When HA is not available at startup, the Wyoming server accepts connections and the sync tries again.
- AC-5.4-3: When no model folder exists, `run` returns an error that names the folder.
- AC-5.4-4: When the classifier is swapped, the next `handle` uses the new classifier.
- AC-5.4-5: When `main` runs without a command, it prints the usage and returns.
- AC-5.4-6: When `run` gets a shutdown signal, it stops accepting connections and returns within 5 s of virtual time.
- AC-5.4-7: When `responses.toml` has a template with a variable that its pool cannot use, `run` returns an error that names the pool before it opens a port.

**Proofs.**

- P-5.4-1. Argument: `run` calls `Config::load` and `Secrets::resolve` first and returns on `Err`, before the server binds.
- P-5.4-3. Argument: `ModelLoader::load` returns `Err` with the folder (P-3.12c-1), and `run` returns it before the server binds.
- P-5.4-4 (AC-5.4-4). Ensures: each `handle` uses exactly one classifier, the one that was current when it loaded the pointer. Argument: `handle` calls `load()` on the `ArcSwap` once. `store` replaces the pointer atomically, so a later `load()` returns the new one.
- P-5.4-7. Argument: `run` calls `validate_catalog` and `validate_pools` (P-4.2-6) before the server binds.
- P-5.4-2, P-5.4-5, P-5.4-6. Evidence: test.

**Field log.** On the GPU host, let the firewall accept port 10700 only from the HA host (`01-architecture.md` section 10). Record that another host cannot connect.
