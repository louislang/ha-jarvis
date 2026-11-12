//! Voice-controlled home automation for Home Assistant.

// Verus reads loop invariants from attributes on loops, and the `split_once` contract names a
// `Pattern`.
#![cfg_attr(verus_only, feature(proc_macro_hygiene, pattern))]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use clap::Parser;
use clap::error::ErrorKind;
use jiff::Timestamp;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Deserialize;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::signal::unix;
use tokio::signal::unix::SignalKind;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

use crate::adapters::files::FileStore;
use crate::adapters::ha::HaHome;
use crate::adapters::onnx::OnnxLoader;
use crate::adapters::open_meteo::OpenMeteo;
use crate::adapters::openai::OpenAiReasoner;
use crate::adapters::plex::Plex;
use crate::adapters::sherpa::SherpaStt;
use crate::adapters::sqlite::SqliteLog;
use crate::adapters::supertonic::SupertonicTts;
use crate::app::background::Backfill;
use crate::app::background::Habits;
use crate::app::background::HabitsTask;
use crate::app::conversation::Change;
use crate::app::conversation::Conversation;
use crate::app::conversation::PoolsTask;
use crate::app::conversation::Queues;
use crate::app::conversation::Recorder;
use crate::app::conversation::SystemTwo;
use crate::app::plugins::Plugins;
use crate::app::speech::Listener;
use crate::app::speech::Speech;
use crate::app::speech::Utterances;
use crate::domain::catalog::Catalog;
use crate::domain::habits::RequestHabits;
use crate::domain::model::Prediction;
use crate::domain::responses::Pool;
use crate::domain::responses::Pools;
use crate::domain::responses::Response;
use crate::domain::speech::Audio;
use crate::domain::speech::CommitPolicy;
use crate::domain::state::Snapshot;
use crate::domain::state_cache::StateCache;
use crate::domain::training::Settings;
use crate::domain::training::TrainingSet;
use crate::ports::Assistant;
use crate::ports::Classifier;
use crate::ports::Clock;
use crate::ports::Handled;
use crate::ports::Home;
use crate::ports::Info;
use crate::ports::Listening;
use crate::ports::ModelLoader;
use crate::ports::Plugin;
use crate::ports::Reasoner;
use crate::ports::SpeechToText;
use crate::ports::Store;

/// Pure rules.
pub mod domain {
    pub mod catalog;
    #[cfg(verus_only)]
    pub mod contracts;
    pub mod decide;
    pub mod dialog;
    pub mod feedback;
    pub mod habits;
    pub mod intent;
    pub mod model;
    pub mod responses;
    pub mod select;
    pub mod speech;
    pub mod state;
    pub mod state_cache;
    pub mod training;
}

pub mod ports;

/// Use cases.
pub mod app {
    pub mod background;
    pub mod conversation;
    pub mod plugins;
    pub mod speech;
}

/// Port implementations, one technology each.
pub mod adapters {
    pub mod files;
    pub mod ha;
    pub mod onnx;
    pub mod open_meteo;
    pub mod openai;
    pub mod plex;
    pub mod sherpa;
    pub mod sqlite;
    pub mod supertonic;
    pub mod wyoming;
}

/// Shared test code.
#[cfg(test)]
pub mod testing {
    pub mod e2e;
    pub mod fakes;
}

/// Runtime configuration from `jarvis.toml`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address of the Wyoming server.
    pub listen: SocketAddr,
    /// Where the models run.
    pub device: Device,
    /// The intent catalog file.
    pub intents: PathBuf,
    /// The source response pools file.
    pub responses: PathBuf,
    /// The saved pools file: use counts and generated responses.
    pub pools: PathBuf,
    /// Home Assistant connection.
    pub ha: HaConfig,
    /// System One model.
    pub model: ModelConfig,
    /// Speech to text.
    pub asr: AsrConfig,
    /// Text to speech.
    pub tts: TtsConfig,
    /// System Two.
    pub openai: OpenAiConfig,
    /// Plugins.
    pub plugins: PluginsConfig,
    /// Response backfill.
    pub backfill: BackfillConfig,
    /// Announces.
    pub announce: AnnounceConfig,
    /// Dialogs.
    pub dialog: DialogConfig,
    /// Request habits.
    pub habits: HabitsConfig,
    /// Training.
    pub train: TrainConfig,
    /// The interaction log.
    pub log: LogConfig,
}

impl Config {
    /// Parses the text of a configuration file.
    ///
    /// # Errors
    ///
    /// The text is not TOML, a field is unknown, absent or of the wrong type, or
    /// [`Config::validate`] fails.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Checks [`Config::validate_positive`], the range of each other field, then
    /// [`Config::validate_relations`].
    ///
    /// # Errors
    ///
    /// A field is out of its range, or a pair of fields breaks its relation. The error names the
    /// field and the range, or both fields.
    fn validate(&self) -> anyhow::Result<()> {
        self.validate_positive()?;
        let probabilities = [
            ("model.threshold", self.model.threshold),
            ("model.commit_threshold", self.model.commit_threshold),
        ];
        for (name, value) in probabilities {
            anyhow::ensure!(
                (0.0..=1.0).contains(&value),
                "{name} = {value} is out of range: it must be in [0, 1]"
            );
        }
        let positive = [
            ("asr.vad_threshold", f64::from(self.asr.vad_threshold)),
            ("habits.half_life_days", self.habits.half_life_days),
            ("habits.min_score", self.habits.min_score),
        ];
        for (name, value) in positive {
            anyhow::ensure!(
                value > 0.0,
                "{name} = {value} is out of range: it must be more than 0"
            );
        }
        let fractions = [
            ("train.holdout", self.train.holdout),
            ("dialog.choice_similarity", self.dialog.choice_similarity),
        ];
        for (name, value) in fractions {
            anyhow::ensure!(
                (0.0..=1.0).contains(&value),
                "{name} = {value} is out of range: it must be in [0, 1]"
            );
        }
        self.validate_relations()
    }

    /// Checks that each timeout, interval, count and limit is more than 0.
    ///
    /// # Errors
    ///
    /// A field is 0. The error names the field and the range.
    fn validate_positive(&self) -> anyhow::Result<()> {
        let positive = self.durations().into_iter().chain(self.counts());
        for (name, value) in positive {
            anyhow::ensure!(
                value > 0,
                "{name} = {value} is out of range: it must be more than 0"
            );
        }
        Ok(())
    }

    /// The name and the value of each timeout and interval.
    fn durations(&self) -> [(&'static str, u64); 15] {
        let (ha, asr, openai) = (&self.ha, &self.asr, &self.openai);
        [
            ("ha.request_timeout_ms", ha.request_timeout_ms),
            ("ha.announce_timeout_s", ha.announce_timeout_s),
            ("ha.ping_interval_s", ha.ping_interval_s),
            ("asr.vad_min_silence_ms", asr.vad_min_silence_ms),
            ("asr.early_commit_ms", asr.early_commit_ms),
            ("asr.endpoint_ms", asr.endpoint_ms),
            ("asr.max_utterance_ms", asr.max_utterance_ms),
            ("openai.timeout_ms", openai.timeout_ms),
            ("openai.warm_idle_s", openai.warm_idle_s),
            ("plugins.cache_s", self.plugins.cache_s),
            ("plugins.timeout_ms", self.plugins.timeout_ms),
            ("announce.margin_ms", self.announce.margin_ms),
            ("announce.idle_wait_s", self.announce.idle_wait_s),
            ("dialog.timeout_s", self.dialog.timeout_s),
            ("habits.save_interval_s", self.habits.save_interval_s),
        ]
    }

    /// The name and the value of each count and limit.
    fn counts(&self) -> [(&'static str, u64); 11] {
        let (openai, backfill, train) = (&self.openai, &self.backfill, &self.train);
        [
            ("tts.num_steps", u64::from(self.tts.num_steps)),
            (
                "openai.max_output_tokens",
                u64::from(openai.max_output_tokens),
            ),
            (
                "openai.bulk_max_output_tokens",
                u64::from(openai.bulk_max_output_tokens),
            ),
            ("openai.max_tool_calls", u64::from(openai.max_tool_calls)),
            ("backfill.min_fresh", backfill.min_fresh as u64),
            ("backfill.batch", backfill.batch as u64),
            ("backfill.max_chars", backfill.max_chars as u64),
            ("backfill.max_pool", backfill.max_pool as u64),
            ("dialog.max_turns", u64::from(self.dialog.max_turns)),
            ("train.epochs", train.epochs as u64),
            ("train.batch_size", train.batch_size as u64),
        ]
    }

    /// Checks the relation of each pair of fields.
    ///
    /// # Errors
    ///
    /// A pair of fields breaks its relation. The error names both fields.
    fn validate_relations(&self) -> anyhow::Result<()> {
        let asr = &self.asr;
        let ordered = [
            (
                "asr.vad_min_silence_ms",
                asr.vad_min_silence_ms,
                "asr.early_commit_ms",
                asr.early_commit_ms,
            ),
            (
                "asr.early_commit_ms",
                asr.early_commit_ms,
                "asr.endpoint_ms",
                asr.endpoint_ms,
            ),
            (
                "asr.endpoint_ms",
                asr.endpoint_ms,
                "asr.max_utterance_ms",
                asr.max_utterance_ms,
            ),
        ];
        for (lower, low, upper, high) in ordered {
            anyhow::ensure!(
                high > low,
                "{upper} = {high} must be more than {lower} = {low}"
            );
        }
        let (threshold, commit_threshold) = (self.model.threshold, self.model.commit_threshold);
        anyhow::ensure!(
            threshold > 0.5,
            "model.threshold = {threshold} must be more than 0.5"
        );
        anyhow::ensure!(
            commit_threshold >= threshold,
            "model.commit_threshold = {commit_threshold} must be at least model.threshold = {threshold}"
        );
        Ok(())
    }

    /// Reads and parses the configuration file at `path`.
    ///
    /// # Errors
    ///
    /// The file cannot be read, or [`Config::parse`] fails.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Self::parse(&read(path)?)
    }
}

/// Where the models run.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    /// NVIDIA GPU.
    Cuda,
    /// CPU only.
    Cpu,
}

/// The `[ha]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HaConfig {
    /// URL of the HA WebSocket API.
    pub url: String,
    /// Environment variable that holds the HA token.
    pub token_env: String,
    /// Deadline of the authentication and of each request except an announce.
    pub request_timeout_ms: u64,
    /// Deadline of an announce. HA answers after the satellite plays the audio.
    pub announce_timeout_s: u64,
    /// Time between two pings.
    pub ping_interval_s: u64,
}

