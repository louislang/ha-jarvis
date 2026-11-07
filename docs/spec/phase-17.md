# Phase 17: Plugins

This phase adds the plugin layer (`01-architecture.md` decision 30). Data from a remote server reaches Jarvis only through a plugin: System Two gets each plugin as a tool, and a System One fetch intent names a plugin. The first plugin is the weather forecast from Open-Meteo. The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 9 and before phase 10.

---

### 17.1 Plugin Port and Cache

**Goal:** Add the `Plugin` port and the plugin registry with a result cache and a deadline. Both System Two and System One can then get remote data through one path, and a repeated request in a few minutes does not call the remote server again.

- **Implements:** G6, G7, G10. **Depends on:** 9.2.
- **Files:** create `src/app/plugins.rs`. Modify `src/ports.rs`, `src/domain/decide.rs`, `src/testing/fakes.rs`, `src/lib.rs`, `jarvis.toml`.

**Model.**

- `Tool { name, description, parameters }` is in `src/ports.rs`. `parameters` is the JSON schema of the arguments.
- `Plugin::tool() -> Tool`. `Plugin::fetch(arguments, home) -> Result<Value>` gives the typed result for the arguments and the `HomeConfig` of the house. The adapter builds the request and parses the reply, so no model sees the remote API.
- `PluginCall { plugin, arguments }` is in `src/domain/decide.rs`, because sub-phase 17.4 puts it in `Job::Fetch`. `plugin` is a tool name. `arguments` is a JSON object.
- `Plugins` in `src/app/plugins.rs` is the registry. It keys each plugin by its tool name. `tools()` gives the tools of all plugins. `fetch(call, home)` runs the plugin of the call:
  1. A call of a name that no plugin has gives an error.
  2. The cache key is the plugin name and the JSON text of the arguments. `serde_json::Map` keeps its keys in order, so equal arguments give equal text.
  3. When the cache has a result for the key from less than `plugins.cache_s` ago (`Clock`), `fetch` returns it and makes no request.
  4. Else `fetch` calls the plugin with the deadline `plugins.timeout_ms`. A longer wait is a timeout error. A result replaces the old entry of its key. An error is not cached.
- The key has no `HomeConfig`. The house location changes almost never, and an entry lives only `cache_s`.
- `moka` is not used, because its clock is not the `Clock` port, so tests cannot control it.
- `FakePlugin` has a tool, a script of results, and the record of each `fetch` with its arguments and `HomeConfig`. It can wait without end.
- This sub-phase adds `[plugins]` (`cache_s`, `timeout_ms`).

**Acceptance criteria.**

- AC-17.1-1: When `fetch` runs twice with the same call within `cache_s`, `FakePlugin` records one fetch, and both results are equal.
- AC-17.1-2: When the second `fetch` runs `cache_s` or more after the first, `FakePlugin` records two fetches.
- AC-17.1-3: When two calls have different arguments, `FakePlugin` records two fetches.
- AC-17.1-4: When `FakePlugin` returns an error, `fetch` returns the error, and the next `fetch` of the same call makes a new fetch.
- AC-17.1-5: When the call names no plugin, `fetch` returns an error.
- AC-17.1-6: When `FakePlugin` gives no result within `timeout_ms`, `fetch` returns a timeout error.
- AC-17.1-7: When the registry has two plugins, `tools` gives both tools.

**Proofs.**

- P-17.1-1, P-17.1-2. Argument: `fetch` returns the stored result if and only if the key has an entry and `now - fetched < cache_s`. Else it calls the plugin.
- P-17.1-3. Argument: the key has the JSON text of the arguments, so different arguments give different keys.
- P-17.1-4. Argument: only the `Ok` arm stores a result.
- P-17.1-5. Argument: the lookup by name returns `None`, and the `None` arm returns an error before any fetch.
- P-17.1-6. Argument: the plugin call is inside a timeout of `timeout_ms`, and the timeout arm returns an error.
- P-17.1-7. Evidence: test.

---

### 17.2 Plugin Tools in System Two

**Goal:** Give System Two each plugin as its own typed tool, and run its calls through the registry. System Two can then answer with remote data, and it never sees a raw API call.

