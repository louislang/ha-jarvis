//! The pipeline harness: the real Wyoming server, use cases, STT and TTS adapters and fixture
//! model on `FakeHome` and `FakeReasoner`, driven in the event order of HA.

use std::convert::Infallible;
use std::future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::Interval;
use tokio_util::task::TaskTracker;

use crate::App;
use crate::AsrConfig;
use crate::Config;
use crate::Device;
use crate::Started;
use crate::adapters::files::FileStore;
use crate::adapters::sherpa;
use crate::adapters::wyoming;
use crate::app::background::Answer;
use crate::app::speech::Utterances;
use crate::domain::learn::Learned;
use crate::domain::state::Event;
use crate::domain::state::ServiceCall;
use crate::ports::Classifier;
use crate::ports::Home;
use crate::ports::Models;
use crate::ports::Reasoner;
use crate::ports::Store;
use crate::testing::fakes;
use crate::testing::fakes::FakeHome;
use crate::testing::fakes::FakePlugin;
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
    /// The send of the chunk with the VAD voice end.
    pub(crate) voice_end: Instant,
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
    /// The text of `transcript`.
    pub(crate) transcript: String,
    /// The text of `handled`.
    pub(crate) reply: String,
    /// The number of `audio-chunk` events for the reply.
    pub(crate) chunks: usize,
    /// The times of the events.
    pub(crate) times: Times,
}

/// A Wyoming server of the app of `device` on `FakeHome` and `FakeReasoner`.
pub(crate) struct Pipeline {
    /// The address of the server.
    address: SocketAddr,
    /// The house of the app.
    home: Arc<FakeHome>,
    /// The System Two of the app.
    pub(crate) reasoner: Arc<FakeReasoner>,
    /// The STT settings of the app, with its VAD.
    asr: AsrConfig,
    /// The time between two chunks: real time on CUDA, none on the CPU.
    pace: Option<Duration>,
    /// The connection tasks and the work after their replies.
    tracker: TaskTracker,
    /// The committed utterances of the app.
    utterances: Arc<Utterances>,
    /// The folder of `pools.json`.
    _folder: TempDir,
}

impl Pipeline {
    /// Starts the app of `device` and its server on a local port.
    ///
    /// # Errors
    ///
    /// A file or a model cannot load, or the server cannot bind.
    pub(crate) async fn start(device: Device) -> anyhow::Result<Self> {
        Self::start_with(device, None, None).await
    }

    /// [`Pipeline::start`] with the learned intents and the swapped-in classifier of `trained`,
    /// and with the pre-rendered `answer` of an intent stored as a prefetched answer. Fails as
    /// [`Pipeline::start`] does.
    pub(crate) async fn start_with(
        device: Device,
        trained: Option<(Learned, Arc<dyn Classifier>)>,
        answer: Option<(&str, Answer)>,
    ) -> anyhow::Result<Self> {
        let folder = tempfile::tempdir()?;
        let config = config(device, folder.path())?;
        let home = Arc::new(FakeHome::default());
        home.send(Event::Reconnected);
        let reasoner = Arc::new(FakeReasoner::default());
        let classifier = trained.map(|trained| learn(&config, trained)).transpose()?;
        let started =
            start_app(&config, Arc::clone(&home) as _, Arc::clone(&reasoner) as _).await?;
        let (address, tracker, utterances) = serve(&config, started, (classifier, answer)).await?;
        Ok(Self {
            address,
            home,
            reasoner,
            asr: config.asr,
            pace: (device == Device::Cuda).then_some(Duration::from_millis(20)),
            tracker,
            utterances,
            _folder: folder,
        })
    }

    /// Runs the 16 kHz `speech` as HA does: the audio to `asr`, its transcript from the kitchen
    /// satellite to `handle`, then the reply to `tts`, each on its own connection.
    ///
    /// # Errors
    ///
    /// A connection fails, or an answer is not the next event of the pipeline.
    pub(crate) async fn run(&self, speech: &[i16]) -> anyhow::Result<Run> {
        let (transcript, voice_end, transcribed) = self.transcribe(speech).await?;
        let (reply, handle, handled) = self.handle(&transcript).await?;
        let (chunks, synthesize, first_chunk) = self.synthesize(&reply).await?;
        let times = Times {
            voice_end,
            transcript: transcribed,
            handle,
            handled,
            synthesize,
            first_chunk,
        };
        Ok(Run {
            transcript,
            reply,
            chunks,
            times,
        })
    }

    /// Sends `speech`, then silence, in 20 ms chunks at the pace of the pipeline, until the
    /// `transcript` event. Gives its text, the time after the send of the chunk with the VAD
    /// voice end and the time of the event.
    ///
    /// # Errors
    ///
    /// The VAD fails or finds no voice, or the connection or the transcript fails.
    async fn transcribe(&self, speech: &[i16]) -> anyhow::Result<(String, Instant, Instant)> {
        let parts = voice_parts(&self.asr, speech)?;
        let (mut reader, mut writer) = start_asr(self.address).await?;
        let ticks = self.pace.map(tokio::time::interval);
        let mut voice_end = None;
        let transcript = tokio::select! {
            sent = stream_audio(&mut writer, parts, ticks, &mut voice_end) => {
                let Err(error) = sent;
                return Err(error);
            }
            transcript = next(&mut reader, "transcript") => transcript?,
        };
        let transcribed = Instant::now();
        let voice_end = voice_end.context("the transcript came before the end of the voice")?;
        Ok((text_of(&transcript)?, voice_end, transcribed))
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

/// The classifier that replaces the System One model, and an intent name with its prefetched
/// answer.
type Trained<'name> = (Option<Arc<dyn Classifier>>, Option<(&'name str, Answer)>);

/// Saves `learned` to `learn.file` of `config` and gives `classifier`. Fails when the save fails.
fn learn(
    config: &Config,
    (learned, classifier): (Learned, Arc<dyn Classifier>),
) -> anyhow::Result<Arc<dyn Classifier>> {
    FileStore::try_new(config.learn.file.clone())?.save(&learned)?;
    Ok(classifier)
}

/// The app of `config` on `home`, `reasoner` and a `get_weather` plugin with no result. Fails when
/// a file or a model cannot load.
async fn start_app(
    config: &Config,
    home: Arc<dyn Home>,
    reasoner: Arc<dyn Reasoner>,
) -> anyhow::Result<Started> {
    let catalog = crate::load_catalog(&config.intents)?;
    let pools = crate::load_pools(&config.responses, &catalog)?;
    let plugins = crate::registry(config, vec![Arc::new(FakePlugin::new("get_weather"))]);
    let catalog = Arc::new(catalog);
    let remotes = (None, None);
    crate::start_app(config, catalog, pools, home, reasoner, plugins, remotes).await
}