/// The `[model]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// The folder of the System One model folders.
    pub dir: PathBuf,
    /// The least top probability that System One acts on.
    pub threshold: f32,
    /// The least top probability of a confident partial, which commits after `early_commit_ms`.
    pub commit_threshold: f32,
}

/// The `[asr]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AsrConfig {
    /// The folder of the streaming transducer of the first pass.
    pub model_dir: PathBuf,
    /// The file name of the encoder in the model folder.
    pub encoder: String,
    /// The file name of the decoder in the model folder.
    pub decoder: String,
    /// The file name of the joiner in the model folder.
    pub joiner: String,
    /// The folder of the offline transducer of the second pass, or `None` for no second pass.
    pub second_pass_dir: Option<PathBuf>,
    /// The Silero VAD model file.
    pub vad_model: PathBuf,
    /// The least VAD speech probability of a voiced frame.
    pub vad_threshold: f32,
    /// The silence that ends a VAD speech segment.
    pub vad_min_silence_ms: u64,
    /// The silence after a confident partial that commits.
    pub early_commit_ms: u64,
    /// The silence after any partial that commits.
    pub endpoint_ms: u64,
    /// The utterance length that always commits.
    pub max_utterance_ms: u64,
}

/// The `[tts]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TtsConfig {
    /// The folder of the Supertonic-3 model.
    pub model_dir: PathBuf,
    /// The speaker ID of the voice style.
    pub voice: i32,
    /// The number of denoising steps.
    pub num_steps: u32,
}

/// The `[openai]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenAiConfig {
    /// Environment variable that holds the `OpenAI` API key.
    pub api_key_env: String,
    /// The model of each request.
    pub model: String,
    /// The longest wait for each step of an answer.
    pub timeout_ms: u64,
    /// The time since the last request after which `warm` sends a request.
    pub warm_idle_s: u64,
    /// The output token limit of an answer.
    pub max_output_tokens: u32,
    /// The output token limit of a structured output: `generate`, `paraphrase` and `propose`.
    pub bulk_max_output_tokens: u32,
    /// The most tool calls of one answer.
    pub max_tool_calls: u32,
    /// The domains of the services and the targets that a System Two call can have.
    pub allowed_domains: Vec<String>,
    /// The services that a System Two call runs with `return_response`.
    pub read_services: Vec<String>,
}

/// The `[plugins]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginsConfig {
    /// The time that a plugin result stays in the cache.
    pub cache_s: u64,
    /// The longest wait for a plugin result.
    pub timeout_ms: u64,
    /// The weather plugin.
    pub weather: WeatherConfig,
    /// The Plex plugin. Without it, Jarvis has no Plex plugin.
    pub plex: Option<PlexConfig>,
}

/// The `[plugins.weather]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WeatherConfig {
    /// The Open-Meteo forecast API.
    pub url: String,
}

/// The `[plugins.plex]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlexConfig {
    /// The Plex server.
    pub url: String,
    /// The environment variable of the Plex token.
    pub token_env: String,
}

/// The `[backfill]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BackfillConfig {
    /// A pool with fewer unused responses than this gets a backfill.
    pub min_fresh: usize,
    /// The number of candidates that one backfill asks for.
    pub batch: usize,
    /// The most characters of a new response.
    pub max_chars: usize,
    /// The most responses of a pool.
    pub max_pool: usize,
}

/// The `[announce]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AnnounceConfig {
    /// The wait after the satellite becomes idle and before an announce.
    pub margin_ms: u64,
    /// The deadline for the satellite to become idle. After it, the announce is dropped.
    pub idle_wait_s: u64,
}

/// The `[dialog]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DialogConfig {
    /// The most questions of one dialog.
    pub max_turns: u32,
    /// The time after the last question that closes a dialog.
    pub timeout_s: u64,
    /// The lowest Jaro-Winkler similarity of an answer to the choice that it fills.
    pub choice_similarity: f64,
    /// The normalized phrases that cancel a dialog.
    pub cancel: Vec<String>,
}

/// The `[habits]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HabitsConfig {
    /// The saved request habit model.
    pub file: PathBuf,
    /// The days in which a request habit score halves.
    pub half_life_days: f64,
    /// The lowest decayed score of a usual time.
    pub min_score: f64,
    /// The time between two saves of the model.
    pub save_interval_s: u64,
}

/// The `[train]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrainConfig {
    /// The model to fine-tune.
    pub base_model: String,
    /// The number of passes over the examples.
    pub epochs: usize,
    /// The number of examples in one step.
    pub batch_size: usize,
    /// The share of the examples in the held-out split.
    pub holdout: f64,
}

/// The `[log]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    /// The interaction log database file.
    pub database: PathBuf,
    /// The age in days after which `prune` deletes the audio.
    pub audio_days: u64,
    /// The age in days after which `prune` deletes an unmarked record.
    pub record_days: u64,
    /// The time that a committed utterance waits for its request.
    pub correlate_s: u64,
}

/// The run name of the System One model that Jarvis loads.
const MODEL: &str = "fixture";

/// The `Clock` of the system time.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// The `Assistant` of Jarvis. It delegates each request to its use case.
pub struct App {
    /// The `handle` use case.
    conversation: Conversation,
    /// The `listen` use case.
    listener: Listener,
    /// The `speak` use case.
    speech: Arc<Speech>,
}

#[async_trait]
impl Assistant for App {
    fn info(&self) -> Info {
        Info {
            name: env!("CARGO_PKG_NAME").to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    fn listen(&self) -> anyhow::Result<Box<dyn Listening>> {
        self.listener.listen()
    }

    async fn handle(&self, text: &str, context: ports::Context) -> anyhow::Result<Handled> {
        self.conversation.handle(text, context).await
    }

    async fn speak(&self, text: &str) -> anyhow::Result<Arc<Audio>> {
        self.speech.speak(text).await
    }
}

/// The listener of `config` on `stt`, which predicts each partial with `classifier`, commits early
/// for an answer to an open dialog of `queues`, warms `reasoner` and keeps the utterances for
/// `log.correlate_s`.
fn listener(
    config: &Config,
    stt: Arc<dyn SpeechToText>,
    classifier: Arc<dyn Classifier>,
    reasoner: Arc<dyn Reasoner>,
    queues: &Arc<Queues>,
) -> Listener {
    let asr = &config.asr;
    let policy = CommitPolicy {
        early_commit_ms: asr.early_commit_ms,
        endpoint_ms: asr.endpoint_ms,
        max_utterance_ms: asr.max_utterance_ms,
    };
    let threshold = config.model.commit_threshold;
    let (queues, clock) = (Arc::clone(queues), Arc::new(SystemClock));
    let utterances = Arc::new(Utterances::new(
        Arc::clone(&clock) as _,
        config.log.correlate_s,
    ));
    Listener::new(
        stt, classifier, reasoner, policy, threshold, queues, clock, utterances,
    )
}

/// The System One model of the conversation. A swap affects the next `predict`.
pub struct CurrentClassifier(pub ArcSwap<Arc<dyn Classifier>>);

#[async_trait]
impl Classifier for CurrentClassifier {
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction> {
        // One load, so that one prediction uses one classifier.
        let classifier = self.0.load_full();
        classifier.predict(context, text).await
    }
}

/// Secrets from the environment variables that [`Config`] names.
#[derive(Clone)]
pub struct Secrets {
    /// HA long-lived access token.
    pub ha_token: String,
    /// `OpenAI` API key.
    pub openai_key: String,
    /// Plex token, when `[plugins.plex]` is set.
    pub plex_token: Option<String>,
}

impl Secrets {
    /// Reads each secret with `lookup`, which gives the value of an environment variable.
    ///
    /// # Errors
    ///
    /// A variable is not set or is empty. The error names the variable.
    pub fn resolve(
        config: &Config,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Self> {
        let secret = |name: &str| match lookup(name) {
            Some(value) if !value.is_empty() => Ok(value),
            _ => Err(anyhow::anyhow!(
                "environment variable {name} is not set or is empty"
            )),
        };
        let plex = config.plugins.plex.as_ref();
        Ok(Self {
            ha_token: secret(&config.ha.token_env)?,
            openai_key: secret(&config.openai.api_key_env)?,
            plex_token: plex.map(|plex| secret(&plex.token_env)).transpose()?,
        })
    }
}

/// The command line of `jarvis`.
#[derive(Debug, Parser)]
enum Command {
    /// Writes the training set of the seed examples, without the held-out examples.
    Dataset {
        /// The configuration file.
        config: PathBuf,
        /// The training set file to write.
        out: PathBuf,
        /// The snapshot whose areas the contexts name. Without it, no context names an area.
        #[arg(long)]
        house: Option<PathBuf>,
    },
    /// Runs Jarvis: the HA connection, the sync and the Wyoming server.
    Serve {
        /// The configuration file.
        config: PathBuf,
    },
}

impl Command {
    /// Runs the command.
    ///
    /// # Errors
    ///
    /// The command fails.
    fn run(self) -> anyhow::Result<()> {
        match self {
            Self::Dataset { config, out, house } => dataset(&config, &out, house.as_deref()),
            Self::Serve { config } => serve(&config),
        }
    }
}

/// Reads the text file at `path`.
///
/// # Errors
///
/// The file cannot be read. The error names the path.
fn read(path: &Path) -> anyhow::Result<String> {
    fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
}

/// Writes to `out` the seed training set of the configuration file `config`, without the
/// held-out examples. The contexts name the areas of the snapshot file `house`.
///
/// # Errors
///
/// A file cannot be read, parsed or written, or the catalog is not valid.
fn dataset(config: &Path, out: &Path, house: Option<&Path>) -> anyhow::Result<()> {
    let set = seed_examples(config, house)?;
    fs::write(out, serde_json::to_string_pretty(&set)?)
        .with_context(|| format!("cannot write {}", out.display()))
}

/// The seed training set of the configuration file `config`, without the held-out examples.
/// The contexts name the areas of the snapshot file `house`.
///
/// # Errors
///
/// A file cannot be read or parsed, or the catalog is not valid.
fn seed_examples(config: &Path, house: Option<&Path>) -> anyhow::Result<TrainingSet> {
    let config = Config::load(config)?;
    let catalog = load_catalog(&config.intents)?;
    let areas = match house {
        Some(path) => area_names(path)?,
        None => Vec::new(),
    };
    let train = config.train;
    let settings = Settings {
        base_model: train.base_model,
        epochs: train.epochs,
        batch_size: train.batch_size,
    };
    let mut set =
        domain::training::seed_set(&catalog, &areas, &mut StdRng::seed_from_u64(1), settings);
    set.examples
        .retain(|example| !domain::training::held_out(&example.text, train.holdout));
    Ok(set)
}

/// Reads and validates the intent catalog file at `path`.
///
/// # Errors
///
/// The file cannot be read or parsed, or `validate_catalog` fails.
fn load_catalog(path: &Path) -> anyhow::Result<Catalog> {
    let catalog: Catalog = toml::from_str(&read(path)?)?;
    domain::catalog::validate_catalog(&catalog)
        .map_err(|error| anyhow::anyhow!("{} is not valid: {error:?}", path.display()))?;
    Ok(catalog)
}

/// The file `responses.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesFile {
    /// The pools by pool name.
    pools: BTreeMap<String, PoolFile>,
}

/// One pool of `responses.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolFile {
    /// The response templates.
    responses: Vec<String>,
}

/// Reads the pools of the `responses.toml` file at `path`, with no uses, and validates them
/// against `catalog`.
///
/// # Errors
///
/// The file cannot be read or parsed, or `validate_pools` fails.
fn load_pools(path: &Path, catalog: &Catalog) -> anyhow::Result<Pools> {
    let file: ResponsesFile = toml::from_str(&read(path)?)?;
    let mut pools = Pools::new();
    for (name, file) in file.pools {
        let texts = file.responses.into_iter();
        let source = |text| Response {
            text,
            uses: 0,
            generated: false,
        };
        let responses = texts.map(source).collect();
        let last = None;
        pools.insert(name, Pool { responses, last });
    }
    domain::responses::validate_pools(&pools, catalog)
        .map_err(|error| anyhow::anyhow!("{} is not valid: {error:?}", path.display()))?;
    Ok(pools)
}

/// Reads the area names of the snapshot file at `path`.
///
/// # Errors
///
/// The file cannot be read or is not a snapshot.
fn area_names(path: &Path) -> anyhow::Result<Vec<String>> {
    let snapshot: Snapshot = serde_json::from_str(&read(path)?)?;
    Ok(snapshot.areas.into_iter().map(|area| area.name).collect())
}

/// A started app, its pools task, habit task and log writer task, and its pre-render task.
type Started = (
    App,
    (JoinHandle<()>, HabitsTask, JoinHandle<()>),
    JoinHandle<()>,
);

/// The longest wait of [`run`] for the running tasks and the saves after the shutdown future
/// completes.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

/// The outside world of [`run`].
pub struct Startup<Lookup, Connector, Shutdown> {
    /// Gives the value of an environment variable.
    pub lookup: Lookup,
    /// Opens a transport to HA at a host and a port.
    pub connector: Connector,
    /// Completes when Jarvis must stop.
    pub shutdown: Shutdown,
}

/// Starts Jarvis with the configuration file `config` and the outside world `startup`: HA, the
/// sync, the System One model, the STT and TTS models, the pre-render and the Wyoming server.
/// When `startup.shutdown` completes, stops the start, or stops accepting connections and waits
/// up to [`SHUTDOWN_WAIT`] for the running connections, the work after their replies and the
/// saves of the stores.
///
/// # Errors
///
/// A file cannot be read or is not valid, a secret is not set, a model cannot load, or the
/// server cannot bind or accept.
pub async fn run<Lookup, Connector, Transport, Stream>(
    config: &Path,
    startup: Startup<Lookup, Connector, impl Future<Output = ()>>,
) -> anyhow::Result<()>
where
    Lookup: Fn(&str) -> Option<String>,
    Connector: Fn(&str, u16) -> Transport + Send + 'static,
    Transport: Future<Output = io::Result<Stream>> + Send + 'static,
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let config = Config::load(config)?;
    let secrets = Secrets::resolve(&config, startup.lookup)?;
    let catalog = load_catalog(&config.intents)?;
    let pools = load_pools(&config.responses, &catalog)?;
    let home = start_home(&config.ha, &secrets.ha_token, startup.connector)?;
    let mut shutdown = pin::pin!(startup.shutdown);
    // The models can load for a long time, so a shutdown stops the start too.
    let (app, stores, _) = tokio::select! {
        started = start_live_app(&config, &secrets, catalog, pools, home) => started?,
        () = &mut shutdown => return Ok(()),
    };
    serve_until(config.listen, app, stores, shutdown).await
}

