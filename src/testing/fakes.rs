//! A fake for each port.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::stream::BoxStream;
use jiff::SignedDuration;
use jiff::Timestamp;
use rubato::Fft;
use rubato::FixedSync;
use rubato::Resampler;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rumqttc::AsyncClient;
use rumqttc::EventLoop;
use rumqttc::MqttOptions;
use rumqttc::Packet;
use rumqttc::QoS;
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
use tokio::sync::watch;
use tracing::Dispatch;
use tracing::subscriber::NoSubscriber;

use crate::Config;
use crate::CurrentClassifier;
use crate::Device;
use crate::adapters::supertonic::SupertonicTts;
use crate::app::conversation::Speakers;
use crate::app::review;
use crate::app::training::FIXTURE;
use crate::app::training::Training;
use crate::app::training::Trigger;
use crate::domain::decide::PluginCall;
use crate::domain::decide::Question;
use crate::domain::feedback;
use crate::domain::feedback::Action;
use crate::domain::feedback::Deferred;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::feedback::Route;
use crate::domain::learn::Learned;
use crate::domain::learn::LearnedIntent;
use crate::domain::learn::LearnedStatus;
use crate::domain::learn::PluginRequest;
use crate::domain::model::Prediction;
use crate::domain::speaker::Person;
use crate::domain::speech::Audio;
use crate::domain::state::EntityId;
use crate::domain::state::Event;
use crate::domain::state::HomeConfig;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;
use crate::domain::state_cache::StateCache;
use crate::domain::training::TrainingSet;
use crate::ports::Assistant;
use crate::ports::Change;
use crate::ports::Classifier;
use crate::ports::Clock;
use crate::ports::Context;
use crate::ports::Control;
use crate::ports::Filter;
use crate::ports::Handled;
use crate::ports::Home;
use crate::ports::Info;
use crate::ports::InteractionLog;
use crate::ports::Listening;
use crate::ports::MediaLibrary;
use crate::ports::ModelLoader;
use crate::ports::Models;
use crate::ports::Page;
use crate::ports::Partial;
use crate::ports::Plugin;
use crate::ports::PluginRequests;
use crate::ports::Reasoner;
use crate::ports::Refusal;
use crate::ports::Review;
use crate::ports::Setting;
use crate::ports::SpeechToText;
use crate::ports::Status;
use crate::ports::Step;
use crate::ports::Store;
use crate::ports::SttStream;
use crate::ports::TextToSpeech;
use crate::ports::Title;
use crate::ports::Tool;
use crate::ports::Trainer;
use crate::ports::Unassigned;
use crate::ports::Unresolved;
use crate::ports::VoiceEmbedder;
use crate::ports::VoiceProfiles;

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
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(Arc::new(log.reopen()?));
    Ok(Dispatch::new(subscriber.finish()))
}

/// The contents of `mutex`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("no test panics with the lock")
}

/// A `Home` with a settable snapshot, scripted events, a count of snapshot calls, recorded calls
/// and announces, delayed calls, response data and library responses.
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
    /// The response data of a `get_library` call for each media type, before `next`.
    library: Mutex<BTreeMap<String, Value>>,
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
            library: Mutex::default(),
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

    /// Makes each `get_library` call of `media_type` give `response` as its response data.
    pub(crate) fn respond_library(&self, media_type: &str, response: Value) {
        lock(&self.library).insert(media_type.to_owned(), response);
    }

    /// The `respond_library` response data of `call` when it is a `get_library` call.
    fn library(&self, call: &ServiceCall) -> Option<Value> {
        let media_type = call.data.get("media_type").and_then(Value::as_str);
        let media_type = media_type.filter(|_| call.service == "get_library")?;
        lock(&self.library).get(media_type).cloned()
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
        let library = self.library(&call);
        lock(&self.calls).push(call);
        self.wait().await?;
        let next = library.or_else(|| lock(&self.next).pop_front());
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
    /// The lock that each `load` takes first. A test holds it to block `load`.
    pub gate: tokio::sync::Mutex<()>,
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
        drop(self.gate.lock().await);
        let classifier = lock(&self.classifiers).pop_front();
        classifier.ok_or_else(|| anyhow::anyhow!("no scripted classifier for {}", dir.display()))
    }
}