/// Waits for `started` as [`ready`] does with `trained`, and serves it on a local port. Gives the
/// address, the tracker of the connection tasks and the committed utterances.
///
/// # Errors
///
/// A model cannot load, or the server cannot bind.
async fn serve(
    config: &Config,
    started: Started,
    trained: Trained<'_>,
) -> anyhow::Result<(SocketAddr, TaskTracker, Arc<Utterances>)> {
    let app = Arc::new(ready(config, started, trained).await?);
    let utterances = Arc::clone(app.listener.utterances());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let (address, tracker) = (listener.local_addr()?, TaskTracker::new());
    let tasks = tracker.clone();
    tokio::spawn(async move { wyoming::serve(listener, app, &tasks, future::pending()).await });
    Ok((address, tracker, utterances))
}

/// The app of `started` after its pre-render. On the CUDA device of `config`, its models are
/// released and restored first, so the GPU latency tests run after the pause of a local training
/// run (AC-13.10f-1). Then the classifier of `trained`, when set, replaces the System One model,
/// and the answer of `trained`, when set, is pre-rendered and stored for its intent.
///
/// # Errors
///
/// The pre-render panicked, or a model cannot load.
async fn ready(
    config: &Config,
    started: Started,
    (classifier, answer): Trained<'_>,
) -> anyhow::Result<App> {
    let (app, _, prerender, models) = started;
    prerender.await?;
    if config.device == Device::Cuda {
        models.release().await;
        models.restore().await?;
    }
    if let Some(classifier) = classifier {
        models.classifier().swap("trained".to_owned(), classifier);
    }
    if let Some((name, answer)) = answer {
        app.speech.render(&answer.text).await?;
        let _ = app.conversation.answers().store(name, Some(answer));
    }
    Ok(app)
}

/// A connection to the server at `address` with a started `asr` session, as a reader and a
/// writer.
///
/// # Errors
///
/// The connection or a write fails.
async fn start_asr(
    address: SocketAddr,
) -> anyhow::Result<(
    FakeWyomingClient<impl AsyncRead + AsyncWrite + Unpin>,
    FakeWyomingClient<impl AsyncRead + AsyncWrite + Unpin>,
)> {
    let stream = TcpStream::connect(address).await?;
    stream.set_nodelay(true)?;
    let (read, write) = stream.into_split();
    let reader = FakeWyomingClient::new(io::join(read, io::sink()));
    let mut writer = FakeWyomingClient::new(io::join(io::empty(), write));
    writer.send("transcribe", &json!({})).await?;
    writer.send("audio-start", &audio_format()).await?;
    Ok((reader, writer))
}

/// Sends the two `parts` of the speech on `writer`, then silence, as HA streams the audio until
/// the transcript comes. Sends each chunk after the next tick of `ticks`, and sets `voice_end`
/// after the send of the first part. Ends only when a write fails.
///
/// # Errors
///
/// A write fails.
async fn stream_audio(
    writer: &mut FakeWyomingClient<impl AsyncRead + AsyncWrite + Unpin>,
    [voiced, rest]: [&[i16]; 2],
    mut ticks: Option<Interval>,
    voice_end: &mut Option<Instant>,
) -> anyhow::Result<Infallible> {
    send_chunks(writer, voiced, ticks.as_mut()).await?;
    *voice_end = Some(Instant::now());
    send_chunks(writer, rest, ticks.as_mut()).await?;
    loop {
        send_chunks(writer, &[0; CHUNK_SAMPLES], ticks.as_mut()).await?;
    }
}

/// The configuration of `device` (`01-architecture.md` section 9) with the data files in
/// `folder`: `jarvis.toml` on CUDA, the configuration of a CPU machine on the
/// CPU.
///
/// # Errors
///
/// `jarvis.toml` is not valid.
fn config(device: Device, folder: &Path) -> anyhow::Result<Config> {
    let mut config = Config::parse(include_str!("../../jarvis.toml"))?;
    config.pools = folder.join("pools.json");
    config.habits.file = folder.join("habits.json");
    config.log.database = folder.join("jarvis.db");
    config.train.state = folder.join("training.json");
    config.train.generated = folder.join("generated.json");
    config.learn.file = folder.join("learned.toml");
    if device == Device::Cuda {
        return Ok(config);
    }
    config.device = Device::Cpu;
    let asr = &mut config.asr;
    asr.encoder = "encoder.int8.onnx".to_owned();
    asr.decoder = "decoder.int8.onnx".to_owned();
    asr.joiner = "joiner.int8.onnx".to_owned();
    asr.second_pass_dir = None;
    asr.early_commit_ms = 250;
    Ok(config)
}

/// `speech` up to the end of the 20 ms chunk with the voice end that the VAD of `asr` gives, and
/// the rest of `speech`.
///
/// # Errors
///
/// The VAD cannot load or finds no voice.
fn voice_parts<'speech>(
    asr: &AsrConfig,
    speech: &'speech [i16],
) -> anyhow::Result<[&'speech [i16]; 2]> {
    let end = sherpa::voice_end(asr, speech)?.context("the VAD finds no voice")?;
    let end = usize::try_from(end)?.next_multiple_of(CHUNK_SAMPLES);
    let (voiced, rest) = speech.split_at(end.min(speech.len()));
    Ok([voiced, rest])
}

/// The `audio-start` and `audio-chunk` data: 16 kHz, 16-bit, mono.
fn audio_format() -> Value {
    json!({ "rate": 16_000, "width": 2, "channels": 1 })
}

