# Phase 18: Queries and Media

This phase adds System One query intents about the house, Music Assistant playback and Plex playback. The queries read only the hot cache and make no service call. The music intents call `music_assistant.play_media` and the `media_player` services on the speaker of the area, or on a Music Assistant speaker group. The Plex intents call `media_player.play_media` with a `plex://` content ID on the TV. The list of new Plex shows comes from the Plex server, so it is a plugin (`01-architecture.md` decision 30). The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 17 and before phase 10.

Each sub-phase that adds an intent adds its seed examples to `intents.toml` and its pool to `responses.toml` (`01-architecture.md` section 12). The training set then changes, so `just fixture-model` trains the fixture model again (sub-phase 3.9), and CI trains it again because the cache key is the hash of the training set. AC-3.12-1 to AC-3.12-4 then check the new seed examples. When the fixture model does not learn a new example, the PR changes the examples, not the tests. Jarvis serves the fixture model until sub-phase 13.9, and the training runs of phase 13 use the same seed examples. The tests of the intent sub-phases decide with the catalog of `intents.toml`, the pools of `responses.toml` and the reference house. A test that needs an entity that the reference house does not have adds it to the snapshot before it loads the cache. The tests of AC-3.2-1 and AC-3.2c-2 in `src/domain/catalog.rs` list the intent names in the order of `intents.toml`, so each such sub-phase adds its names there. The fixture model gives each seed example a context by its position, so a new intent can move the contexts of the examples after it. The order of the new intents can change to keep the existing tests passing.

---

### 18.1 List Queries

**Goal:** Add queries over a set of entities: the target `house`, the intent `state` filter and the template variable `entities`. "Which lights are on" and "who is home" can then answer with a list of names.

- **Implements:** G3, G9. **Depends on:** 17.4.
- **Files:** modify `src/domain/catalog.rs`, `src/domain/select.rs`, `src/domain/decide.rs`, `src/domain/responses.rs`.

**Model.**

- `Target::House`: all matching entities of the house. `select` with `target = "house"` returns the house matches (the `house_matches` of sub-phase 4.4c). The result does not depend on the area.
- `Intent.state: Option<String>`: the state that a list query keeps, for example `on` or `home`.
- A list query is a query with `target = "entities"` or `target = "house"`. A query with `target = "house"` needs no area. Step 3 of sub-phase 4.5b still applies: an area slot that matches no area gives `Defer` with `Job::Llm`, and a known area gives the `area` variable.
- The branch of a list query:
  1. `select` gives the selection. An empty selection gives `Defer` with `Job::Llm` (P-4.6-2).
  2. `kept(selection, state, cache)` gives the IDs of the selection that have a state in the cache, in selection order. When `state` is `Some(s)`, it keeps only the IDs whose state is `s`.
  3. The render context gets `entities`: a list with one object for each kept ID. Each object has the state, the attributes and the name, as `entity` (sub-phase 4.5c).
  4. Return `Reply` with the rendered reply and no call. An empty `entities` list does not give `Defer`, so the pool can say "No lights are on."
- `state` applies only to a list query.
- `validate_pools` (sub-phase 4.2): `entities` is a variable only of the pool of a list query. `entity` stays a variable only of an intent with `target = "entity"`, so a `house` pool cannot use it.
- The templates use the `minijinja` filters `map`, `join`, `length`, `round`, `int` and `replace`, the inline `if`, and `{% for %}` with `{% else %}`. These are built in, so this sub-phase adds no crate feature.

**Acceptance criteria.**

- AC-18.1-1: When an intent has `target = "house"` and the filter `{ domain = "person" }`, `select` returns `person.ali` and `person.louis` for the Kitchen and for the Garage.
- AC-18.1-2: When a query has the filters of `lights_on`, `target = "entities"`, `state = "on"` and the pool `{{ entities | map(attribute="name") | join(", ") }}`, and the satellite is in the Living Room, the result is `Reply` with the text "Ali's Living Room Lamp, TV Lamp" and no call.
- AC-18.1-3: When no selected entity has the `state` of a list query, the render context has an empty `entities`, and the result is `Reply` with no call.
- AC-18.1-4: When the selection of a list query is empty, the result is `Defer` with `Job::Llm`.
- AC-18.1-5: When a query has `target = "house"`, and the request has no area slot and no satellite area, the result is `Reply`.
- AC-18.1-6: When a list query has no `state`, `entities` has each selected entity that has a state, with its name, state and attributes.
- AC-18.1-7: When the pool of a `target = "entity"` query or of a command uses `entities`, or the pool of a `target = "house"` query uses `entity`, `validate_pools` returns an error with the pool and the variable.

**Proofs.**

- P-18.1-1 (AC-18.1-1). Ensures: for `target = "house"`, each result entity is a candidate (`picked`), and each candidate entity of the cache is in the result, for any area. Argument: the `House` arm returns `house_matches`, which does not read the area. Its loop adds the invariant that each candidate before `index` is in `house`.
- P-18.1-2 (AC-18.1-2, AC-18.1-3, AC-18.1-6). Ensures: each ID of `kept(ids, state, cache)` is in `ids` and has a state in the cache. When `state` is `Some(s)`, that state is `s`. Each ID of `ids` that has such a state is in the result. Argument: `kept` is one pass over `ids` with this condition. The loop invariant states both directions for the IDs before `index`.
- P-18.1-3 (AC-18.1-4). Ensures: a list query gives `Defer` with `Job::Llm`, or `Reply` with `call = None` and a selection that is not empty. Argument: the branch returns `Defer` when `select` is empty, before it builds the reply, and it builds `Reply { call: None }`.
- P-18.1-4 (AC-18.1-5). Ensures: for a `house` query, a `Reply` has `call = None` and a selection that `targeted` gives for the house, also when the area is `None`. Argument: `act` matches a `house` query before the arm that needs an area, as for a fetch (P-17.4-2).
- P-18.1-5 (AC-18.1-7). Evidence: test. The variables of a template come from `minijinja` `undeclared_variables` (P-4.2-6).

---

### 18.2 Light Queries

**Goal:** Add the queries about lights. "Is the garage light on", "which lights are on" and "how bright is the office light" then get a local answer.

- **Implements:** G3, G9. **Depends on:** 18.1.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`, `justfile` (the fixture epochs, sub-phase 3.9).

**Model.**

- `lights_query`: `target = "entities"` with the filters of `lights_on`, `state = "on"`. It answers "is the garage light on" and "which lights are on". Examples: "is the [garage](area) light on", "which lights are on", "are the lights on in the [kitchen](area)", "are any lights on". Pool: `{% if entities %}{{ entities | map(attribute="name") | join(", ") }} {{ "is" if entities | length == 1 else "are" }} on.{% else %}No lights are on in the {{ area }}.{% endif %}`
- `brightness_query`: `target = "entities"`, `select = [{ domain = "light" }]`, `state = "on"`. Examples: "how bright is the [office](area) light", "how bright are the lights", "what is the brightness of the [bedroom](area) lights". HA gives `brightness` from 0 to 255. An on/off light has no `brightness`, and the pool names it without a value, so one such light does not send the whole request to System Two. Pool: `{% for light in entities %}{{ light.name }} {% if light.attributes.brightness is defined and light.attributes.brightness is number %}is at {{ (light.attributes.brightness / 2.55) | round | int }} percent{% else %}has no brightness value{% endif %}.{{ " " if not loop.last else "" }}{% else %}No lights are on in the {{ area }}.{% endfor %}`
- The area of both intents is the area slot, else the satellite area (sub-phase 4.5b).

**Acceptance criteria.**

- AC-18.2-1: When the intent is `lights_query` with the area slot "garage", the reply is "No lights are on in the garage." and there is no call.
- AC-18.2-2: When the intent is `lights_query` and the satellite is in the Kitchen, the reply is "Strawberry Lamp is on."
- AC-18.2-3: When the intent is `brightness_query`, the satellite is in the Office, and only Desk LED (brightness 255) and Office Lamp (brightness 128) are on there, the reply is "Desk LED is at 100 percent. Office Lamp is at 50 percent." When the satellite is in the Bedroom, the reply is "Bedroom Fan Light is at 100 percent. Bedroom Lamp Left has no brightness value. Bedroom Lamp Right has no brightness value. Bedroom Corner Lamp has no brightness value."
- AC-18.2-4: When `intents.toml` and `responses.toml` are read, `validate_catalog` and `validate_pools` return `Ok`.

**Proofs.**

- P-18.2-1 to P-18.2-4. Evidence: test. The answers come from the catalog data, the reference house and `minijinja`. The rules are P-18.1-2 and P-18.1-3.

---

### 18.3 Door, Window and Lock Queries

**Goal:** Add read-only queries about locks, the garage door and windows. "Is the front door locked" then gets a local answer, and no voice command can lock or unlock a door.

- **Implements:** G3. **Depends on:** 18.2.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`.

