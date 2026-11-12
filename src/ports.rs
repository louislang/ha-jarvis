//! Port traits and their data types.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;

use crate::Device;
use crate::domain::decide::PluginCall;
use crate::domain::decide::Question;
use crate::domain::feedback::Deferred;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::feedback::Spoken;
use crate::domain::model::Prediction;
use crate::domain::speech::Audio;
use crate::domain::state::Event;
use crate::domain::state::HomeConfig;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;

/// The name and the version that Jarvis gives to HA.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// The program name.
    pub name: String,
    /// The program version.
    pub version: String,
}

/// Background work that the caller starts after it sends the reply.
pub type Work = BoxFuture<'static, anyhow::Result<()>>;

/// The reply to a request and the work after it.
#[must_use]
pub struct Handled {
    /// The text to speak.
    pub text: String,
    /// The background work, if any.
    pub after: Option<Work>,
}

/// The IDs that HA sends with a transcript.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Context {
    /// The HA conversation.
    pub conversation_id: Option<String>,
    /// The HA device of the satellite.
    pub device_id: Option<String>,
    /// The satellite entity.
    pub satellite_id: Option<String>,
}

/// The driving port that the Wyoming server calls.
#[async_trait]
pub trait Assistant: Send + Sync {
    /// The name and the version of the conversation agent.
    fn info(&self) -> Info;

    /// A listen session for the audio of one utterance.
    ///
    /// # Errors
    ///
    /// The session cannot start.
    fn listen(&self) -> anyhow::Result<Box<dyn Listening>>;

    /// The reply to the transcript `text` in `context` and the work after it.
    async fn handle(&self, text: &str, context: Context) -> anyhow::Result<Handled>;

    /// The audio of `text`.
    async fn speak(&self, text: &str) -> anyhow::Result<Arc<Audio>>;
}

/// The house: HA states, registries, events and service calls.
#[async_trait]
pub trait Home: Send + Sync {
    /// Loads the states, the registries, the configuration and the user of the token.
    async fn snapshot(&self) -> anyhow::Result<Snapshot>;

    /// The changes in HA, in order.
    fn events(&self) -> BoxStream<'static, Event>;

    /// Calls a service. Gives the response data when `call.return_response` is true.
    async fn call(&self, call: ServiceCall) -> anyhow::Result<Option<Value>>;

    /// Speaks `text` on the satellite entity `satellite` with no preannounce sound. Returns
    /// after the satellite plays the audio.
    async fn announce(&self, satellite: &str, text: &str) -> anyhow::Result<()>;

    /// Speaks `question` on the satellite entity `satellite`, then listens without the wake word.
    async fn ask(&self, satellite: &str, question: &str) -> anyhow::Result<()>;
}

/// The System One model.
#[async_trait]
pub trait Classifier: Send + Sync {
    /// The label probabilities and the slots of the normalized transcript `text` in `context`.
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction>;
}

/// System Two: the remote model for the requests that System One cannot classify.
#[async_trait]
pub trait Reasoner: Send + Sync {
    /// Sends one small request when the last request is `openai.warm_idle_s` or more ago, so that
    /// the next request does not open a new connection.
    async fn warm(&self) -> anyhow::Result<()>;

    /// The steps of the answer to `question`, each step as soon as it is complete.
    fn answer(&self, question: &Question) -> BoxStream<'static, anyhow::Result<Step>>;

    /// `n` new responses for the pool `pool`, which has the responses `existing` and can use the
    /// template variables `variables`.
    async fn generate(
        &self,
        pool: &str,
        existing: &[String],
        variables: &BTreeSet<String>,
        n: usize,
    ) -> anyhow::Result<Vec<String>>;
}

/// One step of a System Two answer.
#[derive(Debug)]
pub enum Step {
    /// A complete sentence to speak.
    Sentence(String),
    /// A service call to run. The answer continues after the consumer sends the result of the
    /// call on the sender.
    Call(ServiceCall, oneshot::Sender<anyhow::Result<Option<Value>>>),
    /// A read of the entity states of the area with this name or alias. The answer continues
    /// after the consumer sends the states on the sender.
    States(String, oneshot::Sender<anyhow::Result<Option<Value>>>),
    /// A plugin call. The answer continues after the consumer sends the result on the sender.
    Plugin(PluginCall, oneshot::Sender<anyhow::Result<Option<Value>>>),
    /// A question to the user. The answer ends, and the next answer continues it.
    Ask {
        /// The question text.
        question: String,
        /// The ID of the response that asks.
        response_id: String,
        /// The ID of the `ask_user` call.
        call_id: String,
    },
    /// The answer is complete.
    Done,
}

/// Opens System One model folders.
#[async_trait]
pub trait ModelLoader: Send + Sync {
    /// The classifier of the model folder `dir`, run on `device`.
    async fn load(&self, dir: &Path, device: Device) -> anyhow::Result<Arc<dyn Classifier>>;
}