- **Implements:** G7. **Depends on:** 17.1.
- **Files:** modify `src/ports.rs`, `src/adapters/openai.rs`, `src/app/conversation.rs`, `src/domain/state_cache.rs`, `src/testing/fakes.rs`, `src/lib.rs`.

**Model.**

- `Step::Plugin(PluginCall, reply)` has the same `reply` as `Step::Call` (sub-phase 7.4).
- `OpenAiReasoner` gets the tools of the registry from `src/lib.rs`. Each `answer` request has a function tool with `strict: true` for each of them, next to `call_service` and `get_states`.
- A tool call with the name of a plugin tool gives `Step::Plugin` with that name and the arguments. Arguments that are not a JSON object give a tool error, as in AC-7.4-3. A plugin call counts toward `openai.max_tool_calls`.
- The `after` work of `Defer(Llm)` (sub-phase 7.5) runs each `Step::Plugin` with `Plugins::fetch` and the `HomeConfig` of the hot cache. It sends the JSON text of the result, or "error: " and the error text, back on `reply`. A plugin only reads data, so the step has no `allowed_domains` check and no `Home` call.
- `StateCache` keeps the whole `HomeConfig` of the snapshot, and `config()` gives it.

**Acceptance criteria.**

- AC-17.2-1: When the reasoner has the tool of `FakePlugin`, each `answer` request to the mock has a function tool with its name, description and parameters, with `strict: true`.
- AC-17.2-2: When the mock returns a call of the plugin tool, `answer` gives `Step::Plugin` with the name and the arguments. When the consumer sends the result, the adapter sends it to the mock and then gives the final text.
- AC-17.2-3: When one answer has `max_tool_calls` plugin calls and then one `call_service` call, `answer` gives no `Step::Call` and sends the tool error "limit reached".
- AC-17.2-4: When `FakeReasoner` gives `Step::Plugin`, `FakePlugin` records the fetch with the `HomeConfig` of the hot cache, `FakeHome` records no call, and the tool result is the JSON text of the result.
- AC-17.2-5: When `FakePlugin` returns an error for a `Step::Plugin`, the tool result starts with "error: ", and the next step runs.

**Proofs.**

- P-17.2-1, P-17.2-2. Evidence: test.
- P-17.2-3. Argument: the adapter counts all tool calls of one answer with one counter (P-7.4-4).
- P-17.2-4. Argument: the `Plugin` arm of the consumer calls only `Plugins::fetch`. It has no `Home` call.
- P-17.2-5. Argument: the `Err` arm of the `Plugin` arm sends the error text as the result and does not end the stream.

---

### 17.3 Open-Meteo Weather Plugin

**Goal:** Add the weather plugin `get_weather(when)` with the Open-Meteo forecast API and the house location from HA. Jarvis can then tell the forecast with no API key and no HA weather integration.

- **Implements:** G6, G7. **Depends on:** 17.2.
- **Files:** create `src/adapters/open_meteo.rs`. Modify `src/domain/state.rs`, `src/domain/contracts.rs`, `src/testing/house.json`, `src/lib.rs`, `jarvis.toml`, `Cargo.toml`, `benches/domain.rs`.

**Model.**

- `HomeConfig` gets `latitude`, `longitude` and `unit_system.temperature` (`°C` or `°F`) from the HA `get_config` result. `house.json` gets a test location, `41.88` and `-87.63`, not the location of the real house, and `°F`.
- `OpenMeteo` implements `Plugin` with one `reqwest` client and `plugins.weather.url`.
- `tool()`: the name `get_weather`, a one-line description, and the parameters `{ when: "today" | "tomorrow" }`, with `when` required and no other property.
- `fetch(arguments, home)`:
  1. A `when` that is not `today` or `tomorrow` gives an error before any request.
  2. `GET url` with `latitude`, `longitude`, `timezone` (the HA time zone), `temperature_unit` (`fahrenheit` for `°F`, else `celsius`), `forecast_days = 2` and `daily = weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max`.
  3. A status that is not a success gives an error.
  4. `forecast(body, day)` is a pure function. `day` is 0 for `today` and 1 for `tomorrow`. It reads that index of each `daily` array and gives `{ condition, high, low, precipitation_chance }`. `high` and `low` are the temperatures rounded to the nearest integer. `precipitation_chance` is the percent. A missing value, a `null` or a weather code that is not in the table gives an error.
