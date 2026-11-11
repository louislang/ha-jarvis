# Jarvis

Jarvis is a voice assistant for Home Assistant, written in Rust. It runs as a Wyoming server that gives HA speech-to-text, text-to-speech and a conversation agent in one process.

A small local model (System One) classifies each request and tags its slots. Common commands and questions run locally, with a target response latency of 300 ms (p95) on a GPU host. When System One is not sure, Jarvis sends the request to OpenAI (System Two), which can call HA services and plugins. Each satellite has an area, so "turn on the lights" controls the lights in the room you are in.

The design is in [`docs/spec/`](docs/spec/00-overview.md).

## Capabilities

"Local" requests run on System One. "OpenAI" requests go to System Two.

| Area | Example requests | How |
|---|---|---|
| Lights and fans | "Turn on the lights", "Turn off the kitchen lights", "Turn on the office fan", "Turn off the fan" | Local |
| Climate | "Turn the air down", "Make it warmer" | Local |
| Home status | "Which lights are on?", "How bright is the office light?", "Are any windows open?", "Is the front door locked?", "Is the garage door open?", "What is the thermostat set to?", "Is the fan on?", "Who is home?", "Is the alarm set?", "What is the temperature in the bedroom?" | Local |
| Weather | "What is the weather like?", "Will it rain today?" | Local, Open-Meteo plugin |
| Music (Music Assistant) | "Play hip hop", "Play jazz in the kitchen", "Play Radiohead", "Play music like Miles Davis", "Queue Abbey Road", "Next song", "Previous song", "Pause the music", "Resume the music", "Turn it up", "Set the volume to 50", "What is playing in the living room?" | Local. "Play" searches your library playlists first, then artists, albums and tracks. An artist starts with one of its tracks, and the rest of the artist follows in the queue. |
| Speaker groups (Music Assistant) | "Play jazz downstairs", "Play Radiohead on the downstairs speakers", "Pause the music downstairs", "Turn it up downstairs" | Local. A place that is not an HA area but matches a Music Assistant group name plays on the whole group. A control acts on the group when the group plays, else on the members that play. |
| TV and Plex | "Play Bluey on the TV", "Put on The Matrix", "Continue The Office", "Play season two episode five of The Office", "Pause the TV", "Skip ahead", "Stop the movie", "Turn off the TV", "Are there any new shows?" | Local, Plex plugin |
| Follow-up questions | "Turn on the lights" from a device with no area: "Which room?" "The kitchen." Say "never mind" to cancel. | Local |
| Everything else | Any other request, for example "Plan dinner for tonight" | OpenAI, with HA tools and plugins. OpenAI can ask you a question and use your answer. |
| Speech | Streaming speech-to-text, then a second pass at the end of speech. Text-to-speech with a cache of rendered responses. A short answer like "Kitchen" ends listening after 100 ms. | Local |
| Responses | Each intent has a pool of replies with variety. Jarvis adds new replies when a pool runs low. A slow request gets a filler reply, then an announcement of the answer. | Local, OpenAI for new replies |

Not built yet: habit learning, the review page and feedback, automatic training, learned intents, speaker identification and timers (phases 11 to 16 in [`docs/spec/00-overview.md`](docs/spec/00-overview.md)).

## Quickstart

You need:

- A Linux x86-64 host. An NVIDIA GPU is optional (see step 7).
- Home Assistant with a long-lived access token, and voice satellites.
- An OpenAI API key.
- Optional: Music Assistant for music, and a Plex server and token for TV.
- To build the System One model: Rust, [`just`](https://github.com/casey/just) and Python 3.12.

Steps:

1. Clone the repository at a release tag, for example `git clone --branch v0.1.15 https://github.com/louislang/ha-jarvis.git jarvis`.
2. Download `jarvis-<tag>-x86_64-unknown-linux-gnu.tar.gz` from the [releases](https://github.com/louislang/ha-jarvis/releases) and extract it in the repository folder. It contains the executable and its shared libraries.
3. Download the speech models with `just models`.
4. On a GPU host, run `just gpu-models`. It needs the fp16 speech models in `/opt/jarvis-models/fp16/`. No download has them: convert them once from the fp32 exports, as `docs/spec/phase-06.md` sub-phases 6.6c and 6.7 describe.
5. Build the System One model with `just install-python`, then `just fixture-model`.
6. Edit `jarvis.toml`: set `ha.url` and `plugins.plex.url`. Remove the `[plugins.plex]` section if you have no Plex.
7. On a host without a GPU, copy `jarvis.toml` to `jarvis.local.toml` and use that file. In it, set `device = "cpu"`, change `.fp16.` to `.int8.` in `asr.encoder`, `asr.decoder` and `asr.joiner`, and remove `second_pass_dir`.
8. Create `jarvis.env` with `HA_TOKEN=...`, `OPENAI_API_KEY=...` and, for Plex, `PLEX_TOKEN=...`. Add `LD_LIBRARY_PATH=<folder>` and `ORT_DYLIB_PATH=<folder>/libonnxruntime.so`, where `<folder>` is the repository folder.
9. Start Jarvis: `set -a; . ./jarvis.env; set +a; ./jarvis serve jarvis.toml` (or `jarvis.local.toml`). It listens on port 10700.
10. In Home Assistant, add the Wyoming Protocol integration with the host of Jarvis and port 10700.
11. Create an Assist pipeline with Jarvis as the conversation agent, speech-to-text and text-to-speech. Turn off "Prefer handling commands locally". Select this pipeline on your satellites.

To run Jarvis as a service, use a systemd unit with `WorkingDirectory` set to the repository folder, `EnvironmentFile` set to `jarvis.env` and `ExecStart=<folder>/jarvis serve jarvis.toml`.

## Development

`CLAUDE.md` has the working rules and `docs/spec/02-rules.md` has the required checks. Run `just setup` once, then `just check` runs the checks of the pre-commit hook.
