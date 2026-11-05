//! The pipeline harness: the real Wyoming server, use cases, sherpa-onnx adapters and fixture
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

use crate::Config;
use crate::Device;
use crate::adapters::wyoming;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;
use crate::ports::Home;
use crate::ports::Reasoner;
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
    /// The time between two chunks: real time on CUDA, none on the CPU.
    pace: Option<Duration>,
    /// The connection tasks and the work after their replies.
    tracker: TaskTracker,
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
        let folder = tempfile::tempdir()?;
        let config = config(device, folder.path())?;
        let home = Arc::new(FakeHome::new(house()?));
        let reasoner = Arc::new(FakeReasoner::default());
        let (house, two) = (Arc::clone(&home) as _, Arc::clone(&reasoner) as _);
        let (address, tracker) = serve(&config, house, two).await?;
        Ok(Self {
            address,
            home,
            reasoner,
            pace: (device == Device::Cuda).then_some(Duration::from_millis(20)),
            tracker,
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
        let (transcript, last_voiced, transcribed) = self.transcribe(speech).await?;
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
            transcript,
            reply,
            chunks,
            times,
        })
    }

    /// Sends `speech` up to its last voiced chunk, then silence, in 20 ms chunks at the pace of
    /// the pipeline, until the `transcript` event. Gives its text, the time after the send of the
    /// last voiced sample and the time of the event.
    ///
    /// # Errors
    ///
    /// The connection or the transcript fails.
    async fn transcribe(&self, speech: &[i16]) -> anyhow::Result<(String, Instant, Instant)> {
        let (mut reader, mut writer) = start_asr(self.address).await?;
        let ticks = self.pace.map(tokio::time::interval);
        let mut last_voiced = None;
        let transcript = tokio::select! {
            sent = stream_audio(&mut writer, speech, ticks, &mut last_voiced) => {
                let Err(error) = sent;
                return Err(error);
            }
            transcript = next(&mut reader, "transcript") => transcript?,
        };
        let transcribed = Instant::now();
        let last_voiced = last_voiced.context("the transcript came before the end of the voice")?;
        Ok((text_of(&transcript)?, last_voiced, transcribed))
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

/// Starts the app of `config` on `home` and `reasoner`, waits for its pre-render and serves it on
/// a local port. Gives the
/// address and the tracker of the connection tasks.
///
/// # Errors
///
/// A file or a model cannot load, or the server cannot bind.
async fn serve(
    config: &Config,
    home: Arc<dyn Home>,
    reasoner: Arc<dyn Reasoner>,
) -> anyhow::Result<(SocketAddr, TaskTracker)> {
    let catalog = crate::load_catalog(&config.intents)?;
    let pools = crate::load_pools(&config.responses, &catalog)?;
    let (app, _, prerender) = crate::start_app(config, catalog, pools, home, reasoner).await?;
    prerender.await?;
    let app = Arc::new(app);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let (address, tracker) = (listener.local_addr()?, TaskTracker::new());
    let tasks = tracker.clone();
    tokio::spawn(async move { wyoming::serve(listener, app, &tasks, future::pending()).await });
    Ok((address, tracker))
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

/// Sends `speech` up to its last voiced chunk on `writer`, then silence, as HA streams the audio
/// until the transcript comes. Sends each chunk after the next tick of `ticks`, and sets
/// `last_voiced` after the send of the last voiced chunk. Ends only when a write fails.
///
/// # Errors
///
/// A write fails.
async fn stream_audio(
    writer: &mut FakeWyomingClient<impl AsyncRead + AsyncWrite + Unpin>,
    speech: &[i16],
    mut ticks: Option<Interval>,
    last_voiced: &mut Option<Instant>,
) -> anyhow::Result<Infallible> {
    send_chunks(writer, voiced(speech), ticks.as_mut()).await?;
    *last_voiced = Some(Instant::now());
    loop {
        send_chunks(writer, &[0; CHUNK_SAMPLES], ticks.as_mut()).await?;
    }
}

/// The reference house with a `weather.home` entity for the `weather` intent.
///
/// # Errors
///
/// The house has no state.
fn house() -> anyhow::Result<Snapshot> {
    let mut house = fakes::house();
    let mut weather = house
        .states
        .first()
        .context("the house has no state")?
        .clone();
    "weather.home".clone_into(&mut weather.entity_id);
    house.states.push(weather);
    Ok(house)
}

/// The configuration of `device` (`01-architecture.md` section 9) with `pools.json` in `folder`:
/// `jarvis.toml` on CUDA, the configuration of a CPU machine on the CPU.
///
/// # Errors
///
/// `jarvis.toml` is not valid.
fn config(device: Device, folder: &Path) -> anyhow::Result<Config> {
    let mut config = Config::parse(include_str!("../../jarvis.toml"))?;
    config.pools = folder.join("pools.json");
    config.device = device;
    if device == Device::Cpu {
        let asr = &mut config.asr;
        asr.encoder = "encoder.int8.onnx".to_owned();
        asr.decoder = "decoder.int8.onnx".to_owned();
        asr.joiner = "joiner.int8.onnx".to_owned();
        asr.second_pass_dir = None;
        asr.early_commit_ms = 250;
    }
    Ok(config)
}

/// `samples` up to the end of the last voiced 20 ms frame.
fn voiced(samples: &[i16]) -> &[i16] {
    let last = samples.chunks(CHUNK_SAMPLES).rposition(voiced_frame);
    let end = last.map_or(0, |index| (index + 1) * CHUNK_SAMPLES);
    &samples[..end.min(samples.len())]
}

/// True when the RMS of `frame` is above -40 dBFS: its mean square is above (32768 / 100)².
/// The TTS audio has a long, quiet tail that a person does not hear as speech.
fn voiced_frame(frame: &[i16]) -> bool {
    let energy: u64 = frame
        .iter()
        .map(|sample| u64::from(sample.unsigned_abs()).pow(2))
        .sum();
    energy * 10_000 > (1 << 30) * frame.len() as u64
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

    use std::io::Write;

    use serde_json::Map;

    use crate::domain::responses;
    use crate::testing::fakes::Scripted;

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

    /// The responses of `pool` in `responses.toml`, rendered in `area` with the show `jeopardy`.
    fn responses(pool: &str, area: &str) -> anyhow::Result<Vec<String>> {
        let config = Config::parse(include_str!("../../jarvis.toml"))?;
        let catalog = crate::load_catalog(&config.intents)?;
        let pools = crate::load_pools(&config.responses, &catalog)?;
        let pool = pools.get(pool).with_context(|| format!("no {pool} pool"))?;
        let variables = [("area", json!(area)), ("show", json!("jeopardy"))];
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
        let pipeline = Pipeline::start(Device::Cpu).await?;
        pipeline.run(&fakes::speech(REQUEST).await?.samples).await?;
        let calls = pipeline.calls().await;
        let turn_on =
            |call: &ServiceCall| call.domain == "homeassistant" && call.service == "turn_on";
        assert!(calls.iter().any(turn_on), "calls: {calls:?}");
        Ok(())
    }

    /// The response latency segments of `times`: the last voiced sample to `transcript`,
    /// `transcript` to `handled`, and `synthesize` to the first audio chunk.
    fn segments(times: &Times) -> [Duration; 3] {
        [
            times.transcript - times.last_voiced,
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
        assert_commands_within(&FAN_COMMANDS, "fan_on", Duration::from_millis(300)).await
    }

    /// AC-9.2-5.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_5_light_command_response_latency_p95_is_400_ms_or_less() -> anyhow::Result<()> {
        assert_commands_within(&LIGHT_COMMANDS, "lights_on", Duration::from_millis(400)).await
    }

    /// Asserts that the response latency of each of `commands` in the office has a p95 of
    /// `limit` or less. Each reply is a response of `pool`.
    ///
    /// # Errors
    ///
    /// The pipeline does not start, or a run fails.
    async fn assert_commands_within(
        commands: &[&str],
        pool: &str,
        limit: Duration,
    ) -> anyhow::Result<()> {
        let pipeline = Pipeline::start(Device::Cuda).await?;
        let replies = responses(pool, "office")?;
        let mut p95s = Vec::new();
        for &text in commands {
            p95s.push((text, p95(&pipeline, text, &replies).await?));
        }
        assert!(p95s.iter().all(|(_, p95)| *p95 <= limit), "p95s: {p95s:?}");
        Ok(())
    }

    /// AC-9.2-2.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_2_weather_filler_latency_p95_is_300_ms_or_less() -> anyhow::Result<()> {
        let pipeline = Pipeline::start(Device::Cuda).await?;
        let fillers = responses("filler", "kitchen")?;
        let p95 = p95(&pipeline, "what is the weather", &fillers).await?;
        assert!(p95 <= Duration::from_millis(300), "p95 is {p95:?}");
        Ok(())
    }

    /// AC-9.2-3.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_3_unknown_filler_latency_p95_is_endpoint_ms_and_150_ms_or_less()
    -> anyhow::Result<()> {
        let pipeline = Pipeline::start(Device::Cuda).await?;
        let fillers = responses("filler", "kitchen")?;
        let p95 = p95(&pipeline, "what is the meaning of life", &fillers).await?;
        let endpoint_ms = Config::parse(include_str!("../../jarvis.toml"))?
            .asr
            .endpoint_ms;
        let limit = Duration::from_millis(endpoint_ms + 150);
        assert!(p95 <= limit, "p95 is {p95:?}, more than {limit:?}");
        Ok(())
    }

    /// AC-9.2-4. Only `tv_show` responses have a variable other than `area`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "GPU host"]
    async fn ac_9_2_4_uncached_command_response_latency_p95_is_700_ms_or_less() -> anyhow::Result<()>
    {
        let pipeline = Pipeline::start(Device::Cuda).await?;
        let replies = responses("tv_show", "kitchen")?;
        let p95 = p95(&pipeline, "turn the tv to jeopardy", &replies).await?;
        assert!(p95 <= Duration::from_millis(700), "p95 is {p95:?}");
        Ok(())
    }
}
