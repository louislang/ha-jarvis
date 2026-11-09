# Phase 18: Queries and Media

This phase adds System One query intents about the house, Music Assistant playback and Plex playback. The queries read only the hot cache and make no service call. The music intents call `music_assistant.play_media` and the `media_player` services on the speaker of the area. The Plex intents call `media_player.play_media` with a `plex://` content ID on the TV. The list of new Plex shows comes from the Plex server, so it is a plugin (`01-architecture.md` decision 30). The work order follows the phase index (`00-overview.md` section 2), so this phase comes after phase 17 and before phase 10.

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
- `play_music`: examples "play [radiohead](media)", "play [jazz](media) in the [kitchen](area)", "play the album [abbey road](media)", "play my [workout](media) playlist", "play the song [yesterday](media)". `data = { media_id = "{{ media }}" }`. Pool: `Playing {{ media }}.`
- `play_radio`: examples "play music like [radiohead](media)", "play something like [miles davis](media)", "start a radio station for [taylor swift](media)". `data = { media_id = "{{ media }}", radio_mode = "{{ true }}" }`. Pool: `Playing music like {{ media }}.`
- `queue_music`: examples "add [yesterday](media) to the queue", "queue [abbey road](media)", "put [hey jude](media) in the queue". `data = { media_id = "{{ media }}", enqueue = "add" }`. Pool: `Adding {{ media }} to the queue.`

**Acceptance criteria.**

- AC-18.7-1: When the intent is `play_music` with the slot `media = radiohead` and the satellite is in the Kitchen, the call is `music_assistant.play_media` with the target `media_player.carson_kitchen` and the data `{ media_id: "radiohead" }`. `media_player.carson_kitchen` is the speaker of the Kitchen satellite device, and its entity ID is lower than `media_player.carson_kitchen_media_player`.
- AC-18.7-2: When the request is from the Office satellite and has the area slot "kitchen", the target is `media_player.carson_kitchen`.
- AC-18.7-3: When the intent is `play_radio`, the data has `radio_mode = true` as a boolean.
- AC-18.7-4: When the intent is `queue_music`, the data has `enqueue = "add"`.
- AC-18.7-5: When the satellite is in the Bedroom, whose only speaker is on the satellite device, the target is `media_player.carson_bedroom`.

**Proofs.**

- P-18.7-1 to P-18.7-5. Evidence: test. The rules are P-4.5c-1, P-4.5c-2, P-4.4b-6, P-4.4c-4 and P-4.5d-1.

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

## Field Log

These steps are not tests and gate nothing.

1. In HA, find the media players of Music Assistant. Put each other speaker in the `exclude` list of the music intents.
2. Ask "Which lights are on?" on a satellite. Compare the answer with HA.
3. Say "Play jazz in the kitchen." Make sure that the kitchen speaker plays.
4. Say "Put on The Matrix." Make sure that the TV plays the movie. If the Plex libraries have other names, change them in `intents.toml`.
5. Set `PLEX_TOKEN` in the environment file of the `jarvis` unit. Ask "Are there any new shows?". Compare the answer with the Plex home screen.