/// A `Trainer` that records each call and gives the scripted results in order, each after its
/// gate opens. A success creates the model folder `<dir>/<run_id>/`.
#[derive(Default)]
pub struct FakeTrainer {
    /// The folder of the model folders.
    pub dir: PathBuf,
    /// The results of the next calls in order: `Ok`, or the error text.
    pub results: Mutex<VecDeque<Result<(), &'static str>>>,
    /// The lock that each call takes after it records the call. A test holds it to block `train`.
    pub gate: Arc<tokio::sync::Mutex<()>>,
    /// The run ID and the set of each call, in order.
    pub calls: Mutex<Vec<(String, TrainingSet)>>,
}

#[async_trait]
impl Trainer for FakeTrainer {
    async fn train(&self, run_id: &str, set: &TrainingSet) -> anyhow::Result<PathBuf> {
        lock(&self.calls).push((run_id.to_owned(), set.clone()));
        drop(self.gate.lock().await);
        let result = lock(&self.results).pop_front();
        result
            .unwrap_or(Err("no scripted result"))
            .map_err(anyhow::Error::msg)?;
        let dir = self.dir.join(run_id);
        std::fs::create_dir(&dir)?;
        Ok(dir)
    }
}

/// A `PluginRequests` that records each request and gives the scripted results in order.
#[derive(Default)]
pub struct FakePluginRequests {
    /// The results of the next calls in order: the path, or the error text.
    pub results: Mutex<VecDeque<Result<PathBuf, &'static str>>>,
    /// The request of each call, in order.
    pub calls: Mutex<Vec<PluginRequest>>,
}

impl PluginRequests for FakePluginRequests {
    fn write(&self, request: &PluginRequest) -> anyhow::Result<PathBuf> {
        lock(&self.calls).push(request.clone());
        let result = lock(&self.results).pop_front();
        result
            .unwrap_or(Err("no scripted result"))
            .map_err(anyhow::Error::msg)
    }
}

/// A training run on `intents.toml` and the reference house with fakes, with the held-out share
/// `holdout`, and its trainer, which creates the model folders in `models`.
///
/// # Panics
///
/// `jarvis.toml` or `intents.toml` is not valid.
#[must_use]
pub fn training(holdout: f64, models: &Path) -> (Training, Arc<FakeTrainer>) {
    let trainer = trainer_in(models);
    let catalog = toml::from_str(include_str!("../../intents.toml")).expect("it is a catalog");
    let training = Training {
        reasoner: Arc::new(FakeReasoner::default()),
        generated: Arc::new(FakeStore::new(None)),
        state: Arc::new(FakeStore::new(None)),
        log: Arc::<FakeLog>::default(),
        trainer: Arc::clone(&trainer) as _,
        runpod: None,
        loader: Arc::new(FakeModelLoader::default()),
        classifier: Arc::new(CurrentClassifier::new(
            FIXTURE.to_owned(),
            Arc::<FakeClassifier>::default(),
        )),
        catalog: Arc::new(catalog),
        learned: Arc::new(FakeStore::new(None)),
        cache: watch::channel(StateCache::load(house())).1,
        clock: Arc::new(FakeClock(Timestamp::UNIX_EPOCH)),
        config: training_config(holdout, models),
        home: Arc::new(FakeHome::default()),
        models: Arc::new(FakeModels::default()),
        stop: watch::Sender::default(),
        status: watch::Sender::default(),
    };
    (training, trainer)
}

/// `jarvis.toml` with the held-out share `holdout` and the model folder `models`.
fn training_config(holdout: f64, models: &Path) -> Config {
    let mut config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
    (config.train.holdout, config.model.dir) = (holdout, models.to_owned());
    config
}

/// A trainer that creates the model folders in `models`.
#[must_use]
pub fn trainer_in(models: &Path) -> Arc<FakeTrainer> {
    Arc::new(FakeTrainer {
        dir: models.to_owned(),
        ..FakeTrainer::default()
    })
}

/// A trigger with no backoff of the run of [`training`] in `models`, and its trainer.
#[must_use]
pub fn trigger(models: &Path) -> (Arc<Trigger>, Arc<FakeTrainer>) {
    let (training, trainer) = training(0.1, models);
    let training = Arc::new(training);
    let trigger = Trigger::new(training, SignedDuration::ZERO, Arc::default());
    (Arc::new(trigger), trainer)
}

/// A `Models` that records the calls and can fail the next `restore`.
#[derive(Default)]
pub struct FakeModels {
    /// The calls in order: `release` or `restore`.
    calls: Mutex<Vec<&'static str>>,
    /// True when the next `restore` fails.
    fail: AtomicBool,
}