/// Serves `app` on `listen` until `shutdown` completes. Then waits up to [`SHUTDOWN_WAIT`] for
/// the connection tasks, the work after their replies, and the pools task and the log writer
/// task of `stores`, and then for the last save of its habit task.
///
/// # Errors
///
/// The server cannot bind or accept, or a task of `stores` panicked.
async fn serve_until(
    listen: SocketAddr,
    app: App,
    stores: (JoinHandle<()>, HabitsTask, JoinHandle<()>),
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let (listener, tracker) = (TcpListener::bind(listen).await?, TaskTracker::new());
    adapters::wyoming::serve(listener, Arc::new(app), &tracker, shutdown).await?;
    tracker.close();
    let (pools_task, habits, writer) = stores;
    // Both tasks run each message. They end when the tracked tasks drop the last sender.
    let drained = async {
        tracker.wait().await;
        pools_task.await.and(writer.await)
    };
    let ended = tokio::time::timeout(SHUTDOWN_WAIT, drained).await;
    // The habits save also when a connection still runs (sub-phase 5.4c).
    drop(habits.stop);
    let saved = habits.task.await;
    let Ok(ended) = ended else {
        tracing::warn!("tasks still run after {SHUTDOWN_WAIT:?}");
        return Ok(saved?);
    };
    Ok(ended.and(saved)?)
}

/// Starts `HaHome` with `token` and `connector` on the host and the port of `ha.url`.
///
/// # Errors
///
/// `ha.url` is not `ws://host:port/...`. Jarvis has no TLS, so a `wss://` URL is an error too.
fn start_home<Connector, Transport, Stream>(
    ha: &HaConfig,
    token: &str,
    connector: Connector,
) -> anyhow::Result<Arc<dyn Home>>
where
    Connector: Fn(&str, u16) -> Transport + Send + 'static,
    Transport: Future<Output = io::Result<Stream>> + Send + 'static,
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let url = &ha.url;
    let authority = url
        .strip_prefix("ws://")
        .and_then(|rest| rest.split('/').next());
    let address = authority.and_then(|authority| authority.rsplit_once(':'));
    // TCP takes an IPv6 host without the brackets of the URL.
    let host_port =
        |(host, port): (&str, &str)| (host.trim_matches(['[', ']']).to_owned(), port.parse());
    let Some((host, Ok(port))) = address.map(host_port) else {
        anyhow::bail!(
            "ha.url = {url} is not valid: Jarvis has no TLS, so it must be ws://host:port/..."
        );
    };
    Ok(HaHome::start(move || connector(&host, port), token, ha))
}

/// The app of `config`, `catalog` and the source pools `pools` on `home`, the plugin registry of
/// `config` and the reasoner of `config` with the tools of that registry and the `OpenAI` API key
/// of `secrets`. Gives the app, its pools task and habit task, and its pre-render task.
///
/// # Errors
///
/// An HTTP client cannot be built, or the saved pools, the habits or a model cannot load.
async fn start_live_app(
    config: &Config,
    secrets: &Secrets,
    catalog: Catalog,
    pools: Pools,
    home: Arc<dyn Home>,
) -> anyhow::Result<Started> {
    let plugins = registry(config, plugins(config, secrets)?);
    let tools = plugins.tools();
    let reasoner = Arc::new(OpenAiReasoner::try_new(
        &config.openai,
        &secrets.openai_key,
        tools,
    )?);
    start_app(config, Arc::new(catalog), pools, home, reasoner, plugins).await
}

/// The plugins of `config`: the Open-Meteo weather plugin, and the Plex plugin with the token of
/// `secrets` when `[plugins.plex]` is set.
///
/// # Errors
///
/// An HTTP client cannot be built.
fn plugins(config: &Config, secrets: &Secrets) -> anyhow::Result<Vec<Arc<dyn Plugin>>> {
    let mut plugins: Vec<Arc<dyn Plugin>> =
        vec![Arc::new(OpenMeteo::try_new(&config.plugins.weather)?)];
    if let (Some(plex), Some(token)) = (&config.plugins.plex, &secrets.plex_token) {
        plugins.push(Arc::new(Plex::try_new(plex, token, Arc::new(SystemClock))?));
    }
    Ok(plugins)
}

/// The registry of `plugins` with the `[plugins]` of `config` and the system time.
fn registry(config: &Config, plugins: Vec<Arc<dyn Plugin>>) -> Arc<Plugins> {
    Arc::new(Plugins::new(
        config.plugins.clone(),
        Arc::new(SystemClock),
        plugins,
    ))
}

/// The app of `config`, `catalog` and the source pools `pools` on `home`, `reasoner` and the
/// plugin registry `plugins`, its pools task and habit task, and its pre-render task. Starts the
/// sync, the pools task and the habit task, and loads the System One model, then the STT and TTS
/// models. Then starts the pre-render.
///
/// # Errors
///
/// The saved pools, the habits or a model cannot load.
async fn start_app(
    config: &Config,
    catalog: Arc<Catalog>,
    pools: Pools,
    home: Arc<dyn Home>,
    reasoner: Arc<dyn Reasoner>,
    plugins: Arc<Plugins>,
) -> anyhow::Result<Started> {
    let (cache, queues) = start_sync(home, config);
    let (task, habits) = start_stores(config, pools, &cache)?;
    let (classifier, listener, speech) = load_models(config, reasoner.clone(), &queues).await?;
    let prerender = start_prerender(&speech, &task.pools, &cache);
    let (recorder, writer) = recorder(config, &listener)?;
    let app = App {
        conversation: Conversation::new(
            task.pools,
            backfill(config, &reasoner, task.changes, &speech, &catalog, cache),
            queues,
            classifier,
            catalog,
            config.model.threshold,
            system_two(config, reasoner, plugins),
            habits.habits.clone(),
            recorder,
        ),
        listener,
        speech,
    };
    Ok((app, (task.task, habits, writer), prerender))
}

/// The recorder of the utterances of `listener` and of the log of `config`, and its new log
/// writer task.
///
/// # Errors
///
/// The log cannot open.
fn recorder(config: &Config, listener: &Listener) -> anyhow::Result<(Recorder, JoinHandle<()>)> {
    let log = SqliteLog::open(&config.log)?;
    let (writer, task) = app::conversation::start_writer(Arc::new(log));
    let recorder = Recorder {
        utterances: Arc::clone(listener.utterances()),
        writer,
        model: MODEL.to_owned(),
    };
    Ok((recorder, task))
}

