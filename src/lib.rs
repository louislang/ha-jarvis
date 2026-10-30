//! Voice-controlled home automation for Home Assistant.

use std::fs;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// Pure rules.
pub mod domain {
    pub mod contracts;
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
    /// The text is not TOML, or a field is unknown, absent or of the wrong type.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
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
}

/// Entry point of Jarvis.
pub fn main() {}

/// Tests of the entry point and the configuration.
#[cfg(test)]
mod tests {
    use super::*;

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
}