**Model.**

- The reference house has no lock, no garage door and no window sensor (`01-architecture.md` section 11). The tests add `lock.front_door` "Front Door" (Outside, `locked`), `cover.garage_door` "Garage Door" (Garage, device class `garage`, `open`), `binary_sensor.kitchen_window` "Kitchen Window" (Kitchen, device class `window`, `on`) and `binary_sensor.bedroom_window` "Bedroom Window" (Bedroom, device class `window`, `off`).
- `lock_query`: `target = "house"`, `select = [{ domain = "lock" }]`. Examples: "is the front door locked", "are the doors locked", "is the door locked", "did i lock the door". The model has no entity name slot, so the answer names each lock with its state. Pool: `{% for lock in entities %}The {{ lock.name }} is {{ lock.state }}.{{ " " if not loop.last else "" }}{% endfor %}`
- `garage_door_query`: `target = "house"`, `select = [{ domain = "cover", device_class = "garage" }]`. Examples: "is the garage door open", "is the garage door closed", "did i close the garage door". Pool: `{% for door in entities %}The {{ door.name }} is {{ door.state }}.{{ " " if not loop.last else "" }}{% endfor %}`
- `windows_query`: `target = "house"`, `select = [{ domain = "binary_sensor", device_class = "window" }]`, no `state`. HA gives `on` for an open window and `off` for a closed window. The pool says "All windows are closed." only when each window is `off`. It names each other window with its state, so an `unavailable` sensor is not a closed window. Examples: "are any windows open", "is a window open", "are the windows closed". Pool: `{% set others = entities | rejectattr("state", "eq", "off") | list %}{% for window in others %}{{ window.name }} is {{ "open" if window.state == "on" else window.state }}.{{ " " if not loop.last else "" }}{% else %}All windows are closed.{% endfor %}`
- The three intents have no `service`. `openai.allowed_domains` stays without `lock` and `cover` (sub-phase 7.5b).

**Acceptance criteria.**

- AC-18.3-1: When the intent is `lock_query` from the Kitchen satellite, the reply is "The Front Door is locked." and there is no call.
- AC-18.3-2: When the intent is `garage_door_query`, the reply is "The Garage Door is open."
- AC-18.3-3: When the intent is `windows_query`, the reply is "Kitchen Window is open." When both windows are `off`, the reply is "All windows are closed." When the Kitchen Window is `unavailable` and the Bedroom Window is `off`, the reply is "Kitchen Window is unavailable."
- AC-18.3-4: When `intents.toml` is read, `lock_query`, `garage_door_query` and `windows_query` have no `service`, and no intent has a `service` in the `lock` domain.

**Proofs.**

- P-18.3-1 to P-18.3-3. Evidence: test. The rules are P-18.1-1 to P-18.1-3.
- P-18.3-4. Evidence: test.

---

### 18.4 Climate and Fan Queries

**Goal:** Add the queries about the thermostat and the fans. "What is the thermostat set to", "is the AC on" and "is the fan on" then get a local answer.

- **Implements:** G3, G9. **Depends on:** 18.3.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`.

**Model.**

- `climate_query`: `target = "entity"`, `select = [{ domain = "climate" }]`. Examples: "what is the thermostat set to", "is the ac on", "is the heat on", "what temperature is the thermostat set to". The one thermostat of the house answers from any area (sub-phase 4.4c). The answer gives the target temperature and the HVAC mode, so it answers both questions. Pool: `{% if entity.state == "off" %}The thermostat is off.{% else %}The thermostat is set to {{ entity.attributes.temperature | int }} degrees in {{ entity.state | replace("_", " ") }} mode.{% endif %}`
- `fan_query`: `target = "entity"` with the filters of `fan_on`. Examples: "is the fan on", "is the [office](area) fan on", "is the fan running". Pool: `The {{ entity.name }} is {{ entity.state }}.`

**Acceptance criteria.**

- AC-18.4-1: When the intent is `climate_query` from the Bedroom satellite, the reply is "The thermostat is set to 66 degrees in heat mode." and there is no call.
- AC-18.4-2: When the thermostat state is `off`, the reply is "The thermostat is off."
- AC-18.4-3: When the intent is `fan_query` and the satellite is in the Living Room, the reply is "The Living Room Fan is off."

**Proofs.**

- P-18.4-1 to P-18.4-3. Evidence: test. The rules are P-4.4c-2, P-4.4c-1 and P-4.6-1.

---

### 18.5 Presence and Alarm Queries

**Goal:** Add the queries "who is home" and "is the alarm set". Jarvis then answers them from the `person` and `alarm_control_panel` states.

- **Implements:** G3. **Depends on:** 18.4.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`.

**Model.**

- `who_is_home`: `target = "house"`, `select = [{ domain = "person" }]`, `state = "home"`. Examples: "who is home", "is anyone home", "who is in the house", "is anybody home". Pool: `{% if entities %}{{ entities | map(attribute="name") | join(" and ") }} {{ "is" if entities | length == 1 else "are" }} home.{% else %}Nobody is home.{% endif %}`
- `alarm_query`: `target = "entity"`, `select = [{ domain = "alarm_control_panel" }]`. Examples: "is the alarm set", "is the alarm on", "is the house armed", "did i set the alarm". Pool: `{% if entity.state == "disarmed" %}The alarm is off.{% else %}The alarm is {{ entity.state | replace("_", " ") }}.{% endif %}`

**Acceptance criteria.**

- AC-18.5-1: When the intent is `who_is_home`, the reply is "Ali and Louis are home." and there is no call.
- AC-18.5-2: When `person.louis` is `not_home`, the reply is "Ali is home." When both are `not_home`, the reply is "Nobody is home."
- AC-18.5-3: When the intent is `alarm_query`, the reply is "The alarm is armed home." When the alarm is `disarmed`, the reply is "The alarm is off."

**Proofs.**

- P-18.5-1 to P-18.5-3. Evidence: test. The rules are P-18.1-1, P-18.1-2 and P-4.4c-2.

---

### 18.6 Now Playing Query

**Goal:** Add the query "what is playing". Jarvis then names the media that plays in the area.

- **Implements:** G3, G9. **Depends on:** 18.5.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`.

**Model.**

- `now_playing`: `target = "entities"`, `select = [{ domain = "media_player" }]`, `state = "playing"`. Examples: "what is playing in the [living room](area)", "what is playing on the [living room](area) tv", "what song is this", "what is playing", "what is this song". The area is the area slot, else the satellite area. HA can leave out `media_title` and `media_artist` or set them to `None`, so the pool checks each one. Pool: `{% for player in entities %}{% if player.attributes.media_title is defined and player.attributes.media_title is string %}{{ player.attributes.media_title }}{% if player.attributes.media_artist is defined and player.attributes.media_artist is string %} by {{ player.attributes.media_artist }}{% endif %}{% else %}Something with no title{% endif %} is playing on the {{ player.name }}.{{ " " if not loop.last else "" }}{% else %}Nothing is playing in the {{ area }}.{% endfor %}`
- The media player of a satellite device is not skipped (sub-phase 4.4b), so the answer also names it when it plays.

**Acceptance criteria.**

- AC-18.6-1: When `media_player.carson_living_room_media_player` is `playing` with `media_title` "Karma Police" and `media_artist` "Radiohead", and the satellite is in the Living Room, the reply is "Karma Police by Radiohead is playing on the Carson Living Room Media Player." When the TV also plays with no `media_title`, the reply adds " Something with no title is playing on the Apple TV."
- AC-18.6-2: When the TV is `playing` with `media_title` "Bluey" and no `media_artist`, and the request has the area slot "living room", the reply is "Bluey is playing on the Apple TV."
- AC-18.6-3: When no media player of the area is `playing`, the reply is "Nothing is playing in the living room."

**Proofs.**

- P-18.6-1 to P-18.6-3. Evidence: test. The rules are P-18.1-2 and P-4.4b-6.

---

### 18.7 Music Playback

**Goal:** Add Music Assistant playback by name, radio mode and the queue. "Play jazz in the kitchen" then plays on the kitchen speaker.

- **Implements:** G3, G9. **Depends on:** 18.6.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`.

