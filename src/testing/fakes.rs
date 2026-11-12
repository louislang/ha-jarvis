//! A fake for each port.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::stream::BoxStream;
use jiff::Timestamp;
use rubato::Fft;
use rubato::FixedSync;
use rubato::Resampler;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tempfile::NamedTempFile;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tracing::Dispatch;
use tracing::subscriber::NoSubscriber;

use crate::Config;
use crate::Device;
use crate::adapters::supertonic::SupertonicTts;
use crate::domain::decide::PluginCall;
use crate::domain::decide::Question;
use crate::domain::feedback;
use crate::domain::feedback::Deferred;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::feedback::Route;
use crate::domain::model::Prediction;
use crate::domain::speech::Audio;
use crate::domain::state::Event;
use crate::domain::state::HomeConfig;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;
use crate::ports::Assistant;
use crate::ports::Change;
use crate::ports::Classifier;
use crate::ports::Clock;
use crate::ports::Context;
use crate::ports::Filter;
use crate::ports::Handled;
use crate::ports::Home;
use crate::ports::Info;
use crate::ports::InteractionLog;
use crate::ports::Listening;
use crate::ports::ModelLoader;
use crate::ports::Page;
use crate::ports::Partial;
use crate::ports::Plugin;
use crate::ports::Reasoner;
use crate::ports::SpeechToText;
use crate::ports::Step;
use crate::ports::Store;
use crate::ports::SttStream;
use crate::ports::TextToSpeech;
use crate::ports::Tool;

/// The reference house snapshot.
#[must_use]
pub(crate) fn house() -> Snapshot {
    serde_json::from_str(include_str!("house.json")).expect("house.json is a snapshot")
}

/// A dispatcher that writes the `info` events to `log`.
///
/// # Errors
///
/// `log` does not open again.
pub fn log_dispatch(log: &NamedTempFile) -> io::Result<Dispatch> {
    // While only one dispatcher is alive, `tracing-core` takes the interest of a callsite from
    // the default dispatcher of the thread that registers it. Thus a parallel test without a
    // dispatcher turns the callsite off. With this second dispatcher alive for the whole run,
    // `tracing-core` asks every live dispatcher.
    static SILENT: LazyLock<Dispatch> = LazyLock::new(|| Dispatch::new(NoSubscriber::default()));
    LazyLock::force(&SILENT);
    let subscriber = tracing_subscriber::fmt().with_writer(Arc::new(log.reopen()?));
    Ok(Dispatch::new(subscriber.finish()))
}

/// The contents of `mutex`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("no test panics with the lock")
}

/// A `Home` with a settable snapshot, scripted events, a count of snapshot calls, recorded calls
/// and announces, delayed calls and response data.
pub(crate) struct FakeHome {
    /// The snapshot that `snapshot` returns.
    snapshot: Mutex<Snapshot>,
    /// The number of `snapshot` calls.
    snapshots: AtomicUsize,
    /// The sender of the scripted events.
    sender: mpsc::UnboundedSender<Event>,
    /// The receiver of the scripted events, until `events` takes it.
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
    /// The gates of the next delayed calls, in order.
    gates: Mutex<VecDeque<oneshot::Receiver<()>>>,
    /// The service calls, in order. An announce is its `assist_satellite.announce` call.
    calls: Mutex<Vec<ServiceCall>>,
    /// The response data that `call` gives.
    response: Mutex<Option<Value>>,
    /// The response data of the next calls, in order, before `response`.
    next: Mutex<VecDeque<Value>>,
}