impl FakeModels {
    /// The calls in order: `release` or `restore`.
    #[must_use]
    pub fn calls(&self) -> Vec<&'static str> {
        lock(&self.calls).clone()
    }

    /// True when the last call is `release`.
    #[must_use]
    pub fn released(&self) -> bool {
        lock(&self.calls).last() == Some(&"release")
    }

    /// Makes the next `restore` fail.
    pub fn fail_restore(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Models for FakeModels {
    async fn release(&self) {
        lock(&self.calls).push("release");
    }

    async fn restore(&self) -> anyhow::Result<()> {
        lock(&self.calls).push("restore");
        anyhow::ensure!(!self.fail.swap(false, Ordering::SeqCst), "no model");
        Ok(())
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
    /// The end of the answer.
    Done,
    /// A wait of this time before the next step.
    Wait(Duration),
}

/// The receiver of the tool result of one call.
type Reply = oneshot::Receiver<anyhow::Result<Option<Value>>>;

/// A `Reasoner` that counts the `warm` calls, records each question, answers with the scripted
/// steps, keeps the reply of each call, records the pool of each `generate` call and holds
/// it until `release`, records the target of each `paraphrase` call and gives its scripted result,
/// gives the scripted embedding of each text, and records the texts of each `propose` call and
/// gives the scripted proposals in order.
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
    /// The target of each `paraphrase` call, in order.
    paraphrased: Mutex<Vec<String>>,
    /// The `paraphrase` result of each target: the examples or the error text. Else no examples.
    paraphrases: Mutex<BTreeMap<String, Result<Vec<String>, &'static str>>>,
    /// The embedding of each text. `embed` fails for a text without one.
    embeddings: Mutex<BTreeMap<String, Vec<f32>>>,
    /// The texts of each `propose` call, in order.
    proposed: Mutex<Vec<Vec<String>>>,
    /// The next proposals, in order. `propose` fails without one.
    proposals: Mutex<VecDeque<LearnedIntent>>,
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

    /// The target of each `paraphrase` call, in order.
    #[must_use]
    pub(crate) fn paraphrased(&self) -> Vec<String> {
        lock(&self.paraphrased).clone()
    }

    /// Makes each `paraphrase` call for `target` give `result`: the examples or the error text.
    pub(crate) fn script_paraphrase(
        &self,
        target: &str,
        result: Result<Vec<String>, &'static str>,
    ) {
        lock(&self.paraphrases).insert(target.to_owned(), result);
    }

    /// Makes `embed` give `embedding` for `text`.
    pub(crate) fn script_embedding(&self, text: &str, embedding: Vec<f32>) {
        lock(&self.embeddings).insert(text.to_owned(), embedding);
    }

    /// The texts of each `propose` call, in order.
    #[must_use]
    pub(crate) fn proposed(&self) -> Vec<Vec<String>> {
        lock(&self.proposed).clone()
    }

    /// Makes the next `propose` call without an earlier scripted proposal give `proposal`.
    pub(crate) fn script_proposal(&self, proposal: LearnedIntent) {
        lock(&self.proposals).push_back(proposal);
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
        Scripted::Done => Some(Ok(Step::Done)),
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

    async fn paraphrase(
        &self,
        target: &str,
        _seeds: &BTreeMap<String, Vec<String>>,
        _slots: &BTreeSet<String>,
        _n: usize,
    ) -> anyhow::Result<Vec<String>> {
        lock(&self.paraphrased).push(target.to_owned());
        let result = lock(&self.paraphrases).get(target).cloned();
        result.unwrap_or(Ok(Vec::new())).map_err(anyhow::Error::msg)
    }

    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let embeddings = lock(&self.embeddings);
        let embedding = |text: &String| embeddings.get(text).cloned();
        let found: Option<Vec<Vec<f32>>> = texts.iter().map(embedding).collect();
        found.ok_or_else(|| anyhow::anyhow!("no embedding"))
    }

    async fn propose(
        &self,
        texts: &[String],
        _intents: &[String],
        _tools: &[Tool],
    ) -> anyhow::Result<LearnedIntent> {
        lock(&self.proposed).push(texts.to_vec());
        let proposal = lock(&self.proposals).pop_front();
        proposal.ok_or_else(|| anyhow::anyhow!("no proposal"))
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

/// A `MediaLibrary` that gives the scripted title lists in order, then an error.
#[derive(Default)]
pub(crate) struct FakeLibrary {
    /// The results of the next calls in order: the titles, or the error text.
    pub(crate) results: Mutex<VecDeque<Result<Vec<Title>, &'static str>>>,
}

/// The title `name` of `kind`.
#[must_use]
pub(crate) fn title(name: &str, kind: &str) -> Title {
    Title {
        name: name.to_owned(),
        kind: kind.to_owned(),
    }
}

#[async_trait]
impl MediaLibrary for FakeLibrary {
    async fn titles(&self) -> anyhow::Result<Vec<Title>> {
        let result = lock(&self.results).pop_front();
        result
            .unwrap_or(Err("no scripted titles"))
            .map_err(anyhow::Error::msg)
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

/// A `VoiceEmbedder` that gives one fixed embedding.
pub(crate) struct FakeVoiceEmbedder;

impl VoiceEmbedder for FakeVoiceEmbedder {
    fn embed(&self, _audio: &Audio) -> anyhow::Result<Vec<f32>> {
        Ok(vec![1.0])
    }
}

/// The samples that one profile of `FakeVoiceProfiles` keeps, as `MAX_SAMPLES` of `speaker.qnt`.
const PROFILE_SAMPLES: usize = 2;

/// A `VoiceProfiles` with fixed similarities and sample counts, which records each enroll and keeps
/// the samples and the picks by the rules of the voice store.
#[derive(Default)]
pub(crate) struct FakeVoiceProfiles {
    /// The similarity of each person.
    pub similarities: BTreeMap<String, f32>,
    /// The sample count of each person. A person without one has 0.
    pub counts: BTreeMap<String, usize>,
    /// The person and the interaction ID of each enroll.
    pub enrolls: Mutex<Vec<(String, u64)>>,
    /// The person of each sample, by interaction ID.
    pub samples: Mutex<BTreeMap<u64, String>>,
    /// The last pick of each interaction ID.
    pub picks: Mutex<BTreeMap<u64, Option<String>>>,
    /// The wait of the next `assign` after its change, until the sender sends or drops.
    pub gate: Mutex<Option<oneshot::Receiver<()>>>,
}

/// Removes the samples of `person` with the oldest IDs until `PROFILE_SAMPLES` are left.
fn trim(samples: &mut BTreeMap<u64, String>, person: &str) {
    let own: Vec<u64> = samples
        .iter()
        .filter(|(_, owner)| *owner == person)
        .map(|(&id, _)| id)
        .collect();
    let oldest = own.iter().take(own.len().saturating_sub(PROFILE_SAMPLES));
    for id in oldest {
        samples.remove(id);
    }
}

#[async_trait]
impl VoiceProfiles for FakeVoiceProfiles {
    async fn enroll(&self, person: &EntityId, id: u64, _embedding: &[f32]) -> anyhow::Result<()> {
        lock(&self.enrolls).push((person.0.clone(), id));
        let picked = lock(&self.picks).contains_key(&id);
        let mut samples = lock(&self.samples);
        if !picked && !samples.contains_key(&id) {
            samples.insert(id, person.0.clone());
            trim(&mut samples, &person.0);
        }
        Ok(())
    }

    async fn assign(
        &self,
        id: u64,
        person: Option<&EntityId>,
        _embedding: &[f32],
    ) -> anyhow::Result<()> {
        lock(&self.picks).insert(id, person.map(|person| person.0.clone()));
        {
            let mut samples = lock(&self.samples);
            samples.remove(&id);
            if let Some(person) = person {
                samples.insert(id, person.0.clone());
                trim(&mut samples, &person.0);
            }
        }
        let gate = lock(&self.gate).take();
        if let Some(gate) = gate {
            // A dropped sender opens the gate too.
            let _ = gate.await;
        }
        Ok(())
    }

    async fn similarities(&self, _embedding: &[f32]) -> anyhow::Result<BTreeMap<String, f32>> {
        Ok(self.similarities.clone())
    }

    fn sample_count(&self, person: &EntityId) -> usize {
        self.counts.get(&person.0).copied().unwrap_or_default()
    }
}

/// The speaker identification of `jarvis.toml` with `voices`.
#[must_use]
pub(crate) fn speakers(voices: Arc<FakeVoiceProfiles>) -> Arc<Speakers> {
    let config = Config::parse(include_str!("../../jarvis.toml")).expect("jarvis.toml is valid");
    Arc::new(Speakers {
        voices,
        config: config.speaker,
        people: Vec::new(),
        satellites: Vec::new(),
    })
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
    /// The hotwords of each `set_hotwords`, in order.
    pub hotwords: Mutex<Vec<Vec<String>>>,
}

impl Default for FakeStt {
    /// No partials, no second pass, an open gate and no hotwords.
    fn default() -> Self {
        Self {
            partials: Vec::new(),
            second_pass: Ok(None),
            gate: Arc::default(),
            hotwords: Mutex::default(),
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

    fn set_hotwords(&self, phrases: &[String]) {
        lock(&self.hotwords).push(phrases.to_vec());
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

    fn flush(&mut self) {}
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
    /// The stored voice embedding of each interaction ID.
    pub embeddings: Mutex<BTreeMap<u64, Vec<f32>>>,
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

    /// Appends a spoken request with `text` at `time` that System Two answered.
    pub(crate) fn defer(&self, text: &str, time: Timestamp) {
        let mut interactions = lock(&self.interactions);
        let interaction = Interaction {
            id: interactions.len() as u64,
            time,
            satellite: Some("assist_satellite.office".to_owned()),
            transcript: text.to_owned(),
            model_context: String::new(),
            model: String::new(),
            top_labels: Vec::new(),
            slots: Vec::new(),
            action: Action {
                intent: "unknown".to_owned(),
                targets: Vec::new(),
            },
            decision: Route::Llm,
            service_calls: Vec::new(),
            responses: Vec::new(),
            latency_ms: None,
            link: None,
            marks: Vec::new(),
            speaker: None,
            source: None,
        };
        interactions.push((interaction, None));
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

/// True when `interaction` has a bad mark.
fn has_bad(interaction: &Interaction) -> bool {
    let marks = &interaction.marks;
    marks
        .iter()
        .any(|mark| matches!(mark.value, Some(feedback::Value::Bad)))
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
        match change {
            Change::Response(spoken) => {
                self.change(id, |interaction| interaction.responses.push(spoken));
            }
            Change::Speaker {
                speaker,
                source,
                embedding,
            } => {
                self.change(id, |interaction| {
                    interaction.speaker = speaker;
                    interaction.source = source;
                });
                if let Some(embedding) = embedding {
                    lock(&self.embeddings).insert(id, embedding);
                }
            }
        }
        Ok(())
    }

    async fn list(&self, filter: Filter, page: Page) -> anyhow::Result<Vec<Interaction>> {
        let mut interactions = self.kept(|interaction| match filter {
            Filter::All => true,
            Filter::Bad => has_bad(interaction),
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

    async fn embedding(&self, id: u64) -> anyhow::Result<Option<Vec<f32>>> {
        Ok(lock(&self.embeddings).get(&id).cloned())
    }

    async fn mark(&self, mark: Mark) -> anyhow::Result<()> {
        self.change(mark.interaction, |interaction| {
            interaction.marks.clear();
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
                && !has_bad(interaction)
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

/// A `Review` of fixed interactions and people that records each mark whose correction is in
/// `intents`, and each assign of one of its people to one of its interactions.
#[derive(Default)]
pub struct FakeReview {
    /// The people of the house.
    pub people: Vec<Person>,
    /// The interaction ID and the person of each accepted assign, in order.
    pub assigns: Mutex<Vec<(u64, Option<EntityId>)>>,
    /// The interactions with their audio, newest first.
    pub interactions: Vec<(Interaction, Option<Audio>)>,
    /// The names that a correction can have.
    pub intents: Vec<String>,
    /// Each accepted mark, in order.
    pub marks: Mutex<Vec<Mark>>,
    /// The learned intents file.
    pub learned: Mutex<Learned>,
}

#[async_trait]
impl Review for FakeReview {
    async fn list(&self, _filter: Filter, _page: Page) -> anyhow::Result<Vec<Interaction>> {
        let interactions = self.interactions.iter();
        Ok(interactions
            .map(|(interaction, _)| interaction.clone())
            .collect())
    }

    async fn audio(&self, id: u64) -> anyhow::Result<Option<Audio>> {
        let mut interactions = self.interactions.iter();
        let found = interactions.find(|(interaction, _)| interaction.id == id);
        Ok(found.and_then(|(_, audio)| audio.clone()))
    }

    async fn mark(&self, mark: Mark) -> anyhow::Result<()> {
        review::check(&mark, &self.intents)?;
        lock(&self.marks).push(mark);
        Ok(())
    }

    fn intents(&self) -> Vec<String> {
        self.intents.clone()
    }

    fn proposals(&self) -> anyhow::Result<Vec<LearnedIntent>> {
        Ok(review::pending(lock(&self.learned).clone()))
    }

    async fn resolve(&self, name: &str, approve: bool) -> Result<(), Unresolved> {
        review::resolve(&mut lock(&self.learned), name, approve)
    }

    fn people(&self) -> Vec<Person> {
        self.people.clone()
    }

    async fn assign(&self, id: u64, person: Option<EntityId>) -> Result<(), Unassigned> {
        review::check_person(&self.people, person.as_ref())?;
        let mut interactions = self.interactions.iter();
        if !interactions.any(|(interaction, _)| interaction.id == id) {
            return Err(Unassigned::NotFound);
        }
        lock(&self.assigns).push((id, person));
        Ok(())
    }
}

/// The learned command entry `name` with `status`, the service `scene.turn_on`, one example and
/// the centroid `[1, 0]`.
///
/// # Panics
///
/// The entry does not parse, which is a bug of the fake.
#[must_use]
pub fn learned(name: &str, status: LearnedStatus) -> LearnedIntent {
    serde_json::from_value(json!({
        "name": name, "centroid": [1.0, 0.0], "service": "scene.turn_on", "kind": "command",
        "examples": ["good night"], "responses": ["Good night."], "select": [], "data": {},
        "status": status, "texts": ["good night jarvis"],
    }))
    .expect("the entry is a learned intent")
}

/// A `Control` that gives a set status, has no settings and gives a set answer to each save.
pub struct FakeControl {
    /// The status.
    pub status: Status,
    /// The refusal of each save, or `None` to accept it.
    pub refusal: Option<Refusal>,
}

impl Control for FakeControl {
    fn status(&self) -> Status {
        self.status.clone()
    }

    fn settings(&self) -> anyhow::Result<Vec<Setting>> {
        Ok(Vec::new())
    }

    fn save(&self, _: BTreeMap<String, Value>) -> Result<(), Refusal> {
        self.refusal.clone().map_or(Ok(()), Err)
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

/// Starts an in-process MQTT broker on a free local port in its own thread, and gives its
/// address once it accepts connections.
pub(crate) fn mqtt_broker() -> anyhow::Result<SocketAddr> {
    let address = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let config: rumqttd::Config = toml::from_str(&format!(
        "id = 0
        [router]
        max_connections = 10
        max_outgoing_packet_count = 200
        max_segment_size = 1048576
        max_segment_count = 10
        [v4.1]
        name = \"v4\"
        listen = \"{address}\"
        next_connection_delay_ms = 1
        [v4.1.connections]
        connection_timeout_ms = 60000
        max_payload_size = 20480
        max_inflight_count = 100"
    ))?;
    let mut broker = rumqttd::Broker::new(config);
    thread::spawn(move || broker.start().map_err(Box::new));
    while std::net::TcpStream::connect(address).is_err() {
        thread::yield_now();
    }
    Ok(address)
}

/// A message that [`MqttSubscriber`] gets.
#[derive(Debug)]
pub(crate) struct MqttMessage {
    /// The topic.
    pub(crate) topic: String,
    /// The JSON payload.
    pub(crate) payload: Value,
    /// The retain flag.
    pub(crate) retain: bool,
}

/// An MQTT client that subscribes to a topic filter.
pub(crate) struct MqttSubscriber {
    /// Keeps the request channel of `events` open.
    _client: AsyncClient,
    /// The connection.
    events: EventLoop,
}

impl MqttSubscriber {
    /// A client `id` of the broker at `address` that subscribes to `filter`.
    pub(crate) fn new(address: SocketAddr, id: &str, filter: &str) -> anyhow::Result<Self> {
        let options = MqttOptions::new(id, address.ip().to_string(), address.port());
        let (client, events) = AsyncClient::new(options, 10);
        client.try_subscribe(filter, QoS::AtLeastOnce)?;
        Ok(Self {
            _client: client,
            events,
        })
    }

    /// Waits for the next message.
    pub(crate) async fn next(&mut self) -> anyhow::Result<MqttMessage> {
        loop {
            if let rumqttc::Event::Incoming(Packet::Publish(publish)) = self.events.poll().await? {
                return Ok(MqttMessage {
                    topic: publish.topic,
                    payload: serde_json::from_slice(&publish.payload)?,
                    retain: publish.retain,
                });
            }
        }
    }
}
