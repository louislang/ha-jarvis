//! The pipeline harness: the real Wyoming server, use cases, sherpa-onnx adapters and fixture
//! model on `FakeHome` and `FakeReasoner`, driven in the event order of HA.

use std::future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_util::task::TaskTracker;

use crate::Config;
use crate::Device;
use crate::adapters::wyoming;
use crate::domain::state::ServiceCall;
use crate::testing::fakes;
use crate::testing::fakes::FakeHome;
use crate::testing::fakes::FakeReasoner;
use crate::testing::fakes::FakeWyomingClient;

/// The samples of one 20 ms chunk of 16 kHz audio.
const CHUNK_SAMPLES: usize = 320;

/// The device of the kitchen satellite of the reference house.
const KITCHEN_DEVICE: &str = "98f4aacf9c4152ffe32dff212df91521";

/// The kitchen satellite of the reference house.
const KITCHEN_SATELLITE: &str = "assist_satellite.carson_kitchen";

/// The times of the events of one run.
pub(crate) struct Times {
    /// The send of the chunk with the last voiced sample.
    pub(crate) last_voiced: Instant,
    /// The `transcript` event of the `asr` connection.
    pub(crate) transcript: Instant,
    /// The send of `transcript` on the `handle` connection.
    pub(crate) handle: Instant,
    /// The `handled` event.
    pub(crate) handled: Instant,
    /// The send of `synthesize` on the `tts` connection.
    pub(crate) synthesize: Instant,
    /// The first `audio-chunk` event.
    pub(crate) first_chunk: Instant,
}

/// The answers of one run.
pub(crate) struct Run {
    /// The text of `handled`.
    pub(crate) reply: String,
    /// The number of `audio-chunk` events for the reply.
    pub(crate) chunks: usize,
    /// The times of the events.
    pub(crate) times: Times,
}

/// A Wyoming server of the app of a CPU machine on `FakeHome` and `FakeReasoner`.
pub(crate) struct Pipeline {
    /// The address of the server.
    address: SocketAddr,
    /// The house of the app.
    home: Arc<FakeHome>,
    /// The connection tasks and the work after their replies.
    tracker: TaskTracker,
    /// The folder of `pools.json`.
    _folder: TempDir,
}

impl Pipeline {
    /// Starts the app and its server on a local port.
    ///
    /// # Errors
    ///
    /// A file or a model cannot load, or the server cannot bind.
    pub(crate) async fn start() -> anyhow::Result<Self> {
        let folder = tempfile::tempdir()?;
        let config = cpu_config(folder.path())?;
        let catalog = crate::load_catalog(&config.intents)?;
        let pools = crate::load_pools(&config.responses, &catalog)?;
        let home = Arc::new(FakeHome::default());
        let reasoner = Arc::new(FakeReasoner::default());
        let started = crate::start_app(&config, catalog, pools, Arc::clone(&home) as _, reasoner);
        let app = Arc::new(started.await?.0);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let (address, tracker) = (listener.local_addr()?, TaskTracker::new());
        let tasks = tracker.clone();
        tokio::spawn(async move { wyoming::serve(listener, app, &tasks, future::pending()).await });
        Ok(Self {
            address,
            home,
            tracker,
            _folder: folder,
        })
    }

    /// Runs `text` as HA does: `speech(text)` to `asr`, its transcript from the kitchen satellite
    /// to `handle`, then the reply to `tts`, each on its own connection.
    ///
    /// # Errors
    ///
    /// A connection fails, or an answer is not the next event of the pipeline.
    pub(crate) async fn run(&self, text: &str) -> anyhow::Result<Run> {
        let (transcript, last_voiced, transcribed) = self.transcribe(text).await?;
        let (reply, handle, handled) = self.handle(&transcript).await?;
        let (chunks, synthesize, first_chunk) = self.synthesize(&reply).await?;
        let times = Times {
            last_voiced,
            transcript: transcribed,
            handle,
            handled,
            synthesize,
            first_chunk,
        };
        Ok(Run {
            reply,
            chunks,
            times,
        })
    }

