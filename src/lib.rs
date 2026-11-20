//! Voice-controlled home automation for Home Assistant.

// Verus reads loop invariants from attributes on loops, and the `split_once` contract names a
// `Pattern`.
#![cfg_attr(verus_only, feature(proc_macro_hygiene, pattern))]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::panic;
use std::path::Path;
use std::path::PathBuf;
use std::pin;
use std::process;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Weak;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Context;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use clap::Parser;
use clap::error::ErrorKind;
use futures::future::BoxFuture;
use jiff::SignedDuration;
use jiff::Timestamp;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Deserialize;
use serde_json::Value;
use tempfile::NamedTempFile;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::signal::unix;
use tokio::signal::unix::SignalKind;
use tokio::sync::Notify;
use tokio::sync::OwnedRwLockReadGuard;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::task::AbortOnDropHandle;
use tokio_util::task::TaskTracker;
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

use crate::adapters::files::FileStore;
use crate::adapters::files::RequestFiles;
use crate::adapters::ha::HaHome;
use crate::adapters::onnx::OnnxLoader;
use crate::adapters::open_meteo::OpenMeteo;
use crate::adapters::openai::OpenAiReasoner;
use crate::adapters::plex::Plex;
use crate::adapters::process::ProcessTrainer;
use crate::adapters::runpod::RunPodTrainer;
use crate::adapters::sherpa::SherpaEmbedder;
use crate::adapters::sherpa::SherpaStt;
use crate::adapters::sherpa::SherpaVoices;
use crate::adapters::sqlite::SqliteLog;
use crate::adapters::supertonic::SupertonicTts;
use crate::app::background::Activation;
use crate::app::background::Backfill;
use crate::app::background::Habits;
use crate::app::background::HabitsTask;
use crate::app::background::Learning;
use crate::app::background::Prefetch;
use crate::app::conversation::Change;
use crate::app::conversation::Conversation;
use crate::app::conversation::PoolsTask;
use crate::app::conversation::Queues;
use crate::app::conversation::Recorder;
use crate::app::conversation::Speakers;
use crate::app::conversation::SystemTwo;
use crate::app::plugins::Plugins;
use crate::app::review::Reviewer;
use crate::app::speech::Listener;
use crate::app::speech::Speech;
use crate::app::speech::Utterances;
use crate::app::training::FIXTURE;
use crate::app::training::Training;
use crate::app::training::Trigger;
use crate::domain::catalog::Catalog;
use crate::domain::habits::RequestHabits;
use crate::domain::learn::Learned;
use crate::domain::model::Prediction;
use crate::domain::responses::Pool;
use crate::domain::responses::Pools;
use crate::domain::responses::Response;
use crate::domain::speaker::PersonTrackers;
use crate::domain::speaker::Satellite;
use crate::domain::speech::Audio;
use crate::domain::speech::CommitPolicy;
use crate::domain::state::Snapshot;
use crate::domain::state_cache::StateCache;
use crate::domain::training::Settings;
use crate::domain::training::TrainingSet;
use crate::domain::training::TrainingState;
use crate::ports::Activity;
use crate::ports::Assistant;
use crate::ports::Classifier;
use crate::ports::Clock;
use crate::ports::Control;
use crate::ports::Handled;
use crate::ports::Home;
use crate::ports::Info;
use crate::ports::InteractionLog;
use crate::ports::Listening;
use crate::ports::MediaLibrary;
use crate::ports::ModelLoader;
use crate::ports::Models;
use crate::ports::Partial;
use crate::ports::Plugin;
use crate::ports::PluginRequests;
use crate::ports::Reasoner;
use crate::ports::Refusal;
use crate::ports::Setting;
use crate::ports::SpeechToText;
use crate::ports::State;
use crate::ports::Status;
use crate::ports::Store;
use crate::ports::SttStream;
use crate::ports::TextToSpeech;
use crate::ports::Tool;
use crate::ports::Trainer;
use crate::ports::VoiceEmbedder;

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
    pub mod learn;
    pub mod model;
    pub mod responses;
    pub mod select;
    pub mod speaker;
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
    pub mod review;
    pub mod speech;
    pub mod training;
}

/// Port implementations, one technology each.
pub mod adapters {
    pub mod files;
    pub mod ha;
    pub mod mqtt;
    pub mod onnx;
    pub mod open_meteo;
    pub mod openai;
    pub mod plex;
    pub mod process;
    pub mod runpod;
    pub mod settings;
    pub mod sherpa;
    pub mod sqlite;
    pub mod supertonic;
    pub mod web;
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
    /// Learned intents.
    pub learn: LearnConfig,
    /// The interaction log.
    pub log: LogConfig,
    /// The web server of the Jarvis tab.
    pub web: WebConfig,
    /// The MQTT broker of the status entity. Without it, Jarvis publishes no status.
    pub mqtt: Option<MqttConfig>,
    /// Speaker identification.
    pub speaker: SpeakerConfig,
    /// The tracker overrides of the people. The people are the `person` entities of HA.
    #[serde(default)]
    pub people: Vec<PersonTrackers>,
    /// The satellites whose presence sensors replace discovery.
    #[serde(default)]
    pub satellites: Vec<Satellite>,
    /// The form of address of each person entity, for `{{ address }}` in responses.
    #[serde(default)]
    pub address: BTreeMap<String, String>,
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
            ("asr.hotwords_score", f64::from(self.asr.hotwords_score)),
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
        let lead = self.learn.prefetch_lead_min;
        anyhow::ensure!(
            (1..=60).contains(&lead),
            "learn.prefetch_lead_min = {lead} is out of range: it must be in [1, 60]"
        );
        self.validate_speaker()?;
        self.validate_relations()
    }

    /// Checks that `speaker.threshold` and `speaker.margin` are more than 0 and that
    /// `speaker.enroll_threshold` is in `[0, 1]`.
    ///
    /// # Errors
    ///
    /// A field is out of its range. The error names the field and the range.
    fn validate_speaker(&self) -> anyhow::Result<()> {
        let speaker = &self.speaker;
        let positive = [
            ("speaker.threshold", speaker.threshold),
            ("speaker.margin", speaker.margin),
        ];
        for (name, value) in positive {
            anyhow::ensure!(
                value > 0.0,
                "{name} = {value} is out of range: it must be more than 0"
            );
        }
        let enroll = speaker.enroll_threshold;
        anyhow::ensure!(
            (0.0..=1.0).contains(&enroll),
            "speaker.enroll_threshold = {enroll} is out of range: it must be in [0, 1]"
        );
        Ok(())
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
    fn durations(&self) -> [(&'static str, u64); 18] {
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
            ("train.check_interval_s", self.train.check_interval_s),
            ("learn.check_interval_s", self.learn.check_interval_s),
            ("speaker.min_enroll_ms", self.speaker.min_enroll_ms),
        ]
    }

    /// The name and the value of each count and limit.
    fn counts(&self) -> [(&'static str, u64); 13] {
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
            ("speaker.max_samples", self.speaker.max_samples as u64),
            ("speaker.min_profile", self.speaker.min_profile as u64),
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
    /// True for a second pass with modified beam search and hotwords, false for greedy search.
    pub hotwords: bool,
    /// The score that each token of a matched hotword adds.
    pub hotwords_score: f32,
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
    /// The model of each embeddings request.
    pub embedding_model: String,
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
    /// The saved training state.
    pub state: PathBuf,
    /// The saved generated examples.
    pub generated: PathBuf,
    /// The model to fine-tune.
    pub base_model: String,
    /// The number of passes over the examples.
    pub epochs: usize,
    /// The number of examples in one step.
    pub batch_size: usize,
    /// The number of changes that starts an automatic training run.
    pub min_changes: usize,
    /// The time between two checks of the change count.
    pub check_interval_s: u64,
    /// The share of the examples in the held-out split.
    pub holdout: f64,
    /// The wait after a failed run before the next automatic run.
    pub failure_backoff_s: u64,
    /// The time after which a training run stops.
    pub timeout_min: u64,
    /// The GPU memory cap of a local run, in GB.
    pub max_vram_gb: f64,
    /// The number of generated examples of each intent.
    pub paraphrases: usize,
    /// The number of generated examples of `unknown`.
    pub unknown_paraphrases: usize,
    /// The Python interpreter of a local run.
    pub python: PathBuf,
    /// The training fallback. Without it, Jarvis has no training fallback.
    pub runpod: Option<RunPodConfig>,
}

/// The `[train.runpod]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunPodConfig {
    /// The environment variable with the `RunPod` API key.
    pub api_key_env: String,
    /// The serverless endpoint of the training image.
    pub endpoint_id: String,
    /// The network volume, which is the bucket of the S3 API.
    pub volume_id: String,
    /// The S3 API of the data center of the volume.
    pub s3_endpoint: String,
    /// The region of the S3 API.
    pub s3_region: String,
    /// The environment variable with the S3 access key.
    pub s3_key_env: String,
    /// The environment variable with the S3 secret key.
    pub s3_secret_env: String,
    /// The time between two status requests of a job.
    pub poll_s: u64,
    /// The time after which a job is cancelled.
    pub timeout_min: u64,
}

/// The `[learn]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LearnConfig {
    /// The days of System Two requests that the clusters use.
    pub window_days: u64,
    /// The fewest requests of a cluster.
    pub min_cluster: usize,
    /// The largest distance between two close requests and between a cluster and its learned
    /// entry.
    pub cluster_distance: f64,
    /// The number of responses that a proposal has.
    pub responses: usize,
    /// The learned intents file.
    pub file: PathBuf,
    /// The time between two learned intent checks.
    pub check_interval_s: u64,
    /// The minutes before the hour of a usual time at which its prefetch window opens.
    pub prefetch_lead_min: u64,
    /// The services that a prefetch can call. Any other call discards the prefetch.
    pub prefetch_services: Vec<String>,
    /// The folder of the plugin requests. Without it, Jarvis writes no plugin requests.
    pub plugin_requests: Option<PathBuf>,
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

/// The `[web]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Address of the web server.
    pub listen: SocketAddr,
    /// The environment variable with the bearer token of the API.
    pub token_env: String,
    /// The number of interactions on a page of the API.
    pub page_size: usize,
}

/// The `[mqtt]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MqttConfig {
    /// The broker, `mqtt://host:port`.
    pub url: String,
    /// The environment variable with the user name.
    pub username_env: String,
    /// The environment variable with the password.
    pub password_env: String,
}

/// The `[speaker]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpeakerConfig {
    /// The speaker embedding model file.
    pub model: PathBuf,
    /// The voice store file.
    pub voices: PathBuf,
    /// The voice similarity of the same person.
    pub voice_threshold: f32,
    /// The samples that each profile keeps.
    pub max_samples: usize,
    /// The combined score of proximity and voice that identifies a speaker.
    pub threshold: f64,
    /// The lead of the top score over each other score.
    pub margin: f64,
    /// The proximity that gives the source `Proximity`.
    pub enroll_threshold: f64,
    /// The voice length of an utterance that can enroll, in ms.
    pub min_enroll_ms: u64,
    /// The sample count from which an enroll needs a voice match.
    pub min_profile: usize,
}

/// The `Clock` of the system time.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Why [`run`] returned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ended {
    /// A save of the settings: Jarvis starts again with the new file.
    Restart,
    /// A signal: Jarvis stops.
    Stop,
}

/// The status channel and the activity that the training trigger and the [`Controller`] share.
#[derive(Clone, Default)]
struct Shared {
    /// The sender of the status channel.
    status: watch::Sender<Status>,
    /// What Jarvis does besides requests.
    activity: Arc<Mutex<Activity>>,
}

/// The `Control` of the root: the status channel, the activity and the configuration file that
/// `run` got.
struct Controller {
    /// The configuration file.
    config: PathBuf,
    /// The sender of the status channel.
    status: watch::Sender<Status>,
    /// What Jarvis does besides requests.
    activity: Arc<Mutex<Activity>>,
    /// Notified after a save.
    restart: Notify,
}

impl Controller {
    /// The control of `config` on `shared`. Sets the message of the last rollback.
    fn new(config: PathBuf, Shared { status, activity }: Shared) -> Self {
        let message = adapters::settings::rolled_back(&config);
        status.send_modify(|status| status.message = message);
        Self {
            status,
            activity,
            restart: Notify::new(),
            config,
        }
    }

    /// Writes the configuration file with `changes` after the checks of the start, and keeps the
    /// old file as the backup. First removes the reason of an earlier rollback, so that a failure
    /// there leaves both files unchanged.
    ///
    /// # Errors
    ///
    /// The file cannot be read or written, or `edit` or `Config::parse` fails.
    fn written(&self, changes: BTreeMap<String, Value>) -> Result<(), Refusal> {
        let text = read(&self.config).map_err(invalid)?;
        let new = adapters::settings::edit(&text, changes).map_err(Refusal::Invalid)?;
        Config::parse(&new).map_err(invalid)?;
        adapters::settings::clear_rollback(&self.config)
            .and_then(|()| write_atomic(&adapters::settings::beside(&self.config, ".bak"), &text))
            .and_then(|()| write_atomic(&self.config, &new))
            .map_err(invalid)
    }