**Model.**

- The service is `music_assistant.play_media`. `media_id` is the name of an artist, an album, a track or a playlist. Without `media_type`, Music Assistant finds the type itself. Thus one intent plays each type.
- The player is the speaker of the area: `target = "entity"` and `select = [{ domain = "media_player", device_class = "speaker" }]`. The area is the area slot, else the satellite area. The speaker of a satellite device is not skipped (sub-phase 4.4b). Of two or more speakers in the area, the player is the one with the lowest entity ID (sub-phase 4.4c). A speaker that Music Assistant does not control goes in `exclude`.
- `play_music`: examples "play [radiohead](media)", "play [jazz](media) in the [kitchen](area)", "play the album [abbey road](media)", "play my [workout](media) playlist", "play the song [yesterday](media)", "play [hip hop](media)", "play [red hot chili peppers](media)", "play some [lo fi beats](media)". `data = { media_id = "{{ media }}" }`. Pool: `Playing {{ media }}.`
- A media name can have common words. Without the last three examples, the fixture model tagged only "hip" of "play hip hop", and Jarvis played "hip". The fix PR `Fix: Multi-Word Media Names` adds them to `intents.toml`. None of them is held out.
- HA returns the result of `music_assistant.play_media` only after the search of Music Assistant. In the field test on 2026-11-09, this took 6 s to 15 s, and the 5 s `ha.request_timeout_ms` gave a false `failure` announce. Thus `ha.request_timeout_ms` is 30000 (`01-architecture.md` section 9). The reply is already spoken, so the longer wait only removes the false failure. The HA ping also waits up to 30 s for `pong` (sub-phase 2.4c). The fix PR `Fix: HA Call Timeout` changes `jarvis.toml` and the tests that read it.
- `play_radio`: examples "play music like [radiohead](media)", "play something like [miles davis](media)", "start a radio station for [taylor swift](media)". `data = { media_id = "{{ media }}", radio_mode = "{{ true }}" }`. Pool: `Playing music like {{ media }}.`
- `queue_music`: examples "add [yesterday](media) to the queue", "queue [abbey road](media)", "put [hey jude](media) in the queue". `data = { media_id = "{{ media }}", enqueue = "add" }`. Pool: `Adding {{ media }} to the queue.`

**Acceptance criteria.**

- AC-18.7-1: When the intent is `play_music` with the slot `media = radiohead` and the satellite is in the Kitchen, the call is `music_assistant.play_media` with the target `media_player.carson_kitchen` and the data `{ media_id: "radiohead" }`. `media_player.carson_kitchen` is the speaker of the Kitchen satellite device, and its entity ID is lower than `media_player.carson_kitchen_media_player`.
- AC-18.7-2: When the request is from the Office satellite and has the area slot "kitchen", the target is `media_player.carson_kitchen`.
- AC-18.7-3: When the intent is `play_radio`, the data has `radio_mode = true` as a boolean.
- AC-18.7-4: When the intent is `queue_music`, the data has `enqueue = "add"`.
- AC-18.7-5: When the satellite is in the Bedroom, whose only speaker is on the satellite device, the target is `media_player.carson_bedroom`.
- AC-18.7-6: When `Config::parse` reads `jarvis.toml`, `ha.request_timeout_ms` is 30000.
- AC-18.7-7: When the fixture model gets "play hip hop", "play red hot chili peppers" and "play some lo fi beats", the slots contain `media = hip hop`, `media = red hot chili peppers` and `media = lo fi beats`.

**Proofs.**

- P-18.7-1 to P-18.7-5. Evidence: test. The rules are P-4.5c-1, P-4.5c-2, P-4.4b-6, P-4.4c-4 and P-4.5d-1.
- P-18.7-6, P-18.7-7. Evidence: test.

---

### 18.8 Music Controls

**Goal:** Add the next track, pause and volume commands for the speaker of the area. "Next song" and "turn it up" then act on the music in the room.

- **Implements:** G3, G9. **Depends on:** 18.7.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`.

**Model.**

- Each intent has `target = "entity"` and the speaker filter of sub-phase 18.7.
- `music_next`: `media_player.media_next_track`. Examples: "next song", "skip this song", "play the next track", "skip to the next song in the [kitchen](area)". Pool: `Next song.`
- `music_pause`: `media_player.media_pause`. Examples: "pause the music", "stop the music", "pause the music in the [kitchen](area)". Pool: `Pausing the music.`
- `volume_up`: `media_player.volume_up`. Examples: "turn it up", "volume up", "turn up the music", "make the music louder". Pool: `Turning it up.`
- `volume_down`: `media_player.volume_down`. Examples: "turn it down", "volume down", "turn down the music", "make the music quieter". Pool: `Turning it down.`
- `volume_set`: `media_player.volume_set` with `data = { volume_level = "{{ volume | int / 100 }}" }`. HA takes a volume level from 0 to 1. Examples: "set the volume to [50](volume)", "volume [30](volume)", "set the music volume to [20](volume) percent". Pool: `Setting the volume to {{ volume }} percent.`

**Acceptance criteria.**

- AC-18.8-1: When the intent is `music_next` and the satellite is in the Kitchen, the call is `media_player.media_next_track` with the target `media_player.carson_kitchen`.
- AC-18.8-2: When the intent is `music_pause`, `volume_up` or `volume_down`, the call has the service of the intent and the same target.
- AC-18.8-3: When the intent is `volume_set` with the slot `volume = 50`, the call data has `volume_level = 0.5` as a number.

**Proofs.**

- P-18.8-1 to P-18.8-3. Evidence: test. The rules are P-4.5c-1, P-4.4b-6, P-4.4c-4 and P-4.5d-1.

---

### 18.9 Plex Playback

**Goal:** Add Plex movies, the next episode of a show and one episode by number on the TV. "Put on The Matrix" and "season 2 episode 5 of The Office" then play through Plex.

- **Implements:** G3. **Depends on:** 18.8.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`, `src/domain/responses.rs`, `Cargo.toml`, `xtask/src/rules.rs` (`text2num` in the domain crate list).

**Model.**

- Each intent calls `media_player.play_media` on the TV: `target = "entity"` and `select = [{ domain = "media_player", name = "tv" }]`, as `tv_show`. HA resolves the `plex://` content ID through the Plex integration. The library names are `Movies` and `TV Shows`. Change them in `intents.toml` when the Plex server uses other names.
- `play_movie`: examples "put on [the matrix](movie)", "play the movie [inception](movie)", "watch [toy story](movie)", "put on the movie [up](movie)". `data = { media_content_type = "movie", media_content_id = 'plex://{"library_name": "Movies", "title": "{{ movie }}"}' }`. Pool: `Putting on {{ movie }}.`
- `continue_show`: examples "continue [the office](show)", "play the next episode of [bluey](show)", "keep watching [severance](show)", "next episode of [the office](show)". `data = { media_content_type = "episode", media_content_id = 'plex://{"library_name": "TV Shows", "show_name": "{{ show }}", "resume": 1}' }`. With `resume`, Plex starts the On Deck episode of the show, else the first episode. Pool: `Continuing {{ show }}.`
- `play_episode`: examples "play season [two](season) episode [five](episode) of [the office](show)", "put on [bluey](show) season [one](season) episode [three](episode)", "watch [severance](show) season [1](season) episode [1](episode)". `data = { media_content_type = "episode", media_content_id = 'plex://{"library_name": "TV Shows", "show_name": "{{ show }}", "season_number": {{ season | number }}, "episode_number": {{ episode | number }}}' }`. Pool: `Playing {{ show }}, season {{ season }}, episode {{ episode }}.`
- The second pass writes numbers as words (`tn_normalize_sentence`, sub-phase 6.7), and HA refuses "two" as a number. Thus the render environments of `responses.rs` and `decide.rs` get the `minijinja` filter `number`: the `text2num` `text2digits` of the value with `Language::english()` ("two" gives "2"). Digits stay unchanged, and other text does not render, which gives `Defer` with `Job::Llm`. This sub-phase also changes the `volume_set` data to `{{ volume | number | int / 100 }}`.