    /// Sends `speech(text)` in 20 ms chunks with no pause, then `audio-stop`. Gives the text of
    /// the `transcript` event, the time after the send of the last voiced sample and the time of
    /// the event.
    ///
    /// # Errors
    ///
    /// The speech, the connection or the transcript fails.
    async fn transcribe(&self, text: &str) -> anyhow::Result<(String, Instant, Instant)> {
        let samples = fakes::speech(text).await?.samples;
        let (voiced, silence) = split_voiced(&samples);
        let mut client = FakeWyomingClient::connect(self.address).await?;
        client.send("transcribe", &json!({})).await?;
        client.send("audio-start", &audio_format()).await?;
        send_chunks(&mut client, voiced).await?;
        let last_voiced = Instant::now();
        send_chunks(&mut client, silence).await?;
        client.send("audio-stop", &json!({})).await?;
        let transcript = next(&mut client, "transcript").await?;
        Ok((text_of(&transcript)?, last_voiced, Instant::now()))
    }

    /// Sends `transcript` with `text` from the kitchen satellite. Gives the text of `handled`,
    /// the time of the send and the time of the answer.
    ///
    /// # Errors
    ///
    /// The connection fails, or the answer is not `handled`.
    async fn handle(&self, text: &str) -> anyhow::Result<(String, Instant, Instant)> {
        let mut client = FakeWyomingClient::connect(self.address).await?;
        let context = json!({ "device_id": KITCHEN_DEVICE, "satellite_id": KITCHEN_SATELLITE });
        let sent = Instant::now();
        let data = json!({ "text": text, "context": context });
        client.send("transcript", &data).await?;
        let handled = next(&mut client, "handled").await?;
        Ok((text_of(&handled)?, sent, Instant::now()))
    }

    /// Sends `synthesize` with `text`. Gives the number of `audio-chunk` events, the time of the
    /// send and the time of the first chunk.
    ///
    /// # Errors
    ///
    /// The connection fails, or the answer is not audio.
    async fn synthesize(&self, text: &str) -> anyhow::Result<(usize, Instant, Instant)> {
        let mut client = FakeWyomingClient::connect(self.address).await?;
        let sent = Instant::now();
        client.send("synthesize", &json!({ "text": text })).await?;
        next(&mut client, "audio-start").await?;
        next(&mut client, "audio-chunk").await?;
        let first = Instant::now();
        let mut chunks = 1;
        while client.receive().await?.context("the server closed")?.0 == "audio-chunk" {
            chunks += 1;
        }
        Ok((chunks, sent, first))
    }

    /// The service calls of `FakeHome` after the connections and the work after their replies
    /// end.
    pub(crate) async fn calls(&self) -> Vec<ServiceCall> {
        self.tracker.close();
        self.tracker.wait().await;
        self.home.calls()
    }
}

/// The configuration of a CPU machine (`01-architecture.md` section 9) with `pools.json` in
/// `folder`.
///
/// # Errors
///
/// `jarvis.toml` is not valid.
fn cpu_config(folder: &Path) -> anyhow::Result<Config> {
    let mut config = Config::parse(include_str!("../../jarvis.toml"))?;
    config.device = Device::Cpu;
    let asr = &mut config.asr;
    asr.encoder = "encoder.int8.onnx".to_owned();
    asr.decoder = "decoder.int8.onnx".to_owned();
    asr.joiner = "joiner.int8.onnx".to_owned();
    asr.second_pass_dir = None;
    asr.early_commit_ms = 250;
    config.pools = folder.join("pools.json");
    Ok(config)
}

/// `samples` before and after the end of the chunk with the last voiced sample, the last sample
/// that is not 0.
fn split_voiced(samples: &[i16]) -> (&[i16], &[i16]) {
    let last = samples.iter().rposition(|&sample| sample != 0);
    let end = (last.unwrap_or_default() / CHUNK_SAMPLES + 1) * CHUNK_SAMPLES;
    samples.split_at(end.min(samples.len()))
}