    /// Completes when `shutdown` completes or after a save.
    async fn until_restart(&self, shutdown: impl Future<Output = ()>) {
        tokio::select! {
            () = shutdown => {}
            () = self.restart.notified() => {}
        }
    }

    /// `Restart` after a save, which keeps the status `restarting`. Else sets `Offline` and gives
    /// `Stop`.
    fn ended(&self) -> Ended {
        if self.status.borrow().state == State::Restarting {
            return Ended::Restart;
        }
        self.status.send_replace(Status {
            state: State::Offline,
            message: None,
        });
        Ended::Stop
    }
}

/// The refusal of `error`, with its message.
#[expect(clippy::needless_pass_by_value, reason = "the argument of `map_err`")]
fn invalid(error: anyhow::Error) -> Refusal {
    Refusal::Invalid(format!("{error:#}"))
}

impl Control for Controller {
    fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    fn settings(&self) -> anyhow::Result<Vec<Setting>> {
        adapters::settings::read(&read(&self.config)?)
    }

    fn save(&self, changes: BTreeMap<String, Value>) -> Result<(), Refusal> {
        if !app::training::change(&self.activity, Activity::Idle, Activity::Restarting) {
            return Err(Refusal::Busy);
        }
        let written = self.written(changes);
        if written.is_err() {
            app::training::change(&self.activity, Activity::Restarting, Activity::Idle);
        }
        written?;
        self.status.send_replace(Status {
            state: State::Restarting,
            message: None,
        });
        self.restart.notify_one();
        Ok(())
    }
}

/// Writes `text` to the file at `path` with a rename, so that a reader gets the old or the new
/// text.
///
/// # Errors
///
/// The write or the rename fails.
fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    // A rename is atomic only inside one file system, so the temporary file is in the folder of
    // the target.
    let mut file = NamedTempFile::new_in(path.parent().unwrap_or(Path::new("")))?;
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

/// The `Assistant` of Jarvis. It delegates each request to its use case.
pub struct App {
    /// The `handle` use case.
    conversation: Conversation,
    /// The `listen` use case.
    listener: Listener,
    /// The `speak` use case.
    speech: Arc<Speech>,
    /// The prefetch task. It holds a sender of the pools task, so it stops when the app drops.
    _prefetch: Option<AbortOnDropHandle<()>>,
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
/// for an answer to an open dialog of `queues`, warms `reasoner` and keeps the utterances with
/// their embeddings by `voice` for `log.correlate_s`.
fn listener(
    config: &Config,
    stt: Arc<dyn SpeechToText>,
    voice: Arc<dyn VoiceEmbedder>,
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
        stt, classifier, reasoner, policy, threshold, queues, clock, utterances, voice,
    )
}

/// A System One classifier and the name of its run.
pub struct RunClassifier {
    /// The run name, which the log records.
    pub run: String,
    /// The classifier.
    pub classifier: Arc<dyn Classifier>,
}

/// The System One model of the conversation, and the live sessions that guard each use of it.
/// A swap affects the next `predict`.
pub struct CurrentClassifier(pub ArcSwap<RunClassifier>, Option<Live>);

impl CurrentClassifier {
    /// The classifier `classifier` of the run `run`.
    #[must_use]
    pub fn new(run: String, classifier: Arc<dyn Classifier>) -> Self {
        let current = RunClassifier { run, classifier };
        Self(ArcSwap::from_pointee(current), None)
    }

    /// Swaps in `classifier` of the run `run`. Each use of it takes a read guard of the live
    /// sessions when they are set.
    pub fn swap(&self, run: String, classifier: Arc<dyn Classifier>) {
        let classifier = match &self.1 {
            Some(live) => Arc::new(live.with(classifier)),
            None => classifier,
        };
        // One store, so that a prediction and its run name come from the same model.
        self.0.store(Arc::new(RunClassifier { run, classifier }));
    }
}

#[async_trait]
impl Classifier for CurrentClassifier {
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction> {
        // One load, so that one prediction uses one classifier.
        let current = self.0.load_full();
        current.classifier.predict(context, text).await
    }
}

/// The STT, TTS and voice embedder sessions of [`LiveModels`].
pub struct Sessions {
    /// Both STT passes.
    pub stt: Arc<dyn SpeechToText>,
    /// The TTS.
    pub tts: Arc<dyn TextToSpeech>,
    /// The voice embedder.
    pub voice: Arc<dyn VoiceEmbedder>,
}

/// Loads the sessions and the System One model of the current run.
pub type Load =
    Box<dyn Fn() -> BoxFuture<'static, anyhow::Result<(Sessions, RunClassifier)>> + Send + Sync>;

/// A use of the sessions of [`LiveModels`]. Each use takes a read guard with `try_read`, so it
/// fails at once while `release` waits or after it.
#[derive(Clone, Default)]
pub struct Live {
    /// The sessions, `None` after `release`.
    sessions: Arc<RwLock<Option<Sessions>>>,
    /// Each System One model that a handle still holds.
    models: Arc<Mutex<Vec<Weak<Model>>>>,
    /// The System One model of this handle. `None` gives no prediction.
    model: Option<Arc<Model>>,
    /// The hotwords of the last `set_hotwords`, which `restore` gives to the new second pass.
    hotwords: Arc<Mutex<Vec<String>>>,
}

/// The System One model of one run. `release` empties it, also while a request holds it.
type Model = Mutex<Option<Arc<dyn Classifier>>>;

impl Live {
    /// The same sessions with `classifier`.
    fn with(&self, classifier: Arc<dyn Classifier>) -> Self {
        let model = Arc::new(Mutex::new(Some(classifier)));
        if let Ok(mut models) = self.models.lock() {
            models.retain(|model| model.strong_count() > 0);
            models.push(Arc::downgrade(&model));
        }
        Self {
            model: Some(model),
            ..self.clone()
        }
    }

    /// Empties each System One model that a handle still holds.
    fn empty(&self) {
        let Ok(models) = self.models.lock() else {
            return;
        };
        let held = models.iter().filter_map(Weak::upgrade);
        for model in held {
            if let Ok(mut model) = model.lock() {
                *model = None;
            }
        }
    }

    /// The hotwords of the last `set_hotwords`.
    fn hotwords(&self) -> MutexGuard<'_, Vec<String>> {
        self.hotwords.lock().expect("no lock holder panics")
    }

    /// A read guard of the loaded sessions, or an error while `release` waits or after it.
    fn read(&self) -> anyhow::Result<OwnedRwLockReadGuard<Option<Sessions>, Sessions>> {
        let guard = Arc::clone(&self.sessions).try_read_owned().ok();
        let loaded =
            guard.and_then(|guard| OwnedRwLockReadGuard::try_map(guard, Option::as_ref).ok());
        loaded.context("the models are released")
    }
}

impl SpeechToText for Live {
    fn stream(&self) -> anyhow::Result<Box<dyn SttStream>> {
        let guard = self.read()?;
        let stream = guard.stt.stream()?;
        Ok(Box::new(LiveStream {
            stream,
            _guard: guard,
        }))
    }

    fn transcribe(&self, samples: &[i16]) -> anyhow::Result<Option<String>> {
        self.read()?.stt.transcribe(samples)
    }

    fn set_hotwords(&self, phrases: &[String]) {
        // The lock stays held until the sessions get the list, so `restore` cannot miss it.
        let mut hotwords = self.hotwords();
        phrases.clone_into(&mut hotwords);
        if let Ok(sessions) = self.read() {
            sessions.stt.set_hotwords(phrases);
        }
    }
}

impl VoiceEmbedder for Live {
    fn embed(&self, audio: &Audio) -> anyhow::Result<Vec<f32>> {
        self.read()?.voice.embed(audio)
    }
}

#[async_trait]
impl TextToSpeech for Live {
    async fn synth(&self, text: &str) -> anyhow::Result<Audio> {
        let (sessions, text) = (self.read()?, text.to_owned());
        // A task keeps the guard until the synthesis ends, also when the caller stops waiting.
        tokio::spawn(async move { sessions.tts.synth(&text).await }).await?
    }
}

#[async_trait]
impl Classifier for Live {
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction> {
        let guard = self.read()?;
        let model = self
            .model
            .as_ref()
            .and_then(|model| model.lock().ok()?.clone());
        let model = model.context("no System One model")?;
        let (context, text) = (context.to_owned(), text.to_owned());
        // A task keeps the guard until the prediction ends, also when the caller stops waiting.
        tokio::spawn(async move {
            let _guard = guard;
            model.predict(&context, &text).await
        })
        .await?
    }
}

/// A first pass that keeps a read guard of the live sessions until it drops.
struct LiveStream {
    /// The first pass.
    stream: Box<dyn SttStream>,
    /// The read guard. It drops after the first pass.
    _guard: OwnedRwLockReadGuard<Option<Sessions>, Sessions>,
}

impl SttStream for LiveStream {
    fn accept(&mut self, samples: &[i16]) {
        self.stream.accept(samples);
    }

    fn partial(&self) -> Partial {
        self.stream.partial()
    }

    fn reset(&mut self) {
        self.stream.reset();
    }

    fn flush(&mut self) {
        self.stream.flush();
    }
}

/// The model sessions of Jarvis: both STT passes, TTS, the voice embedder and the System One
/// model.
pub struct LiveModels {
    /// The STT and TTS sessions, with no System One model.
    live: Live,
    /// The System One model of the conversation.
    classifier: Arc<CurrentClassifier>,
    /// Loads the sessions again.
    load: Load,
}

impl LiveModels {
    /// The sessions of `load`.
    ///
    /// # Errors
    ///
    /// A model cannot load.
    pub async fn try_new(load: Load) -> anyhow::Result<Self> {
        let (sessions, model) = load().await?;
        let live = Live::default();
        *live.sessions.write().await = Some(sessions);
        let classifier = Arc::new(live.with(model.classifier));
        let current = ArcSwap::from_pointee(RunClassifier {
            run: model.run,
            classifier,
        });
        let classifier = Arc::new(CurrentClassifier(current, Some(live.clone())));
        Ok(Self {
            live,
            classifier,
            load,
        })
    }

    /// The System One model of the conversation.
    #[must_use]
    pub fn classifier(&self) -> &Arc<CurrentClassifier> {
        &self.classifier
    }

    /// Both STT passes, the TTS and the voice embedder.
    #[must_use]
    pub fn live(&self) -> Arc<Live> {
        Arc::new(self.live.clone())
    }
}

#[async_trait]
impl Models for LiveModels {
    async fn release(&self) {
        let mut sessions = self.live.sessions.write().await;
        *sessions = None;
        self.live.empty();
    }

    async fn restore(&self) -> anyhow::Result<()> {
        let (sessions, model) = (self.load)().await?;
        let mut loaded = self.live.sessions.write().await;
        // A `set_hotwords` during the pause waits for this lock, then reaches the new sessions.
        let hotwords = self.live.hotwords();
        sessions.stt.set_hotwords(&hotwords);
        *loaded = Some(sessions);
        drop(loaded);
        drop(hotwords);
        self.classifier.swap(model.run, model.classifier);
        Ok(())
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
    /// Bearer token of the web API.
    pub web_token: String,
    /// MQTT login, when `[mqtt]` is set.
    pub mqtt: Option<MqttLogin>,
    /// `RunPod` keys, when `[train.runpod]` is set.
    pub runpod: Option<RunPodKeys>,
}

/// The keys of `RunPod`.
#[derive(Clone)]
pub struct RunPodKeys {
    /// The API key of the REST API.
    pub api_key: String,
    /// The access key of the S3 API.
    pub s3_key: String,
    /// The secret key of the S3 API.
    pub s3_secret: String,
}

/// The login of the MQTT broker.
#[derive(Clone)]
pub struct MqttLogin {
    /// The user name.
    pub username: String,
    /// The password.
    pub password: String,
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
        let secret = |name: &str| lookup_secret(&lookup, name);
        let (plex, mqtt) = (config.plugins.plex.as_ref(), config.mqtt.as_ref());
        Ok(Self {
            ha_token: secret(&config.ha.token_env)?,
            openai_key: secret(&config.openai.api_key_env)?,
            plex_token: plex.map(|plex| secret(&plex.token_env)).transpose()?,
            web_token: secret(&config.web.token_env)?,
            mqtt: mqtt
                .map(|mqtt| MqttLogin::resolve(mqtt, &lookup))
                .transpose()?,
            runpod: RunPodKeys::resolve(config.train.runpod.as_ref(), &lookup)?,
        })
    }
}

impl MqttLogin {
    /// Reads the user name and the password that `mqtt` names with `lookup`.
    ///
    /// # Errors
    ///
    /// A variable is not set or is empty. The error names the variable.
    fn resolve(mqtt: &MqttConfig, lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            username: lookup_secret(&lookup, &mqtt.username_env)?,
            password: lookup_secret(&lookup, &mqtt.password_env)?,
        })
    }
}

impl RunPodKeys {
    /// Reads the keys that `runpod` names with `lookup`, when `runpod` is set.
    ///
    /// # Errors
    ///
    /// A variable is not set or is empty. The error names the variable.
    fn resolve(
        runpod: Option<&RunPodConfig>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(runpod) = runpod else {
            return Ok(None);
        };
        Ok(Some(Self {
            api_key: lookup_secret(&lookup, &runpod.api_key_env)?,
            s3_key: lookup_secret(&lookup, &runpod.s3_key_env)?,
            s3_secret: lookup_secret(&lookup, &runpod.s3_secret_env)?,
        }))
    }
}