**Acceptance criteria.**

- AC-18.9-1: When the intent is `play_movie` with the slot `movie = the matrix`, the call is `media_player.play_media` with the target `media_player.apple_tv`, `media_content_type = "movie"`, and the `media_content_id` `plex://{"library_name": "Movies", "title": "the matrix"}`.
- AC-18.9-2: When the intent is `continue_show` with the slot `show = the office`, the `media_content_id` is `plex://{"library_name": "TV Shows", "show_name": "the office", "resume": 1}`.
- AC-18.9-3: When the intent is `play_episode` with the slots `show = the office`, `season = 2` and `episode = 5`, the `media_content_id` is `plex://{"library_name": "TV Shows", "show_name": "the office", "season_number": 2, "episode_number": 5}`.
- AC-18.9-4: When the slots are `season = two` and `episode = five`, the `media_content_id` is the one of AC-18.9-3. When `volume_set` has `volume = fifty`, `volume_level` is 0.5.

**Proofs.**

- P-18.9-1 to P-18.9-4. Evidence: test. The rules are P-4.5c-1 and P-4.5d-2. The words to digits come from `text2num` (`02-rules.md` section 5, rule 5).

---

### 18.10 TV Controls

**Goal:** Add pause, skip ahead and stop for the TV. "Pause the TV" then acts on the TV of the house.

- **Implements:** G3. **Depends on:** 18.9.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`, `src/app/conversation.rs`.

**Model.**

- Each intent has `target = "entity"` and the TV filter of sub-phase 18.9.
- `tv_pause`: `media_player.media_pause`. Examples: "pause the tv", "pause the show", "pause the movie". Pool: `Pausing the TV.`
- HA has no relative seek, and a player does not update `media_position` while it plays. The position is valid at `media_position_updated_at` (HA media player documentation). Thus `Request` gets `now`, the time of the request, which `handle` takes from `Clock`.
- `position(state, attributes, now)` is a pure function. It gives `media_position` plus the seconds from `media_position_updated_at` to `now` when `state` is `playing`, else `media_position`. A missing value, or a time that does not parse as a `jiff` `Timestamp`, gives `None`. The `entity` object gets `position` when the result is `Some`.
- `tv_skip`: `media_player.media_seek` with `data = { seek_position = "{{ entity.position + 30 }}" }`. Examples: "skip ahead", "skip ahead on the tv", "fast forward the movie". Pool: `Skipping ahead.`
- `tv_stop`: `media_player.media_stop`. Examples: "stop the tv", "stop the movie", "stop the show". Pool: `Stopping the TV.`

**Acceptance criteria.**

- AC-18.10-1: When the intent is `tv_pause`, the call is `media_player.media_pause` with the target `media_player.apple_tv`.
- AC-18.10-2: When the TV is `paused` with `media_position = 600`, the `tv_skip` call is `media_player.media_seek` with `seek_position = 630` as a number.
- AC-18.10-3: When the TV is `playing` with `media_position = 600` and `media_position_updated_at` 60 s before `now`, the `tv_skip` call has `seek_position = 690`.
- AC-18.10-4: When the intent is `tv_stop`, the call is `media_player.media_stop` with the target `media_player.apple_tv`.
- AC-18.10-5: When the TV has no `media_position`, the result of `tv_skip` is `Defer` with `Job::Llm`.

**Proofs.**

- P-18.10-2, P-18.10-3 (AC-18.10-2, AC-18.10-3). Ensures: `position` is `media_position + (now - updated)` in seconds when `state` is `playing`, else `media_position`, and `None` when a value is missing or does not parse (rule 5.1.9). Argument: `position` matches on the state, the two attributes and the parse result. The tests check the values 630 and 690.
- P-18.10-1, P-18.10-4. Evidence: test. The rules are P-4.5c-1 and P-4.5d-1.
- P-18.10-5. Evidence: test. Without `position`, the expression is undefined, which gives `Defer` with `Job::Llm` (sub-phase 4.5d).

---

### 18.11 Plex Plugin

**Goal:** Add the plugin `get_new_shows` with the Plex server API. System Two and System One can then tell which shows are new and which shows to continue.

- **Implements:** G6, G7. **Depends on:** 18.10.
- **Files:** create `src/adapters/plex.rs`. Modify `src/lib.rs`, `jarvis.toml`.

**Model.**

- `Plex` implements `Plugin` with one `reqwest` client, `plugins.plex.url`, the token and the `Clock` port.
- `src/lib.rs` resolves the token from the variable `plugins.plex.token_env` with `Secrets` (sub-phase 1.2). `[plugins.plex]` is optional, as `[train.runpod]`. Without it, `run` registers no Plex plugin. With it, a missing token fails at startup with the variable name.
- `tool()`: the name `get_new_shows`, a one-line description, and parameters with no property: `{ type: "object", properties: {}, required: [], additionalProperties: false }`.
- `fetch(arguments, home)`:
  1. `GET <url>/library/recentlyAdded` and `GET <url>/library/onDeck`, each with the headers `X-Plex-Token` and `Accept: application/json`.
  2. A status that is not a success gives an error.
  3. `shows(recent, on_deck, now)` is a pure function. It gives `{ added, on_deck }`:
     - `added`: the show names of the items of `MediaContainer.Metadata` of `recent` whose `addedAt` (Unix seconds) is `now` minus 86400 or later. The show name is `grandparentTitle` for `type = "episode"`, `parentTitle` for `season` and `title` for `show`. Other types (for example `movie`) are skipped.
     - `on_deck`: the `grandparentTitle` of each `episode` item of `on_deck`. Other types are skipped.
     - Each list has each name once, in the order of the first item with it.
     - A body without `MediaContainer` gives an error. A `MediaContainer` without `Metadata` gives an empty list, because Plex leaves out an empty `Metadata`. An item of a kept type without its name or its `addedAt` gives an error.
- The arguments and `home` are not used. The registry caches the result for `plugins.cache_s` (sub-phase 17.1).
- System Two gets the tool through the registry (sub-phase 17.2).
- This sub-phase adds `[plugins.plex]` (`url`, `token_env`).

**Acceptance criteria.**

- AC-18.11-1: When `tool` runs, the name is `get_new_shows`, and the parameters are an object with no property and `additionalProperties: false`.
- AC-18.11-2: When `fetch` runs, the mock receives one `GET /library/recentlyAdded` and one `GET /library/onDeck`, each with the header `X-Plex-Token` equal to the token and `Accept: application/json`.
- AC-18.11-3: When `recentlyAdded` has an episode of "Bluey" added 1 h before now, a season of "Severance" added 2 h before now, a second episode of "Bluey", a movie added 1 h before now and an episode of "The Office" added 25 h before now, and `onDeck` has episodes of "The Office" and "Bluey", `fetch` gives `{ added: ["Bluey", "Severance"], on_deck: ["The Office", "Bluey"] }`.
- AC-18.11-4: When both `MediaContainer` objects have no `Metadata`, `fetch` gives `{ added: [], on_deck: [] }`.
- AC-18.11-5: When the mock returns status 401, a body without `MediaContainer`, or an episode without `grandparentTitle`, `fetch` returns an error.
- AC-18.11-6: When `shows` runs twice on the same bodies and time, the two results are equal.
- AC-18.11-7: When `run` builds the registry from a configuration with `[plugins.plex]` and the token variable, `tools` has `get_new_shows`. Without `[plugins.plex]`, `tools` does not have it. With `[plugins.plex]` and no token variable, startup fails with an error that names the variable.

**Proofs.**

- P-18.11-3, P-18.11-4. Argument: `shows` keeps an item only when its type is kept and, for `added`, when `addedAt >= now - 86400`. It appends a name only when the list does not have it. A missing `Metadata` gives the empty list.
- P-18.11-5. Argument: the status check, the `MediaContainer` check and the `Option` of each name and `addedAt` each return an error.
- P-18.11-6. Argument: `shows` is a pure function of the bodies and the time.
- P-18.11-7. Argument: `run` registers `Plex` only inside the `Some` arm of `[plugins.plex]`, and `Secrets::resolve` returns `Err` with the variable name for a missing token (P-1.2-1).
- P-18.11-1, P-18.11-2. Evidence: test.

---

### 18.12 New Shows Intent

**Goal:** Add the System One fetch intent `new_shows` through the Plex plugin. "Are there any new shows" then gets a filler, and then the new shows and the shows to continue.

- **Implements:** G3, G6. **Depends on:** 18.11.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/decide.rs`, `src/domain/catalog.rs`, `src/app/conversation.rs`.