/// The `audio-start` and `audio-chunk` data: 16 kHz, 16-bit, mono.
fn audio_format() -> Value {
    json!({ "rate": 16_000, "width": 2, "channels": 1 })
}

/// Sends `samples` on `client` as `audio-chunk` events of [`CHUNK_SAMPLES`] samples.
///
/// # Errors
///
/// A write fails.
async fn send_chunks(client: &mut FakeWyomingClient, samples: &[i16]) -> anyhow::Result<()> {
    for chunk in samples.chunks(CHUNK_SAMPLES) {
        let mut payload = Vec::with_capacity(chunk.len() * 2);
        for sample in chunk {
            payload.extend(sample.to_le_bytes());
        }
        let header = json!({
            "type": "audio-chunk",
            "data": audio_format(),
            "payload_length": payload.len(),
        });
        client
            .send_raw(&[format!("{header}\n").as_bytes(), &payload].concat())
            .await?;
    }
    Ok(())
}

/// The data of the next event on `client`.
///
/// # Errors
///
/// The read fails, the server closes the connection, or the event is not `expected`.
async fn next(client: &mut FakeWyomingClient, expected: &str) -> anyhow::Result<Value> {
    let (event, data) = client.receive().await?.context("the server closed")?;
    anyhow::ensure!(
        event == expected,
        "the event is {event} {data}, not {expected}"
    );
    Ok(data)
}

/// The `text` of the event data `data`.
///
/// # Errors
///
/// The data has no text.
fn text_of(data: &Value) -> anyhow::Result<String> {
    let text = data["text"].as_str().context("the event has no text")?;
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Map;

    use crate::domain::responses;

    /// The text of the runs.
    const REQUEST: &str = "turn on the lights";

    /// The `lights_on` responses of `responses.toml` in the kitchen.
    fn lights_on() -> anyhow::Result<Vec<String>> {
        let config = Config::parse(include_str!("../../jarvis.toml"))?;
        let catalog = crate::load_catalog(&config.intents)?;
        let pools = crate::load_pools(&config.responses, &catalog)?;
        let pool = pools.get("lights_on").context("no lights_on pool")?;
        let kitchen = Map::from_iter([("area".to_owned(), json!("kitchen"))]);
        let render = |template: &str| responses::render(template, &kitchen);
        let texts = pool.responses.iter().map(|response| render(&response.text));
        Ok(texts.collect::<Result<_, _>>()?)
    }

    /// AC-9.1-1.
    #[tokio::test]
    async fn ac_9_1_1_turn_on_the_lights_gives_audio_of_a_lights_on_response() -> anyhow::Result<()>
    {
        let run = Pipeline::start().await?.run(REQUEST).await?;
        let responses = lights_on()?;
        let reply = &run.reply;
        assert!(
            responses.contains(reply),
            "{reply:?} is not in {responses:?}"
        );
        assert!(run.chunks > 0, "no audio for {reply:?}");
        let times = &run.times;
        let order = [
            times.last_voiced,
            times.transcript,
            times.handle,
            times.handled,
            times.synthesize,
            times.first_chunk,
        ];
        assert!(order.is_sorted(), "the times are not in pipeline order");
        Ok(())
    }

    /// AC-9.1-2.
    #[tokio::test]
    async fn ac_9_1_2_turn_on_the_lights_calls_homeassistant_turn_on() -> anyhow::Result<()> {
        let pipeline = Pipeline::start().await?;
        pipeline.run(REQUEST).await?;
        let calls = pipeline.calls().await;
        let turn_on =
            |call: &ServiceCall| call.domain == "homeassistant" && call.service == "turn_on";
        assert!(calls.iter().any(turn_on), "calls: {calls:?}");
        Ok(())
    }
}
