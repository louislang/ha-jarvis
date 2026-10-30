//! Voice-controlled home automation for Home Assistant.

// Verus reads loop invariants from attributes on loops.
#![cfg_attr(verus_only, feature(proc_macro_hygiene))]

use std::fs;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// Pure rules.
pub mod domain {
    #[cfg(verus_only)]
    pub mod contracts;
    pub mod intent;
    pub mod state;
}

pub mod ports;

/// Use cases.
pub mod app {
    pub mod background;
}

/// Port implementations, one technology each.
pub mod adapters {
    pub mod ha;
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
    /// Home Assistant connection.
    pub ha: HaConfig,
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
            ("ha.ping_interval_s", self.ha.ping_interval_s),
        ];
        for (name, value) in positive {
            anyhow::ensure!(
                value > 0,
                "{name} = {value} is out of range: it must be more than 0"
            );
        }
        Ok(())
    }

    /// Reads and parses the configuration file at `path`.
    ///
    /// # Errors
    ///
    /// The file cannot be read, or [`Config::parse`] fails.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        Self::parse(&text)
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
    /// Deadline of the authentication and of each request.
    pub request_timeout_ms: u64,
    /// Time between two pings.
    pub ping_interval_s: u64,
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

/// Entry point of Jarvis.
pub fn main() {}

/// Tests of the entry point and the configuration.
#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

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
        super::main();
    }

    /// AC-1.1-1.
    #[test]
    fn ac_1_1_1_parse_returns_each_value() -> anyhow::Result<()> {
        let expected = Config {
            listen: SocketAddr::from(([0, 0, 0, 0], 10700)),
            device: Device::Cuda,
            ha: HaConfig {
                url: "ws://homeassistant.local:8123/api/websocket".to_owned(),
                token_env: "HA_TOKEN".to_owned(),
                request_timeout_ms: 5000,
                ping_interval_s: 30,
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
}