**Model.**

- `new_shows`: `kind = "fetch"`, `plugin = "get_new_shows"`, no `data`. Examples: "are there any new shows", "what is new on plex", "is there anything new to watch", "what shows can i continue". The arguments are the empty object (sub-phase 17.4).
- Pool: `{% if response.added %}New on Plex: {{ response.added | join(", ") }}.{% else %}Nothing new was added to Plex in the last day.{% endif %}{% if response.on_deck %} You can continue {{ response.on_deck | join(", ") }}.{% endif %}`

**Acceptance criteria.**

- AC-18.12-1: When the intent is `new_shows`, the result is `Defer` with a filler and `Job::Fetch` with the plugin `get_new_shows`, empty arguments and the pool `new_shows`.
- AC-18.12-2: When `FakePlugin` gives `{ added: ["Bluey"], on_deck: ["The Office", "Severance"] }`, `FakeHome` records the announce "New on Plex: Bluey. You can continue The Office, Severance."
- AC-18.12-3: When `FakePlugin` gives `{ added: [], on_deck: [] }`, `FakeHome` records the announce "Nothing new was added to Plex in the last day."

**Proofs.**

- P-18.12-1. Argument: the fetch branch builds the `PluginCall` from `intent.plugin` and the rendered empty `data` (P-17.4-1).
- P-18.12-2, P-18.12-3. Evidence: test.

---

### 18.13 Music Search Data

**Goal:** Add the data that a Music Assistant search needs: the best match of a name in a search response, the config entry of each entity, and service calls with no target. Sub-phase 18.13b then plays the best match.

- **Implements:** G3, G9. **Depends on:** 18.12.
- **Files:** modify `src/domain/decide.rs`, `src/domain/state.rs`, `src/domain/state_cache.rs` and `benches/domain.rs` (the new field in their `Entity` values), `src/adapters/ha.rs`.

The search was one sub-phase. With its tests and its Verus proof, it changes about 465 lines, which is more than the limit of 400 (`02-rules.md` section 7.1, rule 8). Thus 18.13 has the domain and adapter part (about 255 lines), and 18.13b has the use case part (about 210 lines). The criteria keep their numbers.

**Model.**

- Without `media_type`, `music_assistant.play_media` plays the best match of any type. In the field test on 2026-11-09, "play jazz" played the track "Jazz" by Mick Jenkins.
- `Entity.config_entry_id: Option<String>` is the config entry of the entity in `config/entity_registry/list`. `music_assistant.play_media` acts only on a Music Assistant player, so the config entry of the target is the config entry of Music Assistant. `music_assistant.search` needs it. Thus Jarvis needs no configuration for it.
- `SearchResults { playlists, artists, albums, tracks }` holds a `Vec<MediaItem>` for each key. `MediaItem { media_type, uri, name }` holds the fields of one item. Both are in `src/domain/decide.rs` and derive `Deserialize`. A missing list is empty. The other keys of the response (`radio`, `audiobooks`, `podcasts`, `image`, `favorite` and more) are ignored.
- `best_match(results, name) -> Option<&MediaItem>` is a pure function. With `normalize` of each name (sub-phase 3.1), it gives the first playlist whose name equals the name, else the first playlist whose name contains it, else the first artist, then album, then track whose name equals it.
- `HaHome::call` (sub-phase 2.5) sends `target` only when `call.target` is not empty. HA adds the target to the service data, and the schema of `music_assistant.search` does not accept `entity_id`. Verified on the real HA on 2026-11-09: `music_assistant.search` succeeds with no `target` key, and fails with `target: {"entity_id": []}` and with an entity target.

**Acceptance criteria.**

- AC-18.13-1: When `best_match` gets the playlists "Hip Hop Mix" and "Hip Hop" (`library://playlist/82`) and the name "hip hop", the result is "Hip Hop".
- AC-18.13-2: When `best_match` gets the playlist "jazzy" and the name "jazz", the result is "jazzy".
- AC-18.13-3: When there is no playlist, and the artists, the albums and the tracks each have an item "Radiohead", the result for "radiohead" is the artist. When only the tracks have "Jazz", the result for "jazz" is that track.
- AC-18.13-4: When no playlist contains the name, and no artist, album or track has the same name, the result is `None`. The track "Jazz Standards" is not a match for "jazz".
- AC-18.13-9: When `call` runs with no target, the `call_service` message that `FakeHaServer` receives has no `target` key.
- AC-18.13-10: When `FakeHaServer` gives an entity registry entry with `config_entry_id`, the entity of the snapshot has that `config_entry_id`.

**Proofs.**

- P-18.13-1 to P-18.13-4 (AC-18.13-1 to AC-18.13-4). Ensures: `best_match(results, name)` is the first item of `equal(playlists) + containing(playlists) + equal(artists) + equal(albums) + equal(tracks)`, and `None` when this sequence is empty. `equal(items)` keeps each item with `normalized(item.name) = normalized(name)`. `containing(items)` keeps each item whose `normalized(item.name)` contains `normalized(name)`. Argument: `best_match` makes the five passes in this order and returns the first item that passes. The loop invariant of each pass states that no earlier item of its list passes. `normalize` gives `normalized` (P-3.1-2).
- P-18.13-9, P-18.13-10. Evidence: test.

---

### 18.13b Music Search Playback

**Goal:** Before `play_music` plays a name, find the best Music Assistant match: a library playlist first, then an artist, an album or a track with the same name. "Play jazz" then plays the playlist "jazzy" of the library, not the first track with the name "Jazz".

- **Implements:** G3, G9. **Depends on:** 18.13.
- **Files:** modify `intents.toml`, `src/domain/catalog.rs`, `src/app/conversation.rs`, `src/testing/fakes.rs`.

The criteria of this part keep the numbers AC-18.13-5 to AC-18.13-8 from before the split, so their tests are named `ac_18_13_5_...` to `ac_18_13_8_...`.

**Model.**

- `Intent.search: bool`, default false. `play_music` has `search = true`. `play_radio` and `queue_music` do not.
- `decide` does not change. The reply of a `Reply` comes from the pool of its intent (sub-phase 4.5c), and that pool has the name of the intent (sub-phase 4.2). Thus `dispatch` finds the intent in the catalog by the pool of the reply. When the intent has `search`, the background work of the call runs `resolve` before the call. `dispatch` returns the reply first (P-4.8-1), so the search adds no response latency.
- `resolve(call) -> ServiceCall`:
  1. The name is the `media_id` string of `call.data`. The config entry is the `config_entry_id` of the registry entry of the first target in the hot cache. When one is missing, `resolve` gives `call` unchanged.
  2. Call `music_assistant.search` with no target, `return_response: true` and the data `{ config_entry_id, name, media_type: ["playlist"], library_only: true }`. The match is `best_match` of the response.
  3. With no match, call `music_assistant.search` with the data `{ config_entry_id, name, media_type: ["artist", "album", "track"] }`. The match is `best_match` of this response. The media types keep the playlists of the streaming providers out of this step.
  4. With a match, `resolve` gives `call` with `media_id` set to the `uri` of the match and `media_type` set to its `media_type`. With no match, it gives `call` unchanged, so Music Assistant plays the name as before.
  5. A search error, or a response that does not parse, is logged. `resolve` then gives `call` unchanged.
- The search call has no target, so `HaHome::call` sends no `target` key (sub-phase 18.13).
- Each search takes 1 s to 2 s on the real house. Each call has the deadline `ha.request_timeout_ms` (sub-phase 18.7).
- `FakeHome` can give different responses to consecutive calls, so a test can give two different search responses.

