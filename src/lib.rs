//! Voice-controlled home automation for Home Assistant.

// Verus reads loop invariants from attributes on loops, and the `split_once` contract names a
// `Pattern`.
#![cfg_attr(verus_only, feature(proc_macro_hygiene, pattern))]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::future;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
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
use tokio::sync::watch;

use crate::adapters::files::FileStore;
use crate::adapters::ha::HaHome;
use crate::adapters::onnx::OnnxLoader;
use crate::app::conversation::Conversation;
use crate::app::conversation::Queues;
use crate::domain::catalog::Catalog;
use crate::domain::model::Prediction;
use crate::domain::responses::Pool;
use crate::domain::responses::Pools;
use crate::domain::responses::Response;
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
use crate::ports::ModelLoader;

/// Pure rules.
pub mod domain {
    pub mod catalog;
    #[cfg(verus_only)]
    pub mod contracts;
    pub mod decide;
    pub mod intent;
    pub mod model;
    pub mod responses;
    pub mod select;
    pub mod state;
    pub mod state_cache;
    pub mod training;
}

pub mod ports;

/// Use cases.
pub mod app {
    pub mod background;
    pub mod conversation;
}

/// Port implementations, one technology each.
pub mod adapters {
    pub mod files;
    pub mod ha;
    pub mod onnx;
    pub mod wyoming;
}

/// Shared test code.
#[cfg(test)]
pub mod testing {
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
    /// Announces.
    pub announce: AnnounceConfig,
    /// Training.
    pub train: TrainConfig,
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

