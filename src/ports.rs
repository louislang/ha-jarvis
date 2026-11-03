//! Port traits and their data types.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::Device;
use crate::domain::model::Prediction;
use crate::domain::speech::Audio;
use crate::domain::state::Event;
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

    /// The reply to the transcript `text` in `context` and the work after it.
    async fn handle(&self, text: &str, context: Context) -> anyhow::Result<Handled>;
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
}

/// The System One model.
#[async_trait]
pub trait Classifier: Send + Sync {
    /// The label probabilities and the slots of the normalized transcript `text` in `context`.
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction>;
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

/// The current time.
pub trait Clock: Send + Sync {
    /// The time now.
    fn now(&self) -> Timestamp;
}