**Acceptance criteria.**

- AC-18.13-5: When the intent is `play_music` with `media = hip hop` from the Kitchen satellite, the registry entry of `media_player.carson_kitchen` has `config_entry_id = "music_assistant"` (the test sets it in its copy of the reference house), and the first search gives the playlist "Hip Hop" (`library://playlist/82`), the reply is "Playing hip hop." Then `FakeHome` records `music_assistant.search` with no target, `return_response: true` and the data `{ config_entry_id: "music_assistant", name: "hip hop", media_type: ["playlist"], library_only: true }`. Then it records `music_assistant.play_media` on `media_player.carson_kitchen` with `{ media_id: "library://playlist/82", media_type: "playlist" }`.
- AC-18.13-6: When the first search gives no match and the second gives the artist "Radiohead" (`spotify--x7LaiKmM://artist/4Z8W4fKeB5YxbusRsdQVPb`), `FakeHome` records the second search with `media_type: ["artist", "album", "track"]` and no `library_only`. Then it records `play_media` with `{ media_id: "spotify--x7LaiKmM://artist/4Z8W4fKeB5YxbusRsdQVPb", media_type: "artist" }`.
- AC-18.13-7: When no search gives a match, when a search fails, or when the target has no `config_entry_id`, `FakeHome` records `play_media` with the data `{ media_id: "radiohead" }` of AC-18.7-1.
- AC-18.13-8: When the intent is `play_radio` or `queue_music`, `FakeHome` records no `music_assistant.search`.

**Proofs.**

- P-18.13-5, P-18.13-6. Argument: `dispatch` returns the reply before the background work runs (P-4.8-1). `resolve` makes the playlist search first, and the second search only when `best_match` gives `None`.
- P-18.13-7. Argument: each missing value, each error and the `None` of the second `best_match` give `call` unchanged.
- P-18.13-8. Argument: `dispatch` runs `resolve` only when the intent of the reply pool has `search`, and only `play_music` has it.

---

### 18.14 Fast Artist Start

**Goal:** Start the music of an artist match in about 3 s, not in 13 s to 17 s. "Play Radiohead" then plays one track of the artist immediately and adds the artist to the queue.

- **Implements:** G3, G9. **Depends on:** 18.13b.
- **Files:** modify `src/domain/decide.rs`, `src/domain/contracts.rs`, `src/app/conversation.rs`.

**Model.**

- On the Office speaker, `music_assistant.play_media` with a Spotify artist `uri` took 13.4 s (warm) and 17.1 s (cold) before the music started. Music Assistant first loads all tracks of the artist. One track took about 2.9 s. A second `play_media` with the artist `uri` and `enqueue = "add"` returned in 0.1 s and did not stop the track.
- The response of the artist, album and track search (sub-phase 18.13b, step 3) also has tracks. Each track item has an `artists` list. Each entry of this list has the `name` of one artist of the track.
- `MediaItem.artists: Vec<Artist>` holds this list. A missing list is empty. `Artist { name }` is in `src/domain/decide.rs` and derives `Deserialize`. The other keys of an entry are ignored.
- The `uri` of a track artist can be different from the `uri` of the artist match (`library://` and `spotify--x7LaiKmM://`). Thus Jarvis compares the names, as `best_match` does (sub-phase 18.13).
- `first_track(results, artist) -> Option<&MediaItem>` is a pure function. With `normalize` of each name (sub-phase 3.1), it gives the first track of `results.tracks` that has an artist whose name equals `artist`. It does not read the name of the track. A search for "the national" also gives the track "The National Anthem" by Radiohead, and that track is not a match.
- `resolve` (sub-phase 18.13b) now gives a list of calls. When the match has `media_type = "artist"` and `first_track` of the same response with the name of the match gives a track, the list has two calls:
  1. `call` with `media_id` set to the `uri` of the track and `media_type = "track"`.
  2. `call` with `media_id` set to the `uri` of the match, `media_type = "artist"` and `enqueue = "add"`.
- With no such track, and for a playlist, an album or a track match, the list has the one call of sub-phase 18.13b.
- The background work makes the calls in order. When a call fails, it makes no more calls and announces `failure` (sub-phase 4.9c).
- The first track can play again later in the queue of the artist. This is accepted.

**Acceptance criteria.**

- AC-18.14-1: When the first search gives no match, and the second gives the artist "Radiohead" (`library://artist/5`) and the track "Karma Police" (`spotify--x7LaiKmM://track/63OQupATfueTdZMWTxW03A`) with the artist "Radiohead", `FakeHome` records two `play_media` calls on `media_player.carson_kitchen` after the searches. The first has the data `{ media_id: "spotify--x7LaiKmM://track/63OQupATfueTdZMWTxW03A", media_type: "track" }`. The second has the data `{ media_id: "library://artist/5", media_type: "artist", enqueue: "add" }`.
- AC-18.14-2: When the second search gives the artist "The National" (`library://artist/7`) and only the track "The National Anthem" with the artist "Radiohead", `FakeHome` records one `play_media` call after the searches, with the data `{ media_id: "library://artist/7", media_type: "artist" }`.
- AC-18.14-3: When `first_track` gets the tracks "The National Anthem" by "Radiohead" and "Bloodbuzz Ohio" by "The National", and the artist "the national", the result is "Bloodbuzz Ohio". When no track has an artist with the name, the result is `None`.

**Proofs.**

- P-18.14-1, P-18.14-2. Argument: `resolve` gives two calls only when the match is an artist and `first_track` gives a track (P-18.14-3). Else it gives the one call of P-18.13-6. The background work makes the calls in order.
- P-18.14-3 (AC-18.14-3). Ensures: `first_track(results, artist)` is the first item of `credited(results.tracks)`, and `None` when this sequence is empty. `credited(items)` keeps each item that has an entry in `artists` with `normalized(entry.name) = normalized(artist)`. Argument: `first_track` makes one pass over the tracks and returns the first track that passes. The loop invariant states that no earlier track passes. The pass over the artists of one track has the loop invariant that no earlier artist has the name. `normalize` gives `normalized` (P-3.1-2).

---

### 18.15 Speaker Groups

**Goal:** Play music on a Music Assistant speaker group. "Play jazz downstairs" and "play jazz on the downstairs speakers" then play on all speakers of the group "Downstairs Speakers".

- **Implements:** G3, G9. **Depends on:** 18.14.
- **Files:** modify `intents.toml`, `src/domain/select.rs`, `src/domain/decide.rs`, `src/domain/contracts.rs`, `src/adapters/onnx.rs`, `src/testing/house.json`.

Speaker groups were one sub-phase. With the controls, its tests and its Verus proofs, it changes about 480 lines, which is more than the limit of 400 (`02-rules.md` section 7.1, rule 8). Thus 18.15 plays on a group (about 270 lines), and 18.15b selects the target of the music controls on a group (about 210 lines).

**Model.**