/// Starts the pools task of the source pools `pools` and the saved pools of `config`, and the
/// habit task of the saved habits of `config`, or of empty habits without the habit file, in the
/// HA time zone of `cache` with the system time.
///
/// # Errors
///
/// The saved pools or the habit file cannot load.
fn start_stores(
    config: &Config,
    pools: Pools,
    cache: &watch::Receiver<StateCache>,
) -> anyhow::Result<(PoolsTask, HabitsTask)> {
    let pools_store = Arc::new(FileStore::try_new(config.pools.clone())?);
    let task = app::conversation::start_pools(pools, pools_store)?;
    let store = Arc::new(FileStore::try_new(config.habits.file.clone())?);
    let saved: Option<RequestHabits> = store.load()?;
    let half_life_days = config.habits.half_life_days;
    let habits = Habits::new(saved.unwrap_or_default(), half_life_days, cache.clone());
    let interval = Duration::from_secs(config.habits.save_interval_s);
    let clock = Arc::new(SystemClock);
    let habits = app::background::start_habits(Arc::new(habits), store, clock, interval);
    Ok((task, habits))
}

/// The backfill of `config` with `reasoner`, the pools task of `changes`, the pre-render of
/// `speech`, `catalog` and the hot cache `cache`.
fn backfill(
    config: &Config,
    reasoner: &Arc<dyn Reasoner>,
    changes: mpsc::Sender<Change>,
    speech: &Arc<Speech>,
    catalog: &Arc<Catalog>,
    cache: watch::Receiver<StateCache>,
) -> Arc<Backfill> {
    let (reasoner, speech) = (Arc::clone(reasoner), Arc::clone(speech));
    let (backfill, catalog) = (config.backfill.clone(), Arc::clone(catalog));
    Arc::new(Backfill::new(
        backfill, reasoner, changes, speech, catalog, cache,
    ))
}

/// System Two on `reasoner`, the plugin registry `plugins` and the system time with the timeout
/// and the call rules of `config.openai`.
fn system_two(config: &Config, reasoner: Arc<dyn Reasoner>, plugins: Arc<Plugins>) -> SystemTwo {
    let openai = &config.openai;
    SystemTwo {
        reasoner,
        plugins,
        clock: Arc::new(SystemClock),
        timeout: Duration::from_millis(openai.timeout_ms),
        allowed_domains: openai.allowed_domains.clone(),
        read_services: openai.read_services.clone(),
    }
}

/// The System One model of `config`, then the listener on the STT models of `config`, which
/// predicts with that model, sees the dialogs of `queues` and warms `reasoner`, and the speech use
/// case on the TTS model of `config`. The STT and TTS models load on a blocking thread.
///
/// # Errors
///
/// A model cannot load.
async fn load_models(
    config: &Config,
    reasoner: Arc<dyn Reasoner>,
    queues: &Arc<Queues>,
) -> anyhow::Result<(Arc<dyn Classifier>, Listener, Arc<Speech>)> {
    let dir = config.model.dir.join(MODEL);
    let model = OnnxLoader.load(&dir, config.device).await?;
    let classifier: Arc<dyn Classifier> = Arc::new(CurrentClassifier(ArcSwap::from_pointee(model)));
    let (asr, tts, device) = (config.asr.clone(), config.tts.clone(), config.device);
    let stt = tokio::task::spawn_blocking(move || SherpaStt::try_new(&asr, device)).await??;
    let tts = tokio::task::spawn_blocking(move || SupertonicTts::try_new(&tts, device)).await??;
    let stt: Arc<dyn SpeechToText> = Arc::new(stt);
    let listener = listener(config, stt, Arc::clone(&classifier), reasoner, queues);
    Ok((classifier, listener, Arc::new(Speech::new(Arc::new(tts)))))
}

/// Starts the sync of a hot cache with `home`. Gives the hot cache and the announce queues of
/// `config` on it.
fn start_sync(home: Arc<dyn Home>, config: &Config) -> (watch::Receiver<StateCache>, Arc<Queues>) {
    let (cache, receiver) = watch::channel(StateCache::default());
    let sync = Arc::clone(&home);
    tokio::spawn(async move { app::background::sync(sync.as_ref(), &cache).await });
    let margin = Duration::from_millis(config.announce.margin_ms);
    let idle_wait = Duration::from_secs(config.announce.idle_wait_s);
    let dialog = config.dialog.clone();
    let queues = Queues::new(home, receiver.clone(), margin, idle_wait, dialog);
    (receiver, Arc::new(queues))
}

/// Starts a task that pre-renders `pools` with `speech` for the area names of the first snapshot
/// in `cache`, and gives its handle.
fn start_prerender(
    speech: &Arc<Speech>,
    pools: &Arc<ArcSwap<Pools>>,
    cache: &watch::Receiver<StateCache>,
) -> JoinHandle<()> {
    let (speech, pools, mut cache) = (Arc::clone(speech), Arc::clone(pools), cache.clone());
    tokio::spawn(async move {
        if cache.changed().await.is_err() {
            return;
        }
        let areas = app::background::area_names(&cache.borrow());
        if let Err(error) = speech.prerender(&pools.load_full(), &areas).await {
            tracing::warn!("the pre-render failed: {error:#}");
        }
    })
}

/// Runs Jarvis with the configuration file `config`, the environment and TCP, until it fails
/// or gets SIGINT or SIGTERM.
///
/// # Errors
///
/// A signal handler cannot be registered, or `run` fails.
fn serve(config: &Path) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async {
        let mut interrupt = unix::signal(SignalKind::interrupt())?;
        let mut terminate = unix::signal(SignalKind::terminate())?;
        let startup = Startup {
            lookup: |name: &str| env::var(name).ok(),
            connector: |host: &str, port: u16| TcpStream::connect((host.to_owned(), port)),
            shutdown: async move {
                tokio::select! {
                    _ = interrupt.recv() => {}
                    _ = terminate.recv() => {}
                }
            },
        };
        run(config, startup).await
    });
    // `run` waited up to 5 s. A drop of the runtime would wait for the blocking tasks with no
    // limit, and the pools task blocks until its last sender drops.
    runtime.shutdown_background();
    result
}

/// Entry point of Jarvis: installs the stderr log subscriber of `RUST_LOG` and runs the command
/// of the command line.
#[must_use]
pub fn main() -> ExitCode {
    let filter = env::var(EnvFilter::DEFAULT_ENV).ok();
    // Only the tests call `main` a second time in one process.
    let _ = tracing::subscriber::set_global_default(subscriber(filter.as_deref(), io::stderr));
    start(env::args_os())
}

/// The log subscriber that writes to `writer` the events that `filter` enables, else the `info`
/// events, with no ANSI colors and no time, because the journal adds the time.
fn subscriber<Writer>(filter: Option<&str>, writer: Writer) -> impl Subscriber + Send + Sync
where
    Writer: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter.unwrap_or("info")))
        .with_ansi(false)
        .without_time()
        .with_writer(writer)
        .finish()
}

