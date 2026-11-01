//! Voice-controlled home automation for Home Assistant.

// Verus reads loop invariants from attributes on loops, and the `split_once` contract names a
// `Pattern`.
#![cfg_attr(verus_only, feature(proc_macro_hygiene, pattern))]

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use jiff::Timestamp;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Deserialize;

use crate::domain::catalog::Catalog;
use crate::domain::state::Snapshot;
use crate::domain::training::Settings;
use crate::domain::training::TrainingSet;
use crate::ports::Clock;

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

/// Reads the area names of the snapshot file at `path`.
///
/// # Errors
///
/// The file cannot be read or is not a snapshot.
fn area_names(path: &Path) -> anyhow::Result<Vec<String>> {
    let snapshot: Snapshot = serde_json::from_str(&read(path)?)?;
    Ok(snapshot.areas.into_iter().map(|area| area.name).collect())
}

/// Entry point of Jarvis: runs the command of the command line.
#[must_use]
pub fn main() -> ExitCode {
    let command = match Command::try_parse() {
        Ok(command) => command,
        Err(error) => {
            // `--help` is an error of `try_parse` too, and prints to stdout.
            let _ = error.print();
            return if error.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
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

    use crate::domain::training;
    use crate::testing::fakes;

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
            model: ModelConfig { threshold: 0.6 },
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
}