/// Sends `samples` on `client` as `audio-chunk` events of [`CHUNK_SAMPLES`] samples, each after
/// the next tick of `ticks`.
///
/// # Errors
///
/// A write fails.
async fn send_chunks(
    client: &mut FakeWyomingClient<impl AsyncRead + AsyncWrite + Unpin>,
    samples: &[i16],
    mut ticks: Option<&mut Interval>,
) -> anyhow::Result<()> {
    for chunk in samples.chunks(CHUNK_SAMPLES) {
        if let Some(ticks) = ticks.as_mut() {
            ticks.tick().await;
        }
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
async fn next(
    client: &mut FakeWyomingClient<impl AsyncRead + AsyncWrite + Unpin>,
    expected: &str,
) -> anyhow::Result<Value> {
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

    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::sync::Mutex;

    use arc_swap::ArcSwap;
    use futures::channel::oneshot;
    use jiff::Timestamp;
    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;
    use serde_json::Map;
    use tokio::runtime::Runtime;
    use tokio::sync::watch;
    use tokio::task::JoinHandle;

    use crate::CurrentClassifier;
    use crate::app::background::Backfill;
    use crate::app::background::Habits;
    use crate::app::conversation::Conversation;
    use crate::app::conversation::Queues;
    use crate::app::conversation::Recorder;
    use crate::app::conversation::SystemTwo;
    use crate::app::speech::Listener;
    use crate::app::speech::Speech;
    use crate::app::training::FIXTURE;
    use crate::app::training::Training;
    use crate::domain::catalog::Catalog;
    use crate::domain::decide::PluginCall;
    use crate::domain::habits::RequestHabits;
    use crate::domain::learn::LearnedKind;
    use crate::domain::learn::LearnedStatus;
    use crate::domain::model::Prediction;
    use crate::domain::responses;
    use crate::domain::responses::Pools;
    use crate::domain::state::AreaId;
    use crate::domain::state_cache::StateCache;
    use crate::ports;
    use crate::ports::Handled;
    use crate::ports::Listening;
    use crate::ports::Work;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeModelLoader;
    use crate::testing::fakes::FakeStt;
    use crate::testing::fakes::FakeTrainer;
    use crate::testing::fakes::FakeTts;
    use crate::testing::fakes::FakeVoiceEmbedder;
    use crate::testing::fakes::Scripted;
    use crate::testing::fakes::SetClock;

    /// The text of the runs of sub-phase 9.1.
    const REQUEST: &str = "turn on the lights";

    /// Fan command phrases in the office whose top intent with the fixture model is at or above
    /// `model.commit_threshold`. Each `fan_on` response is pre-rendered.
    const FAN_COMMANDS: [&str; 2] = ["turn on the office fan", "start the office fan"];

    /// Light command phrases in the office whose top intent with the fixture model is at or
    /// above `model.commit_threshold`. Each `lights_on` response is pre-rendered.
    const LIGHT_COMMANDS: [&str; 3] = [
        "turn on the office lights",
        "office lights on",
        "turn the office lights on",
    ];

    /// The number of measured runs of each phrase.
    const RUNS: usize = 20;

    /// The responses of `pool` in `responses.toml`, rendered in `area` with the show `bluey` and
    /// an empty form of address.
    fn responses(pool: &str, area: &str) -> anyhow::Result<Vec<String>> {
        let config = Config::parse(include_str!("../../jarvis.toml"))?;
        let catalog = crate::load_catalog(&config.intents)?;
        let pools = crate::load_pools(&config.responses, &catalog)?;
        let pool = pools.get(pool).with_context(|| format!("no {pool} pool"))?;
        // The app has no `[address]`, so a response with `address` never replies.
        let variables = [
            ("area", json!(area)),
            ("show", json!("bluey")),
            ("address", json!("")),
        ];
        let variables = Map::from_iter(variables.map(|(name, value)| (name.to_owned(), value)));
        let render = |template: &str| responses::render(template, &variables);
        let texts = pool.responses.iter().map(|response| render(&response.text));
        Ok(texts.collect::<Result<_, _>>()?)
    }

    /// AC-9.1-1.
    #[tokio::test]
    async fn ac_9_1_1_turn_on_the_lights_gives_audio_of_a_lights_on_response() -> anyhow::Result<()>
    {
        let speech = fakes::speech(REQUEST).await?.samples;
        let run = Pipeline::start(Device::Cpu).await?.run(&speech).await?;
        let responses = responses("lights_on", "kitchen")?;
        let reply = &run.reply;
        assert!(
            responses.contains(reply),
            "{reply:?} is not in {responses:?}"
        );
        assert!(run.chunks > 0, "no audio for {reply:?}");
        let times = &run.times;
        let order = [
            times.voice_end,
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
        let pipeline = Pipeline::start(Device::Cpu).await?;
        pipeline.run(&fakes::speech(REQUEST).await?.samples).await?;
        let calls = pipeline.calls().await;
        let turn_on =
            |call: &ServiceCall| call.domain == "homeassistant" && call.service == "turn_on";
        assert!(calls.iter().any(turn_on), "calls: {calls:?}");
        Ok(())
    }

    /// The response latency segments of `times`: the voice end to `transcript`,
    /// `transcript` to `handled`, and `synthesize` to the first audio chunk.
    fn segments(times: &Times) -> [Duration; 3] {
        [
            times.transcript - times.voice_end,
            times.handled - times.handle,
            times.first_chunk - times.synthesize,
        ]
    }

    /// The p95 (the 19th smallest of 20) and the median (the 10th smallest) of `values`.
    fn percentiles(mut values: Vec<Duration>) -> (Duration, Duration) {
        values.sort();
        (values[RUNS - 2], values[RUNS / 2 - 1])
    }

    /// The p95 of the response latencies of `RUNS` runs of `text` on `pipeline` after one
    /// warm-up run, with the [`report`]. Each reply is in `replies`. System Two answers each run
    /// after 2 s.
    ///
    /// # Errors
    ///
    /// A run fails, or its reply is not in `replies`.
    async fn p95(pipeline: &Pipeline, text: &str, replies: &[String]) -> anyhow::Result<Duration> {
        let speech = fakes::speech(text).await?.samples;
        let mut runs = Vec::new();
        for _ in 0..=RUNS {
            let wait = Scripted::Wait(Duration::from_secs(2));
            pipeline.reasoner.script(vec![wait]);
            let run = pipeline.run(&speech).await?;
            let reply = &run.reply;
            anyhow::ensure!(replies.contains(reply), "{text:?} gives {reply:?}");
            runs.push(run);
        }
        report(text, &runs[1..])
    }

    /// The p95 of the response latencies of `runs` of `text`. Writes it to stderr with the p95
    /// and the median of each segment, and the texts of the last run.
    ///
    /// # Errors
    ///
    /// `runs` is empty, or the write fails.
    fn report(text: &str, runs: &[Run]) -> anyhow::Result<Duration> {
        let measured: Vec<[Duration; 3]> = runs.iter().map(|run| segments(&run.times)).collect();
        let (p95, _) = percentiles(measured.iter().map(|three| three.iter().sum()).collect());
        let [a, b, c] =
            [0, 1, 2].map(|part| percentiles(measured.iter().map(|three| three[part]).collect()));
        let Run {
            transcript, reply, ..
        } = runs.last().context("no run")?;
        // The test harness captures `eprintln!` of a test that passes, but not a direct write.
        let segments = format!("(p95, median) of a {a:?} b {b:?} c {c:?}");
        let texts = format!("transcript {transcript:?}; reply {reply:?}");
        writeln!(
            std::io::stderr(),
            "{text:?}: p95 {p95:?}; {segments}; {texts}"
        )?;
        Ok(p95)
    }

    /// AC-9.2-1.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_1_fan_command_response_latency_p95_is_300_ms_or_less() -> anyhow::Result<()> {
        within(&FAN_COMMANDS, "fan_on", "office", 300).await
    }

    /// AC-9.2-2.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_2_weather_filler_latency_p95_is_300_ms_or_less() -> anyhow::Result<()> {
        within(&["what is the weather"], "filler", "kitchen", 300).await
    }

    /// AC-9.2-3.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_3_unknown_filler_latency_p95_is_endpoint_ms_and_150_ms_or_less()
    -> anyhow::Result<()> {
        let config = Config::parse(include_str!("../../jarvis.toml"))?;
        let limit_ms = config.asr.endpoint_ms + 150;
        within(
            &["what is the meaning of life"],
            "filler",
            "kitchen",
            limit_ms,
        )
        .await
    }

    /// AC-9.2-5.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_5_light_command_response_latency_p95_is_400_ms_or_less() -> anyhow::Result<()> {
        within(&LIGHT_COMMANDS, "lights_on", "office", 400).await
    }

    /// AC-15.4-3.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_15_4_3_cuda_pipeline_embeds_each_committed_utterance() -> anyhow::Result<()> {
        use crate::ports::VoiceEmbedder as _;

        // The Phase 9.2 latency tests run on this pipeline, so they measure with embeddings on.
        let pipeline = Pipeline::start(Device::Cuda).await?;
        let speech = fakes::speech(FAN_COMMANDS[0]).await?.samples;
        let (transcript, _, _) = pipeline.transcribe(&speech).await?;
        let committed = pipeline.utterances.correlate(&transcript);
        let committed = committed.context("no committed utterance")?;
        let speaker = Config::parse(include_str!("../../jarvis.toml"))?.speaker;
        let model = sherpa::SherpaEmbedder::try_new(&speaker, Device::Cpu)?;
        let expected = model.embed(&committed.utterance.audio)?.len();
        assert_eq!(committed.embedding.await??.len(), expected);
        Ok(())
    }

    /// AC-9.3c-1. Only `tv_show` responses have a variable other than `area`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_3c_1_uncached_command_response_latency_p95_is_400_ms_or_less()
    -> anyhow::Result<()> {
        within(&["play bluey on the tv"], "tv_show", "kitchen", 400).await
    }

    /// Sends `REQUEST` from the kitchen satellite with no device, so it has no area and opens the
    /// dialog of "Which room?".
    ///
    /// # Errors
    ///
    /// The connection fails, or the reply is not a question.
    async fn ask_room(pipeline: &Pipeline) -> anyhow::Result<()> {
        let mut client = FakeWyomingClient::connect(pipeline.address).await?;
        let context = json!({ "satellite_id": KITCHEN_SATELLITE });
        let data = json!({ "text": REQUEST, "context": context });
        client.send("transcript", &data).await?;
        let reply = text_of(&next(&mut client, "handled").await?)?;
        anyhow::ensure!(
            reply.is_empty(),
            "{REQUEST:?} gives {reply:?}, not a question"
        );
        Ok(())
    }

    /// AC-10.7-4.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_10_7_4_room_answer_response_latency_p95_is_300_ms_or_less() -> anyhow::Result<()> {
        let (pipeline, answer) = (Pipeline::start(Device::Cuda).await?, "the kitchen");
        let (replies, speech) = (
            responses("lights_on", "kitchen")?,
            fakes::speech(answer).await?,
        );
        let mut runs = Vec::new();
        for _ in 0..=RUNS {
            ask_room(&pipeline).await?;
            let run = pipeline.run(&speech.samples).await?;
            let reply = &run.reply;
            anyhow::ensure!(replies.contains(reply), "{answer:?} gives {reply:?}");
            runs.push(run);
        }
        let (p95, limit) = (report(answer, &runs[1..])?, Duration::from_millis(300));
        assert!(p95 <= limit, "p95 {p95:?}, limit {limit:?}");
        Ok(())
    }

    /// Asserts that each of `texts` in `area` on CUDA, with a reply of `pool`, has a p95 latency
    /// of `limit_ms` or less.
    async fn within(texts: &[&str], pool: &str, area: &str, limit_ms: u64) -> anyhow::Result<()> {
        let pipeline = Pipeline::start(Device::Cuda).await?;
        let replies = responses(pool, area)?;
        let mut p95s = Vec::new();
        for &text in texts {
            p95s.push((text, p95(&pipeline, text, &replies).await?));
        }
        let limit = Duration::from_millis(limit_ms);
        assert!(
            p95s.iter().all(|(_, p95)| *p95 <= limit),
            "p95s: {p95s:?}, limit {limit:?}"
        );
        Ok(())
    }

    /// AC-14.7b-2. The classifier gives `tell_joke` to each prediction.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_14_7b_2_learned_intent_response_latency_p95_is_300_ms_or_less() -> anyhow::Result<()>
    {
        let (joke, classifier) = ("Why did the fan stop?", always("tell_joke"));
        let mut entry = fakes::learned("tell_joke", LearnedStatus::Active);
        (entry.service, entry.responses) = (None, vec![joke.to_owned()]);
        entry.activated = Some(Timestamp::UNIX_EPOCH);
        let learned = Learned {
            intents: vec![entry],
        };
        let pipeline =
            Pipeline::start_with(Device::Cuda, Some((learned, classifier)), None).await?;
        let p95 = p95(&pipeline, "tell me a joke", &[joke.to_owned()]).await?;
        assert!(p95 <= Duration::from_millis(300), "p95 {p95:?}");
        Ok(())
    }

    /// A classifier that gives `label` with 0.9 to each prediction.
    fn always(label: &str) -> Arc<FakeClassifier> {
        let classifier = Arc::new(FakeClassifier::default());
        for _ in 0..10_000 {
            classifier.script(predicted(label));
        }
        classifier
    }

    /// A prediction of `label` with 0.9 and no slots.
    fn predicted(label: &str) -> Prediction {
        let ranked = vec![(label.to_owned(), 0.9)];
        Prediction {
            ranked,
            slots: Vec::new(),
        }
    }

    /// AC-14.10d-4. The classifier gives `family_calendar` to each prediction, and its stored
    /// answer is fresh in the kitchen.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_14_10d_4_prefetch_answer_response_latency_p95_is_300_ms_or_less()
    -> anyhow::Result<()> {
        let (name, text) = ("family_calendar", "There are two events today.");
        let mut entry = fakes::learned(name, LearnedStatus::Active);
        (entry.kind, entry.service, entry.fresh_min) = (LearnedKind::Prefetch, None, Some(30));
        let learned = Learned {
            intents: vec![entry],
        };
        let answer = Answer {
            text: text.to_owned(),
            call: None,
            area: Some(AreaId("kitchen".to_owned())),
            time: Timestamp::now(),
        };
        let trained = Some((learned, always(name) as Arc<dyn Classifier>));
        let pipeline = Pipeline::start_with(Device::Cuda, trained, Some((name, answer))).await?;
        let p95 = p95(
            &pipeline,
            "what is on the calendar today",
            &[text.to_owned()],
        )
        .await?;
        let limit = Duration::from_millis(300);
        assert!(p95 <= limit, "p95 {p95:?}, limit {limit:?}");
        Ok(())
    }

    /// AC-14.10b-8. The fixture model gives `weather`, and its answer is fresh in the kitchen.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_14_10b_8_fetch_answer_response_latency_p95_is_300_ms_or_less() -> anyhow::Result<()>
    {
        let text = "Today will be sunny, with a high of 72 degrees.";
        let call = PluginCall {
            plugin: "get_weather".to_owned(),
            arguments: Map::from_iter([("when".to_owned(), json!("today"))]),
        };
        let answer = Answer {
            text: text.to_owned(),
            call: Some(call),
            area: Some(AreaId("kitchen".to_owned())),
            time: Timestamp::now(),
        };
        let pipeline = Pipeline::start_with(Device::Cuda, None, Some(("weather", answer))).await?;
        let p95 = p95(&pipeline, "what is the weather like", &[text.to_owned()]).await?;
        let limit = Duration::from_millis(300);
        assert!(p95 <= limit, "p95 {p95:?}, limit {limit:?}");
        Ok(())
    }

    /// The transcript of each request of `jarvis.qnt`.
    const TRANSCRIPT: &str = "conformance request";

    /// The state of `jarvis.qnt` that the driver compares.
    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    struct System {
        /// The phase of each request.
        phase: BTreeMap<usize, Tag>,
        /// The number of `Handled` messages of each request.
        handled: BTreeMap<usize, usize>,
        /// The model version of each request, 0 before `handle`.
        model_version: BTreeMap<usize, usize>,
        /// The model version that `handle` reads.
        active_model: usize,
        /// The time of the driver.
        now: i64,
        /// The time of the reply or the filler of each request, -1 before it.
        reply_time: BTreeMap<usize, i64>,
        /// The time of the first call of each request, -1 without one.
        call_time: BTreeMap<usize, i64>,
        /// The satellite of each request became idle after its reply or filler.
        idle_after_reply: BTreeMap<usize, bool>,
        /// The times of the announces of each request.
        announce_times: BTreeMap<usize, BTreeSet<i64>>,
        /// The satellite entity of each satellite is `idle` in the hot cache.
        idle: BTreeMap<String, bool>,
        /// The last started request of each satellite, 0 before any.
        started: BTreeMap<String, usize>,
        /// The last handled request of each satellite, 0 before any.
        newest: BTreeMap<String, usize>,
    }

    /// The time, the outputs and the satellites that the driver saw, as `System` has them, and
    /// the outputs that wait for a step.
    struct Seen {
        /// The time of the driver, which stamps each output.
        now: i64,
        /// The time of the reply or the filler of each request, -1 before it.
        reply_time: BTreeMap<usize, i64>,
        /// The time of the call of each request, -1 without one.
        call_time: BTreeMap<usize, i64>,
        /// The satellite of each request became idle after its reply or filler.
        idle_after_reply: BTreeMap<usize, bool>,
        /// The times of the announces of each request.
        announce_times: BTreeMap<usize, BTreeSet<i64>>,
        /// The last started request of each satellite, 0 before any.
        started: BTreeMap<String, usize>,
        /// The last handled request of each satellite, 0 before any.
        newest: BTreeMap<String, usize>,
        /// The number of `FakeHome` calls that have a stamp.
        recorded: usize,
        /// The work after the reply of each replied request, until it runs.
        afters: BTreeMap<usize, Work>,
        /// The sender of the next step and the task of the System Two answer of each deferred
        /// request.
        answers: BTreeMap<usize, Answering>,
    }

    impl Default for Seen {
        /// The state of `init`.
        fn default() -> Self {
            let satellites = || MODEL_SATELLITES.map(|s| (s.to_owned(), 0)).into();
            Self {
                now: 0,
                reply_time: per_request(|_| -1),
                call_time: per_request(|_| -1),
                idle_after_reply: per_request(|_| false),
                announce_times: per_request(|_| BTreeSet::new()),
                started: satellites(),
                newest: satellites(),
                recorded: 0,
                afters: BTreeMap::new(),
                answers: BTreeMap::new(),
            }
        }
    }

    /// The value that `value` gives for each request of `jarvis.qnt`.
    fn per_request<T>(value: impl Fn(usize) -> T) -> BTreeMap<usize, T> {
        (1..=3).map(|r| (r, value(r))).collect()
    }

    /// The variant of a Quint sum type value.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Tag {
        /// The variant name.
        tag: String,
    }

    impl ModelState<SystemDriver> for System {
        /// What the driver read from the app for each request, and the version of the classifier
        /// of the app.
        fn from_driver(driver: &SystemDriver) -> anyhow::Result<Self> {
            let phase = |r| *driver.phases.get(&r).unwrap_or(&"Listening");
            let read = |map: &BTreeMap<usize, usize>, r| map.get(&r).copied().unwrap_or(0);
            let tag = |r| Tag {
                tag: phase(r).to_owned(),
            };
            let seen = &driver.seen;
            Ok(Self {
                phase: per_request(tag),
                handled: per_request(|r| read(&driver.handled, r)),
                model_version: per_request(|r| read(&driver.models, r)),
                active_model: driver.active_model()?,
                now: seen.now,
                reply_time: seen.reply_time.clone(),
                call_time: seen.call_time.clone(),
                idle_after_reply: seen.idle_after_reply.clone(),
                announce_times: seen.announce_times.clone(),
                idle: driver.idle(),
                started: seen.started.clone(),
                newest: seen.newest.clone(),
            })
        }
    }

    /// Replays `jarvis.qnt` against the app with port fakes on two satellites. Each request says
    /// `TRANSCRIPT`, and each model version is a `FakeClassifier` that holds its predictions.
    struct SystemDriver {
        /// The paused runtime of the app, the `handle` tasks and the training runs.
        runtime: Runtime,
        /// The app and its pools.
        app: (Arc<App>, Arc<ArcSwap<Pools>>),
        /// The System One model of the app.
        current: Arc<CurrentClassifier>,
        /// The classifiers of the model versions 1 and 2.
        versions: [Arc<FakeClassifier>; 2],
        /// The training run that swaps the model.
        swap: Swap,
        /// The listen session of each request, from `audio` to `commit`.
        sessions: BTreeMap<usize, Box<dyn Listening>>,
        /// The result sender of the waiting prediction and the `handle` task of each request.
        waiting: BTreeMap<usize, Waiting>,
        /// The phase of each request that the app showed.
        phases: BTreeMap<usize, &'static str>,
        /// The number of ended `handle` calls of each request.
        handled: BTreeMap<usize, usize>,
        /// The model version whose classifier held the prediction of each request.
        models: BTreeMap<usize, usize>,
        /// The fakes and the hot cache that give the outputs and the satellites of the app.
        probes: Probes,
        /// The time, the outputs and the satellites that the driver saw.
        seen: Seen,
    }

    /// The fakes and the hot cache that give the outputs and the satellites of the app.
    struct Probes {
        /// HA of the app, which records its calls and announces and sends the state events.
        home: Arc<FakeHome>,
        /// System Two of the app, which holds each step of an answer.
        reasoner: Arc<FakeReasoner>,
        /// The hot cache of the app.
        cache: watch::Receiver<StateCache>,
        /// The wait of an announce after the satellite becomes idle.
        margin: Duration,
    }

    /// The sender of the next step of a System Two answer and the task of the answer.
    type Answering = (oneshot::Sender<Scripted>, JoinHandle<anyhow::Result<()>>);

    /// A training run, its loader, its trainer, its clock and the folder of its model folders.
    type Swap = (
        Training,
        Arc<FakeModelLoader>,
        Arc<FakeTrainer>,
        Arc<SetClock>,
        TempDir,
    );

    /// The result sender of a waiting prediction and its `handle` task.
    type Waiting = (
        oneshot::Sender<anyhow::Result<Prediction>>,
        JoinHandle<anyhow::Result<Handled>>,
    );

    impl SystemDriver {
        /// A driver at `init`: no request, and model version 1 active.
        fn new() -> Self {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            let runtime = runtime.enable_time().start_paused(true).build();
            let runtime = runtime.expect("the runtime builds");
            let versions = [always("lights_on"), always("lights_on")];
            for version in &versions {
                version.hold(TRANSCRIPT);
            }
            let first = Arc::clone(&versions[0]) as _;
            let current = Arc::new(CurrentClassifier::new(FIXTURE.to_owned(), first));
            let config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
            let app = runtime.block_on(system_app(&config, Arc::clone(&current)));
            let (app, pools, probes) = app.expect("the app starts");
            Self {
                runtime,
                app: (app, pools),
                swap: training_run(&current),
                current,
                versions,
                sessions: BTreeMap::new(),
                waiting: BTreeMap::new(),
                phases: BTreeMap::new(),
                handled: BTreeMap::new(),
                models: BTreeMap::new(),
                probes,
                seen: Seen::default(),
            }
        }

        /// `Assistant::listen` and one chunk of silence for request `r`.
        fn audio(&mut self, r: usize) -> anyhow::Result<()> {
            use crate::ports::Assistant as _;

            let app = Arc::clone(&self.app.0);
            let session = self.runtime.block_on(async move {
                let mut session = app.listen()?;
                session.push(vec![0; CHUNK_SAMPLES]).await?;
                anyhow::Ok(session)
            })?;
            self.sessions.insert(r, session);
            self.set_satellite(satellite(r), "listening")?;
            self.seen
                .started
                .insert(MODEL_SATELLITES[satellite(r)].to_owned(), r);
            Ok(())
        }

        /// `Listening::finish` of request `r`.
        fn commit(&mut self, r: usize) -> anyhow::Result<()> {
            let session = self.sessions.remove(&r).context("no listen session")?;
            let transcript = self.runtime.block_on(session.finish())?;
            anyhow::ensure!(transcript == TRANSCRIPT, "the transcript is {transcript:?}");
            self.phases.insert(r, "Committed");
            Ok(())
        }

        /// `Assistant::handle` of request `r` on its satellite until its prediction waits in the
        /// classifier of a model version.
        fn handle(&mut self, r: usize) -> anyhow::Result<()> {
            use crate::ports::Assistant as _;

            let (app, context) = (Arc::clone(&self.app.0), satellite_context(r));
            let task = self
                .runtime
                .spawn(async move { app.handle(TRANSCRIPT, context).await });
            let mut held = || {
                let mut versions = self.versions.iter().zip(1..);
                versions.find_map(|(version, v)| Some((version.take_held()?, v)))
            };
            let found = settle(&self.runtime, &task, &mut held);
            let (sender, v) = found.context("the prediction does not wait")?;
            self.waiting.insert(r, (sender, task));
            self.models.insert(r, v);
            self.phases.insert(r, "Handling");
            self.seen
                .newest
                .insert(MODEL_SATELLITES[satellite(r)].to_owned(), r);
            Ok(())
        }

        /// Gives `result` to the waiting prediction of request `r`, and reads the end of its
        /// `handle`.
        fn finish(&mut self, r: usize, result: anyhow::Result<Prediction>) -> anyhow::Result<()> {
            let (sender, task) = self.waiting.remove(&r).context("no waiting prediction")?;
            let sent = sender.send(result);
            sent.map_err(|_unsent| anyhow::anyhow!("the prediction does not wait"))?;
            let ended = self.runtime.block_on(task)?;
            let phase = self.phase(&ended);
            self.phases.insert(r, phase);
            *self.handled.entry(r).or_default() += 1;
            if phase != "Done" {
                self.seen.reply_time.insert(r, self.seen.now);
            }
            self.keep(r, phase, ended.ok().and_then(|handled| handled.after));
            Ok(())
        }

        /// Keeps the work `after` of the reply of request `r` in `phase` until `call`, or starts
        /// the System Two answer of a deferred request.
        fn keep(&mut self, r: usize, phase: &str, after: Option<Work>) {
            match (phase, after) {
                ("Replied", Some(after)) => _ = self.seen.afters.insert(r, after),
                ("Deferred", Some(after)) => self.wait_answer(r, self.runtime.spawn(after)),
                _ => {}
            }
        }

        /// The model phase after the end `ended` of `handle`: a filler defers, and an error ends
        /// the request.
        fn phase(&self, ended: &anyhow::Result<Handled>) -> &'static str {
            let pools = self.app.1.load();
            let filler = |text: &str| pools["filler"].responses.iter().any(|f| f.text == text);
            match ended {
                Ok(handled) if filler(&handled.text) => "Deferred",
                Ok(_) => "Replied",
                Err(_) => "Done",
            }
        }

        /// The model version whose classifier the app uses.
        fn active_model(&self) -> anyhow::Result<usize> {
            let active = Arc::clone(&self.current.0.load().classifier);
            let same = |version: &Arc<FakeClassifier>| {
                std::ptr::addr_eq(Arc::as_ptr(version), Arc::as_ptr(&active))
            };
            let index = self.versions.iter().position(same);
            Ok(index.context("an unknown classifier")? + 1)
        }

        /// Runs the work after the reply of replied request `r`, or sends one `Call` step to the
        /// System Two answer of deferred request `r`.
        fn call(&mut self, r: usize) -> anyhow::Result<()> {
            match self.seen.afters.remove(&r) {
                Some(after) => self.runtime.block_on(after)?,
                None => self.send(r, Scripted::Call(light_call()), Duration::ZERO)?,
            }
            self.stamp(Some(r))
        }

        /// Sends one sentence to the System Two answer of request `r`, with a clock advance of
        /// `announce.margin_ms`.
        fn announce(&mut self, r: usize) -> anyhow::Result<()> {
            self.send(r, Scripted::Sentence("the answer"), self.probes.margin)?;
            self.stamp(Some(r))
        }

        /// Sends `step` to the System Two answer of request `r`, advances the clock by `advance`
        /// and runs the app until the answer waits for its next step or ends.
        fn send(&mut self, r: usize, step: Scripted, advance: Duration) -> anyhow::Result<()> {
            let (sender, task) = self.seen.answers.remove(&r).context("no waiting answer")?;
            let sent = sender.send(step);
            sent.map_err(|_unsent| anyhow::anyhow!("the answer does not wait"))?;
            self.runtime
                .block_on(async { tokio::time::sleep(advance).await });
            self.wait_answer(r, task);
            Ok(())
        }

        /// Runs the app until the System Two answer `task` of request `r` waits for its next
        /// step, and keeps the sender of the step. Keeps nothing when the answer ends.
        fn wait_answer(&mut self, r: usize, task: JoinHandle<anyhow::Result<()>>) {
            let reasoner = Arc::clone(&self.probes.reasoner);
            if let Some(sender) = settle(&self.runtime, &task, || reasoner.take_step()) {
                self.seen.answers.insert(r, (sender, task));
            }
        }

        /// Sets the satellite entity of model satellite `s` in the hot cache to `value` through a
        /// state event of `FakeHome`.
        fn set_satellite(&mut self, s: usize, value: &str) -> anyhow::Result<()> {
            let entity = SATELLITES[s].0;
            let state = self.probes.cache.borrow().state(entity).cloned();
            let mut state = state.context("no satellite entity")?;
            state.state = value.to_owned();
            self.probes.home.send(Event::StateChanged {
                entity_id: entity.to_owned(),
                old: None,
                context: state.context.clone(),
                new: Some(Box::new(state)),
            });
            let shown =
                |cache: &StateCache| cache.state(entity).is_some_and(|new| new.state == value);
            let mut cache = self.probes.cache.clone();
            self.runtime
                .block_on(async { cache.wait_for(shown).await.map(drop) })?;
            Ok(())
        }

        /// Sets the satellite entity of `s` to `idle`, which ends the filler or the reply of each
        /// replied or deferred request of `s`.
        fn pipeline_end(&mut self, s: &str) -> anyhow::Result<()> {
            let index = usize::from(s == MODEL_SATELLITES[1]);
            self.set_satellite(index, "idle")?;
            let replied = |r: &usize| {
                satellite(*r) == index
                    && matches!(self.phases.get(r), Some(&("Replied" | "Deferred")))
            };
            let ended = (1..=3).filter(replied).map(|r| (r, true));
            self.seen.idle_after_reply.extend(ended);
            Ok(())
        }

        /// Stamps each new call and announce of `FakeHome` with the time of the driver as an
        /// output of request `r`. Fails when a step without a request made one.
        fn stamp(&mut self, r: Option<usize>) -> anyhow::Result<()> {
            let (calls, now) = (self.probes.home.calls(), self.seen.now);
            for call in calls.iter().skip(self.seen.recorded) {
                let r = r.with_context(|| format!("{call:?} in a step without a request"))?;
                if call.service == "announce" {
                    self.seen.announce_times.entry(r).or_default().insert(now);
                } else {
                    self.seen.call_time.insert(r, now);
                }
            }
            self.seen.recorded = calls.len();
            Ok(())
        }

        /// Each model satellite and whether its satellite entity is `idle` in the hot cache.
        fn idle(&self) -> BTreeMap<String, bool> {
            let cache = self.probes.cache.borrow();
            let idle = SATELLITES.map(|(id, _)| cache.state(id).is_some_and(|e| e.state == "idle"));
            let names = MODEL_SATELLITES.map(str::to_owned);
            names.into_iter().zip(idle).collect()
        }

        /// A training run whose loader gives the classifier of model version `v`.
        fn swap(&mut self, v: usize) -> anyhow::Result<()> {
            let (training, loader, trainer, clock, _) = &self.swap;
            loader.script(Arc::clone(&self.versions[v - 1]) as _);
            trainer.results.lock().expect("no panic").push_back(Ok(()));
            *clock.0.lock().expect("no panic") += Duration::from_secs(1);
            self.runtime.block_on(training.run())
        }
    }

    /// The satellite entity and its device for the model satellites `s1` and `s2`.
    const SATELLITES: [(&str, &str); 2] = [
        (KITCHEN_SATELLITE, KITCHEN_DEVICE),
        (
            "assist_satellite.carson_office",
            "1c8aff5b3943869c175c09b3c12f63d4",
        ),
    ];

    /// The satellites of `jarvis.qnt`, in the order of `SATELLITES`.
    const MODEL_SATELLITES: [&str; 2] = ["s1", "s2"];

    /// The index in `SATELLITES` of the satellite of request `r`: requests 1 and 2 on the model
    /// satellite `s1`, and request 3 on `s2`.
    fn satellite(r: usize) -> usize {
        usize::from(r > 2)
    }

    /// The context of request `r` from its satellite.
    fn satellite_context(r: usize) -> ports::Context {
        let (satellite, device) = SATELLITES[satellite(r)];
        ports::Context {
            device_id: Some(device.to_owned()),
            satellite_id: Some(satellite.to_owned()),
            ..ports::Context::default()
        }
    }

    impl Driver for SystemDriver {
        type State = System;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            self.apply(step)?;
            self.stamp(None)
        }
    }

    impl SystemDriver {
        /// Runs the model step `step` on the app.
        fn apply(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                audio(r: usize) => self.audio(r)?,
                commit(r: usize) => self.commit(r)?,
                handle(r: usize) => self.handle(r)?,
                reply(r: usize) => self.finish(r, Ok(predicted("lights_on")))?,
                defer(r: usize) => self.finish(r, Ok(predicted("unknown")))?,
                failRequest(r: usize) => self.finish(r, Err(anyhow::anyhow!("the model failed")))?,
                swap(v: usize) => self.swap(v)?,
                call(r: usize) => self.call(r)?,
                pipelineEnd(s: String) => self.pipeline_end(&s)?,
                announce(r: usize) => self.announce(r)?,
                tick => self.seen.now += 1,
            })
        }
    }

    /// Runs `runtime` until `take` gives a value or `task` ends.
    fn settle<T, U>(
        runtime: &Runtime,
        task: &JoinHandle<U>,
        mut take: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        runtime.block_on(async {
            loop {
                let found = take();
                if found.is_some() || task.is_finished() {
                    return found;
                }
                tokio::task::yield_now().await;
            }
        })
    }

    /// The call of a System Two answer: a light of the reference house on.
    fn light_call() -> ServiceCall {
        let call = json!({ "domain": "light", "service": "turn_on", "data": {},
            "target": ["light.office_fan_light"], "return_response": false });
        serde_json::from_value(call).expect("it is a call")
    }

    /// The app of `config` with the System One model `current`, the listener of
    /// [`system_listener`], `FakeTts` and the [`system_probes`], its pools and its probes.
    ///
    /// # Errors
    ///
    /// A file cannot load, or the sync ends before its first snapshot.
    async fn system_app(
        config: &Config,
        current: Arc<CurrentClassifier>,
    ) -> anyhow::Result<(Arc<App>, Arc<ArcSwap<Pools>>, Probes)> {
        let (probes, queues) = system_probes(config).await?;
        let reasoner: Arc<dyn Reasoner> = Arc::clone(&probes.reasoner) as _;
        let speech = Arc::new(Speech::new(Arc::new(FakeTts::default())));
        let listener = system_listener(config, &current, &reasoner, &queues);
        let recorder = recorder(Arc::clone(listener.utterances()));
        let cache = probes.cache.clone();
        let parts = (current, queues, cache, reasoner, Arc::clone(&speech));
        let (conversation, pools) = system_conversation(config, parts, recorder)?;
        let app = App {
            conversation,
            listener,
            speech,
            _prefetch: None,
        };
        Ok((Arc::new(app), pools, probes))
    }

    /// The probes of `config`: `FakeHome`, `FakeReasoner`, which holds each step of an answer, and
    /// the hot cache of `FakeHome` after its first snapshot. Also gives the queues of the cache.
    ///
    /// # Errors
    ///
    /// The sync ends before its first snapshot.
    async fn system_probes(config: &Config) -> anyhow::Result<(Probes, Arc<Queues>)> {
        let home = Arc::new(FakeHome::default());
        home.send(Event::Reconnected);
        let (mut cache, queues) = crate::start_sync(Arc::clone(&home) as _, config);
        cache.changed().await?;
        let reasoner = Arc::new(FakeReasoner::default());
        reasoner.hold_steps();
        let margin = Duration::from_millis(config.announce.margin_ms);
        let probes = Probes {
            home,
            reasoner,
            cache,
            margin,
        };
        Ok((probes, queues))
    }

    /// The System One model, the queues, the hot cache, System Two and the speech of an app.
    type Parts = (
        Arc<CurrentClassifier>,
        Arc<Queues>,
        watch::Receiver<StateCache>,
        Arc<dyn Reasoner>,
        Arc<Speech>,
    );

    /// The conversation of `config` on `parts` and `recorder`, with the System Two of
    /// [`fake_two`], and its pools.
    ///
    /// # Errors
    ///
    /// `intents.toml` or `responses.toml` cannot load.
    fn system_conversation(
        config: &Config,
        (current, queues, cache, reasoner, speech): Parts,
        recorder: Recorder,
    ) -> anyhow::Result<(Conversation, Arc<ArcSwap<Pools>>)> {
        let catalog = crate::load_catalog(&config.intents)?;
        let pools = crate::load_pools(&config.responses, &catalog)?;
        let pools = Arc::new(ArcSwap::from_pointee(pools));
        let catalog = Arc::new(ArcSwap::from_pointee(catalog));
        let backfill = Backfill::new(
            config.backfill.clone(),
            Arc::clone(&reasoner),
            std::sync::mpsc::channel().0,
            speech,
            Arc::clone(&catalog),
            cache.clone(),
        );
        let habits = Arc::new(Habits::new(RequestHabits::default(), 14.0, cache));
        let conversation = Conversation::new(
            Arc::clone(&pools),
            Arc::new(backfill),
            queues,
            current,
            catalog,
            config.model.threshold,
            fake_two(config, reasoner),
            habits,
            recorder,
        );
        Ok((conversation, pools))
    }

    /// The System Two of `config` on `reasoner` at the time of `FakeClock`, with a step timeout
    /// longer than a trace.
    fn fake_two(config: &Config, reasoner: Arc<dyn Reasoner>) -> SystemTwo {
        let mut two = crate::system_two(config, reasoner, crate::registry(config, Vec::new()));
        two.timeout = Duration::from_secs(3600);
        let clock = Arc::new(FakeClock(Timestamp::UNIX_EPOCH));
        SystemTwo { clock, ..two }
    }

    /// The listener of `config` on `FakeStt`, whose second pass gives `TRANSCRIPT`, and
    /// `FakeVoiceEmbedder`, with `current`, `reasoner` and `queues`.
    fn system_listener(
        config: &Config,
        current: &Arc<CurrentClassifier>,
        reasoner: &Arc<dyn Reasoner>,
        queues: &Arc<Queues>,
    ) -> Listener {
        let second_pass = Ok(Some(TRANSCRIPT.to_owned()));
        let stt = Arc::new(FakeStt {
            second_pass,
            ..FakeStt::default()
        });
        let classifier = Arc::clone(current) as _;
        let reasoner = Arc::clone(reasoner);
        crate::listener(
            config,
            stt,
            Arc::new(FakeVoiceEmbedder),
            classifier,
            reasoner,
            queues,
        )
    }

    /// A recorder of `utterances` with the training trigger of `FakeTrainer` and `FakeLog`, and a
    /// log writer that has ended.
    fn recorder(utterances: Arc<Utterances>) -> Recorder {
        Recorder {
            utterances,
            writer: tokio::sync::mpsc::unbounded_channel().0,
            last_id: None,
            trigger: fakes::trigger(Path::new("unused")).0,
            speakers: fakes::speakers(Arc::default()),
        }
    }

    /// A training run of `lights_on` with each example held out, on the classifier `current`.
    fn training_run(current: &Arc<CurrentClassifier>) -> Swap {
        let models = tempfile::tempdir().expect("a temporary folder");
        let (training, trainer) = fakes::training(1.0, models.path());
        let mut catalog = Catalog::clone(&training.catalog);
        catalog.intents.retain(|intent| intent.name == "lights_on");
        catalog.unknown_examples.clear();
        let loader = Arc::new(FakeModelLoader::default());
        let clock = Arc::new(SetClock(Mutex::new(Timestamp::UNIX_EPOCH)));
        let training = Training {
            loader: Arc::clone(&loader) as _,
            classifier: Arc::clone(current),
            clock: Arc::clone(&clock) as _,
            catalog: Arc::new(catalog),
            ..training
        };
        (training, loader, trainer, clock, models)
    }

    /// AC-16.2b-1. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/jarvis.qnt", max_steps = 30, seed = "1")]
    fn ac_16_2b_1_conformance() -> impl Driver {
        SystemDriver::new()
    }

    /// AC-16.2c-1. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/jarvis.qnt", max_steps = 30, seed = "1")]
    fn ac_16_2c_1_conformance() -> impl Driver {
        SystemDriver::new()
    }

    /// AC-16.2b-2. The steps of `ac_16_1_3_swap_during_request` of `jarvis.qnt`: a swap to
    /// version 2 before `handle`, and a swap to version 1 between `handle` and the reply.
    #[test]
    fn ac_16_2b_2_reply_uses_the_model_version_of_handle() -> anyhow::Result<()> {
        let mut driver = SystemDriver::new();
        driver.audio(1)?;
        driver.commit(1)?;
        driver.swap(2)?;
        driver.handle(1)?;
        driver.swap(1)?;
        driver.finish(1, Ok(predicted("lights_on")))?;
        let state = System::from_driver(&driver)?;
        assert_eq!((state.model_version[&1], state.active_model), (2, 1));
        assert_eq!(state.phase[&1].tag, "Replied");
        let first = driver.versions[0].calls();
        assert!(first.iter().all(|call| call.1 != TRANSCRIPT), "{first:?}");
        Ok(())
    }
}