- Music Assistant can join speakers in a group player. `music_assistant.play_media` on a group player plays on all members of the group, and Music Assistant keeps them in sync.
- HA has no floors, and no area has the name "Downstairs". Thus "play jazz downstairs" gave an area slot that matches no area, and `decide` asked "Which room?" (sub-phase 10.4).
- Each Music Assistant player has the state attribute `mass_player_type`: `player` or `group`. A group player has no area, the device class `speaker` and the attribute `group_members`: the list of the entity IDs of its members. On the real house on 2026-11-10, `media_player.downstairs` ("Downstairs Speakers") has three members, `media_player.office_speakers` ("Office Speakers") has two, and `media_player.office_speakers_2` ("Office Speakers") has none. The last one is a stale copy.
- The name of an entity is its registry name, else its `friendly_name` (sub-phase 2.3). Thus the hot cache already has all the data of this sub-phase.
- An entity is a group of a text when the state of the entity has `mass_player_type = "group"` and a `group_members` list with one or more items, and `normalized` of its name (sub-phase 3.1) is `normalized(text)` or `normalized(text)` followed by " speakers". Thus "downstairs" and "downstairs speakers" both match "Downstairs Speakers".
- `group(intent, text, cache) -> Option<EntityId>` in `src/domain/select.rs` is a pure function. It gives the candidate of `intent` (sub-phase 4.4b: it fits a filter, is not excluded and is not skipped) that is a group of `text`, with the lowest entity ID (the rule of sub-phase 4.4c). It gives `None` when no candidate is a group of `text`. A group player is a `media_player` with the device class `speaker`, so it is a candidate of each intent with the speaker filter of sub-phase 18.7, and of no other intent of `intents.toml`.
- Step 3 of `decide` (sub-phase 4.5b) changes for an area slot that matches no area. For a command with a `service`, when `group` of the slot value gives a player, the result is `Reply` with the reply of the intent and the call of its service on that player, with the data of the intent. Else the result is `Ask` from `ask_area`, as before (sub-phase 10.4). The template context has the slots, and no `area` and no `entity`. The pools of the speaker intents do not use them. A data value or a reply that does not render also gives `Ask`.
- An area name comes first. "Play jazz in the office" plays on the speaker of the Office, not on "Office Speakers".
- A query does not change. "What is playing downstairs" still asks for the area.
- Until sub-phase 18.15b, the music controls of sub-phase 18.8 also call their service on the group player.
- The choices of the area question do not change (sub-phase 10.4). Thus the answer "downstairs" to "Which room?" does not fill the area (sub-phase 10.5).
- The group player is a Music Assistant entity, so `resolve` (sub-phases 18.13b and 18.14) finds its config entry and searches as before.
- `play_music` gets the examples "play [jazz](media) [downstairs](area)", "play [radiohead](media) on the [downstairs speakers](area)" and "play [hip hop](media) on the [office speakers](area)". None of them is held out.
- The reference house (`src/testing/house.json`) gets the three group players of the real house, with no area and no device. `media_player.downstairs` has the members `media_player.carson_kitchen` and `media_player.carson_living_room`. `media_player.office_speakers` has the members `media_player.office` and `media_player.carson_office`. `media_player.office_speakers_2` has no members. The player `media_player.downstairs_speakers` ("Downstairs Speakers") of the reference house has no `mass_player_type`, so it is not a group.
- `src/domain/contracts.rs` gets a contract for `Value::as_array`.

**Acceptance criteria.**

- AC-18.15-1: When the intent is `play_music` with the slots `media = jazz` and `area = downstairs` from the Kitchen satellite, the reply is "Playing jazz." and the call is `music_assistant.play_media` with the target `media_player.downstairs` and the data `{ media_id: "jazz" }`.
- AC-18.15-2: When the area slot is "downstairs speakers", the target is `media_player.downstairs`.
- AC-18.15-3: When the intent is `play_radio` or `queue_music` with the area slot "downstairs", the target is `media_player.downstairs`.
- AC-18.15-4: When the intent is `play_music` with the area slot "upstairs", `decide` returns `Ask` from `ask_area`.
- AC-18.15-5: When the intent is `lights_on` with the area slot "downstairs", `decide` returns `Ask` from `ask_area`.
- AC-18.15-6: When the test gives `media_player.office_speakers_2` the member `media_player.carson_office`, the target of `play_music` with the area slot "office speakers" is `media_player.office_speakers`, the lower entity ID.
- AC-18.15-7: When the test also sets `group_members = []` on `media_player.office_speakers`, the target is `media_player.office_speakers_2`.
- AC-18.15-8: When the test removes the state of `media_player.downstairs`, `play_music` with the area slot "downstairs speakers" gives `Ask` from `ask_area`. `media_player.downstairs_speakers` has the name "Downstairs Speakers" and no `mass_player_type`.
- AC-18.15-9: When the fixture model gets "play jazz downstairs" and "play radiohead on the downstairs speakers", the slots contain `media = jazz` and `area = downstairs`, and `media = radiohead` and `area = downstairs speakers`.

**Proofs.**

- P-18.15-1 to P-18.15-5 (AC-18.15-1 to AC-18.15-5). Ensures: for a top command whose first area slot matches no area, `decide` gives `Ask` from `ask_area`, `Defer` with `Job::Llm`, or `Reply` with a call. The call has the domain and the service of the intent (P-4.5c-1), its target is the one entity that `group` gives for the intent and the slot value, and each data value that is not braced is a string (P-4.5d-1). A `Reply` call whose target is not the selection in the area of the request (P-4.5c-2) has such a slot. Argument: step 3 calls `group` only when `area_by_name` of the slot gives `None` (P-2.8b-2). It gives `ask` when `group` gives `None` or a template does not render. "upstairs" matches no group, and a group player is not a candidate of `lights_on` (P-18.15-6).
- P-18.15-6 to P-18.15-8 (AC-18.15-6 to AC-18.15-8). Ensures: `group(intent, text, cache)` is `Some(id)` if and only if `id` is the entity ID of a candidate of `intent` that is a group of `text`, and no candidate that is a group of `text` has a lower entity ID. It is `None` if and only if no candidate of `intent` is a group of `text`. Argument: `group` makes one pass over the entities of `cache`. The loop invariant states that the best entity so far is a candidate group of `text` with the lowest entity ID of the entities before the index, and that no entity before the index is a candidate group when there is no best entity. `is_candidate` gives `candidate` (P-4.4b-2), and `normalize` gives `normalized` (P-3.1-2).
- P-18.15-9. Evidence: test.

---

### 18.15b Speaker Group Controls

**Goal:** Act with the music controls on the speakers of a group that play. "Pause the music downstairs" then pauses the group when the group plays, or only the speaker of the group that plays.

- **Implements:** G3, G9. **Depends on:** 18.15.
- **Files:** modify `intents.toml`, `src/domain/select.rs`, `src/domain/decide.rs`, `src/adapters/onnx.rs`.

**Model.**

- The music controls are the intents of sub-phase 18.8: `music_next`, `music_pause`, `volume_up`, `volume_down` and `volume_set`. In an area, they act on the speaker with the lowest entity ID (sub-phase 18.8). That rule does not change.
- A speaker of a group can play alone, for example after "play jazz in the kitchen". Then "pause the music downstairs" must pause that speaker, and the group player is not `playing`.
- `played(group, cache) -> Vec<EntityId>` in `src/domain/select.rs` is a pure function. It gives the target of a control on the group player `group`:
  1. `group`, when the state of `group` is `playing`.
  2. Else each member of `group` whose state is `playing`, in the order of `group_members`, when one or more members play. A member is a JSON string of `group_members`. A member with no state does not play.
  3. Else `group`.
- In step 3 of `decide` (sub-phase 18.15), a command whose `service` is in the domain `music_assistant` targets the group player. Another command targets `played(group, cache)`. Thus `play_music`, `play_radio` and `queue_music` always play on the whole group.
- `music_pause` gets the examples "stop the music [downstairs](area)" and "pause the music on the [downstairs speakers](area)". `volume_up` gets the example "turn up the music on the [downstairs speakers](area)". None of them is held out.

**Acceptance criteria.**

- AC-18.15b-1: When the intent is `music_pause` with the area slot "downstairs" and `media_player.downstairs` is `playing`, the call is `media_player.media_pause` with the target `media_player.downstairs`.
- AC-18.15b-2: When `media_player.downstairs` is `idle`, `media_player.carson_kitchen` is `playing` and `media_player.carson_living_room` is `idle`, the target is `media_player.carson_kitchen`.
- AC-18.15b-3: When `media_player.downstairs` is `idle` and both of its members are `playing`, the target is `media_player.carson_kitchen` and `media_player.carson_living_room`, in this order.
- AC-18.15b-4: When no member of `media_player.downstairs` and not the group player is `playing`, the target is `media_player.downstairs`.
- AC-18.15b-5: When the intent is `volume_set` with the slots `volume = 50` and `area = downstairs`, and only `media_player.carson_kitchen` is `playing`, the call is `media_player.volume_set` with the target `media_player.carson_kitchen` and `volume_level = 0.5`.
- AC-18.15b-6: When the intent is `play_music` with the area slot "downstairs" and only `media_player.carson_kitchen` is `playing`, the target is `media_player.downstairs`.
- AC-18.15b-7: When the fixture model gets "stop the music downstairs" and "pause the music on the downstairs speakers", the slots contain `area = downstairs` and `area = downstairs speakers`.