impl FakeHome {
    /// A house that gives `snapshot`.
    #[must_use]
    pub(crate) fn new(snapshot: Snapshot) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            snapshot: Mutex::new(snapshot),
            snapshots: AtomicUsize::new(0),
            sender,
            receiver: Mutex::new(Some(receiver)),
            gates: Mutex::default(),
            calls: Mutex::default(),
            response: Mutex::default(),
            next: Mutex::default(),
        }
    }

    /// Makes the later `snapshot` calls give `snapshot`.
    pub(crate) fn set_snapshot(&self, snapshot: Snapshot) {
        *lock(&self.snapshot) = snapshot;
    }

    /// Sends `event` on the event stream.
    pub(crate) fn send(&self, event: Event) {
        self.sender
            .unbounded_send(event)
            .expect("the event stream is open");
    }

    /// Makes the next call wait until the returned sender sends. The call fails when it drops.
    pub(crate) fn delay(&self) -> oneshot::Sender<()> {
        let (sender, receiver) = oneshot::channel();
        lock(&self.gates).push_back(receiver);
        sender
    }

    /// Makes the later calls give `response` as their response data.
    pub(crate) fn respond(&self, response: Value) {
        *lock(&self.response) = Some(response);
    }

    /// Makes the next call without an earlier queued response give `response` as its response
    /// data.
    pub(crate) fn respond_next(&self, response: Value) {
        lock(&self.next).push_back(response);
    }

    /// The number of `snapshot` calls.
    #[must_use]
    pub(crate) fn snapshots(&self) -> usize {
        self.snapshots.load(Ordering::SeqCst)
    }

    /// The service calls, in order.
    #[must_use]
    pub(crate) fn calls(&self) -> Vec<ServiceCall> {
        lock(&self.calls).clone()
    }

    /// Waits for the gate of this call, if `delay` gave one.
    async fn wait(&self) -> anyhow::Result<()> {
        let gate = lock(&self.gates).pop_front();
        if let Some(gate) = gate {
            gate.await?;
        }
        Ok(())
    }
}

impl Default for FakeHome {
    /// The reference house.
    fn default() -> Self {
        Self::new(house())
    }
}

#[async_trait]
impl Home for FakeHome {
    async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        let snapshot = lock(&self.snapshot).clone();
        self.wait().await?;
        Ok(snapshot)
    }

    fn events(&self) -> BoxStream<'static, Event> {
        let receiver = lock(&self.receiver).take();
        receiver.expect("events is called once").boxed()
    }

    async fn call(&self, call: ServiceCall) -> anyhow::Result<Option<Value>> {
        lock(&self.calls).push(call);
        self.wait().await?;
        let next = lock(&self.next).pop_front();
        Ok(next.or_else(|| lock(&self.response).clone()))
    }

    async fn announce(&self, satellite: &str, text: &str) -> anyhow::Result<()> {
        let data = [("message", json!(text)), ("preannounce", json!(false))];
        lock(&self.calls).push(ServiceCall {
            domain: "assist_satellite".to_owned(),
            service: "announce".to_owned(),
            target: vec![satellite.to_owned()],
            data: data
                .map(|(key, value)| (key.to_owned(), value))
                .into_iter()
                .collect(),
            return_response: false,
        });
        Ok(())
    }

    async fn ask(&self, satellite: &str, question: &str) -> anyhow::Result<()> {
        let data = json!({ "start_message": question, "preannounce": false });
        lock(&self.calls).push(ServiceCall {
            domain: "assist_satellite".to_owned(),
            service: "start_conversation".to_owned(),
            target: vec![satellite.to_owned()],
            data: serde_json::from_value(data)?,
            return_response: false,
        });
        self.wait().await
    }
}

/// A `Classifier` that gives scripted predictions in order and records each `(context, text)`.
#[derive(Default)]
pub struct FakeClassifier {
    /// The predictions of the next calls, in order.
    predictions: Mutex<VecDeque<Prediction>>,
    /// The `(context, text)` of each call, in order.
    calls: Mutex<Vec<(String, String)>>,
}

impl FakeClassifier {
    /// Makes the next call without an earlier scripted prediction give `prediction`.
    pub fn script(&self, prediction: Prediction) {
        lock(&self.predictions).push_back(prediction);
    }

    /// The `(context, text)` of each call, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<(String, String)> {
        lock(&self.calls).clone()
    }
}