- `condition(code)` maps the WMO weather code of Open-Meteo to a word: 0 `clear`, 1 `mostly clear`, 2 `partly cloudy`, 3 `cloudy`, 45 and 48 `foggy`, 51 to 57 `drizzly`, 61 to 67 and 80 to 82 `rainy`, 71 to 77 and 85 to 86 `snowy`, 95 to 99 `stormy`.
- `src/lib.rs` gets `WeatherConfig`. Sub-phase 17.3b registers `OpenMeteo`.
- `HomeConfig` derives `Deserialize`, so `src/domain/contracts.rs` gets the external type specification of `UnitSystem` (`02-rules.md` section 5.1).
- `benches/domain.rs` builds a `HomeConfig`, so it gets the new fields.
- This sub-phase adds `[plugins.weather]` (`url`) and the `reqwest` features `json` and `query`.

**Acceptance criteria.**

- AC-17.3-1: When `house.json` is parsed, `config` has the latitude `41.88`, the longitude `-87.63` and the temperature unit `°F`.
- AC-17.3-2: When `fetch` runs with `when = today` and the reference house config, the mock receives one `GET` with the latitude, the longitude, `timezone = America/Chicago`, `temperature_unit = fahrenheit`, `forecast_days = 2` and the four daily variables.
- AC-17.3-3: When the temperature unit is `°C`, the request has `temperature_unit = celsius`.
- AC-17.3-4: When the mock returns weather code 2, a maximum of 71.6, a minimum of 54.4 and a precipitation probability of 20 for the first day, `fetch` with `when = today` gives `{ condition: "partly cloudy", high: 72, low: 54, precipitation_chance: 20 }`.
- AC-17.3-5: When `when = tomorrow`, the result has the values of the second day.
- AC-17.3-6: When `forecast` runs twice on the same body and day, the two results are equal.
- AC-17.3-7: When `when` is `monday`, `fetch` returns an error, and the mock receives no request.
- AC-17.3-8: When the mock returns status 500, a `null` value or weather code 42, `fetch` returns an error.
- AC-17.3-9: When `tool` runs, the name is `get_weather` and the `when` parameter allows only `today` and `tomorrow`.

**Proofs.**

- P-17.3-4, P-17.3-5. Argument: `forecast` reads index `day` of each `daily` array, and `day` is 0 for `today` and 1 for `tomorrow`. `condition(2)` is `partly cloudy`, and the temperatures round to 72 and 54.
- P-17.3-6. Argument: `forecast` is a pure function of the body and the day.
- P-17.3-7. Argument: the `when` check returns an error before the request is built.
- P-17.3-8. Argument: the status check, the `Option` of each value and the `None` arm of `condition` each return an error.
- P-17.3-1 to P-17.3-3, P-17.3-9. Evidence: test.

---

### 17.3b One Plugin Registry

**Goal:** Register `OpenMeteo` in one registry that `run` builds, and give the registry to System Two and its tools to `OpenAiReasoner`. System Two can then call `get_weather`.

- **Implements:** G7. **Depends on:** 17.3.
- **Files:** modify `src/lib.rs`, `src/testing/e2e.rs`.

**Model.**

- The 17.3 change was 442 lines, so it is split. Sub-phase 17.3 adds the adapter, and 17.3b adds the registry.
- `src/lib.rs` registers `OpenMeteo` in the registry of `run`. Tests that build the application use `FakePlugin`, so no test calls the real API.
- `run` builds one registry and gives its tools to `OpenAiReasoner` and the registry to System Two. `start_app` takes the registry as a parameter, and the pipeline harness gives it a registry with no plugins until sub-phase 17.4.

**Acceptance criteria.**

- AC-17.3b-1: When `run` builds the registry from `jarvis.toml`, `tools` has `get_weather`.

**Proofs.**

- P-17.3b-1. Evidence: test.

---

### 17.4 Weather Intent through the Plugin

**Goal:** Make each System One fetch intent name a plugin, and move the `weather` intent from the HA service `weather.get_forecasts` to `get_weather`. "What is the weather like" then gets a filler, and then the Open-Meteo forecast.