/// The first-pass result of the audio so far, with audio positions in milliseconds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Partial {
    /// The decoded text.
    pub text: String,
    /// The end of the last voiced frame, or `None` before any speech.
    pub voice_end_ms: Option<u64>,
    /// The end of the audio that the VAD has decided.
    pub vad_ms: u64,
}

/// Speech to text.
pub trait SpeechToText: Send + Sync {
    /// A new first pass of 16 kHz audio.
    ///
    /// # Errors
    ///
    /// The VAD cannot load.
    fn stream(&self) -> anyhow::Result<Box<dyn SttStream>>;

    /// The second-pass text of the 16 kHz `samples` of one utterance, or `None` without a second
    /// pass. It blocks while it decodes.
    ///
    /// # Errors
    ///
    /// The second pass gives no result.
    fn transcribe(&self, samples: &[i16]) -> anyhow::Result<Option<String>>;
}

/// The first pass of one utterance. `accept` blocks while it decodes.
pub trait SttStream: Send {
    /// Decodes the next `samples`.
    fn accept(&mut self, samples: &[i16]);

    /// The result of the audio so far.
    fn partial(&self) -> Partial;

    /// Drops the audio so far.
    fn reset(&mut self);
}

/// One listen session: the audio of one utterance, chunk by chunk.
#[async_trait]
pub trait Listening: Send {
    /// Accepts the 16 kHz mono `samples` of one audio chunk. Gives the transcript at a commit.
    async fn push(&mut self, samples: Vec<i16>) -> anyhow::Result<Option<String>>;

    /// The transcript of the audio so far.
    async fn finish(self: Box<Self>) -> anyhow::Result<String>;
}

/// Text to speech.
#[async_trait]
pub trait TextToSpeech: Send + Sync {
    /// The audio of `text`. Empty text gives no samples.
    async fn synth(&self, text: &str) -> anyhow::Result<Audio>;
}

/// A value that survives a restart.
pub trait Store<T>: Send + Sync {
    /// The saved value, or `None` when no value is saved.
    ///
    /// # Errors
    ///
    /// The value cannot be read or parsed.
    fn load(&self) -> anyhow::Result<Option<T>>;

    /// Saves `value`. A stop during the save keeps the last saved value.
    ///
    /// # Errors
    ///
    /// The value cannot be serialized or written.
    fn save(&self, value: &T) -> anyhow::Result<()>;
}

/// A tool that a model can call.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// The name of the tool.
    pub name: String,
    /// What the tool gives, for the model.
    pub description: String,
    /// The JSON schema of the arguments.
    pub parameters: Value,
}

/// A data source for one remote API.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// The tool that calls the plugin.
    fn tool(&self) -> Tool;

    /// The typed result for `arguments` in the house with the HA configuration `home`.
    async fn fetch(
        &self,
        arguments: &Map<String, Value>,
        home: &HomeConfig,
    ) -> anyhow::Result<Value>;
}

/// The current time.
pub trait Clock: Send + Sync {
    /// The time now.
    fn now(&self) -> Timestamp;
}

/// A change to a logged interaction.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Adds a later announce or question to the responses.
    Response(Spoken),
}

/// The interactions that `list` keeps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Filter {
    /// Every interaction.
    All,
    /// The interactions with a bad mark.
    Bad,
}

/// One page of a list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Page {
    /// The page number, from 0.
    pub number: usize,
    /// The number of items on a page.
    pub size: usize,
}

/// The log of the requests with their audio.
#[async_trait]
pub trait InteractionLog: Send + Sync {
    /// Stores `interaction` with its 16 kHz `audio`.
    async fn append(&self, interaction: Interaction, audio: Option<Audio>) -> anyhow::Result<()>;

    /// Applies `change` to the interaction `id`.
    async fn update(&self, id: u64, change: Change) -> anyhow::Result<()>;

    /// The interactions of `page` that `filter` keeps, newest first.
    async fn list(&self, filter: Filter, page: Page) -> anyhow::Result<Vec<Interaction>>;

    /// The interaction `id`, or `None` when it does not exist.
    async fn get(&self, id: u64) -> anyhow::Result<Option<Interaction>>;

    /// The audio of the interaction `id`, or `None` when it has none.
    async fn audio(&self, id: u64) -> anyhow::Result<Option<Audio>>;

    /// Stores `mark` instead of the earlier mark of its interaction and target. A `None` value
    /// removes the earlier mark.
    async fn mark(&self, mark: Mark) -> anyhow::Result<()>;

    /// The interactions with a mark, oldest first.
    async fn marked(&self) -> anyhow::Result<Vec<Interaction>>;

    /// The System Two requests after `since` without a bad action mark, oldest first. A dialog
    /// answer is not a request.
    async fn deferred(&self, since: Timestamp) -> anyhow::Result<Vec<Deferred>>;

    /// Deletes the audio older than `log.audio_days`, and the unmarked records older than
    /// `log.record_days` that no marked record links to.
    async fn prune(&self, now: Timestamp) -> anyhow::Result<()>;
}