#[async_trait]
impl Classifier for FakeClassifier {
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction> {
        lock(&self.calls).push((context.to_owned(), text.to_owned()));
        let prediction = lock(&self.predictions).pop_front();
        prediction.ok_or_else(|| anyhow::anyhow!("no scripted prediction for {text}"))
    }
}

/// A `ModelLoader` that gives scripted classifiers in order.
#[derive(Default)]
pub struct FakeModelLoader {
    /// The classifiers of the next loads, in order.
    classifiers: Mutex<VecDeque<Arc<dyn Classifier>>>,
}

impl FakeModelLoader {
    /// Makes the next load without an earlier scripted classifier give `classifier`.
    pub fn script(&self, classifier: Arc<dyn Classifier>) {
        lock(&self.classifiers).push_back(classifier);
    }
}

#[async_trait]
impl ModelLoader for FakeModelLoader {
    async fn load(&self, dir: &Path, _device: Device) -> anyhow::Result<Arc<dyn Classifier>> {
        let classifier = lock(&self.classifiers).pop_front();
        classifier.ok_or_else(|| anyhow::anyhow!("no scripted classifier for {}", dir.display()))
    }
}

/// One scripted step of a `FakeReasoner` answer.
pub(crate) enum Scripted {
    /// A sentence.
    Sentence(&'static str),
    /// A service call.
    Call(ServiceCall),
    /// A read of the states of the area with this name.
    States(&'static str),
    /// A plugin call.
    Plugin(PluginCall),
    /// A question with this text, from the response `response`, with the text as the call ID.
    Ask(&'static str),
    /// An error with this text.
    Error(&'static str),
    /// A wait of this time before the next step.
    Wait(Duration),
}

/// The receiver of the tool result of one call.
type Reply = oneshot::Receiver<anyhow::Result<Option<Value>>>;

/// A `Reasoner` that counts the `warm` calls, records each question, answers with the scripted
/// steps, keeps the reply of each call, records the pool of each `generate` call and holds
/// it until `release`.
#[derive(Default)]
pub(crate) struct FakeReasoner {
    /// The number of `warm` calls.
    warms: AtomicUsize,
    /// Each question, in order.
    questions: Mutex<Vec<Question>>,
    /// The steps of each next answer, in order.
    scripts: Mutex<VecDeque<Vec<Scripted>>>,
    /// The reply of each call, in order.
    replies: Arc<Mutex<Vec<Reply>>>,
    /// The pool of each `generate` call, in order.
    generated: Mutex<Vec<String>>,
    /// The result sender of each held `generate` call, by pool.
    held: Mutex<BTreeMap<String, oneshot::Sender<anyhow::Result<Vec<String>>>>>,
}

impl FakeReasoner {
    /// The number of `warm` calls.
    #[must_use]
    pub(crate) fn warms(&self) -> usize {
        self.warms.load(Ordering::SeqCst)
    }

    /// The text of each question, in order.
    #[must_use]
    pub(crate) fn questions(&self) -> Vec<String> {
        let questions = lock(&self.questions);
        questions
            .iter()
            .map(|question| question.text.clone())
            .collect()
    }

    /// The call ID that each question answers, in order.
    #[must_use]
    pub(crate) fn answered(&self) -> Vec<Option<String>> {
        let questions = lock(&self.questions);
        let calls = questions.iter().map(|question| question.answers.as_ref());
        calls.map(|call| Some(call?.call_id.clone())).collect()
    }

    /// Makes the next answer without earlier scripted steps give `steps`.
    pub(crate) fn script(&self, steps: Vec<Scripted>) {
        lock(&self.scripts).push_back(steps);
    }

    /// The tool result of each call that came back: the error text, else the result.
    #[must_use]
    pub(crate) fn results(&self) -> Vec<String> {
        let mut replies = lock(&self.replies);
        let results = replies.iter_mut().map(|reply| match reply.try_recv() {
            Ok(Some(Err(error))) => error.to_string(),
            other => format!("{other:?}"),
        });
        results.collect()
    }

    /// The pool of each `generate` call, in order.
    #[must_use]
    pub(crate) fn generated(&self) -> Vec<String> {
        lock(&self.generated).clone()
    }

    /// Ends the held `generate` call of `pool` with `result`.
    pub(crate) fn release(&self, pool: &str, result: anyhow::Result<Vec<String>>) {
        let sender = lock(&self.held).remove(pool);
        let sender = sender.expect("a generate call of the pool waits");
        sender.send(result).expect("the generate call waits");
    }
}

/// A sender of a tool result whose receiver is kept in `replies`.
fn reply(replies: &Mutex<Vec<Reply>>) -> oneshot::Sender<anyhow::Result<Option<Value>>> {
    let (sender, receiver) = oneshot::channel();
    lock(replies).push(receiver);
    sender
}

/// The answer step of `step`, with its reply receiver kept in `replies`, or `None` after a wait.
async fn answered(step: Scripted, replies: Arc<Mutex<Vec<Reply>>>) -> Option<anyhow::Result<Step>> {
    match step {
        Scripted::Sentence(text) => Some(Ok(Step::Sentence(text.to_owned()))),
        Scripted::Call(call) => Some(Ok(Step::Call(call, reply(&replies)))),
        Scripted::States(area) => Some(Ok(Step::States(area.to_owned(), reply(&replies)))),
        Scripted::Plugin(call) => Some(Ok(Step::Plugin(call, reply(&replies)))),
        Scripted::Ask(text) => Some(Ok(Step::Ask {
            question: text.to_owned(),
            response_id: "response".to_owned(),
            call_id: text.to_owned(),
        })),
        Scripted::Error(text) => Some(Err(anyhow::anyhow!(text))),
        Scripted::Wait(time) => {
            tokio::time::sleep(time).await;
            None
        }
    }
}

#[async_trait]
impl Reasoner for FakeReasoner {
    async fn warm(&self) -> anyhow::Result<()> {
        self.warms.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn answer(&self, question: &Question) -> BoxStream<'static, anyhow::Result<Step>> {
        lock(&self.questions).push(question.clone());
        let steps = lock(&self.scripts).pop_front().unwrap_or_default();
        let replies = Arc::clone(&self.replies);
        let step = move |step| answered(step, Arc::clone(&replies));
        futures::stream::iter(steps).filter_map(step).boxed()
    }

    async fn generate(
        &self,
        pool: &str,
        _existing: &[String],
        _variables: &BTreeSet<String>,
        _n: usize,
    ) -> anyhow::Result<Vec<String>> {
        lock(&self.generated).push(pool.to_owned());
        let (sender, receiver) = oneshot::channel();
        lock(&self.held).insert(pool.to_owned(), sender);
        receiver.await?
    }
}

/// The arguments and the HA configuration of one plugin fetch.
pub(crate) type Fetch = (Map<String, Value>, HomeConfig);

/// A `Plugin` with a tool that gives the scripted results in order and records each fetch. A
/// fetch with no scripted result waits without end.
pub(crate) struct FakePlugin {
    /// The tool of the plugin.
    tool: Tool,
    /// The results of the next fetches, in order.
    results: Mutex<VecDeque<anyhow::Result<Value>>>,
    /// The arguments and the HA configuration of each fetch, in order.
    fetches: Mutex<Vec<Fetch>>,
}

impl FakePlugin {
    /// A plugin with the tool `name`.
    #[must_use]
    pub(crate) fn new(name: &str) -> Self {
        Self {
            tool: Tool {
                name: name.to_owned(),
                description: format!("Gives {name} test data."),
                parameters: json!({ "type": "object" }),
            },
            results: Mutex::default(),
            fetches: Mutex::default(),
        }
    }

    /// Makes the next fetch without an earlier scripted result give `result`.
    pub(crate) fn script(&self, result: anyhow::Result<Value>) {
        lock(&self.results).push_back(result);
    }

    /// The arguments and the HA configuration of each fetch, in order.
    #[must_use]
    pub(crate) fn fetches(&self) -> Vec<Fetch> {
        lock(&self.fetches).clone()
    }
}

#[async_trait]
impl Plugin for FakePlugin {
    fn tool(&self) -> Tool {
        self.tool.clone()
    }

    async fn fetch(
        &self,
        arguments: &Map<String, Value>,
        home: &HomeConfig,
    ) -> anyhow::Result<Value> {
        lock(&self.fetches).push((arguments.clone(), home.clone()));
        let result = lock(&self.results).pop_front();
        match result {
            Some(result) => result,
            None => futures::future::pending().await,
        }
    }
}

/// A `TextToSpeech` that gives audio with no samples and records each text.
#[derive(Default)]
pub(crate) struct FakeTts {
    /// The text of each call, in order.
    texts: Mutex<Vec<String>>,
}

impl FakeTts {
    /// The text of each call, in order.
    #[must_use]
    pub(crate) fn texts(&self) -> Vec<String> {
        lock(&self.texts).clone()
    }
}

#[async_trait]
impl TextToSpeech for FakeTts {
    async fn synth(&self, text: &str) -> anyhow::Result<Audio> {
        lock(&self.texts).push(text.to_owned());
        // Other tasks run during a synthesis, as with a real TTS.
        tokio::task::yield_now().await;
        Ok(Audio {
            samples: Vec::new(),
            rate: 16_000,
        })
    }
}

/// The 16 kHz audio of `text` from the `SupertonicTts` of `jarvis.toml` on the CPU, with 250 ms
/// of silence before it and 500 ms after it.
pub(crate) async fn speech(text: &str) -> anyhow::Result<Audio> {
    let config = Config::parse(include_str!("../../jarvis.toml"))?;
    let audio = SupertonicTts::try_new(&config.tts, Device::Cpu)?
        .synth(text)
        .await?;
    let rate = 16_000;
    // The int8 first pass mishears a first word that starts at sample 0 (sub-phase 9.3c).
    let mut samples = vec![0; rate / 4];
    samples.extend(resample(&audio, rate)?);
    samples.resize(samples.len() + rate / 2, 0);
    Ok(Audio {
        samples,
        rate: u32::try_from(rate)?,
    })
}

/// The samples of `audio` at `rate` Hz.
fn resample(audio: &Audio, rate: usize) -> anyhow::Result<Vec<i16>> {
    let input: Vec<f32> = audio.samples.iter().copied().map(f32::from).collect();
    let from = usize::try_from(audio.rate)?;
    let mut resampler = Fft::<f32>::new(from, rate, 1024, 1, FixedSync::Input)?;
    let buffer = InterleavedSlice::new(&input, 1, input.len())?;
    let output = resampler.process_all(&buffer, input.len(), None)?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a float cast saturates at the `i16` range"
    )]
    let samples = output.take_data().into_iter().map(|value| value as i16);
    Ok(samples.collect())
}

/// A `SpeechToText` with scripted first-pass partials, a scripted second-pass result and a gate
/// on `accept`.
pub struct FakeStt {
    /// The partials of each stream, with their voice ends, in order, one after each `accept`.
    pub partials: Vec<Partial>,
    /// The result of each `transcribe`: the text, or the error message.
    pub second_pass: Result<Option<String>, String>,
    /// The lock that each `accept` takes first. A test holds it to block `accept`.
    pub gate: Arc<tokio::sync::Mutex<()>>,
}

impl Default for FakeStt {
    /// No partials, no second pass and an open gate.
    fn default() -> Self {
        Self {
            partials: Vec::new(),
            second_pass: Ok(None),
            gate: Arc::default(),
        }
    }
}

impl SpeechToText for FakeStt {
    fn stream(&self) -> anyhow::Result<Box<dyn SttStream>> {
        Ok(Box::new(FakeSttStream {
            partials: self.partials.clone().into(),
            current: Partial::default(),
            gate: Arc::clone(&self.gate),
        }))
    }

    fn transcribe(&self, _samples: &[i16]) -> anyhow::Result<Option<String>> {
        self.second_pass.clone().map_err(anyhow::Error::msg)
    }
}

/// A stream of `FakeStt`.
struct FakeSttStream {
    /// The partials of the next `accept` calls, in order.
    partials: VecDeque<Partial>,
    /// The partial of the last `accept`, or the empty partial after `reset`.
    current: Partial,
    /// The lock that each `accept` takes first.
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl SttStream for FakeSttStream {
    fn accept(&mut self, _samples: &[i16]) {
        drop(self.gate.blocking_lock());
        if let Some(next) = self.partials.pop_front() {
            self.current = next;
        }
    }

    fn partial(&self) -> Partial {
        self.current.clone()
    }

    fn reset(&mut self) {
        self.current = Partial::default();
    }
}

/// A `Store` in memory that records each save.
pub(crate) struct FakeStore<T> {
    /// The saved value.
    value: Mutex<Option<T>>,
    /// The saved values, in order.
    saves: Mutex<Vec<T>>,
}

impl<T: Clone> FakeStore<T> {
    /// A store whose saved value is `value`.
    #[must_use]
    pub(crate) fn new(value: Option<T>) -> Self {
        Self {
            value: Mutex::new(value),
            saves: Mutex::default(),
        }
    }

    /// The saved values, in order.
    #[must_use]
    pub(crate) fn saves(&self) -> Vec<T> {
        lock(&self.saves).clone()
    }
}

impl<T: Clone + Send> Store<T> for FakeStore<T> {
    fn load(&self) -> anyhow::Result<Option<T>> {
        Ok(lock(&self.value).clone())
    }

    fn save(&self, value: &T) -> anyhow::Result<()> {
        *lock(&self.value) = Some(value.clone());
        lock(&self.saves).push(value.clone());
        Ok(())
    }
}

/// An `InteractionLog` in memory.
#[derive(Default)]
pub struct FakeLog {
    /// Each appended interaction with its audio, in order.
    pub interactions: Mutex<Vec<(Interaction, Option<Audio>)>>,
    /// The time of each `prune` call, in order.
    pub prunes: Mutex<Vec<Timestamp>>,
    /// The wait of each `append` and `update`.
    pub delay: Duration,
}

impl FakeLog {
    /// The interaction `id` with its audio.
    fn find(&self, id: u64) -> Option<(Interaction, Option<Audio>)> {
        let interactions = lock(&self.interactions);
        interactions
            .iter()
            .find(|(found, _)| found.id == id)
            .cloned()
    }

    /// The interactions that `keep` keeps, oldest first.
    fn kept(&self, keep: impl Fn(&Interaction) -> bool) -> Vec<Interaction> {
        let interactions = lock(&self.interactions);
        let mut kept: Vec<Interaction> = interactions
            .iter()
            .filter(|(interaction, _)| keep(interaction))
            .map(|(interaction, _)| interaction.clone())
            .collect();
        kept.sort_by_key(|interaction| interaction.time);
        kept
    }

    /// Applies `change` to the interaction `id`.
    fn change(&self, id: u64, change: impl FnOnce(&mut Interaction)) {
        let mut interactions = lock(&self.interactions);
        let found = interactions.iter_mut().find(|(found, _)| found.id == id);
        if let Some((interaction, _)) = found {
            change(interaction);
        }
    }
}

/// True when `interaction` has a bad mark on a target that `on` keeps.
fn has_bad(interaction: &Interaction, on: impl Fn(feedback::Target) -> bool) -> bool {
    let marks = &interaction.marks;
    let bad = |mark: &Mark| matches!(mark.value, Some(feedback::Value::Bad)) && on(mark.target);
    marks.iter().any(bad)
}

#[async_trait]
impl InteractionLog for FakeLog {
    async fn append(&self, interaction: Interaction, audio: Option<Audio>) -> anyhow::Result<()> {
        tokio::time::sleep(self.delay).await;
        lock(&self.interactions).push((interaction, audio));
        Ok(())
    }

    async fn update(&self, id: u64, change: Change) -> anyhow::Result<()> {
        tokio::time::sleep(self.delay).await;
        let Change::Response(spoken) = change;
        self.change(id, |interaction| interaction.responses.push(spoken));
        Ok(())
    }

    async fn list(&self, filter: Filter, page: Page) -> anyhow::Result<Vec<Interaction>> {
        let mut interactions = self.kept(|interaction| match filter {
            Filter::All => true,
            Filter::Bad => has_bad(interaction, |_| true),
        });
        interactions.reverse();
        let skipped = interactions.into_iter().skip(page.number * page.size);
        Ok(skipped.take(page.size).collect())
    }

    async fn get(&self, id: u64) -> anyhow::Result<Option<Interaction>> {
        Ok(self.find(id).map(|(interaction, _)| interaction))
    }

    async fn audio(&self, id: u64) -> anyhow::Result<Option<Audio>> {
        Ok(self.find(id).and_then(|(_, audio)| audio))
    }

    async fn mark(&self, mark: Mark) -> anyhow::Result<()> {
        self.change(mark.interaction, |interaction| {
            interaction
                .marks
                .retain(|other| other.target != mark.target);
            if mark.value.is_some() {
                interaction.marks.push(mark);
            }
        });
        Ok(())
    }

    async fn marked(&self) -> anyhow::Result<Vec<Interaction>> {
        Ok(self.kept(|interaction| !interaction.marks.is_empty()))
    }

    async fn deferred(&self, since: Timestamp) -> anyhow::Result<Vec<Deferred>> {
        let found = self.kept(|interaction| {
            interaction.time > since
                && interaction.decision == Route::Llm
                && interaction.link.is_none()
                && !has_bad(interaction, |target| target == feedback::Target::Action)
        });
        let deferred = found.into_iter().map(|interaction| Deferred {
            id: interaction.id,
            text: interaction.transcript,
            time: interaction.time,
            spoken: interaction.satellite.is_some(),
        });
        Ok(deferred.collect())
    }

    async fn prune(&self, now: Timestamp) -> anyhow::Result<()> {
        lock(&self.prunes).push(now);
        Ok(())
    }
}

/// A `Clock` that always gives one time.
pub struct FakeClock(pub Timestamp);

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// A `Clock` that gives the time that a test sets.
pub(crate) struct SetClock(pub(crate) Mutex<Timestamp>);

impl Clock for SetClock {
    fn now(&self) -> Timestamp {
        *lock(&self.0)
    }
}

/// An `Assistant` with a fixed name and version that records each request. `handle` replies
/// with work that marks its start, and `speak` gives 1500 samples of silence, or both fail after
/// `fail`. `listen` gives the scripted sessions in order.
#[derive(Default)]
pub(crate) struct FakeAssistant {
    /// The text and the context of each request, in order.
    requests: Mutex<Vec<(String, Context)>>,
    /// The listen sessions of the next `listen` calls, in order.
    sessions: Mutex<VecDeque<FakeListening>>,
    /// The samples of each `push` of all sessions, in order.
    pushes: Arc<Mutex<Vec<Vec<i16>>>>,
    /// The error text of the later requests, if any.
    error: Mutex<Option<String>>,
    /// True when the work of a reply has started.
    started: Arc<AtomicBool>,
}

impl FakeAssistant {
    /// Makes the later requests fail with `error`.
    pub(crate) fn fail(&self, error: &str) {
        *lock(&self.error) = Some(error.to_owned());
    }

    /// The text and the context of each request, in order.
    #[must_use]
    pub(crate) fn requests(&self) -> Vec<(String, Context)> {
        lock(&self.requests).clone()
    }

    /// True when the work of a reply has started.
    #[must_use]
    pub(crate) fn started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Makes the next `listen` give a session whose `push` gives `push` and whose `finish` gives
    /// `finish`.
    pub(crate) fn script(&self, push: Option<&str>, finish: &str) {
        lock(&self.sessions).push_back(FakeListening {
            push: push.map(str::to_owned),
            finish: finish.to_owned(),
            pushes: Arc::clone(&self.pushes),
        });
    }

    /// The samples of each `push` of all sessions, in order.
    #[must_use]
    pub(crate) fn pushes(&self) -> Vec<Vec<i16>> {
        lock(&self.pushes).clone()
    }
}

#[async_trait]
impl Assistant for FakeAssistant {
    fn info(&self) -> Info {
        Info {
            name: "jarvis-test".to_owned(),
            version: "1.2.3".to_owned(),
        }
    }

    fn listen(&self) -> anyhow::Result<Box<dyn Listening>> {
        let session = lock(&self.sessions).pop_front();
        Ok(Box::new(
            session.ok_or_else(|| anyhow::anyhow!("no scripted session"))?,
        ))
    }

    async fn handle(&self, text: &str, context: Context) -> anyhow::Result<Handled> {
        lock(&self.requests).push((text.to_owned(), context));
        if let Some(error) = lock(&self.error).clone() {
            anyhow::bail!(error);
        }
        let started = Arc::clone(&self.started);
        Ok(Handled {
            text: format!("reply to {text}"),
            after: Some(
                async move {
                    started.store(true, Ordering::SeqCst);
                    Ok(())
                }
                .boxed(),
            ),
        })
    }

    async fn speak(&self, _text: &str) -> anyhow::Result<Arc<Audio>> {
        if let Some(error) = lock(&self.error).clone() {
            anyhow::bail!(error);
        }
        Ok(Arc::new(Audio {
            samples: vec![0; 1500],
            rate: 16_000,
        }))
    }
}

/// A scripted listen session of `FakeAssistant`.
struct FakeListening {
    /// The result of each `push`.
    push: Option<String>,
    /// The result of `finish`.
    finish: String,
    /// The samples of each `push`, in order.
    pushes: Arc<Mutex<Vec<Vec<i16>>>>,
}

#[async_trait]
impl Listening for FakeListening {
    async fn push(&mut self, samples: Vec<i16>) -> anyhow::Result<Option<String>> {
        lock(&self.pushes).push(samples);
        Ok(self.push.clone())
    }

    async fn finish(self: Box<Self>) -> anyhow::Result<String> {
        Ok(self.finish)
    }
}

/// A Wyoming client over TCP or an in-memory stream.
pub(crate) struct FakeWyomingClient<S = TcpStream> {
    /// The connection to the server.
    stream: BufReader<S>,
}

impl FakeWyomingClient {
    /// Connects to the server at `address`.
    pub(crate) async fn connect(address: SocketAddr) -> anyhow::Result<Self> {
        Ok(Self::new(TcpStream::connect(address).await?))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> FakeWyomingClient<S> {
    /// A client on the open connection `stream`.
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream: BufReader::new(stream),
        }
    }

    /// Writes `bytes` with no framing.
    pub(crate) async fn send_raw(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        Ok(self.stream.write_all(bytes).await?)
    }

    /// Sends the event `event_type` with `data`.
    pub(crate) async fn send(&mut self, event_type: &str, data: &Value) -> anyhow::Result<()> {
        let data = data.to_string();
        let header = json!({ "type": event_type, "data_length": data.len() });
        self.send_raw(format!("{header}\n{data}").as_bytes()).await
    }

    /// The type and the data of the next event, or `None` when the server closes the connection.
    /// Drops the payload.
    pub(crate) async fn receive(&mut self) -> anyhow::Result<Option<(String, Value)>> {
        let mut line = String::new();
        if self.stream.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let header: Value = serde_json::from_str(&line)?;
        let length = usize::try_from(header["data_length"].as_u64().unwrap_or(0))?;
        let payload = usize::try_from(header["payload_length"].as_u64().unwrap_or(0))?;
        let mut data = vec![0; length + payload];
        self.stream.read_exact(&mut data).await?;
        data.truncate(length);
        let event_type = header["type"].as_str().unwrap_or_default().to_owned();
        Ok(Some((event_type, serde_json::from_slice(&data)?)))
    }
}