**Proofs.**

- P-18.15b-1 to P-18.15b-4 (AC-18.15b-1 to AC-18.15b-4). Ensures: `played(group, cache)` is `[group]` when the state of `group` is `playing`. Else it is the members of `group` in the order of `group_members` whose state is `playing`, when this sequence is not empty. Else it is `[group]`. Argument: `played` checks the state of `group` first. Then it makes one pass over `group_members`, with the loop invariant that the result is the playing members before the index. It gives `[group]` when the result is empty.
- P-18.15b-5, P-18.15b-6 (AC-18.15b-5, AC-18.15b-6). Ensures: the call of P-18.15-1 targets the group player when the `service` of the intent is in the domain `music_assistant`, else `played(group, cache)`. Argument: step 3 checks the domain of the `service` before it selects the target.
- P-18.15b-7. Evidence: test.

---

### 18.16 Opposite Commands

**Goal:** Add the local opposites of the existing commands: lights off, fan off, heat up, resume, previous track and TV off. Without them, the System One model gives the opposite intent, and "turn off the lights" turns the lights on.

- **Implements:** G3, G9. **Depends on:** 18.14.
- **Files:** modify `intents.toml`, `responses.toml`, `src/domain/catalog.rs`, `src/domain/decide.rs`, `src/adapters/onnx.rs`.

**Model.**

- The catalog has no opposite of `lights_on`, `fan_on`, `climate_down`, `music_pause`, `music_next` and `tv_show`. Thus the model gives the nearest label. On 2026-11-10, the fixture model of `main` gave these top labels with the context `area: none | habits: none`:

  | Phrase | Top label | Probability |
  |---|---|---|
  | "turn off the lights" | `lights_on` | 0.924 |
  | "turn the lights off" | `lights_on` | 0.910 |
  | "lights off" | `lights_on` | 0.928 |
  | "turn off the fan" | `fan_on` | 0.959 |
  | "turn the air up" | `climate_down` | 0.905 |
  | "make it warmer" | `climate_down` | 0.970 |
  | "turn the heat up" | `climate_down` | 0.882 |
  | "previous song" | `music_next` | 0.959 |
  | "unpause the music" | `volume_down` | 0.689 |
  | "resume the music" | `music_pause` | 0.540 |
  | "turn off the tv" | `volume_down` | 0.365 |

- A probability of `model.threshold` (0.6) or more makes Jarvis act on the wrong intent. A probability of `model.commit_threshold` (0.9) or more also commits early (sub-phase 6.5).
- This sub-phase adds six commands after `new_shows` at the end of `intents.toml`. Thus the contexts of the existing examples do not change (the preface of this phase). Each command has the target and the filters of its opposite:
  - `lights_off`: `homeassistant.turn_off`, the `target` and `select` of `lights_on`. Examples: "turn off the lights", "turn off the [kitchen](area) lights", "lights off", "turn the lights off", "switch the lights off in the [living room](area)", "can you turn the [office](area) lights off". Pool: `I am turning the lights off.`, `I will turn off the lights.`, `Turning off the {{ area }} lights.`
  - `fan_off`: `homeassistant.turn_off`, the `target` and `select` of `fan_on`. Examples: "turn off the fan", "turn off the [office](area) fan", "fan off please", "turn the fan off", "stop the fan". Pool: `I am turning the fan off.`, `Stopping the {{ area }} fan.`
  - `climate_up`: `climate.set_temperature`, the `target` and `select` of `climate_down`, `data = { temperature = "{{ entity.attributes.temperature + 2 }}" }`. Examples: "turn the air up", "turn the heat up", "make it warmer", "raise the temperature". Pool: `I am turning the heat up.`, `Making it warmer.`
  - `music_resume`: `media_player.media_play`, the speaker filter of sub-phase 18.7. Examples: "unpause the music", "resume the music", "resume the music in the [kitchen](area)", "start the music again". Pool: `Resuming the music.`
  - `music_previous`: `media_player.media_previous_track`, the speaker filter. Examples: "previous song", "go back a song", "play the previous track", "go back to the previous song in the [kitchen](area)". Pool: `Previous song.`
  - `tv_off`: `media_player.turn_off`, the TV filter of sub-phase 18.9. Examples: "turn off the tv", "turn the tv off", "switch off the tv", "tv off". Pool: `Turning off the TV.` `tv_stop` already calls `media_player.media_stop`, which stops the media and keeps the TV on. "Turn off the TV" asks for the power state, so `tv_off` calls `turn_off`. `tv_on` of sub-phase 11.6 is its opposite.
- The new commands use the existing service shapes, so `decide` and `validate_catalog` do not change.
- Of the examples, "turn off the fan", "resume the music" and "switch the lights off in the living room" are held out (sub-phase 3.3). They measure the accuracy of a training run (sub-phase 13.2). The fixture model does not train on them, so AC-18.16-7 does not check them (sub-phase 3.9). "turn the fan off" and "unpause the music" are the trained phrases of these intents.
- Each "on" command keeps its examples. AC-3.12-1 checks that each trained example of each intent still has its intent as the top label. AC-9.2-5 checks that the light commands keep an early commit.

**Acceptance criteria.**

- AC-18.16-1: When the intent is `lights_off` and the satellite is in the Kitchen, the call is `homeassistant.turn_off` with all kitchen lights, the target of AC-4.5c-1.
- AC-18.16-2: When the intent is `fan_off` and the satellite is in the Office, the call is `homeassistant.turn_off` with the target `fan.office_fan`. In the Living Room, the target is `switch.living_room_fan`.
- AC-18.16-3: When the intent is `climate_up` and the target temperature is 72, the call is `climate.set_temperature` with `temperature = 74` as a number.
- AC-18.16-4: When the intent is `music_resume` and the satellite is in the Kitchen, the call is `media_player.media_play` with the target `media_player.carson_kitchen`.
- AC-18.16-5: When the intent is `music_previous` and the satellite is in the Kitchen, the call is `media_player.media_previous_track` with the target `media_player.carson_kitchen`.
- AC-18.16-6: When the intent is `tv_off`, the call is `media_player.turn_off` with the target `media_player.apple_tv`.
- AC-18.16-7: When the fixture model gets each phrase of this table with the context `area: none | habits: none`, the top label is the new intent, with a probability of `model.commit_threshold` or more:

  | Phrase | New intent | Old wrong intent |
  |---|---|---|
  | "turn off the lights", "turn the lights off", "lights off" | `lights_off` | `lights_on` |
  | "turn the fan off" | `fan_off` | `fan_on` |
  | "turn the air up", "make it warmer", "turn the heat up" | `climate_up` | `climate_down` |
  | "previous song" | `music_previous` | `music_next` |
  | "unpause the music" | `music_resume` | `volume_down` |
  | "turn off the tv" | `tv_off` | `volume_down` |

- AC-18.16-8: When the fixture model gets each phrase of the table of AC-18.16-7, the top label is not the old wrong intent of the phrase.

**Proofs.**

- P-18.16-1 to P-18.16-6. Evidence: test. The rules are P-4.5c-1, P-4.4c-4 and P-4.5d-1.
- P-18.16-7, P-18.16-8. Evidence: test. The probabilities come from the model file (`02-rules.md` section 5, rule 5).

## Field Log

These steps are not tests and gate nothing.

1. In HA, find the media players of Music Assistant. Put each other speaker in the `exclude` list of the music intents.
2. Ask "Which lights are on?" on a satellite. Compare the answer with HA.
3. Say "Play jazz in the kitchen." Make sure that the kitchen speaker plays.
4. Say "Put on The Matrix." Make sure that the TV plays the movie. If the Plex libraries have other names, change them in `intents.toml`.
5. Set `PLEX_TOKEN` in the environment file of the `jarvis` unit. Ask "Are there any new shows?". Compare the answer with the Plex home screen.
6. Say "Play hip hop", "Play jazz" and "Play Radiohead" in the Office. Make sure that the speaker plays the playlist "Hip Hop", the playlist "jazzy" and the artist "Radiohead", and that Jarvis announces no failure.
7. Say "Play jazz downstairs." Make sure that all speakers of "Downstairs Speakers" play. Say "Pause the music downstairs." Make sure that they pause.