    /// Checks the range of each field.
    ///
    /// # Errors
    ///
    /// A field is out of its range. The error names the field and the range.
    fn validate(&self) -> anyhow::Result<()> {
        let positive = [
            ("ha.request_timeout_ms", self.ha.request_timeout_ms),
            ("ha.announce_timeout_s", self.ha.announce_timeout_s),
            ("ha.ping_interval_s", self.ha.ping_interval_s),
        ];
        for (name, value) in positive {
            anyhow::ensure!(
                value > 0,
                "{name} = {value} is out of range: it must be more than 0"
            );
        }
        let threshold = self.model.threshold;
        anyhow::ensure!(
            (0.0..=1.0).contains(&threshold),
            "model.threshold = {threshold} is out of range: it must be in [0, 1]"
        );
        let holdout = self.train.holdout;
        anyhow::ensure!(
            (0.0..=1.0).contains(&holdout),
            "train.holdout = {holdout} is out of range: it must be in [0, 1]"
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
}

#[async_trait]
impl Assistant for App {
    fn info(&self) -> Info {
        Info {
            name: env!("CARGO_PKG_NAME").to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    async fn handle(&self, text: &str, context: ports::Context) -> anyhow::Result<Handled> {
        self.conversation.handle(text, context).await
    }
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
        let name = &config.ha.token_env;
        match lookup(name) {
            Some(ha_token) if !ha_token.is_empty() => Ok(Self { ha_token }),
            _ => anyhow::bail!("environment variable {name} is not set or is empty"),
        }
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
        let responses = texts.map(|text| Response { text, uses: 0 }).collect();
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

/// The outside world of [`run`].
pub struct Startup<Lookup, Connector, Shutdown> {
    /// Gives the value of an environment variable.
    pub lookup: Lookup,
    /// Opens a transport to HA at a host and a port.
    pub connector: Connector,
    /// Completes when Jarvis must stop.
    pub shutdown: Shutdown,
}

/// Starts Jarvis with the configuration file `config` and the outside world `startup`: HA,
/// the sync, the System One model and the Wyoming server. Returns when `startup.shutdown`
/// completes.
///
/// # Errors
///
/// A file cannot be read or is not valid, a secret is not set, the model cannot load, or the
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
    let conversation = conversation(&config, catalog, pools, home).await?;
    let listener = TcpListener::bind(config.listen).await?;
    tokio::select! {
        result = adapters::wyoming::serve(listener, Arc::new(App { conversation })) => result,
        () = startup.shutdown => Ok(()),
    }
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

/// The conversation of `config`, `catalog` and the source pools `pools` on `home`. Starts the
/// pools task and the sync, and loads the System One model.
///
/// # Errors
///
/// The saved pools or the model cannot load.
async fn conversation(
    config: &Config,
    catalog: Catalog,
    pools: Pools,
    home: Arc<dyn Home>,
) -> anyhow::Result<Conversation> {
    let store = Arc::new(FileStore::try_new(config.pools.clone())?);
    let task = app::conversation::start_pools(pools, store)?;
    let queues = start_sync(home, &config.announce);
    let dir = config.model.dir.join("fixture");
    let model = OnnxLoader.load(&dir, config.device).await?;
    let classifier = Arc::new(CurrentClassifier(ArcSwap::from_pointee(model)));
    let rng = StdRng::seed_from_u64(SystemClock.now().as_millisecond().unsigned_abs());
    let (catalog, threshold) = (Arc::new(catalog), config.model.threshold);
    let (pools, changes) = (task.pools, task.changes);
    Ok(Conversation::new(
        pools, changes, queues, rng, classifier, catalog, threshold,
    ))
}

/// Starts the sync of a hot cache with `home`. Gives the announce queues of `announce` on it.
fn start_sync(home: Arc<dyn Home>, announce: &AnnounceConfig) -> Arc<Queues> {
    let (cache, receiver) = watch::channel(StateCache::default());
    let sync = Arc::clone(&home);
    tokio::spawn(async move { app::background::sync(sync.as_ref(), &cache).await });
    let margin = Duration::from_millis(announce.margin_ms);
    let idle_wait = Duration::from_secs(announce.idle_wait_s);
    Arc::new(Queues::new(home, receiver, margin, idle_wait))
}

/// Runs Jarvis with the configuration file `config`, the environment and TCP, until it fails.
///
/// # Errors
///
/// `run` fails.
fn serve(config: &Path) -> anyhow::Result<()> {
    let startup = Startup {
        lookup: |name: &str| env::var(name).ok(),
        connector: |host: &str, port: u16| TcpStream::connect((host.to_owned(), port)),
        shutdown: future::pending(),
    };
    tokio::runtime::Runtime::new()?.block_on(run(config, startup))
}

/// Entry point of Jarvis: runs the command of the command line.
#[must_use]
pub fn main() -> ExitCode {
    start(env::args_os())
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
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::Instant;

    use serde_json::json;
    use tokio::io::DuplexStream;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio::sync::watch;

    use crate::adapters::onnx::OnnxLoader;
    use crate::app::conversation::Queues;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Pools;
    use crate::domain::responses::Response;
    use crate::domain::state_cache::StateCache;
    use crate::domain::training;
    use crate::ports::ModelLoader;
    use crate::testing::fakes;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeStore;
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
                request_timeout_ms: 5000,
                announce_timeout_s: 60,
                ping_interval_s: 30,
            },
            model: ModelConfig {
                dir: PathBuf::from("models/system-one"),
                threshold: 0.6,
            },
            announce: AnnounceConfig {
                margin_ms: 300,
                idle_wait_s: 30,
            },
            train: TrainConfig {
                base_model: "sentence-transformers/all-MiniLM-L12-v2".to_owned(),
                epochs: 8,
                batch_size: 8,
                holdout: 0.1,
            },
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
        let error =
            parse_error(&VALID.replace("request_timeout_ms = 5000", "request_timeout_ms = 0"));
        assert!(
            error.contains("ha.request_timeout_ms") && error.contains("more than 0"),
            "error has no field or range: {error}"
        );
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
        assert_eq!(resolve(&[("HA_TOKEN", "abc")])?.ha_token, "abc");
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
            .map(|area| training::context_text(Some(area.name.as_str()), &[]))
            .collect();
        for context in contexts(&HOUSE)? {
            assert!(
                expected.contains(&context),
                "{context} names no area of {expected:?}"
            );
        }
        Ok(())
    }

    /// AC-3.4-3.
    #[test]
    fn ac_3_4_3_without_house_each_context_is_none() -> anyhow::Result<()> {
        let contexts = contexts(&[])?;
        assert!(!contexts.is_empty(), "the training set has no examples");
        for context in contexts {
            assert_eq!(context, "area: none | habits: none");
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
        let names = names.chain(["filler", "failure", "fallback"]);
        names.map(pool).collect()
    }

    /// Queues on `FakeHome` and the hot cache of the reference house.
    fn queues() -> Arc<Queues> {
        let (_cache, receiver) = watch::channel(StateCache::load(fakes::house()));
        let wait = Duration::from_secs(1);
        let home = Arc::new(FakeHome::default());
        Arc::new(Queues::new(home, receiver, wait, wait))
    }

    /// The app of `VALID` with `classifier`, the reference house and `pools`.
    fn app_with(classifier: Arc<dyn Classifier>) -> anyhow::Result<App> {
        let config = Config::parse(VALID)?;
        let catalog = load_catalog(&config.intents)?;
        let task = app::conversation::start_pools(pools(&catalog), Arc::new(FakeStore::new(None)))?;
        let (rng, threshold) = (StdRng::seed_from_u64(1), config.model.threshold);
        let (pools, changes, catalog) = (task.pools, task.changes, Arc::new(catalog));
        let conversation = Conversation::new(
            pools,
            changes,
            queues(),
            rng,
            classifier,
            catalog,
            threshold,
        );
        Ok(App { conversation })
    }

    /// The app of `VALID` with the fixture model, the reference house and `pools`.
    async fn fixture_app() -> anyhow::Result<App> {
        let fixture = Path::new("models/system-one/fixture");
        let device = Config::parse(VALID)?.device;
        app_with(OnnxLoader.load(fixture, device).await?)
    }

    /// The time from each of `count` kitchen `transcript` events to its `handled` event, through
    /// a Wyoming server of `fixture_app()`.
    async fn round_trips(count: usize) -> anyhow::Result<Vec<Duration>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Arc::new(fixture_app().await?);
        tokio::spawn(adapters::wyoming::serve(listener, app));
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
        // The first 100 requests warm up the model.
        let mut times = round_trips(1100).await?.split_off(100);
        times.sort();
        let p95 = times[times.len() * 95 / 100];
        assert!(p95 < Duration::from_millis(20), "p95 is {p95:?}");
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
        let folder = tempfile::tempdir()?;
        let config = config_in(folder.path(), text)?;
        let startup = Startup {
            lookup: |_: &str| Some("token".to_owned()),
            connector: no_ha(mpsc::unbounded_channel().0),
            shutdown: future::pending(),
        };
        let error = run(&config, startup).await.err().context("run succeeded")?;
        Ok(format!("{error:#}"))
    }

    /// `VALID` with the file `pools.json` in `folder`.
    fn with_pools_in(folder: &Path) -> String {
        let pools = folder.join("pools.json");
        VALID.replace("\"pools.json\"", &format!("\"{}\"", pools.display()))
    }

    /// AC-5.4-1.
    #[tokio::test(start_paused = true)]
    async fn ac_5_4_1_invalid_configuration_fails_before_a_port_opens() -> anyhow::Result<()> {
        let cases = [
            ("= 5000", "= 0", "ha.request_timeout_ms"),
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
        let text = with_pools_in(folder.path()).replace("\"cuda\"", "\"cpu\"");
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
        let app = app_with(Arc::clone(&current) as _)?;
        let handle = || app.handle("turn on the lights", ports::Context::default());
        let _ = handle().await;
        current.0.store(Arc::new(new.clone() as _));
        let _ = handle().await;
        assert_eq!((old.calls().len(), new.calls().len()), (1, 1));
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
            run_error(&with_pools_in(folder.path()).replace("\"responses.toml\"", &path)).await?;
        assert!(error.contains("lights_on"), "no pool: {error}");
        Ok(())
    }

    /// Connects to the Wyoming server at `address` once it listens, sends `describe` and gives
    /// the type of the answer. Waits for an HA connection attempt on `attempts`, advances past
    /// the backoff and waits for the next one.
    async fn describe_without_ha(
        address: SocketAddr,
        attempts: &mut mpsc::UnboundedReceiver<()>,
    ) -> anyhow::Result<Option<String>> {
        let mut client = loop {
            if let Ok(client) = FakeWyomingClient::connect(address).await {
                break client;
            }
            tokio::task::yield_now().await;
        };
        client.send("describe", &json!({})).await?;
        let event = client.receive().await?.map(|(event, _)| event);
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
        // A free port, because `run` binds the fixed address of `listen`.
        let address = TcpListener::bind("127.0.0.1:0").await?.local_addr()?;
        let text = with_pools_in(folder.path()).replace("\"cuda\"", "\"cpu\"");
        let config = config_in(
            folder.path(),
            &text.replace("0.0.0.0:10700", &address.to_string()),
        )?;
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
}