/// The value of the environment variable `name` from `lookup`.
///
/// # Errors
///
/// The variable is not set or is empty. The error names the variable.
fn lookup_secret(lookup: impl Fn(&str) -> Option<String>, name: &str) -> anyhow::Result<String> {
    match lookup(name) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(anyhow::anyhow!(
            "environment variable {name} is not set or is empty"
        )),
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
    /// Runs the command. A command other than `serve` gives `Stop`.
    ///
    /// # Errors
    ///
    /// The command fails.
    fn run(self) -> anyhow::Result<Ended> {
        match self {
            Self::Dataset { config, out, house } => {
                dataset(&config, &out, house.as_deref()).map(|()| Ended::Stop)
            }
            Self::Serve { config } => trial(&config, serve(&config)),
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

/// A started app, its pools task, habit task, log writer task, review and stop receiver, its
/// pre-render task, and its live models.
type Started = (
    App,
    (JoinHandle<()>, HabitsTask, Logged),
    JoinHandle<()>,
    Arc<LiveModels>,
);

/// The pools task, the queues, the habit task and the reasoner of an app.
type Parts = (PoolsTask, Arc<Queues>, HabitsTask, Arc<dyn Reasoner>);

/// The live models, the listener, the activation, the recorder and the log of an app.
type Loaded = (Arc<LiveModels>, Listener, Activation, (Recorder, Logged));

/// A log writer task, the review of its log, and the receiver of the error text of a failed
/// `restore` of the training run. Also the training trigger task, the learned intent check task
/// and the [`Shared`] of the trigger.
type Logged = (Tasks, Reviewer, watch::Receiver<Option<String>>, Shared);

/// The log writer task, the training trigger task and the learned intent check task.
type Tasks = (JoinHandle<()>, (JoinHandle<()>, JoinHandle<()>));

/// The `RunPod` trainer of the training trigger, and the tools and the live catalog of the check.
type Extras = (Option<Arc<dyn Trainer>>, (Vec<Tool>, Arc<ArcSwap<Catalog>>));

/// The sender of the pools task and the request habits of the learned intent check.
type Learn = (mpsc::Sender<Change>, Arc<Habits>);

/// The [`Extras`] and the [`Learn`] of the learned intent check.
type Checks = (Extras, Learn);

/// The plugins of a configuration and its media library, if any.
type Sources = (Vec<Arc<dyn Plugin>>, Option<Arc<dyn MediaLibrary>>);

/// The [`Checks`] with the activation in place of the live catalog and the pools task.
type Activated = (
    (Option<Arc<dyn Trainer>>, Vec<Tool>),
    (Activation, Arc<Habits>),
);

/// The training trigger task and the learned intent check task, and the review.
type Checked = ((JoinHandle<()>, JoinHandle<()>), Reviewer);

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

/// Starts Jarvis with the configuration file `file` of [`begin`] and the outside world `startup`: HA, the
/// sync, the System One model, the STT and TTS models, the pre-render and the Wyoming server.
/// When `startup.shutdown` completes or a save restarts Jarvis, stops the start, or stops
/// accepting connections and waits up to [`SHUTDOWN_WAIT`] for the running connections, the
/// work after their replies and the saves of the stores. Gives `Restart` after a save.
///
/// # Errors
///
/// A file cannot be read or is not valid, a secret is not set, a model cannot load, or the
/// server cannot bind or accept.
pub async fn run<Lookup, Connector, Transport, Stream>(
    file: &Path,
    startup: Startup<Lookup, Connector, impl Future<Output = ()>>,
) -> anyhow::Result<Ended>
where
    Lookup: Fn(&str) -> Option<String>,
    Connector: Fn(&str, u16) -> Transport + Send + 'static,
    Transport: Future<Output = io::Result<Stream>> + Send + 'static,
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let config = begin(file)?;
    let secrets = Secrets::resolve(&config, startup.lookup)?;
    let catalog = load_catalog(&config.intents)?;
    let pools = load_pools(&config.responses, &catalog)?;
    let home = start_home(&config.ha, &secrets.ha_token, startup.connector)?;
    let mut shutdown = pin::pin!(startup.shutdown);
    // The models can load for a long time, so a shutdown stops the start too.
    let started = tokio::select! {
        started = start_live_app(&config, &secrets, catalog, pools, home) => started?,
        () = &mut shutdown => return stopped(file),
    };
    serve_until(file.to_owned(), &config, &secrets, started, shutdown).await
}

/// `Stop` after a signal before the start of `file` was ready. Keeps the saved file for the next
/// trial start with `adapters::settings::cancel`.
///
/// # Errors
///
/// The marker of the trial start cannot be removed.
fn stopped(file: &Path) -> anyhow::Result<Ended> {
    adapters::settings::cancel(file)?;
    Ok(Ended::Stop)
}

/// Rolls back or marks the trial start with `adapters::settings::begin`, then loads the
/// configuration file `config`.
///
/// # Errors
///
/// A file of the trial start cannot change, or [`Config::load`] fails.
fn begin(config: &Path) -> anyhow::Result<Config> {
    adapters::settings::begin(config).context(NoRestart)?;
    Config::load(config)
}

/// Serves the app of `started` on `listen`, its review, the status and the settings of `file` on
/// `web.listen` with the web token of `secrets`, and the status entity on the broker of `mqtt`,
/// until `shutdown` completes, a save restarts Jarvis or the stop receiver of `started` gets the
/// error text of a failed `restore`. Then gives [`Controller::ended`], closes the status and waits
/// as [`drain`] does.
///
/// # Errors
///
/// `mqtt.url` is not valid, a server cannot bind or accept, a task of `started` panicked, or
/// `restore` failed.
async fn serve_until(
    file: PathBuf,
    config: &Config,
    secrets: &Secrets,
    started: Started,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<Ended> {
    let (app, (pools_task, habits, (tasks, review, mut stopped, shared)), _, _) = started;
    let (listener, tracker) = (TcpListener::bind(config.listen).await?, TaskTracker::new());
    let control = Arc::new(Controller::new(file, shared));
    let web = serve_control(config, secrets, review, &control, &tracker).await?;
    let shutdown = until_stopped(control.until_restart(shutdown), &mut stopped);
    let served = adapters::wyoming::serve(listener, Arc::new(app), &tracker, shutdown).await;
    drop(web);
    let ended = control.ended();
    drop(control);
    served?;
    drain(tracker, (pools_task, habits, tasks)).await?;
    failed(&stopped, ended)
}

/// Serves `review` and `control` with the `[web]` and the web token of `secrets` as [`serve_web`]
/// does, and starts the status entity of `control` with `config` and `secrets`, in tasks of
/// `tracker`. Gives the stop sender of the web server.
///
/// # Errors
///
/// The web server cannot bind, the trial start cannot end, or `mqtt.url` is not valid.
async fn serve_control(
    config: &Config,
    secrets: &Secrets,
    review: Reviewer,
    control: &Arc<Controller>,
    tracker: &TaskTracker,
) -> anyhow::Result<oneshot::Sender<()>> {
    let web = serve_web(&config.web, &secrets.web_token, review, control, tracker).await?;
    // After the bind, so a failed start leaves no `online` behind.
    start_status_entity(config, secrets, &control.status, tracker)?;
    Ok(web)
}

/// `ended`, or an error with the text of `stopped`.
///
/// # Errors
///
/// `stopped` has a text.
fn failed(stopped: &watch::Receiver<Option<String>>, ended: Ended) -> anyhow::Result<Ended> {
    let text = stopped.borrow().clone();
    text.map_or(Ok(ended), |text| Err(anyhow::anyhow!(text)))
}

/// Completes when `shutdown` completes or `stopped` gets a text.
async fn until_stopped(
    shutdown: impl Future<Output = ()>,
    stopped: &mut watch::Receiver<Option<String>>,
) {
    tokio::select! {
        () = shutdown => {}
        Ok(_) = stopped.wait_for(Option::is_some) => {}
    }
}

/// Starts the status entity of `status` in a task of `tracker` with the `[mqtt]` of `config` and
/// the login of `secrets`, when `[mqtt]` is set.
///
/// # Errors
///
/// `mqtt.url` is not valid.
fn start_status_entity(
    config: &Config,
    secrets: &Secrets,
    status: &watch::Sender<Status>,
    tracker: &TaskTracker,
) -> anyhow::Result<()> {
    if let (Some(mqtt), Some(login)) = (&config.mqtt, &secrets.mqtt) {
        let options = adapters::mqtt::options(&mqtt.url, &login.username, &login.password)?;
        tracker.spawn(adapters::mqtt::publish(options, status.subscribe()));
    }
    Ok(())
}

/// Serves `review` and `control` with `token` on `web.listen` in a task of `tracker` until the
/// sender that it gives drops. The Wyoming listener binds before, so after the bind the start is
/// ready, and `adapters::settings::confirm` ends a trial start before the first connection.
///
/// # Errors
///
/// The server cannot bind, or the trial start cannot end.
async fn serve_web(
    web: &WebConfig,
    token: &str,
    review: Reviewer,
    control: &Arc<Controller>,
    tracker: &TaskTracker,
) -> anyhow::Result<oneshot::Sender<()>> {
    let listener = TcpListener::bind(web.listen).await?;
    adapters::settings::confirm(&control.config)?;
    let (stop, stopped) = oneshot::channel();
    let web = adapters::web::Web {
        review: Arc::new(review),
        token: token.to_owned(),
        page_size: web.page_size,
        clock: Arc::new(SystemClock),
        control: Arc::clone(control) as _,
    };
    tracker.spawn(adapters::web::serve(listener, web, async {
        let _ = stopped.await;
    }));
    Ok(stop)
}

/// Closes `tracker` and waits up to [`SHUTDOWN_WAIT`] for its tasks: the connections, the work
/// after their replies, the web server and the status entity. Then waits for the pools task and
/// the log writer task of `stores`, and then for the last save of its habit task.
/// First stops the training trigger task and the learned intent check task of `stores`.
///
/// # Errors
///
/// A task of `stores` panicked.
async fn drain(
    tracker: TaskTracker,
    stores: (JoinHandle<()>, HabitsTask, Tasks),
) -> anyhow::Result<()> {
    let (pools_task, habits, (writer, (trigger, learning))) = stores;
    // The status entity ends only when the trigger task drops its status sender.
    trigger.abort();
    // The learned intent check holds a sender of the pools task.
    learning.abort();
    tracker.close();
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
/// `config`, the reasoner of `config` with the tools of that registry and the `OpenAI` API key
/// of `secrets`, and the `RunPod` trainer of [`runpod`]. Starts the title refresh of the media
/// library of [`plugins`]. Gives the app, its pools task and habit task, its pre-render task and
/// its live models.
///
/// # Errors
///
/// An HTTP client or the S3 store cannot be built, the saved pools, the habits or a model cannot
/// load, or `learn.file` is not a `.toml` or `.json` file.
async fn start_live_app(
    config: &Config,
    secrets: &Secrets,
    catalog: Catalog,
    pools: Pools,
    home: Arc<dyn Home>,
) -> anyhow::Result<Started> {
    let (plugins, library) = plugins(config, secrets)?;
    let plugins = registry(config, plugins);
    let tools = plugins.tools();
    let reasoner = Arc::new(OpenAiReasoner::try_new(
        &config.openai,
        &secrets.openai_key,
        tools,
        config.learn.responses,
    )?);
    let runpod = runpod(config, secrets)?;
    let catalog = Arc::new(catalog);
    let started = start_app(config, catalog, pools, home, reasoner, plugins, runpod).await?;
    if let Some(library) = library {
        started.0.conversation.set_library(Arc::clone(&library));
        app::background::start_titles(library, started.0.conversation.titles());
    }
    Ok(started)
}

/// The `RunPod` trainer of `[train.runpod]` of `config` with the keys of `secrets`, when it is set.
///
/// # Errors
///
/// The HTTP client or the S3 store cannot be built.
fn runpod(config: &Config, secrets: &Secrets) -> anyhow::Result<Option<Arc<dyn Trainer>>> {
    let (Some(runpod), Some(keys)) = (&config.train.runpod, &secrets.runpod) else {
        return Ok(None);
    };
    let trainer = RunPodTrainer::try_new(runpod, keys, config.model.dir.clone())?;
    Ok(Some(Arc::new(trainer)))
}

/// The plugin request files in `learn.plugin_requests`, when it is set.
fn plugin_requests(learn: &LearnConfig) -> Option<Arc<dyn PluginRequests>> {
    let folder = learn.plugin_requests.clone()?;
    Some(Arc::new(RequestFiles::new(folder)))
}

/// The plugins of `config`: the Open-Meteo weather plugin, and the Plex plugin with the token of
/// `secrets` when `[plugins.plex]` is set. Also the Plex plugin as the media library.
///
/// # Errors
///
/// An HTTP client cannot be built.
fn plugins(config: &Config, secrets: &Secrets) -> anyhow::Result<Sources> {
    let mut plugins: Vec<Arc<dyn Plugin>> =
        vec![Arc::new(OpenMeteo::try_new(&config.plugins.weather)?)];
    let (Some(plex), Some(token)) = (&config.plugins.plex, &secrets.plex_token) else {
        return Ok((plugins, None));
    };
    let plex = Arc::new(Plex::try_new(plex, token, Arc::new(SystemClock))?);
    plugins.push(Arc::clone(&plex) as _);
    Ok((plugins, Some(plex)))
}

/// The registry of `plugins` with the `[plugins]` of `config` and the system time.
fn registry(config: &Config, plugins: Vec<Arc<dyn Plugin>>) -> Arc<Plugins> {
    Arc::new(Plugins::new(
        config.plugins.clone(),
        Arc::new(SystemClock),
        plugins,
    ))
}

/// The app of [`start_app_with`] with System Two on `reasoner` and the plugin registry `plugins`,
/// and with the `RunPod` trainer `runpod`, the tools of `plugins` and [`learned`].
///
/// # Errors
///
/// [`learned`] or [`start_app_with`] fails.
async fn start_app(
    config: &Config,
    catalog: Arc<Catalog>,
    pools: Pools,
    home: Arc<dyn Home>,
    reasoner: Arc<dyn Reasoner>,
    plugins: Arc<Plugins>,
    runpod: Option<Arc<dyn Trainer>>,
) -> anyhow::Result<Started> {
    let tools = plugins.tools();
    let system_two = system_two(config, Arc::clone(&reasoner), plugins);
    let (live, pools) = learned(config, &catalog, pools)?;
    let extras = (runpod, (tools, live));
    start_app_with(config, catalog, pools, home, reasoner, system_two, extras).await
}

/// The app of `config`, `catalog` and the source pools `pools` on `home`, `reasoner` and
/// `system_two`, its pools task and habit task, its pre-render task and its live models. Starts
/// the sync, the pools task and the habit task, loads the live models, and starts the training
/// trigger and the learned intent check with `extras`. Then starts the pre-render.
///
/// # Errors
///
/// The saved pools, the habits, the training state, a model or the log cannot load, or
/// `learn.file` is not a `.toml` or `.json` file.
async fn start_app_with(
    config: &Config,
    catalog: Arc<Catalog>,
    pools: Pools,
    home: Arc<dyn Home>,
    reasoner: Arc<dyn Reasoner>,
    system_two: SystemTwo,
    extras: Extras,
) -> anyhow::Result<Started> {
    let (cache, queues, task, habits) = start_tasks(&home, config, pools)?;
    let (models, listener, activation, (recorder, logged)) = load_models(
        config,
        &reasoner,
        &queues,
        home,
        &cache,
        &catalog,
        (extras, (task.changes.clone(), habits.habits.clone())),
    )
    .await?;
    let prerender = start_prerender(config, &activation.speech, &task.pools, &cache);
    start_report(config, &cache);
    let parts = (task, queues, habits, reasoner);
    let loaded = (models, listener, activation, (recorder, logged));
    Ok(started(config, parts, system_two, loaded, prerender))
}

/// The app of `config`, `parts`, `system_two`, `loaded` and `prerender`. Starts its prefetch.
fn started(
    config: &Config,
    (task, queues, habits, reasoner): Parts,
    system_two: SystemTwo,
    (models, listener, activation, (recorder, logged)): Loaded,
    prerender: JoinHandle<()>,
) -> Started {
    let mut started = (
        App {
            conversation: Conversation::new(
                task.pools,
                backfill(config, &reasoner, &activation),
                queues,
                Arc::clone(models.classifier()),
                Arc::clone(&activation.catalog),
                config.model.threshold,
                system_two,
                habits.habits.clone(),
                recorder,
            ),
            listener,
            speech: Arc::clone(&activation.speech),
            _prefetch: None,
        },
        (task.task, habits, logged),
        prerender,
        models,
    );
    let conversation = started.0.conversation.clone();
    let habits = Arc::clone(&started.1.1.habits);
    let prefetch = Prefetch::new(conversation, activation, habits, config);
    let task = AbortOnDropHandle::new(app::background::start_prefetch(prefetch));
    started.0 = App {
        _prefetch: Some(task),
        ..started.0
    };
    started
}

/// The recorder of the utterances of `listener` on the log of `config` with the training trigger,
/// its log writer task, the review of the log with the activation of `checks`, and the receiver
/// of the error text of a failed `restore`. Starts the training trigger on the log with
/// `reasoner`, `catalog`, the hot cache `cache`, the `RunPod` trainer of `checks`, the house
/// `home` and the live models `models`, and the learned intent check of [`start_checks`] with the
/// plugin tools, the activation and the request habits of `checks`. Also gives the two check tasks and the
/// [`Shared`] of the trigger.
///
/// # Errors
///
/// The log cannot open, or a store path of `[train]` is not a `.toml` or `.json` file.
fn start_log(
    config: &Config,
    listener: &Listener,
    catalog: &Arc<Catalog>,
    reasoner: &Arc<dyn Reasoner>,
    cache: &watch::Receiver<StateCache>,
    ((runpod, tools), learn): Activated,
    (home, models): (Arc<dyn Home>, Arc<LiveModels>),
) -> anyhow::Result<(Recorder, Logged)> {
    let (log, last_id) = open_log(config)?;
    let (stop, stopped) = watch::channel(None);
    let shared = Shared::default();
    let trigger = trigger(
        config,
        Arc::clone(reasoner),
        &log,
        catalog,
        cache,
        runpod,
        (home, models, stop, shared.clone()),
    )?;
    let (recorded, writer) = recorder(config, listener, &log, last_id, &trigger)?;
    let (checks, review) = start_checks(
        config,
        trigger,
        (log, &recorded.speakers),
        (reasoner, catalog),
        (learn, tools),
    );
    Ok((recorded, ((writer, checks), review, stopped, shared)))
}

/// Starts the training trigger `trigger` each `train.check_interval_s`, and the learned intent
/// check of `config` on `log` with `reasoner`, `catalog`, the activation and the request habits
/// of `learn` and the plugin tools `tools` each `learn.check_interval_s`. Gives both tasks, and the
/// review of `log` and the voice profiles of `speakers` with the activation of the check.
fn start_checks(
    config: &Config,
    trigger: Arc<Trigger>,
    (log, speakers): (Arc<dyn InteractionLog>, &Arc<Speakers>),
    (reasoner, catalog): (&Arc<dyn Reasoner>, &Arc<Catalog>),
    ((activation, habits), tools): ((Activation, Arc<Habits>), Vec<Tool>),
) -> Checked {
    let voices = Arc::clone(&speakers.voices);
    let review = Reviewer::new(Arc::clone(&log), activation.clone(), voices);
    let learning = Learning {
        config: config.learn.clone(),
        log,
        reasoner: Arc::clone(reasoner),
        catalog: Arc::clone(catalog),
        tools,
        activation,
        habits,
        requests: plugin_requests(&config.learn),
        min_score: config.habits.min_score,
    };
    let learn = Duration::from_secs(config.learn.check_interval_s);
    let train = Duration::from_secs(config.train.check_interval_s);
    let tasks = (
        app::training::start_trigger(trigger, train),
        app::background::start_learning(learning, learn),
    );
    (tasks, review)
}

/// The log of `config` and its largest stored ID. Starts the daily prune of the log.
///
/// # Errors
///
/// The log cannot open.
fn open_log(config: &Config) -> anyhow::Result<(Arc<dyn InteractionLog>, Option<u64>)> {
    let log = SqliteLog::open(&config.log)?;
    let last_id = log.last_id()?;
    let log: Arc<dyn InteractionLog> = Arc::new(log);
    tokio::spawn(app::background::prune(
        Arc::clone(&log),
        Arc::new(SystemClock),
    ));
    Ok((log, last_id))
}

/// The speaker identification of `config`, with the voice store `speaker.voices`.
///
/// # Errors
///
/// The voice store path is not a `.toml` or `.json` file, or the store cannot load.
fn speakers(config: &Config) -> anyhow::Result<Arc<Speakers>> {
    let store = Arc::new(FileStore::try_new(config.speaker.voices.clone())?);
    Ok(Arc::new(Speakers {
        voices: Arc::new(SherpaVoices::try_new(&config.speaker, store)?),
        config: config.speaker.clone(),
        people: config.people.clone(),
        satellites: config.satellites.clone(),
        addresses: config.address.clone(),
    }))
}

/// The recorder of the utterances of `listener` and `log` with the largest stored ID `last_id`,
/// `trigger` and the speaker identification of `config`, and its new log writer task.
///
/// # Errors
///
/// The voice store cannot load.
fn recorder(
    config: &Config,
    listener: &Listener,
    log: &Arc<dyn InteractionLog>,
    last_id: Option<u64>,
    trigger: &Arc<Trigger>,
) -> anyhow::Result<(Recorder, JoinHandle<()>)> {
    let speakers = speakers(config)?;
    let (writer, task) = app::conversation::start_writer(Arc::clone(log));
    let recorder = Recorder {
        utterances: Arc::clone(listener.utterances()),
        writer,
        last_id,
        trigger: Arc::clone(trigger),
        speakers,
    };
    Ok((recorder, task))
}

/// The training trigger of `config` with the backoff `train.failure_backoff_s`, the process
/// trainer, the `RunPod` trainer `runpod`, the ONNX loader, the stores of `train.state`,
/// `train.generated` and `learn.file`, the system time, `reasoner`, `log`, `catalog`, the hot
/// cache `cache`, the house `home`, the live models `models`, whose classifier the run swaps, and
/// `stop`, which gets the error text of a failed `restore`. The run and the trigger use the status
/// and the activity of `shared`.
///
/// # Errors
///
/// A store path is not a `.toml` or `.json` file.
fn trigger(
    config: &Config,
    reasoner: Arc<dyn Reasoner>,
    log: &Arc<dyn InteractionLog>,
    catalog: &Arc<Catalog>,
    cache: &watch::Receiver<StateCache>,
    runpod: Option<Arc<dyn Trainer>>,
    (home, models, stop, shared): (
        Arc<dyn Home>,
        Arc<LiveModels>,
        watch::Sender<Option<String>>,
        Shared,
    ),
) -> anyhow::Result<Arc<Trigger>> {
    let train = &config.train;
    let training = Training {
        reasoner,
        generated: Arc::new(FileStore::try_new(train.generated.clone())?),
        state: Arc::new(FileStore::try_new(train.state.clone())?),
        log: Arc::clone(log),
        trainer: Arc::new(process_trainer(config)),
        runpod,
        loader: Arc::new(OnnxLoader),
        classifier: Arc::clone(models.classifier()),
        catalog: Arc::clone(catalog),
        learned: Arc::new(FileStore::try_new(config.learn.file.clone())?),
        cache: cache.clone(),
        clock: Arc::new(SystemClock),
        config: config.clone(),
        home,
        models,
        stop,
        status: shared.status,
    };
    Ok(Arc::new(Trigger::new(
        Arc::new(training),
        backoff(train)?,
        shared.activity,
    )))
}

/// The failure backoff `train.failure_backoff_s` of `train`.
///
/// # Errors
///
/// The backoff is too large for a `SignedDuration`.
fn backoff(train: &TrainConfig) -> anyhow::Result<SignedDuration> {
    Ok(SignedDuration::try_from(Duration::from_secs(
        train.failure_backoff_s,
    ))?)
}

/// The process trainer of `config`.
fn process_trainer(config: &Config) -> ProcessTrainer {
    let train = &config.train;
    ProcessTrainer {
        python: train.python.clone(),
        dir: config.model.dir.clone(),
        device: config.device,
        max_vram_gb: train.max_vram_gb,
        timeout: Duration::from_secs(train.timeout_min * 60),
    }
}

/// Starts the sync of a hot cache with `home`, and the pools task of the source pools `pools` and
/// the habit task of `config` on it. Gives the hot cache, its announce queues and the two tasks.
///
/// # Errors
///
/// The saved pools or the habit file cannot load.
fn start_tasks(
    home: &Arc<dyn Home>,
    config: &Config,
    pools: Pools,
) -> anyhow::Result<(
    watch::Receiver<StateCache>,
    Arc<Queues>,
    PoolsTask,
    HabitsTask,
)> {
    let (cache, queues) = start_sync(Arc::clone(home), config);
    let (task, habits) = start_stores(config, pools, &cache)?;
    Ok((cache, queues, task, habits))
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

/// The live catalog of `catalog` and the entries of `learn.file` of `config`, and the source pools
/// `pools` with the pool of each entry with an intent and a new name. Fails when the file or its
/// store cannot load.
fn learned(
    config: &Config,
    catalog: &Catalog,
    mut pools: Pools,
) -> anyhow::Result<(Arc<ArcSwap<Catalog>>, Pools)> {
    let store = FileStore::try_new(config.learn.file.clone())?;
    let entries = Store::<Learned>::load(&store)?.unwrap_or_default().intents;
    let live = domain::learn::live(catalog.clone(), entries.clone());
    for entry in entries {
        if !pools.contains_key(&entry.name) && domain::learn::intent(entry.clone()).is_some() {
            pools.insert(entry.name, app::background::pool(&entry.responses));
        }
    }
    Ok((Arc::new(ArcSwap::from_pointee(live)), pools))
}

/// The activation, alone and in `checks`, of `learn.file` of `config` with the speech use case on
/// `tts`, the hot cache `cache` and the system time. Fails when the path of
/// `learn.file` is not a `.toml` or `.json` file.
fn activation(
    config: &Config,
    ((runpod, (tools, catalog)), (changes, habits)): Checks,
    tts: Arc<dyn TextToSpeech>,
    cache: &watch::Receiver<StateCache>,
) -> anyhow::Result<(Activation, Activated)> {
    let activation = Activation {
        store: Arc::new(FileStore::try_new(config.learn.file.clone())?),
        changes,
        speech: Arc::new(Speech::new(tts)),
        cache: cache.clone(),
        catalog,
        clock: Arc::new(SystemClock),
    };
    Ok((activation.clone(), ((runpod, tools), (activation, habits))))
}

/// The backfill of `config` with `reasoner`, and the pools task, the pre-render, the live catalog
/// and the hot cache of `activation`.
fn backfill(
    config: &Config,
    reasoner: &Arc<dyn Reasoner>,
    activation: &Activation,
) -> Arc<Backfill> {
    let (reasoner, speech) = (Arc::clone(reasoner), Arc::clone(&activation.speech));
    let (changes, catalog) = (activation.changes.clone(), Arc::clone(&activation.catalog));
    let (backfill, cache) = (config.backfill.clone(), activation.cache.clone());
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
        prefetch_services: config.learn.prefetch_services.clone(),
        answers: Arc::default(),
    }
}

/// The live models of `config`, then the listener on their STT and voice embedder, which predicts
/// with their System One model, sees the dialogs of `queues` and warms `reasoner`, the
/// [`activation`] on their TTS, and the recorder of [`start_log`]. Starts the hotword refresh of
/// the STT with `home`, `cache` and `catalog`, and the training trigger and the learned intent
/// check with `checks`, `home`, the live models and the [`activation`].
///
/// # Errors
///
/// The training state, a model, the log or the store of `learn.file` cannot load.
async fn load_models(
    config: &Config,
    reasoner: &Arc<dyn Reasoner>,
    queues: &Arc<Queues>,
    home: Arc<dyn Home>,
    cache: &watch::Receiver<StateCache>,
    catalog: &Arc<Catalog>,
    checks: Checks,
) -> anyhow::Result<(Arc<LiveModels>, Listener, Activation, (Recorder, Logged))> {
    let models = Arc::new(LiveModels::try_new(load(config)).await?);
    let live = models.live();
    let stt = Arc::clone(&live) as _;
    let (activation, checks) = activation(config, checks, Arc::clone(&live) as _, cache)?;
    start_hotwords(config, &home, cache, catalog, Arc::clone(&stt));
    let classifier = Arc::clone(models.classifier()) as _;
    let listener = listener(config, stt, live, classifier, Arc::clone(reasoner), queues);
    let recorded = start_log(
        config,
        &listener,
        catalog,
        reasoner,
        cache,
        checks,
        (home, Arc::clone(&models)),
    )?;
    Ok((models, listener, activation, recorded))
}

/// The load of [`load_sessions`] of `config`.
fn load(config: &Config) -> Load {
    let config = config.clone();
    Box::new(move || Box::pin(load_sessions(config.clone())))
}

/// The STT, TTS and voice embedder sessions of `config`, loaded on a blocking thread, and the System One model of
/// [`load_classifier`].
///
/// # Errors
///
/// The training state or a model cannot load.
async fn load_sessions(config: Config) -> anyhow::Result<(Sessions, RunClassifier)> {
    let model = load_classifier(&config).await?;
    let sessions = tokio::task::spawn_blocking(move || {
        let device = config.device;
        anyhow::Ok(Sessions {
            stt: Arc::new(SherpaStt::try_new(&config.asr, device)?),
            tts: Arc::new(SupertonicTts::try_new(&config.tts, device)?),
            voice: Arc::new(SherpaEmbedder::try_new(&config.speaker, device)?),
        })
    });
    Ok((sessions.await??, model))
}

/// The System One model of the run that `train.state` of `config` names, else of the fixture.
///
/// # Errors
///
/// The training state or the model cannot load.
async fn load_classifier(config: &Config) -> anyhow::Result<RunClassifier> {
    let state: Option<TrainingState> = FileStore::try_new(config.train.state.clone())?.load()?;
    let run = state.and_then(|state| state.current);
    let run = run.unwrap_or_else(|| FIXTURE.to_owned());
    let classifier = OnnxLoader
        .load(&config.model.dir.join(&run), config.device)
        .await?;
    Ok(RunClassifier { run, classifier })
}

/// Starts the hotword refresh of `stt` with `home`, `cache` and `catalog` when `asr.hotwords` is
/// true and `asr.second_pass_dir` is set in `config`.
fn start_hotwords(
    config: &Config,
    home: &Arc<dyn Home>,
    cache: &watch::Receiver<StateCache>,
    catalog: &Arc<Catalog>,
    stt: Arc<dyn SpeechToText>,
) {
    if config.asr.hotwords && config.asr.second_pass_dir.is_some() {
        app::background::start_hotwords(Arc::clone(home), cache.clone(), Arc::clone(catalog), stt);
    }
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
/// in `cache` and the non-empty forms of address of `config`, and gives its handle.
fn start_prerender(
    config: &Config,
    speech: &Arc<Speech>,
    pools: &Arc<ArcSwap<Pools>>,
    cache: &watch::Receiver<StateCache>,
) -> JoinHandle<()> {
    let (speech, pools, mut cache) = (Arc::clone(speech), Arc::clone(pools), cache.clone());
    let addresses = config
        .address
        .values()
        .filter(|address| !address.is_empty());
    let addresses: Vec<String> = addresses.cloned().collect();
    tokio::spawn(async move {
        if cache.changed().await.is_err() {
            return;
        }
        let areas = app::background::area_names(&cache.borrow());
        if let Err(error) = speech
            .prerender_addressed(&pools.load_full(), &areas, &addresses)
            .await
        {
            tracing::warn!("the pre-render failed: {error:#}");
        }
    })
}

/// Starts a task that logs the discovery report of the first snapshot in `cache`, with the
/// overrides of `config`.
fn start_report(config: &Config, cache: &watch::Receiver<StateCache>) {
    let (people, satellites) = (config.people.clone(), config.satellites.clone());
    let mut cache = cache.clone();
    tokio::spawn(async move {
        if cache.changed().await.is_err() {
            return;
        }
        let report = domain::speaker::discovery_report(&cache.borrow(), &people, &satellites);
        for line in report {
            tracing::info!("discovery: {line}");
        }
    });
}

/// Sets a panic hook that, after the default hook, exits with `adapters::settings::panic_code`
/// of `config` when the main thread panics.
fn exit_on_panic(config: &Path) {
    let (file, default) = (config.to_owned(), panic::take_hook());
    panic::set_hook(Box::new(move |info| {
        default(info);
        // A panic of a `tokio` task is an error of `run`.
        if thread::current().name() == Some("main") {
            process::exit(adapters::settings::panic_code(&file));
        }
    }));
}

/// Runs Jarvis with the configuration file `config`, the environment and TCP, until it fails,
/// gets SIGINT or SIGTERM, or a save restarts it. A panic of the main thread exits as
/// [`exit_on_panic`] says.
///
/// # Errors
///
/// A signal handler cannot be registered, or `run` fails.
fn serve(config: &Path) -> anyhow::Result<Ended> {
    exit_on_panic(config);
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
    exit_code(command.run())
}

/// The context of an error of the rollback or of the mark of the trial start. It ends Jarvis
/// without a restart, so a rollback that keeps failing cannot restart Jarvis in a loop.
#[derive(Debug)]
struct NoRestart;

impl fmt::Display for NoRestart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the settings rollback failed")
    }
}

/// `served`, the result of `serve` with the configuration file `config`, or `Restart` after an
/// error of the trial start, which goes to `<config>.trying`.
fn trial(config: &Path, served: anyhow::Result<Ended>) -> anyhow::Result<Ended> {
    match served {
        Err(error)
            if !error.is::<NoRestart>()
                && adapters::settings::failed_trial(config, &format!("{error:#}")) =>
        {
            eprintln!("{error:#}");
            Ok(Ended::Restart)
        }
        served => served,
    }
}

/// The exit code of `ended`, the result of a command.
fn exit_code(ended: anyhow::Result<Ended>) -> ExitCode {
    match ended {
        Ok(Ended::Restart) => ExitCode::from(adapters::settings::RESTART),
        Ok(Ended::Stop) => ExitCode::SUCCESS,
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

    use std::any::Any;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::future;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
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
    use crate::domain::learn;
    use crate::domain::learn::LearnedStatus;
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
    use crate::testing::fakes::FakeVoiceEmbedder;
    use crate::testing::fakes::FakeWyomingClient;
    use crate::testing::fakes::MqttSubscriber;

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
            hotwords: true,
            hotwords_score: 1.5,
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
            embedding_model: "text-embedding-3-small".to_owned(),
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

    /// The `[train]` section of `jarvis.toml`.
    fn expected_train() -> TrainConfig {
        TrainConfig {
            state: PathBuf::from("training.json"),
            generated: PathBuf::from("generated.json"),
            base_model: "sentence-transformers/all-MiniLM-L12-v2".to_owned(),
            epochs: 8,
            batch_size: 8,
            min_changes: 20,
            check_interval_s: 600,
            holdout: 0.1,
            failure_backoff_s: 21600,
            timeout_min: 60,
            max_vram_gb: 4.5,
            paraphrases: 50,
            unknown_paraphrases: 300,
            python: PathBuf::from(".venv/bin/python"),
            runpod: None,
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

    /// The `[web]` section of `jarvis.toml`.
    fn expected_web() -> WebConfig {
        WebConfig {
            listen: SocketAddr::from(([0, 0, 0, 0], 10701)),
            token_env: "JARVIS_WEB_TOKEN".to_owned(),
            page_size: 50,
        }
    }

    /// The `[learn]` section of `VALID`.
    fn expected_learn() -> LearnConfig {
        LearnConfig {
            window_days: 14,
            min_cluster: 5,
            cluster_distance: 0.3,
            responses: 10,
            file: PathBuf::from("learned.toml"),
            check_interval_s: 3600,
            prefetch_lead_min: 5,
            prefetch_services: vec!["calendar.get_events".to_owned()],
            plugin_requests: Some(PathBuf::from("plugin-requests")),
        }
    }

    /// The `[mqtt]` section of `VALID`.
    fn expected_mqtt() -> MqttConfig {
        MqttConfig {
            url: "mqtt://homeassistant.local:1883".to_owned(),
            username_env: "MQTT_USERNAME".to_owned(),
            password_env: "MQTT_PASSWORD".to_owned(),
        }
    }

    /// The `[speaker]` section of `VALID`.
    fn expected_speaker() -> SpeakerConfig {
        SpeakerConfig {
            model: PathBuf::from("models/wespeaker_en_voxceleb_CAM++.onnx"),
            voices: PathBuf::from("voices.json"),
            voice_threshold: 0.6,
            max_samples: 20,
            threshold: 0.6,
            margin: 0.15,
            enroll_threshold: 0.5,
            min_enroll_ms: 1500,
            min_profile: 5,
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
            train: expected_train(),
            learn: expected_learn(),
            log: expected_log(),
            web: expected_web(),
            mqtt: Some(expected_mqtt()),
            speaker: expected_speaker(),
            people: Vec::new(),
            satellites: Vec::new(),
            address: BTreeMap::new(),
        };
        assert_eq!(Config::parse(VALID)?, expected);
        Ok(())
    }

    /// AC-15.1-4.
    #[test]
    fn ac_15_1_4_config_without_people_has_no_overrides() -> anyhow::Result<()> {
        assert!(!VALID.contains("\n[[people]]"));
        assert_eq!(Config::parse(VALID)?.people, []);
        Ok(())
    }

    /// A zero `learn.check_interval_s` is an error, so the check task never gets a zero interval.
    #[test]
    fn zero_learn_interval_names_field_and_range() {
        let text = VALID.replace("check_interval_s = 3600", "check_interval_s = 0");
        let error = parse_error(&text);
        assert!(
            error.contains("learn.check_interval_s") && error.contains("more than 0"),
            "error for learn.check_interval_s = 0 has no field or range: {error}"
        );
    }

    /// A `[speaker]` score out of its range gives an error with the field and the range.
    #[test]
    fn speaker_score_out_of_range_names_field_and_range() {
        let cases = [
            ("\nthreshold = 0.6 #", "\nthreshold = 0.0 #", "more than 0"),
            ("margin = 0.15", "margin = -0.1", "more than 0"),
            (
                "enroll_threshold = 0.5",
                "enroll_threshold = 1.5",
                "in [0, 1]",
            ),
        ];
        for (field, broken, range) in cases {
            let name = format!(
                "speaker.{}",
                broken.trim().split(' ').next().unwrap_or_default()
            );
            let error = parse_error(&VALID.replace(field, broken));
            assert!(
                error.contains(&name) && error.contains(range),
                "error for {broken} has no field or range: {error}"
            );
        }
    }

    /// A zero `speaker.min_enroll_ms` or `speaker.min_profile` gives an error with the field and
    /// the range.
    #[test]
    fn zero_enroll_limit_names_field_and_range() {
        for (field, value) in [("min_enroll_ms", 1500), ("min_profile", 5)] {
            let text = VALID.replace(&format!("{field} = {value}"), &format!("{field} = 0"));
            let error = parse_error(&text);
            assert!(
                error.contains(&format!("speaker.{field}")) && error.contains("more than 0"),
                "error for speaker.{field} = 0 has no field or range: {error}"
            );
        }
    }

    /// A zero `speaker.max_samples` is an error, so `enroll` never empties a profile.
    #[test]
    fn zero_max_samples_names_field_and_range() {
        let text = VALID.replace("max_samples = 20", "max_samples = 0");
        let error = parse_error(&text);
        assert!(
            error.contains("speaker.max_samples") && error.contains("more than 0"),
            "error for speaker.max_samples = 0 has no field or range: {error}"
        );
    }

    /// AC-14.5c-2.
    #[test]
    fn ac_14_5c_2_jarvis_toml_has_learn_file_and_interval() -> anyhow::Result<()> {
        let learn = Config::parse(VALID)?.learn;
        assert_eq!(learn.file, PathBuf::from("learned.toml"));
        assert_eq!(learn.check_interval_s, 3600);
        Ok(())
    }

    /// A `none` entry that reuses the name of an active entry adds nothing to its start pool.
    #[test]
    fn start_pool_has_only_the_active_entry() -> anyhow::Result<()> {
        let (folder, mut config) = (tempfile::tempdir()?, Config::parse(VALID)?);
        config.learn.file = folder.path().join("learned.toml");
        let mut reused = fakes::learned("tell_joke", LearnedStatus::None);
        reused.responses = vec!["Not a joke.".to_owned()];
        let intents = vec![fakes::learned("tell_joke", LearnedStatus::Active), reused];
        FileStore::try_new(config.learn.file.clone())?.save(&Learned { intents })?;
        let (_, pools) = learned(&config, &load_catalog(&config.intents)?, Pools::new())?;
        assert_eq!(pools["tell_joke"].responses[0].text, "Good night.");
        Ok(())
    }

    /// AC-14.9-5.
    #[test]
    fn ac_14_9_5_prefetch_lead_is_1_to_60() -> anyhow::Result<()> {
        let with_lead = |lead: u64| {
            VALID.replace(
                "prefetch_lead_min = 5",
                &format!("prefetch_lead_min = {lead}"),
            )
        };
        for lead in [0, 61] {
            let error = parse_error(&with_lead(lead));
            assert!(
                error.contains("learn.prefetch_lead_min"),
                "error for prefetch_lead_min = {lead} does not name it: {error}"
            );
        }
        let lead = Config::parse(&with_lead(60))?.learn.prefetch_lead_min;
        let usual = [Bucket {
            day: DayType::Weekdays,
            hour: 8,
        }];
        let zone = jiff::tz::TimeZone::get("America/Chicago")?;
        // 2026-11-16 is a Monday.
        for local in ["2026-11-16T07:00:00", "2026-11-16T08:59:59"] {
            let now: jiff::Zoned = format!("{local}[America/Chicago]").parse()?;
            assert!(
                learn::in_window(&usual, now.timestamp(), &zone, lead),
                "the window of weekdays 08:00 with a lead of 60 min is closed at {local}"
            );
        }
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
            ("train.check_interval_s", 600),
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

    /// AC-18.17-8.
    #[test]
    fn ac_18_17_8_zero_hotwords_score_names_field_and_range() {
        let error = parse_error(&VALID.replace("hotwords_score = 1.5", "hotwords_score = 0"));
        assert!(
            error.contains("asr.hotwords_score") && error.contains("more than 0"),
            "error has no field or range: {error}"
        );
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

    /// The secrets of `VALID`.
    const ENVIRONMENT: [(&str, &str); 6] = [
        ("HA_TOKEN", "abc"),
        ("OPENAI_API_KEY", "k"),
        ("PLEX_TOKEN", "p"),
        ("JARVIS_WEB_TOKEN", "w"),
        ("MQTT_USERNAME", "u"),
        ("MQTT_PASSWORD", "m"),
    ];

    /// Resolves the secrets of the configuration `text` from an environment with `variables`.
    fn resolve_in(text: &str, variables: &[(&str, &str)]) -> anyhow::Result<Secrets> {
        let environment: BTreeMap<&str, &str> = variables.iter().copied().collect();
        Secrets::resolve(&Config::parse(text)?, |name| {
            environment.get(name).map(|value| (*value).to_owned())
        })
    }

    /// Resolves the secrets of `VALID` from an environment with `variables`.
    fn resolve(variables: &[(&str, &str)]) -> anyhow::Result<Secrets> {
        resolve_in(VALID, variables)
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
        let secrets = resolve(&ENVIRONMENT)?;
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

    /// AC-13.8c-1.
    #[test]
    fn ac_13_8c_1_no_section_needs_no_runpod_keys() -> anyhow::Result<()> {
        let secrets = resolve(&ENVIRONMENT)?;
        assert!(
            secrets.runpod.is_none(),
            "RunPod keys without [train.runpod]"
        );
        Ok(())
    }

    /// AC-13.8c-2.
    #[test]
    fn ac_13_8c_2_section_needs_the_s3_secret() {
        let lines = VALID.lines().skip_while(|line| *line != "# [train.runpod]");
        let section: Vec<&str> = lines.map_while(|line| line.strip_prefix("# ")).collect();
        let text = VALID.replace(&format!("# {}", section.join("\n# ")), &section.join("\n"));
        let keys = [("RUNPOD_API_KEY", "a"), ("RUNPOD_S3_KEY", "s")];
        let variables = [ENVIRONMENT.as_slice(), &keys].concat();
        let Err(error) = resolve_in(&text, &variables) else {
            panic!("resolve without RUNPOD_S3_SECRET passed");
        };
        assert!(error.to_string().contains("RUNPOD_S3_SECRET"), "{error}");
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
        let tools = registry(&config, plugins(&config, &secrets)?.0).tools();
        Ok(tools.into_iter().map(|tool| tool.name).collect())
    }

    /// AC-17.3b-1.
    #[test]
    fn ac_17_3b_1_registry_has_weather() -> anyhow::Result<()> {
        let names = tool_names(VALID)?;
        assert!(names.contains(&"get_weather".to_owned()), "{names:?}");
        Ok(())
    }

    /// AC-14.12-5.
    #[test]
    fn ac_14_12_5_no_folder_makes_no_plugin_requests() -> anyhow::Result<()> {
        let line = "plugin_requests = \"plugin-requests\"\n";
        assert!(
            VALID.contains(line),
            "jarvis.toml has no learn.plugin_requests"
        );
        assert!(plugin_requests(&Config::parse(VALID)?.learn).is_some());
        let config = Config::parse(&VALID.replace(line, ""))?;
        assert!(
            plugin_requests(&config.learn).is_none(),
            "plugin requests without a folder"
        );
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
        let cache = watch::channel(StateCache::default()).1;
        let habits = Habits::new(RequestHabits::default(), 14.0, cache.clone());
        let reasoner: Arc<dyn Reasoner> = Arc::new(FakeReasoner::default());
        let activation = activation_of(task.changes, speech, cache, catalog);
        let backfill = backfill(config, &reasoner, &activation);
        let (queues, threshold) = (queues(config), config.model.threshold);
        Ok(Conversation::new(
            task.pools,
            backfill,
            queues,
            Arc::new(CurrentClassifier::new(FIXTURE.to_owned(), classifier)),
            Arc::clone(&activation.catalog),
            threshold,
            two(config),
            Arc::new(habits),
            recorder(utterances),
        ))
    }

    /// An activation on `changes`, `speech`, `cache` and `catalog`, with an empty file.
    fn activation_of(
        changes: std::sync::mpsc::Sender<Change>,
        speech: &Arc<Speech>,
        cache: watch::Receiver<StateCache>,
        catalog: Catalog,
    ) -> Activation {
        Activation {
            store: Arc::new(FakeStore::new(None)),
            changes,
            speech: Arc::clone(speech),
            cache,
            catalog: Arc::new(ArcSwap::from_pointee(catalog)),
            clock: Arc::new(FakeClock(Timestamp::UNIX_EPOCH)),
        }
    }

    /// A recorder of `utterances` with no stored interactions and a log writer that has ended.
    fn recorder(utterances: Arc<Utterances>) -> Recorder {
        Recorder {
            utterances,
            writer: tokio::sync::mpsc::unbounded_channel().0,
            last_id: None,
            trigger: fakes::trigger(Path::new("unused")).0,
            speakers: fakes::speakers(Arc::default()),
        }
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
        let voice = Arc::new(FakeVoiceEmbedder);
        let listener = listener(
            &config,
            stt,
            voice,
            Arc::clone(&classifier),
            reasoner,
            &queues,
        );
        let utterances = Arc::clone(listener.utterances());
        Ok(App {
            conversation: conversation(&config, catalog, classifier, &speech, utterances)?,
            listener,
            speech,
            _prefetch: None,
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

    /// `VALID` with the files `pools.json`, `habits.json`, `jarvis.db`, `training.json` and
    /// `generated.json` in `folder`.
    fn with_stores_in(folder: &Path) -> String {
        let in_folder = |file: &str| format!("\"{}\"", folder.join(file).display());
        let text = VALID.replace("\"pools.json\"", &in_folder("pools.json"));
        let text = text.replace("\"jarvis.db\"", &in_folder("jarvis.db"));
        let text = text.replace("\"training.json\"", &in_folder("training.json"));
        let text = text.replace("\"generated.json\"", &in_folder("generated.json"));
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

    /// The error of `run` on the CPU with the stores in `folder` and the model folders in
    /// `<folder>/models`, which does not exist, and the folder of the run `name` in it.
    async fn missing_model(folder: &Path, name: &str) -> anyhow::Result<(String, String)> {
        let models = folder.join("models");
        let text = with_stores_in(folder).replace("\"cuda\"", "\"cpu\"");
        let text = text.replace("models/system-one", &models.display().to_string());
        let error = run_error(&text).await?;
        Ok((error, models.join(name).display().to_string()))
    }

    /// AC-5.4-3.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4_3_missing_model_folder_is_named() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let (error, fixture) = missing_model(folder.path(), FIXTURE).await?;
        assert!(error.contains(&fixture), "no folder: {error}");
        Ok(())
    }

    /// AC-13.9d-1.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9d_1_run_loads_the_current_run() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let state = TrainingState {
            current: Some("20260101-000000".to_owned()),
            ..TrainingState::default()
        };
        FileStore::try_new(folder.path().join("training.json"))?.save(&state)?;
        let (error, run) = missing_model(folder.path(), "20260101-000000").await?;
        assert!(error.contains(&run), "no folder: {error}");
        Ok(())
    }

    /// AC-5.4-4.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4_4_swap_gives_the_next_handle_the_new_classifier() -> anyhow::Result<()> {
        let (old, new): (Arc<FakeClassifier>, Arc<FakeClassifier>) = Default::default();
        let current = Arc::new(CurrentClassifier::new(FIXTURE.to_owned(), old.clone()));
        let app = app_with(Arc::clone(&current) as _, Arc::new(FakeStt::default()))?;
        let handle = || app.handle("turn on the lights", ports::Context::default());
        let _ = handle().await;
        let (run, classifier) = (FIXTURE.to_owned(), new.clone() as _);
        current.0.store(Arc::new(RunClassifier { run, classifier }));
        let _ = handle().await;
        assert_eq!((old.calls().len(), new.calls().len()), (1, 1));
        Ok(())
    }

    /// Weak references to the fakes of each load of [`fake_load`].
    type Loaded = Arc<Mutex<Vec<Weak<dyn Any + Send + Sync>>>>;

    /// A load of [`fake_sessions`] into `loaded`. It fails while `fails` is true.
    fn fake_load(loaded: &Loaded, fails: &Arc<AtomicBool>) -> Load {
        let (loaded, fails) = (Arc::clone(loaded), Arc::clone(fails));
        Box::new(move || {
            let result = if fails.load(Ordering::SeqCst) {
                Err(anyhow::anyhow!("no model"))
            } else {
                Ok(fake_sessions(&loaded))
            };
            Box::pin(future::ready(result))
        })
    }

    /// A new `FakeStt`, `FakeTts`, `FakeClassifier` that gives `unknown` once, so `handle`
    /// answers with a filler, and `FakeVoiceEmbedder`. Keeps weak references to them in `loaded`.
    fn fake_sessions(loaded: &Loaded) -> (Sessions, RunClassifier) {
        let (stt, tts) = (Arc::new(FakeStt::default()), Arc::new(FakeTts::default()));
        let voice = Arc::new(FakeVoiceEmbedder);
        let classifier = Arc::new(FakeClassifier::default());
        classifier.script(Prediction {
            ranked: vec![("unknown".to_owned(), 0.9)],
            slots: Vec::new(),
        });
        let fakes: [Weak<dyn Any + Send + Sync>; 4] = [
            Arc::downgrade(&stt) as _,
            Arc::downgrade(&tts) as _,
            Arc::downgrade(&classifier) as _,
            Arc::downgrade(&voice) as _,
        ];
        loaded.lock().expect("no panic").extend(fakes);
        let model = RunClassifier {
            run: FIXTURE.to_owned(),
            classifier,
        };
        (Sessions { stt, tts, voice }, model)
    }

    /// The number of fakes of `loaded` that still have a reference.
    fn alive(loaded: &Loaded) -> usize {
        let fakes = loaded.lock().expect("no panic");
        fakes.iter().filter(|fake| fake.strong_count() > 0).count()
    }

    /// The reply of `app` to "turn on the lights" with no context.
    async fn request(app: &App) -> anyhow::Result<Handled> {
        app.handle("turn on the lights", ports::Context::default())
            .await
    }

    /// AC-13.10d-2.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10d_2_release_drops_the_sessions_until_restore() -> anyhow::Result<()> {
        let (loaded, fails): (Loaded, Arc<AtomicBool>) = Default::default();
        let models = LiveModels::try_new(fake_load(&loaded, &fails)).await?;
        let app = app_with(Arc::clone(models.classifier()) as _, models.live())?;
        // A request keeps its snapshot of the classifier until it ends.
        let _held = models.classifier().0.load_full();
        models.release().await;
        assert_eq!(alive(&loaded), 0, "references to the fakes after release");
        assert!(app.listen().is_err(), "listen works after release");
        assert!(request(&app).await.is_err(), "handle works after release");
        models.restore().await?;
        let _: Handled = request(&app).await?;
        fails.store(true, Ordering::SeqCst);
        let error = models.restore().await.err().context("restore loaded")?;
        assert_eq!(format!("{error:#}"), "no model");
        Ok(())
    }

    /// AC-15.3-8.
    #[tokio::test(start_paused = true)]
    async fn ac_15_3_8_release_drops_the_voice_embedder_until_restore() -> anyhow::Result<()> {
        let (loaded, fails): (Loaded, Arc<AtomicBool>) = Default::default();
        let models = LiveModels::try_new(fake_load(&loaded, &fails)).await?;
        let live = models.live();
        let audio = Audio {
            samples: vec![0; 16_000],
            rate: 16_000,
        };
        models.release().await;
        let voice = loaded.lock().expect("no panic")[3].clone();
        assert_eq!(
            voice.strong_count(),
            0,
            "references to the fake after release"
        );
        assert!(live.embed(&audio).is_err(), "embed works after release");
        models.restore().await?;
        assert!(
            !live.embed(&audio)?.is_empty(),
            "no embedding after restore"
        );
        Ok(())
    }

    /// After a release and a restore, the new second pass has the hotwords of the last
    /// `set_hotwords`, also of one during the pause.
    #[tokio::test(start_paused = true)]
    async fn restore_gives_the_last_hotwords() -> anyhow::Result<()> {
        let (loaded, fails): (Loaded, Arc<AtomicBool>) = Default::default();
        let models = LiveModels::try_new(fake_load(&loaded, &fails)).await?;
        let live = models.live();
        live.set_hotwords(&["kitchen".to_owned()]);
        models.release().await;
        let words = vec!["radiohead".to_owned()];
        live.set_hotwords(&words);
        models.restore().await?;
        let stt = loaded.lock().expect("no panic")[4].upgrade();
        let stt = stt.context("the new second pass")?.downcast::<FakeStt>();
        let stt = stt.map_err(|_| anyhow::anyhow!("the fake is not a FakeStt"))?;
        assert_eq!(*stt.hotwords.lock().expect("no panic"), [words]);
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
            voiced_ms: 0,
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

    /// The `[mqtt]` section of `jarvis.toml`.
    const MQTT: &str = "[mqtt]\nurl = \"mqtt://homeassistant.local:1883\"\nusername_env = \"MQTT_USERNAME\"\npassword_env = \"MQTT_PASSWORD\"\n";

    /// A CPU configuration in `folder` with the Wyoming and web server addresses on free local
    /// ports, and both addresses. It names the int8 files and no second pass, as on a CPU machine,
    /// and has no `[mqtt]`.
    fn listening_config(folder: &Path) -> anyhow::Result<(PathBuf, SocketAddr, SocketAddr)> {
        let (address, web) = (free_address(), free_address());
        let text = with_stores_in(folder).replace(MQTT, "");
        let text = text.replace("\"cuda\"", "\"cpu\"");
        let text = text.replace(".fp16.", ".int8.");
        let text = text.replace("second_pass_dir =", "# second_pass_dir =");
        let text = text.replace("0.0.0.0:10700", &address.to_string());
        let text = text.replace("0.0.0.0:10701", &web.to_string());
        Ok((config_in(folder, &text)?, address, web))
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
        let (config, address, _) = listening_config(folder.path())?;
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
        let (config, address, _) = listening_config(folder.path())?;
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
            result = run(&config, startup) => { result?; }
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

    /// Sends the text `no model` on `stop` once the Wyoming server at `address` answers. Ends
    /// only on an error.
    async fn stop_when_listening(
        address: SocketAddr,
        stop: &watch::Sender<Option<String>>,
    ) -> anyhow::Result<()> {
        describe_when_listening(address).await?;
        stop.send_replace(Some("no model".to_owned()));
        future::pending().await
    }

    /// The configuration and the secrets of `file`, and its app started on `FakeHome` with the
    /// stop receiver `stopped`.
    async fn started_with(
        file: &Path,
        stopped: watch::Receiver<Option<String>>,
    ) -> anyhow::Result<(Config, Secrets, Started)> {
        let config = Config::load(file)?;
        let secrets = Secrets::resolve(&config, |_: &str| Some("token".to_owned()))?;
        let catalog = load_catalog(&config.intents)?;
        let pools = load_pools(&config.responses, &catalog)?;
        let home = Arc::new(FakeHome::default());
        let (app, (pools_task, habits, (tasks, review, _, shared)), prerender, models) =
            start_live_app(&config, &secrets, catalog, pools, home).await?;
        let stores = (pools_task, habits, (tasks, review, stopped, shared));
        Ok((config, secrets, (app, stores, prerender, models)))
    }

    /// AC-13.10g-2. The test holds the sender of a new stop channel.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10g_2_restore_error_stops_the_server() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let (file, address, _) = listening_config(folder.path())?;
        let (stop, stopped) = watch::channel(None);
        let (config, secrets, started) = started_with(&file, stopped).await?;
        tokio::select! {
            biased;
            served = serve_until(file, &config, &secrets, started, future::pending()) => {
                let error = served.err().context("serve_until gave no error")?;
                assert_eq!(format!("{error:#}"), "no model");
            }
            result = stop_when_listening(address, &stop) => anyhow::bail!("no stop: {result:?}"),
        }
        let connected = TcpStream::connect(address).await;
        assert!(connected.is_err(), "the server accepts after the stop");
        Ok(())
    }

    /// The status entity ends only when the status channel closes.
    #[tokio::test(start_paused = true)]
    async fn status_channel_closes_after_serve_until() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let (file, _, _) = listening_config(folder.path())?;
        let (config, secrets, started) = started_with(&file, watch::channel(None).1).await?;
        let mut status = (started.1).2.3.status.subscribe();
        serve_until(file, &config, &secrets, started, future::ready(())).await?;
        let closed = async { while status.changed().await.is_ok() {} };
        tokio::time::timeout(Duration::from_secs(60), closed)
            .await
            .context("the status channel is open")
    }

    /// Gets `api/<path>` from the web server at `address` once it listens.
    async fn get_when_listening(address: SocketAddr, path: &str) -> reqwest::Response {
        let (url, client) = (
            format!("http://{address}/api/{path}"),
            reqwest::Client::new(),
        );
        loop {
            if let Ok(response) = client.get(&url).bearer_auth("token").send().await {
                return response;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Gets `api/intents` from the web server at `address` once it listens, then stops `run` with
    /// `stop`. Ends only on an error.
    async fn intents_then_stop(
        address: SocketAddr,
        stop: oneshot::Sender<()>,
    ) -> anyhow::Result<()> {
        let response = get_when_listening(address, "intents").await;
        anyhow::ensure!(response.status().is_success(), "the answer is {response:?}");
        stop.send(())
            .map_err(|()| anyhow::anyhow!("run dropped the shutdown future"))?;
        future::pending().await
    }

    /// AC-12.8b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_12_8b_1_run_serves_the_web_server_until_the_shutdown() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let (config, _, web) = listening_config(folder.path())?;
        let (stop, stopped) = oneshot::channel();
        let startup = Startup {
            lookup: |_: &str| Some("token".to_owned()),
            connector: no_ha(mpsc::unbounded_channel().0),
            shutdown: async {
                let _ = stopped.await;
            },
        };
        tokio::select! {
            result = run(&config, startup) => { result?; }
            result = intents_then_stop(web, stop) => anyhow::bail!("run did not stop: {result:?}"),
        }
        let connected = TcpStream::connect(web).await;
        assert!(connected.is_err(), "the web server accepts after the stop");
        Ok(())
    }

    /// The status of `api/status` after the start of `run` with the configuration of
    /// `listening_config` and the environment `lookup`.
    async fn status_after_start(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        let folder = tempfile::tempdir()?;
        let (config, _, web) = listening_config(folder.path())?;
        status_of(&config, web, lookup).await
    }

    /// The status of `api/status` on `web` after the start of `run` with `config` and the
    /// environment `lookup`.
    async fn status_of(
        config: &Path,
        web: SocketAddr,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        let startup = Startup {
            lookup,
            connector: no_ha(mpsc::unbounded_channel().0),
            shutdown: future::pending(),
        };
        tokio::select! {
            result = run(config, startup) => anyhow::bail!("run returned: {result:?}"),
            response = get_when_listening(web, "status") => Ok(response.json().await?),
        }
    }

    /// AC-12.9b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_12_9b_1_after_the_start_the_status_is_online() -> anyhow::Result<()> {
        let status = status_after_start(|_: &str| Some("token".to_owned())).await?;
        assert_eq!(status, json!({ "state": "online", "message": null }));
        Ok(())
    }

    /// AC-12.9h-2. Without `[mqtt]`, `run` has no broker address and needs no MQTT secret.
    #[tokio::test(start_paused = true)]
    async fn ac_12_9h_2_without_mqtt_run_starts() -> anyhow::Result<()> {
        assert!(VALID.contains(MQTT), "jarvis.toml has no [mqtt]");
        let lookup = |name: &str| (!name.starts_with("MQTT_")).then(|| "token".to_owned());
        let status = status_after_start(lookup).await?;
        assert_eq!(status["state"], "online");
        Ok(())
    }

    /// Waits until `client` gets the status `online`, then stops `run` with `stop`. Ends only on
    /// an error.
    async fn online_then_stop(
        client: &mut MqttSubscriber,
        stop: oneshot::Sender<()>,
    ) -> anyhow::Result<()> {
        let status = client.next().await?.payload;
        anyhow::ensure!(status["state"] == "online", "the status is {status}");
        stop.send(())
            .map_err(|()| anyhow::anyhow!("run dropped the shutdown future"))?;
        future::pending().await
    }

    /// The configuration of `listening_config` in `folder` with the MQTT broker at `broker`.
    fn mqtt_config(folder: &Path, broker: SocketAddr) -> anyhow::Result<PathBuf> {
        let (config, _, _) = listening_config(folder)?;
        let mqtt = MQTT.replace("homeassistant.local:1883", &broker.to_string());
        fs::write(&config, format!("{}{mqtt}", read(&config)?))?;
        Ok(config)
    }

    /// AC-12.9h-1. The clock runs, because the MQTT client has timers and a real socket.
    #[tokio::test]
    async fn ac_12_9h_1_after_the_shutdown_the_status_is_offline() -> anyhow::Result<()> {
        let (folder, broker) = (tempfile::tempdir()?, fakes::mqtt_broker()?);
        let config = mqtt_config(folder.path(), broker)?;
        let (stop, stopped) = oneshot::channel();
        let startup = Startup {
            lookup: |_: &str| Some("token".to_owned()),
            connector: no_ha(mpsc::unbounded_channel().0),
            shutdown: async {
                let _ = stopped.await;
            },
        };
        let mut client = MqttSubscriber::new(broker, "early", "jarvis/status")?;
        tokio::select! {
            result = run(&config, startup) => { result?; }
            result = online_then_stop(&mut client, stop) => anyhow::bail!("no stop: {result:?}"),
        }
        let mut late = MqttSubscriber::new(broker, "late", "jarvis/status")?;
        let status = late.next().await?.payload;
        assert_eq!(status, json!({ "state": "offline", "message": null }));
        Ok(())
    }

    /// A `Controller` of a copy of `jarvis.toml` in `folder`.
    fn controller(folder: &Path) -> anyhow::Result<Controller> {
        let config = folder.join("jarvis.toml");
        fs::write(&config, VALID)?;
        Ok(Controller::new(config, Shared::default()))
    }

    /// The changes that set `name` to `value`.
    fn change(name: &str, value: Value) -> BTreeMap<String, Value> {
        BTreeMap::from([(name.to_owned(), value)])
    }

    /// AC-12.9e-1.
    #[test]
    fn ac_12_9e_1_save_writes_the_value_and_settings_reads_it() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let control = controller(folder.path())?;
        assert_eq!(control.save(change("asr.endpoint_ms", json!(700))), Ok(()));
        let text = fs::read_to_string(&control.config)?;
        assert!(text.contains("endpoint_ms = 700"), "{text}");
        let settings = control.settings()?;
        let endpoint = settings
            .iter()
            .find(|setting| setting.name == "asr.endpoint_ms");
        assert_eq!(endpoint.map(|setting| &setting.value), Some(&json!(700)));
        Ok(())
    }

    /// AC-12.9e-2.
    #[test]
    fn ac_12_9e_2_save_refuses_what_parse_refuses_and_keeps_the_file() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let control = controller(folder.path())?;
        let changes = change("asr.early_commit_ms", json!(700));
        let edited =
            adapters::settings::edit(VALID, changes.clone()).map_err(anyhow::Error::msg)?;
        let error = Config::parse(&edited).expect_err("early_commit_ms after endpoint_ms");
        let refused = control.save(changes);
        assert_eq!(refused, Err(Refusal::Invalid(format!("{error:#}"))));
        assert_eq!(fs::read_to_string(&control.config)?, VALID);
        Ok(())
    }

    /// AC-12.9e-3.
    #[test]
    fn ac_12_9e_3_save_refuses_a_secret_and_keeps_the_file() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let control = controller(folder.path())?;
        let refused = control.save(change("ha.token_env", json!("OTHER")));
        assert!(matches!(refused, Err(Refusal::Invalid(_))), "{refused:?}");
        assert_eq!(fs::read_to_string(&control.config)?, VALID);
        Ok(())
    }

    /// `run` of `config` with the value `token` of each variable, no HA and no shutdown.
    async fn run_until_save(config: &Path) -> anyhow::Result<Ended> {
        let startup = Startup {
            lookup: |_: &str| Some("token".to_owned()),
            connector: no_ha(mpsc::unbounded_channel().0),
            shutdown: future::pending(),
        };
        run(config, startup).await
    }

    /// Saves `dialog.timeout_s = 20` with `POST api/settings` on the web server at `address`.
    /// Ends only on an error.
    async fn save_then_wait(address: SocketAddr) -> anyhow::Result<()> {
        let url = format!("http://{address}/api/settings");
        let request = reqwest::Client::new().post(url).bearer_auth("token");
        let status = request
            .json(&json!({ "dialog.timeout_s": 20 }))
            .send()
            .await?
            .status();
        anyhow::ensure!(status == 204, "the answer is {status}");
        future::pending().await
    }

    /// AC-12.9f-1. The clock runs, because the MQTT client has timers and a real socket.
    #[tokio::test]
    async fn ac_12_9f_1_save_keeps_the_backup_and_restarts() -> anyhow::Result<()> {
        let (folder, broker) = (tempfile::tempdir()?, fakes::mqtt_broker()?);
        let config = mqtt_config(folder.path(), broker)?;
        let (old, web) = (read(&config)?, Config::load(&config)?.web.listen);
        let mut client = MqttSubscriber::new(broker, "early", "jarvis/status")?;
        let saved = async {
            client.next().await?;
            save_then_wait(web).await
        };
        let ended = tokio::select! {
            ended = run_until_save(&config) => ended?,
            result = saved => anyhow::bail!("run did not restart: {result:?}"),
        };
        assert_eq!(ended, Ended::Restart);
        assert_eq!(read(&adapters::settings::beside(&config, ".bak"))?, old);
        assert!(read(&config)?.contains("timeout_s = 20"), "the new text");
        assert_eq!(client.next().await?.payload["state"], "restarting");
        Ok(())
    }

    /// AC-12.9f-2.
    #[test]
    fn ac_12_9f_2_restart_exits_with_75_and_stop_with_0() {
        assert_eq!(exit_code(Ok(Ended::Restart)), ExitCode::from(75));
        assert_eq!(exit_code(Ok(Ended::Stop)), ExitCode::SUCCESS);
    }

    /// The results of two saves of `control` on two threads at the same time.
    fn saves_at_once(control: &Controller) -> [Result<(), Refusal>; 2] {
        let save = |seconds: u64| control.save(change("dialog.timeout_s", json!(seconds)));
        thread::scope(|scope| {
            let other = scope.spawn(|| save(20));
            [save(30), other.join().expect("the save does not panic")]
        })
    }

    /// AC-12.9f-3.
    #[test]
    fn ac_12_9f_3_one_save_at_a_time() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let control = controller(folder.path())?;
        let mut saved = saves_at_once(&control);
        saved.sort_by_key(Result::is_err);
        assert_eq!(saved, [Ok(()), Err(Refusal::Busy)]);
        assert_eq!(
            read(&adapters::settings::beside(&control.config, ".bak"))?,
            VALID
        );
        let other = tempfile::tempdir()?;
        let control = controller(other.path())?;
        let refused = control.save(change("asr.early_commit_ms", json!(700)));
        assert!(matches!(refused, Err(Refusal::Invalid(_))), "{refused:?}");
        assert_eq!(control.save(change("dialog.timeout_s", json!(20))), Ok(()));
        Ok(())
    }

    /// Whether `begin` makes the marker of the trial start for a configuration with a backup.
    fn begin_marks() -> anyhow::Result<bool> {
        let folder = tempfile::tempdir()?;
        let config = config_in(folder.path(), VALID)?;
        fs::write(adapters::settings::beside(&config, ".bak"), VALID)?;
        adapters::settings::begin(&config)?;
        Ok(adapters::settings::beside(&config, ".trying").exists())
    }

    /// AC-12.9i-1.
    #[tokio::test(start_paused = true)]
    async fn ac_12_9i_1_ready_start_ends_the_trial_start() -> anyhow::Result<()> {
        assert!(begin_marks()?, "begin marks the trial start");
        let folder = tempfile::tempdir()?;
        let (config, _, web) = listening_config(folder.path())?;
        fs::write(adapters::settings::beside(&config, ".bak"), VALID)?;
        status_of(&config, web, |_: &str| Some("token".to_owned())).await?;
        let left =
            [".bak", ".trying"].map(|suffix| adapters::settings::beside(&config, suffix).exists());
        assert_eq!(left, [false, false], "the backup and the marker are gone");
        Ok(())
    }

    /// AC-12.9i-2.
    #[test]
    fn ac_12_9i_2_error_of_the_trial_start_is_kept_and_restarts() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let config = config_in(folder.path(), VALID)?;
        let trying = adapters::settings::beside(&config, ".trying");
        fs::write(&trying, "")?;
        let code = exit_code(trial(&config, Err(anyhow::anyhow!("no model"))));
        assert_eq!(
            (code, read(&trying)?),
            (ExitCode::from(75), "no model".to_owned())
        );
        assert_eq!(adapters::settings::panic_code(&config), 75);
        Ok(())
    }

    /// AC-12.9i-1. A signal before the start is ready keeps the saved file for the next trial.
    #[test]
    fn ac_12_9i_1_stopped_trial_start_keeps_the_saved_file() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let config = config_in(folder.path(), VALID)?;
        fs::write(adapters::settings::beside(&config, ".bak"), "old")?;
        adapters::settings::begin(&config)?;
        adapters::settings::cancel(&config)?;
        adapters::settings::begin(&config)?;
        assert_eq!(read(&config)?, VALID, "no rollback");
        assert!(adapters::settings::beside(&config, ".trying").exists());
        Ok(())
    }

    /// AC-12.9i-4. A rollback that cannot keep its reason stops Jarvis without a restart.
    #[test]
    fn ac_12_9i_4_failed_rollback_does_not_restart() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let config = config_in(folder.path(), VALID)?;
        fs::write(adapters::settings::beside(&config, ".trying"), "no model")?;
        // A folder in place of the reason makes the rollback fail.
        fs::create_dir(adapters::settings::beside(&config, ".rollback"))?;
        let code = exit_code(trial(&config, begin(&config).map(|_| Ended::Stop)));
        assert_eq!(code, ExitCode::FAILURE);
        Ok(())
    }

    /// AC-12.9i-4. A missing secret fails each start.
    #[tokio::test(start_paused = true)]
    async fn ac_12_9i_4_failed_start_after_the_rollback_ends_the_restarts() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let control = controller(folder.path())?;
        assert_eq!(control.save(change("dialog.timeout_s", json!(20))), Ok(()));
        let config = &control.config;
        let mut codes = vec![exit_code(Ok(Ended::Restart))];
        for _ in 0..2 {
            let startup = Startup {
                lookup: |_: &str| None,
                connector: no_ha(mpsc::unbounded_channel().0),
                shutdown: future::pending(),
            };
            codes.push(exit_code(trial(config, run(config, startup).await)));
        }
        assert_eq!(codes, [75, 75, 1].map(ExitCode::from));
        assert_eq!(read(config)?, VALID);
        assert_eq!(adapters::settings::panic_code(config), 101);
        Ok(())
    }

    /// AC-12.9i-5.
    #[tokio::test(start_paused = true)]
    async fn ac_12_9i_5_rollback_message_stays_until_the_next_save() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let (config, _, web) = listening_config(folder.path())?;
        let rollback = adapters::settings::beside(&config, ".rollback");
        fs::write(&rollback, "bind failed")?;
        let status = status_of(&config, web, |_: &str| Some("token".to_owned())).await?;
        assert_eq!(status["message"], "settings rolled back: bind failed");
        let saved =
            Controller::new(config, Shared::default()).save(change("dialog.timeout_s", json!(20)));
        assert_eq!(saved, Ok(()));
        assert!(!rollback.exists(), "the save removes the reason");
        Ok(())
    }

    /// AC-12.9i-6.
    #[tokio::test(start_paused = true)]
    async fn ac_12_9i_6_bind_error_of_the_trial_start_rolls_back() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let (config, _, web) = listening_config(folder.path())?;
        let backup = adapters::settings::beside(&config, ".bak");
        fs::write(&backup, "old")?;
        let _taken = std::net::TcpListener::bind(web)?;
        let error = run_until_save(&config).await.err().context("run started")?;
        let error = format!("{error:#}");
        let code = exit_code(trial(&config, Err(anyhow::anyhow!(error.clone()))));
        assert_eq!(code, ExitCode::from(75));
        assert_eq!(
            read(&adapters::settings::beside(&config, ".trying"))?,
            error
        );
        assert!(error.contains("in use") && backup.exists(), "{error}");
        adapters::settings::begin(&config)?;
        assert_eq!(read(&config)?, "old");
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

    /// AC-15.3-7. sherpa-onnx falls back to the CPU with only a log line, so the GPU memory of
    /// this process shows that the voice embedder loads on CUDA.
    #[test]
    #[ignore = "GPU host"]
    fn ac_15_3_7_cuda_runs_the_voice_embedder() -> anyhow::Result<()> {
        if !alone("tests::ac_15_3_7_cuda_runs_the_voice_embedder")? {
            return Ok(());
        }
        let start = gpu_memory()?;
        let _voice = SherpaEmbedder::try_new(&Config::parse(VALID)?.speaker, Device::Cuda)?;
        let end = gpu_memory()?;
        assert!(
            start < end,
            "GPU memory in MiB: {start} before and {end} after the voice embedder"
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

    /// The free GPU memory in MiB that `nvidia-smi` reports.
    fn free_gpu_memory() -> anyhow::Result<u32> {
        let query = ["--query-gpu=memory.free", "--format=csv,noheader,nounits"];
        let output = std::process::Command::new("nvidia-smi")
            .args(query)
            .output()?;
        anyhow::ensure!(output.status.success(), "nvidia-smi fails: {output:?}");
        let text = String::from_utf8(output.stdout)?;
        Ok(text.lines().next().context("no GPU")?.trim().parse()?)
    }

    /// AC-13.10f-1. The other GPU tests change the memory of their process, so the test runs again
    /// alone in a child process. The Phase 9.2 tests run after a release and a restore of the
    /// models of their pipeline (`testing::e2e`).
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_13_10f_1_release_frees_the_gpu_for_a_local_run() -> anyhow::Result<()> {
        if !alone("tests::ac_13_10f_1_release_frees_the_gpu_for_a_local_run")? {
            return Ok(());
        }
        let config = Config::parse(VALID)?;
        let models = LiveModels::try_new(load(&config)).await?;
        let loaded = gpu_memory()?;
        models.release().await;
        let (released, free) = (gpu_memory()?, free_gpu_memory()?);
        // GB in MiB, as in AC-9.3c-2.
        let needed = (config.train.max_vram_gb + 0.5) * 1e9 / f64::from(1 << 20);
        assert!(
            released < loaded && f64::from(free) >= needed,
            "GPU memory in MiB: {loaded} loaded, {released} released, {free} free, {needed} needed"
        );
        models.restore().await?;
        answers(&models).await
    }

    /// Runs one prediction, one first pass and one synthesis of `models`.
    async fn answers(models: &LiveModels) -> anyhow::Result<()> {
        let classifier = models.classifier();
        classifier
            .predict("area: kitchen", "turn on the lights")
            .await?;
        models.live().stream()?;
        let audio = models.live().synth("Hello").await?;
        anyhow::ensure!(!audio.samples.is_empty(), "the audio has no samples");
        Ok(())
    }
}