- **Implements:** G3, G6. **Depends on:** 17.3b.
- **Files:** modify `src/domain/decide.rs`, `src/domain/catalog.rs`, `src/app/conversation.rs`, `src/testing/fakes.rs`, `src/testing/e2e.rs`, `src/lib.rs`, `src/domain/responses.rs`, `intents.toml`, `responses.toml`.

**Model.**

- `Intent` gets `plugin`: the tool name of the plugin of a fetch intent, for example `get_weather`.
- `Job::Fetch { call: PluginCall, pool }` replaces the service call of sub-phase 4.6.
- `decide` for a fetch intent, after the slots of step 3 (sub-phase 4.5d):
  1. An intent without `plugin` gives `Fallback`.
  2. Render the `data` of the intent as in step 8 of sub-phase 4.5d. The rendered data is the `arguments`. A data value that does not render gives `Fallback`.
  3. Return `Defer` with a filler and `Job::Fetch { call: PluginCall { plugin, arguments }, pool: intent.name }`.
- A fetch needs no area and no selection. Its `service`, `target`, `entity` and `select` are not used.
- A command call always has `return_response: false`.
- The `after` work of `Defer(Fetch)` (sub-phase 4.9d) runs `Plugins::fetch` with the `HomeConfig` of the hot cache instead of `Home::call`. The plugin result is `response` in the answer pool. The tests of AC-4.9d-2 and AC-4.9d-3 get the result from `FakePlugin`.
- The pipeline harness (sub-phase 9.1) builds its registry with `FakePlugin`. Thus AC-9.2-2 makes no remote call. The harness no longer adds a `weather.forecast_home` state to its house.
- The test of the HA condition words in `src/domain/responses.rs` (fix PR "Weather Condition Words") reads the old `weather` pool, so this sub-phase deletes it. The plugin gives the condition words (sub-phase 17.3), and AC-17.4-5 checks the new pool.
- The `weather` intent of `intents.toml` gets `plugin = "get_weather"` and `data = { when = "today" }`, and loses `service`, `target`, `entity` and the old `data`. Its pool in `responses.toml` is "Today will be {{ response.condition }}, with a high of {{ response.high }} degrees."

**Acceptance criteria.**

- AC-17.4-1: When the intent is `weather`, the result is `Defer` with a filler and `Job::Fetch` with the plugin `get_weather`, the arguments `{ when: "today" }` and the pool `weather`. This replaces AC-4.6-3.
- AC-17.4-2: When the intent is `weather` and the request has no area, the result is `Defer` with `Job::Fetch`.
- AC-17.4-3: When a fetch intent has no `plugin`, the result is `Fallback`.
- AC-17.4-4: When dispatch gets `Defer` with `Job::Fetch`, `FakePlugin` records the fetch with the arguments, and `FakeHome` records no call. This replaces AC-4.9d-1.
- AC-17.4-5: When `FakePlugin` gives `{ condition: "partly cloudy", high: 72, low: 54, precipitation_chance: 20 }` for the `weather` intent of `intents.toml` and the pools of `responses.toml`, `FakeHome` records the announce "Today will be partly cloudy, with a high of 72 degrees."

**Proofs.**

- P-17.4-1 (AC-17.4-1). Ensures: a fetch intent gives `Fallback`, or `Defer` with `Job::Fetch` whose `call.plugin` is the `plugin` of the intent and whose `pool` is the intent name. Argument: the fetch branch builds the `PluginCall` from `intent.plugin` and the rendered data, and the pool from `intent.name`. This replaces P-4.6-3.
- P-17.4-2 (AC-17.4-2). Ensures: the decision of a fetch intent does not depend on the area. Argument: `act` matches `Kind::Fetch` before the arms that need an area.
- P-17.4-3 (AC-17.4-3). Ensures: a fetch intent with `plugin = None` gives `Fallback`. Argument: the fetch branch returns `Fallback` in its `None` arm, before it builds a `Job`.
- P-17.4-4. Argument: the `Fetch` arm of `after` calls only `Plugins::fetch`. It has no `Home::call`.
- P-17.4-5. Evidence: test.

## Field Log

These steps are not tests and gate nothing.

1. Ask "What is the weather like?" on a satellite. Compare the answer with the Open-Meteo forecast for the HA location.
2. Ask System Two "Will it rain tomorrow?". Make sure the log shows one `get_weather` call with `when = tomorrow`.