/// Runs the command of the command line `args`. Without a command, prints the usage.
fn start(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let command = match Command::try_parse_from(args) {
        Ok(command) => command,
        Err(error) => {
            // `--help` is an error of `try_parse` too, and prints to stdout.
            let _ = error.print();
            let usage = error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand;
            return if usage || !error.use_stderr() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
    };
    match command.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Tests of the entry point and the configuration.
#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::future;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU16;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::Instant;

    use serde_json::json;
    use tokio::io::DuplexStream;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;
    use tokio::sync::watch;

    use crate::adapters::onnx::OnnxLoader;
    use crate::app::conversation::Queues;
    use crate::domain::habits::Bucket;
    use crate::domain::habits::DayType;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Pools;
    use crate::domain::responses::Response;
    use crate::domain::state_cache::StateCache;
    use crate::domain::training;
    use crate::ports::ModelLoader;
    use crate::ports::Partial;
    use crate::ports::TextToSpeech;
    use crate::testing::fakes;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::FakeStt;
    use crate::testing::fakes::FakeTts;
    use crate::testing::fakes::FakeWyomingClient;

    /// A valid configuration with every field.
    const VALID: &str = include_str!("../jarvis.toml");

    /// Returns the text of the error that `Config::parse` gives for `text`.
    fn parse_error(text: &str) -> String {
        Config::parse(text)
            .expect_err("the text is not a valid configuration")
            .to_string()
    }

    /// AC-0.1-2.
    #[test]
    fn ac_0_1_2_main_returns() {
        let _ = super::main();
    }

    /// AC-6.11-1.
    #[test]
    fn ac_6_11_1_without_rust_log_info_is_logged_and_debug_is_not() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        tracing::subscriber::with_default(subscriber(None, Arc::new(log.reopen()?)), || {
            tracing::info!("shown");
            tracing::debug!("hidden");
        });
        let text = fs::read_to_string(log.path())?;
        assert!(
            text.contains("shown") && !text.contains("hidden"),
            "the log is {text:?}"
        );
        Ok(())
    }

    /// The `[asr]` section of `jarvis.toml`.
    fn expected_asr() -> AsrConfig {
        AsrConfig {
            model_dir: PathBuf::from(
                "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
            ),
            encoder: "encoder.fp16.onnx".to_owned(),
            decoder: "decoder.fp16.onnx".to_owned(),
            joiner: "joiner.fp16.onnx".to_owned(),
            second_pass_dir: Some(PathBuf::from(
                "models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming",
            )),
            vad_model: PathBuf::from("models/silero_vad.onnx"),
            vad_threshold: 0.65,
            vad_min_silence_ms: 60,
            early_commit_ms: 100,
            endpoint_ms: 600,
            max_utterance_ms: 8000,
        }
    }

    /// The `[openai]` section of `jarvis.toml`.
    fn expected_openai() -> OpenAiConfig {
        OpenAiConfig {
            api_key_env: "OPENAI_API_KEY".to_owned(),
            model: "gpt-5.4-mini".to_owned(),
            timeout_ms: 8000,
            warm_idle_s: 60,
            max_output_tokens: 150,
            bulk_max_output_tokens: 8000,
            max_tool_calls: 5,
            allowed_domains: [
                "light",
                "switch",
                "fan",
                "media_player",
                "climate",
                "calendar",
                "todo",
            ]
            .map(str::to_owned)
            .to_vec(),
            read_services: vec![
                "calendar.get_events".to_owned(),
                "todo.get_items".to_owned(),
            ],
        }
    }

    /// The `[plugins]` section of `jarvis.toml`.
    fn expected_plugins() -> PluginsConfig {
        PluginsConfig {
            cache_s: 300,
            timeout_ms: 5000,
            weather: WeatherConfig {
                url: "https://api.open-meteo.com/v1/forecast".to_owned(),
            },
            plex: Some(PlexConfig {
                url: "http://plex.local:32400".to_owned(),
                token_env: "PLEX_TOKEN".to_owned(),
            }),
        }
    }

    /// The `[dialog]` section of `jarvis.toml`.
    fn expected_dialog() -> DialogConfig {
        DialogConfig {
            max_turns: 3,
            timeout_s: 15,
            choice_similarity: 0.85,
            cancel: ["never mind", "cancel", "forget it", "stop"]
                .map(str::to_owned)
                .to_vec(),
        }
    }

    /// The `[habits]` section of `jarvis.toml`.
    fn expected_habits() -> HabitsConfig {
        HabitsConfig {
            file: PathBuf::from("habits.json"),
            half_life_days: 14.0,
            min_score: 2.0,
            save_interval_s: 300,
        }
    }

    /// The `[log]` section of `jarvis.toml`.
    fn expected_log() -> LogConfig {
        LogConfig {
            database: PathBuf::from("jarvis.db"),
            audio_days: 30,
            record_days: 90,
            correlate_s: 10,
        }
    }

    /// AC-1.1-1.
    #[test]
    fn ac_1_1_1_parse_returns_each_value() -> anyhow::Result<()> {
        let expected = Config {
            listen: SocketAddr::from(([0, 0, 0, 0], 10700)),
            device: Device::Cuda,
            intents: PathBuf::from("intents.toml"),
            responses: PathBuf::from("responses.toml"),
            pools: PathBuf::from("pools.json"),
            ha: HaConfig {
                url: "ws://homeassistant.local:8123/api/websocket".to_owned(),
                token_env: "HA_TOKEN".to_owned(),
                request_timeout_ms: 30000,
                announce_timeout_s: 60,
                ping_interval_s: 30,
            },
            model: ModelConfig {
                dir: PathBuf::from("models/system-one"),
                threshold: 0.6,
                commit_threshold: 0.9,
            },
            asr: expected_asr(),
            tts: TtsConfig {
                model_dir: PathBuf::from("models/sherpa-onnx-supertonic-3-tts-int8-2026-05-11"),
                voice: 6,
                num_steps: 4,
            },
            openai: expected_openai(),
            plugins: expected_plugins(),
            backfill: BackfillConfig {
                min_fresh: 3,
                batch: 5,
                max_chars: 80,
                max_pool: 30,
            },
            announce: AnnounceConfig {
                margin_ms: 300,
                idle_wait_s: 30,
            },
            dialog: expected_dialog(),
            habits: expected_habits(),
            train: TrainConfig {
                base_model: "sentence-transformers/all-MiniLM-L12-v2".to_owned(),
                epochs: 8,
                batch_size: 8,
                holdout: 0.1,
            },
            log: expected_log(),
        };
        assert_eq!(Config::parse(VALID)?, expected);
        Ok(())
    }

    /// AC-1.1-2.
    #[test]
    fn ac_1_1_2_load_error_contains_path() {
        let error = Config::load(Path::new("missing/jarvis.toml"))
            .expect_err("the file does not exist")
            .to_string();
        assert!(
            error.contains("missing/jarvis.toml"),
            "error has no path: {error}"
        );
    }

    /// AC-1.1-3.
    #[test]
    fn ac_1_1_3_wrong_type_names_field() {
        let error = parse_error(&VALID.replace("token_env = \"HA_TOKEN\"", "token_env = 1"));
        assert!(error.contains("token_env"), "error has no field: {error}");
    }

    /// AC-1.1-4.
    #[test]
    fn ac_1_1_4_missing_field_names_field() {
        let error = parse_error(&VALID.replace("token_env = \"HA_TOKEN\"", ""));
        assert!(error.contains("token_env"), "error has no field: {error}");
    }

    /// AC-1.1-5.
    #[test]
    fn ac_1_1_5_unknown_field_names_field() {
        let error = parse_error(&format!("colour = 1\n{VALID}"));
        assert!(error.contains("colour"), "error has no field: {error}");
    }

    /// AC-1.1-6.
    #[test]
    fn ac_1_1_6_unknown_device_names_device() {
        let error = parse_error(&VALID.replace("\"cuda\"", "\"gpu\""));
        assert!(error.contains("device"), "error has no field: {error}");
    }

    /// AC-2.2-6.
    #[test]
    fn ac_2_2_6_out_of_range_names_field_and_range() {
        let cases = [
            (
                "request_timeout_ms = 30000",
                "request_timeout_ms = 0",
                "ha.request_timeout_ms",
            ),
            (
                "vad_threshold = 0.65",
                "vad_threshold = 0.0",
                "asr.vad_threshold",
            ),
            ("min_score = 2.0", "min_score = 0.0", "habits.min_score"),
        ];
        for (field, broken, name) in cases {
            let error = parse_error(&VALID.replace(field, broken));
            assert!(
                error.contains(name) && error.contains("more than 0"),
                "error for {broken} has no field or range: {error}"
            );
        }
    }

    /// AC-2.2-7.
    #[test]
    fn ac_2_2_7_zero_field_names_field_and_range() {
        let fields = [
            ("tts.num_steps", 4),
            ("openai.timeout_ms", 8000),
            ("openai.warm_idle_s", 60),
            ("openai.max_output_tokens", 150),
            ("openai.bulk_max_output_tokens", 8000),
            ("openai.max_tool_calls", 5),
            ("plugins.cache_s", 300),
            ("plugins.timeout_ms", 5000),
            ("backfill.min_fresh", 3),
            ("backfill.batch", 5),
            ("backfill.max_chars", 80),
            ("backfill.max_pool", 30),
            ("announce.margin_ms", 300),
            ("announce.idle_wait_s", 30),
            ("habits.half_life_days", 14),
            ("habits.save_interval_s", 300),
            ("train.epochs", 8),
            ("train.batch_size", 8),
        ];
        for (name, value) in fields {
            let (_, key) = name.split_once('.').expect("the name has a section");
            let text = VALID.replace(&format!("{key} = {value}"), &format!("{key} = 0"));
            let error = parse_error(&text);
            assert!(
                error.contains(name) && error.contains("more than 0"),
                "error for {name} = 0 has no field or range: {error}"
            );
        }
    }

    /// AC-10.2-7.
    #[test]
    fn ac_10_2_7_zero_dialog_limit_names_field_and_range() {
        for (field, value) in [("max_turns", 3), ("timeout_s", 15)] {
            let text = VALID.replace(&format!("{field} = {value}"), &format!("{field} = 0"));
            let error = parse_error(&text);
            assert!(
                error.contains(&format!("dialog.{field}")) && error.contains("more than 0"),
                "error for dialog.{field} = 0 has no field or range: {error}"
            );
        }
    }

    /// `parse` rejects a `dialog.choice_similarity` outside `[0, 1]` and names the field.
    #[test]
    fn choice_similarity_out_of_range_names_field_and_range() {
        for similarity in ["-0.1", "1.1", "nan"] {
            let text = VALID.replace(
                "choice_similarity = 0.85",
                &format!("choice_similarity = {similarity}"),
            );
            let error = parse_error(&text);
            assert!(
                error.contains("dialog.choice_similarity") && error.contains("[0, 1]"),
                "error for {similarity} has no field or range: {error}"
            );
        }
    }

    /// AC-4.5-13.
    #[test]
    fn ac_4_5_13_threshold_out_of_range_names_field_and_range() {
        for threshold in ["-0.1", "1.1"] {
            let text = VALID.replace("threshold = 0.6", &format!("threshold = {threshold}"));
            let error = parse_error(&text);
            assert!(
                error.contains("model.threshold") && error.contains("[0, 1]"),
                "error for {threshold} has no field or range: {error}"
            );
        }
    }

    /// AC-6.5-6.
    #[test]
    fn ac_6_5_6_broken_relation_names_both_fields() {
        let cases = [
            (
                "endpoint_ms = 600",
                "endpoint_ms = 100",
                ["asr.endpoint_ms", "asr.early_commit_ms"],
            ),
            (
                "commit_threshold = 0.9",
                "commit_threshold = 0.5",
                ["model.commit_threshold", "model.threshold"],
            ),
            (
                "vad_min_silence_ms = 60",
                "vad_min_silence_ms = 200",
                ["asr.early_commit_ms", "asr.vad_min_silence_ms"],
            ),
            (
                "max_utterance_ms = 8000",
                "max_utterance_ms = 600",
                ["asr.max_utterance_ms", "asr.endpoint_ms"],
            ),
        ];
        for (field, broken, names) in cases {
            let error = parse_error(&VALID.replace(field, broken));
            assert!(
                names.iter().all(|name| error.contains(name)),
                "error for {broken} does not name {names:?}: {error}"
            );
        }
    }

    /// AC-9.1b-3.
    #[test]
    fn ac_9_1b_3_jarvis_toml_sets_the_commit_silences() -> anyhow::Result<()> {
        let asr = Config::parse(VALID)?.asr;
        assert_eq!((asr.early_commit_ms, asr.vad_min_silence_ms), (100, 60));
        Ok(())
    }

    /// AC-18.7-6.
    #[test]
    fn ac_18_7_6_jarvis_toml_waits_30_s_for_an_ha_call() -> anyhow::Result<()> {
        assert_eq!(Config::parse(VALID)?.ha.request_timeout_ms, 30000);
        Ok(())
    }

    /// `model.threshold` must be more than 0.5 (sub-phase 2.2).
    #[test]
    fn fix_threshold_of_half_or_less_is_an_error() {
        for threshold in ["0.5", "0.3"] {
            let text = VALID.replace("threshold = 0.6", &format!("threshold = {threshold}"));
            let error = parse_error(&text);
            assert!(
                error.contains("model.threshold") && error.contains("0.5"),
                "error for {threshold} has no field or bound: {error}"
            );
        }
    }

    /// Resolves the secrets of `VALID` from an environment with `variables`.
    fn resolve(variables: &[(&str, &str)]) -> anyhow::Result<Secrets> {
        let environment: BTreeMap<&str, &str> = variables.iter().copied().collect();
        Secrets::resolve(&Config::parse(VALID)?, |name| {
            environment.get(name).map(|value| (*value).to_owned())
        })
    }

    /// Returns the text of the error that `resolve` gives for `variables`.
    fn resolve_error(variables: &[(&str, &str)]) -> String {
        resolve(variables)
            .err()
            .expect("the environment has no valid secrets")
            .to_string()
    }

    /// AC-1.2-1.
    #[test]
    fn ac_1_2_1_resolve_returns_ha_token() -> anyhow::Result<()> {
        let secrets = resolve(&[
            ("HA_TOKEN", "abc"),
            ("OPENAI_API_KEY", "k"),
            ("PLEX_TOKEN", "p"),
        ])?;
        assert_eq!(secrets.ha_token, "abc");
        Ok(())
    }

    /// AC-1.2-2.
    #[test]
    fn ac_1_2_2_unset_token_names_variable() {
        let error = resolve_error(&[]);
        assert!(error.contains("HA_TOKEN"), "error has no variable: {error}");
    }

    /// AC-1.2-3.
    #[test]
    fn ac_1_2_3_empty_token_names_variable() {
        let error = resolve_error(&[("HA_TOKEN", "")]);
        assert!(error.contains("HA_TOKEN"), "error has no variable: {error}");
    }

    /// Runs `jarvis dataset jarvis.toml <out> <options>` and returns the text of `<out>`.
    fn dataset(options: &[&str]) -> anyhow::Result<String> {
        let folder = tempfile::tempdir()?;
        let out = folder.path().join("out.json");
        let start = ["jarvis", "dataset", "jarvis.toml"].map(OsStr::new);
        let line = start
            .into_iter()
            .chain([out.as_os_str()])
            .chain(options.iter().map(OsStr::new));
        Command::try_parse_from(line)?.run()?;
        Ok(fs::read_to_string(out)?)
    }

    /// Returns the contexts of the training set that `dataset(options)` writes.
    fn contexts(options: &[&str]) -> anyhow::Result<Vec<String>> {
        let set: TrainingSet = serde_json::from_str(&dataset(options)?)?;
        Ok(set
            .examples
            .into_iter()
            .map(|example| example.context)
            .collect())
    }

    /// The options that give the areas of the reference house.
    const HOUSE: [&str; 2] = ["--house", "src/testing/house.json"];

    /// AC-3.4-1.
    #[test]
    fn ac_3_4_1_house_dataset_is_a_training_set() -> anyhow::Result<()> {
        let set: TrainingSet = serde_json::from_str(&dataset(&HOUSE)?)?;
        assert!(!set.examples.is_empty(), "the training set has no examples");
        Ok(())
    }

    /// AC-3.4-2.
    #[test]
    fn ac_3_4_2_each_context_names_a_house_area() -> anyhow::Result<()> {
        let expected: Vec<String> = fakes::house()
            .areas
            .iter()
            .map(|area| training::context_text(Some(area.name.as_str())))
            .collect();
        for context in contexts(&HOUSE)? {
            assert!(
                expected.contains(&context),
                "{context} names no area of {expected:?}"
            );
        }
        Ok(())
    }

    /// AC-11.5-2.
    #[test]
    fn ac_11_5_2_contexts_have_no_habits() -> anyhow::Result<()> {
        let empty = contexts(&[])?;
        assert!(!empty.is_empty(), "the training set has no examples");
        for context in empty {
            assert_eq!(context, "area: none");
        }
        for context in contexts(&HOUSE)? {
            assert!(!context.contains("habits"), "{context} has habits");
        }
        Ok(())
    }

    /// AC-3.4-4.
    #[test]
    fn ac_3_4_4_two_runs_write_equal_files() -> anyhow::Result<()> {
        assert_eq!(dataset(&HOUSE)?, dataset(&HOUSE)?);
        Ok(())
    }

    /// AC-3.4-5.
    #[test]
    fn ac_3_4_5_no_example_is_held_out() -> anyhow::Result<()> {
        let set: TrainingSet = serde_json::from_str(&dataset(&HOUSE)?)?;
        for example in set.examples {
            assert!(
                !training::held_out(&example.text, 0.1),
                "{} is held out",
                example.text
            );
        }
        Ok(())
    }

    /// One response for each intent of `catalog` and each answer pool.
    fn pools(catalog: &Catalog) -> Pools {
        let names = catalog.intents.iter().map(|intent| intent.name.as_str());
        let pool = |name: &str| {
            let response = Response {
                text: "Done.".to_owned(),
                uses: 0,
                generated: false,
            };
            let responses = vec![response];
            (
                name.to_owned(),
                Pool {
                    responses,
                    last: None,
                },
            )
        };
        let names = names.chain(["filler", "failure"]);
        names.map(pool).collect()
    }

    /// Queues on `FakeHome` and the hot cache of the reference house, with the `[dialog]` of
    /// `config`.
    fn queues(config: &Config) -> Arc<Queues> {
        let (_cache, receiver) = watch::channel(StateCache::load(fakes::house()));
        let wait = Duration::from_secs(1);
        let home = Arc::new(FakeHome::default());
        let dialog = config.dialog.clone();
        Arc::new(Queues::new(home, receiver, wait, wait, dialog))
    }

    /// System Two of `config` on `FakeReasoner` at the Unix epoch.
    fn two(config: &Config) -> SystemTwo {
        let clock = Arc::new(FakeClock(Timestamp::UNIX_EPOCH));
        let reasoner = Arc::new(FakeReasoner::default());
        let two = system_two(config, reasoner, registry(config, Vec::new()));
        SystemTwo { clock, ..two }
    }

    /// The tool names of the registry of the configuration `text` with every variable set.
    fn tool_names(text: &str) -> anyhow::Result<Vec<String>> {
        let config = Config::parse(text)?;
        let secrets = Secrets::resolve(&config, |_| Some("token".to_owned()))?;
        let tools = registry(&config, plugins(&config, &secrets)?).tools();
        Ok(tools.into_iter().map(|tool| tool.name).collect())
    }

    /// AC-17.3b-1.
    #[test]
    fn ac_17_3b_1_registry_has_weather() -> anyhow::Result<()> {
        let names = tool_names(VALID)?;
        assert!(names.contains(&"get_weather".to_owned()), "{names:?}");
        Ok(())
    }

    /// AC-18.11-7.
    #[tokio::test(start_paused = true)]
    async fn ac_18_11_7_plex_needs_its_section_and_token() -> anyhow::Result<()> {
        let plex = "get_new_shows".to_owned();
        assert!(tool_names(VALID)?.contains(&plex), "no Plex plugin");
        let section =
            "[plugins.plex]\nurl = \"http://plex.local:32400\"\ntoken_env = \"PLEX_TOKEN\"\n";
        assert!(VALID.contains(section), "jarvis.toml has no [plugins.plex]");
        let names = tool_names(&VALID.replace(section, ""))?;
        assert!(!names.contains(&plex), "{names:?}");
        let lookup = |name: &str| (name != "PLEX_TOKEN").then(|| "token".to_owned());
        let error = run_error_with(VALID, lookup).await?;
        assert!(error.contains("PLEX_TOKEN"), "{error}");
        Ok(())
    }

    /// The conversation of `config` with `classifier`, `queues(config)`, `two(config)`, the pools
    /// of `catalog`, a backfill on `FakeReasoner` and `speech`, and `utterances`.
    fn conversation(
        config: &Config,
        catalog: Catalog,
        classifier: Arc<dyn Classifier>,
        speech: &Arc<Speech>,
        utterances: Arc<Utterances>,
    ) -> anyhow::Result<Conversation> {
        let task = app::conversation::start_pools(pools(&catalog), Arc::new(FakeStore::new(None)))?;
        let (catalog, cache) = (Arc::new(catalog), watch::channel(StateCache::default()).1);
        let habits = Habits::new(RequestHabits::default(), 14.0, cache.clone());
        let reasoner: Arc<dyn Reasoner> = Arc::new(FakeReasoner::default());
        let backfill = backfill(config, &reasoner, task.changes, speech, &catalog, cache);
        let (queues, threshold) = (queues(config), config.model.threshold);
        Ok(Conversation::new(
            task.pools,
            backfill,
            queues,
            classifier,
            catalog,
            threshold,
            two(config),
            Arc::new(habits),
            Recorder {
                utterances,
                writer: tokio::sync::mpsc::unbounded_channel().0,
                model: MODEL.to_owned(),
            },
        ))
    }

    /// The app of `VALID` with `classifier`, `stt`, the reference house and `pools`.
    fn app_with(
        classifier: Arc<dyn Classifier>,
        stt: Arc<dyn SpeechToText>,
    ) -> anyhow::Result<App> {
        let config = Config::parse(VALID)?;
        let catalog = load_catalog(&config.intents)?;
        let speech = Arc::new(Speech::new(Arc::new(FakeTts::default())));
        let (reasoner, queues) = (Arc::new(FakeReasoner::default()), queues(&config));
        let listener = listener(&config, stt, Arc::clone(&classifier), reasoner, &queues);
        let utterances = Arc::clone(listener.utterances());
        Ok(App {
            conversation: conversation(&config, catalog, classifier, &speech, utterances)?,
            listener,
            speech,
        })
    }

    /// The app of `VALID` with the fixture model, the reference house and `pools`.
    async fn fixture_app() -> anyhow::Result<App> {
        let fixture = Path::new("models/system-one/fixture");
        let device = Config::parse(VALID)?.device;
        let classifier = OnnxLoader.load(fixture, device).await?;
        app_with(classifier, Arc::new(FakeStt::default()))
    }

    /// Serves `app` on `listener` in its own task, with no shutdown.
    fn spawn_server(listener: TcpListener, app: Arc<App>) {
        let tracker = TaskTracker::new();
        tokio::spawn(async move {
            adapters::wyoming::serve(listener, app, &tracker, future::pending()).await
        });
    }

    /// The time from each of `count` kitchen `transcript` events to its `handled` event, through
    /// a Wyoming server of `fixture_app()`.
    async fn round_trips(count: usize) -> anyhow::Result<Vec<Duration>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Arc::new(fixture_app().await?);
        spawn_server(listener, app);
        let mut client = FakeWyomingClient::connect(address).await?;
        let kitchen = "98f4aacf9c4152ffe32dff212df91521";
        let data = json!({ "text": "turn on the lights", "context": { "device_id": kitchen } });
        let mut times = Vec::new();
        for _ in 0..count {
            let start = Instant::now();
            client.send("transcript", &data).await?;
            let event = client.receive().await?.map(|(event, _)| event);
            times.push(start.elapsed());
            anyhow::ensure!(
                event.as_deref() == Some("handled"),
                "the reply is {event:?}"
            );
        }
        Ok(times)
    }

    /// AC-5.3-6.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_5_3_6_transcript_to_handled_p95_is_less_than_20_ms() -> anyhow::Result<()> {
        let p95 = p95(round_trips(1100).await?);
        assert!(p95 < Duration::from_millis(20), "p95 is {p95:?}");
        Ok(())
    }

    /// The 95th percentile of `times` after the first 100, which warm up the models.
    fn p95(mut times: Vec<Duration>) -> Duration {
        let mut times = times.split_off(100);
        times.sort();
        times[times.len() * 95 / 100]
    }

    /// Serves an app with the TTS of `VALID` and the pre-rendered pools on a local port, and
    /// gives its address.
    async fn speech_server() -> anyhow::Result<SocketAddr> {
        let config = Config::parse(VALID)?;
        let speech = Speech::new(Arc::new(voice(config.device)?));
        let catalog = load_catalog(&config.intents)?;
        let areas = ["Kitchen".to_owned()];
        speech.prerender(&pools(&catalog), &areas).await?;
        let (classifier, stt) = (FakeClassifier::default(), FakeStt::default());
        let app = App {
            speech: Arc::new(speech),
            ..app_with(Arc::new(classifier), Arc::new(stt))?
        };
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        spawn_server(listener, Arc::new(app));
        Ok(address)
    }

    /// The time from a `synthesize` event of a pre-rendered text on `client` to its first
    /// `audio-chunk` event. Reads the rest of the audio.
    async fn first_chunk(client: &mut FakeWyomingClient) -> anyhow::Result<Duration> {
        let start = Instant::now();
        client
            .send("synthesize", &json!({ "text": "Done." }))
            .await?;
        client.receive().await?;
        let first = client.receive().await?.map(|(event, _)| event);
        let time = start.elapsed();
        anyhow::ensure!(
            first.as_deref() == Some("audio-chunk"),
            "the second event is {first:?}"
        );
        while client.receive().await?.context("the server closed")?.0 != "audio-stop" {}
        Ok(time)
    }

    /// AC-6.10-3.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_10_3_cached_synthesize_to_first_chunk_p95_is_less_than_10_ms()
    -> anyhow::Result<()> {
        let mut client = FakeWyomingClient::connect(speech_server().await?).await?;
        let mut times = Vec::new();
        for _ in 0..1100 {
            times.push(first_chunk(&mut client).await?);
        }
        let p95 = p95(times);
        assert!(p95 < Duration::from_millis(10), "p95 is {p95:?}");
        Ok(())
    }

    /// A connector to no HA. It sends to `attempt` on each call.
    fn no_ha(
        attempt: mpsc::UnboundedSender<()>,
    ) -> impl Fn(&str, u16) -> future::Ready<io::Result<DuplexStream>> + Send + 'static {
        move |_: &str, _: u16| {
            attempt.send(()).ok();
            future::ready(Err(io::Error::other("HA is not available")))
        }
    }

    /// Writes the configuration `text` to `jarvis.toml` in `folder` and returns its path.
    fn config_in(folder: &Path, text: &str) -> anyhow::Result<PathBuf> {
        let config = folder.join("jarvis.toml");
        fs::write(&config, text)?;
        Ok(config)
    }

    /// The error of `run` for the configuration `text`, with no HA.
    async fn run_error(text: &str) -> anyhow::Result<String> {
        run_error_with(text, |_: &str| Some("token".to_owned())).await
    }

    /// The error of `run` for the configuration `text` and the environment `lookup`, with no HA.
    async fn run_error_with(
        text: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<String> {
        let folder = tempfile::tempdir()?;
        let config = config_in(folder.path(), text)?;
        let startup = Startup {
            lookup,
            connector: no_ha(mpsc::unbounded_channel().0),
            shutdown: future::pending(),
        };
        let error = run(&config, startup).await.err().context("run succeeded")?;
        Ok(format!("{error:#}"))
    }

    /// `VALID` with the files `pools.json`, `habits.json` and `jarvis.db` in `folder`.
    fn with_stores_in(folder: &Path) -> String {
        let in_folder = |file: &str| format!("\"{}\"", folder.join(file).display());
        let text = VALID.replace("\"pools.json\"", &in_folder("pools.json"));
        let text = text.replace("\"jarvis.db\"", &in_folder("jarvis.db"));
        text.replace("\"habits.json\"", &in_folder("habits.json"))
    }

    /// A model with `get_traffic` at 08:10 on Monday, Tuesday and Wednesday in the HA time zone of
    /// the reference house, and the date of that Wednesday.
    fn three_mornings() -> (RequestHabits, jiff::civil::Date) {
        let zone = jiff::tz::TimeZone::get("America/Chicago").expect("a time zone");
        let monday: Timestamp = "2026-01-05T08:10:00-06:00".parse().expect("a time");
        let days = [0, 1, 2].map(|day| monday + jiff::SignedDuration::from_hours(24 * day));
        let mut model = RequestHabits::default();
        for time in days {
            model = model.observe("get_traffic".to_owned(), time, &zone, 14.0);
        }
        (model, zone.to_datetime(days[2]).date())
    }

    /// AC-11.4b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_11_4b_3_stored_model_gives_its_usual_times() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let config = Config::parse(&with_stores_in(folder.path()))?;
        let (model, wednesday) = three_mornings();
        FileStore::try_new(config.habits.file.clone())?.save(&model)?;
        let cache = watch::channel(StateCache::default()).1;
        let (_, task) = start_stores(&config, Pools::new(), &cache)?;
        let usual = |saved: RequestHabits| saved.usual_times("get_traffic", wednesday, 14.0, 2.0);
        let bucket = Bucket {
            day: DayType::Weekdays,
            hour: 8,
        };
        assert_eq!(task.habits.current().map(usual), Some(vec![bucket]));
        Ok(())
    }

    /// AC-5.4-1.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4_1_invalid_configuration_fails_before_a_port_opens() -> anyhow::Result<()> {
        let cases = [
            ("= 30000", "= 0", "ha.request_timeout_ms"),
            ("ws:", "wss:", "ha.url"),
        ];
        for (from, to, field) in cases {
            let error = run_error(&VALID.replace(from, to)).await?;
            assert!(error.contains(field), "the error has no {field}: {error}");
        }
        Ok(())
    }

    /// AC-5.4-3.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4_3_missing_model_folder_is_named() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let models = folder.path().join("models");
        let text = with_stores_in(folder.path()).replace("\"cuda\"", "\"cpu\"");
        let text = text.replace("models/system-one", &models.display().to_string());
        let error = run_error(&text).await?;
        let fixture = models.join("fixture").display().to_string();
        assert!(error.contains(&fixture), "no folder: {error}");
        Ok(())
    }

    /// AC-5.4-4.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4_4_swap_gives_the_next_handle_the_new_classifier() -> anyhow::Result<()> {
        let (old, new): (Arc<FakeClassifier>, Arc<FakeClassifier>) = Default::default();
        let current = Arc::new(CurrentClassifier(ArcSwap::from_pointee(old.clone() as _)));
        let app = app_with(Arc::clone(&current) as _, Arc::new(FakeStt::default()))?;
        let handle = || app.handle("turn on the lights", ports::Context::default());
        let _ = handle().await;
        current.0.store(Arc::new(new.clone() as _));
        let _ = handle().await;
        assert_eq!((old.calls().len(), new.calls().len()), (1, 1));
        Ok(())
    }

    /// AC-12.1b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_12_1b_2_handle_without_satellite_does_not_correlate() -> anyhow::Result<()> {
        let text = "turn on the lights";
        let partial = Partial {
            text: text.to_owned(),
            voice_end_ms: None,
            vad_ms: 20,
        };
        let stt = FakeStt {
            partials: vec![partial],
            ..FakeStt::default()
        };
        let app = app_with(Arc::new(FakeClassifier::default()), Arc::new(stt))?;
        let mut session = app.listen()?;
        session.push(vec![0; 320]).await?;
        session.finish().await?;
        let _ = app.handle(text, ports::Context::default()).await;
        let found = app.listener.utterances().correlate(text);
        assert!(found.is_some(), "handle without a satellite_id correlated");
        Ok(())
    }

    /// AC-6.8b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8b_3_blocked_accept_does_not_delay_handle() -> anyhow::Result<()> {
        let stt = Arc::new(FakeStt::default());
        let closed = stt.gate.lock().await;
        let app = app_with(Arc::new(FakeClassifier::default()), Arc::clone(&stt) as _)?;
        let mut session = app.listen()?;
        let push = tokio::spawn(async move { session.push(vec![0; 320]).await });
        tokio::task::yield_now().await;
        let _ = app
            .handle("turn on the lights", ports::Context::default())
            .await;
        assert!(!push.is_finished(), "accept ended before handle returned");
        drop(closed);
        push.await??;
        Ok(())
    }

    /// AC-5.4-5.
    #[test]
    fn ac_5_4_5_no_command_prints_the_usage_and_succeeds() {
        let error = Command::try_parse_from(["jarvis"]).expect_err("there is no command");
        let text = error.render().to_string();
        assert!(text.contains("Usage: jarvis <COMMAND>"), "no usage: {text}");
        assert_eq!(start([OsString::from("jarvis")]), ExitCode::SUCCESS);
    }

    /// AC-5.4-7.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4_7_unusable_variable_names_the_pool_before_a_port_opens() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let responses = folder.path().join("responses.toml");
        let text =
            read(Path::new("responses.toml"))?.replace("{{ area }} lights", "{{ show }} lights");
        fs::write(&responses, text)?;
        let path = format!("\"{}\"", responses.display());
        let error =
            run_error(&with_stores_in(folder.path()).replace("\"responses.toml\"", &path)).await?;
        assert!(error.contains("lights_on"), "no pool: {error}");
        Ok(())
    }

    /// The next port that `free_address` tries.
    static NEXT_PORT: AtomicU16 = AtomicU16::new(20000);

    /// A free local address. `run` binds the fixed address of `listen`, so the port is free only
    /// until then. The port is below the ephemeral ranges of Linux and macOS, so no client socket
    /// takes it in that time.
    fn free_address() -> SocketAddr {
        loop {
            let address =
                SocketAddr::from(([127, 0, 0, 1], NEXT_PORT.fetch_add(1, Ordering::Relaxed)));
            if std::net::TcpListener::bind(address).is_ok() {
                return address;
            }
        }
    }

    /// A CPU configuration in `folder` with a server address on a free local port, and the
    /// address. It names the int8 files and no second pass, as on a CPU machine.
    fn listening_config(folder: &Path) -> anyhow::Result<(PathBuf, SocketAddr)> {
        let address = free_address();
        let text = with_stores_in(folder).replace("\"cuda\"", "\"cpu\"");
        let text = text.replace(".fp16.", ".int8.");
        let text = text.replace("second_pass_dir =", "# second_pass_dir =");
        let text = text.replace("0.0.0.0:10700", &address.to_string());
        Ok((config_in(folder, &text)?, address))
    }

    /// Connects to the Wyoming server at `address` once it listens, sends `describe` and gives
    /// the client and the type of the answer.
    async fn describe_when_listening(
        address: SocketAddr,
    ) -> anyhow::Result<(FakeWyomingClient, Option<String>)> {
        let mut client = loop {
            if let Ok(client) = FakeWyomingClient::connect(address).await {
                break client;
            }
            tokio::task::yield_now().await;
        };
        client.send("describe", &json!({})).await?;
        let event = client.receive().await?.map(|(event, _)| event);
        Ok((client, event))
    }

    /// Sends `describe` to the Wyoming server at `address` once it listens and gives the type of
    /// the answer. Waits for an HA connection attempt on `attempts`, advances past the backoff
    /// and waits for the next one.
    async fn describe_without_ha(
        address: SocketAddr,
        attempts: &mut mpsc::UnboundedReceiver<()>,
    ) -> anyhow::Result<Option<String>> {
        let (_client, event) = describe_when_listening(address).await?;
        attempts.recv().await;
        // The longest backoff delay of `HaHome`.
        tokio::time::advance(Duration::from_secs(2)).await;
        attempts.recv().await.context("no second attempt")?;
        Ok(event)
    }

    /// AC-5.4b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4b_1_without_ha_the_server_accepts_and_the_sync_tries_again() -> anyhow::Result<()>
    {
        let folder = tempfile::tempdir()?;
        let (config, address) = listening_config(folder.path())?;
        let (attempt, mut attempts) = mpsc::unbounded_channel();
        let startup = Startup {
            lookup: |_: &str| Some("token".to_owned()),
            connector: no_ha(attempt),
            shutdown: future::pending(),
        };
        tokio::select! {
            result = run(&config, startup) => anyhow::bail!("run returned: {result:?}"),
            event = describe_without_ha(address, &mut attempts) => {
                assert_eq!(event?.as_deref(), Some("info"));
            }
        }
        Ok(())
    }
    /// Holds a connection to the Wyoming server at `address` and stops `run` with `stop`. Then
    /// checks that a new connection fails and advances virtual time by 5 s, then 1 ms.
    async fn stop_while_connected(
        address: SocketAddr,
        stop: oneshot::Sender<()>,
    ) -> anyhow::Result<()> {
        let (_client, event) = describe_when_listening(address).await?;
        anyhow::ensure!(event.as_deref() == Some("info"), "the answer is {event:?}");
        stop.send(())
            .map_err(|()| anyhow::anyhow!("run dropped the shutdown future"))?;
        // `run` comes first in the biased `select!`, so it sees the stop before the next connect.
        tokio::task::yield_now().await;
        let connected = TcpStream::connect(address).await;
        anyhow::ensure!(connected.is_err(), "the server accepts after the stop");
        tokio::time::advance(Duration::from_secs(5)).await;
        // The last saves run on blocking threads, which stop the paused clock, so `run` returns
        // before this sleep ends.
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(())
    }

    /// AC-5.4c-1. The open connection keeps its task running, so `run` waits the full 5 s.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4c_1_shutdown_stops_accepting_and_returns_within_5_s() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let (config, address) = listening_config(folder.path())?;
        let (stop, stopped) = oneshot::channel();
        let startup = Startup {
            lookup: |_: &str| Some("token".to_owned()),
            connector: no_ha(mpsc::unbounded_channel().0),
            shutdown: async {
                let _ = stopped.await;
            },
        };
        let start = tokio::time::Instant::now();
        tokio::select! {
            biased;
            result = run(&config, startup) => result?,
            result = stop_while_connected(address, stop) => {
                anyhow::bail!("run did not return after 5 s: {result:?}");
            }
        }
        let waited = start.elapsed();
        assert_eq!(
            waited,
            Duration::from_secs(5),
            "run waits up to 5 s for the open connection"
        );
        let habits = folder.path().join("habits.json");
        assert!(habits.exists(), "run did not save {}", habits.display());
        Ok(())
    }

    /// Runs one prediction of `classifier` and one synthesis of `tts`.
    async fn predict_and_synth(
        classifier: &dyn Classifier,
        tts: &SupertonicTts,
    ) -> anyhow::Result<()> {
        classifier
            .predict("area: kitchen", "turn on the lights")
            .await?;
        let audio = tts.synth("Hello").await?;
        anyhow::ensure!(!audio.samples.is_empty(), "the audio has no samples");
        Ok(())
    }

    /// The `SupertonicTts` of `VALID` on `device`.
    fn voice(device: Device) -> anyhow::Result<SupertonicTts> {
        SupertonicTts::try_new(&Config::parse(VALID)?.tts, device)
    }

    /// The GPU memory in MiB that `nvidia-smi` reports for this process.
    fn gpu_memory() -> anyhow::Result<u64> {
        let query = [
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ];
        let output = std::process::Command::new("nvidia-smi")
            .args(query)
            .output()?;
        anyhow::ensure!(output.status.success(), "nvidia-smi fails: {output:?}");
        let pid = std::process::id().to_string();
        let mut total = 0;
        for line in String::from_utf8(output.stdout)?.lines() {
            if let Some((process, memory)) = line.split_once(',')
                && process.trim() == pid
            {
                total += memory.trim().parse::<u64>()?;
            }
        }
        Ok(total)
    }

    /// True in the child process that runs the ignored test `name` alone. Else runs that child
    /// and gives false when it passes.
    fn alone(name: &str) -> anyhow::Result<bool> {
        let alone = "JARVIS_ALONE";
        if env::var_os(alone).is_some() {
            return Ok(true);
        }
        let status = std::process::Command::new(env::current_exe()?)
            .args(["--ignored", "--exact", name])
            .env(alone, "1")
            .status()?;
        anyhow::ensure!(status.success(), "the test fails alone: {status}");
        Ok(false)
    }

    /// AC-6.2-4.
    #[tokio::test]
    async fn ac_6_2_4_classifier_and_tts_run_in_one_process() -> anyhow::Result<()> {
        let classifier = OnnxLoader
            .load(Path::new("models/system-one/fixture"), Device::Cpu)
            .await?;
        predict_and_synth(classifier.as_ref(), &voice(Device::Cpu)?).await
    }

    /// AC-6.2-5. The GPU memory of this process shows that each model loads on CUDA. The other
    /// GPU tests change the memory of their process, so the test runs again alone in a child
    /// process.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_2_5_cuda_runs_classifier_and_tts_in_one_process() -> anyhow::Result<()> {
        if !alone("tests::ac_6_2_5_cuda_runs_classifier_and_tts_in_one_process")? {
            return Ok(());
        }
        let start = gpu_memory()?;
        let classifier = OnnxLoader
            .load(Path::new("models/system-one/fixture"), Device::Cuda)
            .await?;
        let middle = gpu_memory()?;
        let tts = voice(Device::Cuda)?;
        let end = gpu_memory()?;
        assert!(
            start < middle && middle < end,
            "GPU memory in MiB: {start} at the start, {middle} after the classifier, {end} after the TTS"
        );
        predict_and_synth(classifier.as_ref(), &tts).await
    }

    /// AC-6.6c-4. sherpa-onnx falls back to the CPU with only a log line, so the GPU memory of
    /// this process shows that the first pass loads on CUDA.
    #[test]
    #[ignore = "GPU host"]
    fn ac_6_6c_4_cuda_runs_the_first_pass() -> anyhow::Result<()> {
        if !alone("tests::ac_6_6c_4_cuda_runs_the_first_pass")? {
            return Ok(());
        }
        let start = gpu_memory()?;
        let _stt = SherpaStt::try_new(&Config::parse(VALID)?.asr, Device::Cuda)?;
        let end = gpu_memory()?;
        assert!(
            start < end,
            "GPU memory in MiB: {start} before and {end} after the first pass"
        );
        Ok(())
    }

    /// AC-6.7-5. sherpa-onnx falls back to the CPU with only a log line, so the GPU memory of
    /// this process shows that the second pass loads on CUDA. The first load also creates the
    /// CUDA context, so the second load of the first pass alone is the reference.
    #[test]
    #[ignore = "GPU host"]
    fn ac_6_7_5_cuda_runs_the_second_pass() -> anyhow::Result<()> {
        if !alone("tests::ac_6_7_5_cuda_runs_the_second_pass")? {
            return Ok(());
        }
        let both = Config::parse(VALID)?.asr;
        let first = AsrConfig {
            second_pass_dir: None,
            ..both.clone()
        };
        let mut memory = vec![gpu_memory()?];
        let mut loaded = Vec::new();
        for asr in [&first, &first, &both] {
            loaded.push(SherpaStt::try_new(asr, Device::Cuda)?);
            memory.push(gpu_memory()?);
        }
        assert!(
            memory[3] - memory[2] > memory[2] - memory[1],
            "GPU memory in MiB at the start and after each load: {memory:?}"
        );
        Ok(())
    }

    /// The GPU memory in MiB of this process before and after each synthesis of `tts` in
    /// AC-9.3c-2.
    async fn synth_memory(tts: &SupertonicTts) -> anyhow::Result<Vec<u64>> {
        let mut memory = vec![gpu_memory()?];
        let sentence = "Tomorrow will be mostly sunny with a light breeze from the west, and the evening should stay clear and mild, so it is a good night for a walk.";
        for text in ["One moment.", sentence] {
            tts.synth(text).await?;
            memory.push(gpu_memory()?);
        }
        Ok(memory)
    }

    /// AC-9.3c-2. ONNX Runtime keeps the memory of its arenas, so the largest reading after each
    /// step is the peak. The other GPU tests change the memory of their process, so the test runs
    /// again alone in a child process.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_9_3c_2_cuda_peak_memory_is_5_5_gb_or_less() -> anyhow::Result<()> {
        use std::io::Write;

        if !alone("tests::ac_9_3c_2_cuda_peak_memory_is_5_5_gb_or_less")? {
            return Ok(());
        }
        let config = Config::parse(VALID)?;
        let _stt = SherpaStt::try_new(&config.asr, Device::Cuda)?;
        let _classifier = OnnxLoader
            .load(Path::new("models/system-one/fixture"), Device::Cuda)
            .await?;
        let tts = SupertonicTts::try_new(&config.tts, Device::Cuda)?;
        let memory = synth_memory(&tts).await?;
        let peak = memory.iter().max().copied().unwrap_or_default();
        writeln!(io::stderr(), "peak GPU memory {peak} MiB")?;
        // 5.5 GB in MiB.
        assert!(
            peak <= 5_500_000_000 >> 20,
            "GPU memory in MiB after the loads and after each synthesis: {memory:?}"
        );
        Ok(())
    }
}
