//! `Plugin` `get_weather` with the Open-Meteo forecast API.

use std::ops::RangeInclusive;

use anyhow::Context as _;
use async_trait::async_trait;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::WeatherConfig;
use crate::domain::state::HomeConfig;
use crate::ports::Plugin;
use crate::ports::Tool;

/// The daily variables of each request.
const DAILY: &str =
    "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max";

/// The word of each range of WMO weather codes.
const CONDITIONS: [(RangeInclusive<u64>, &str); 12] = [
    (0..=0, "clear"),
    (1..=1, "mostly clear"),
    (2..=2, "partly cloudy"),
    (3..=3, "cloudy"),
    (45..=45, "foggy"),
    (48..=48, "foggy"),
    (51..=57, "drizzly"),
    (61..=67, "rainy"),
    (80..=82, "rainy"),
    (71..=77, "snowy"),
    (85..=86, "snowy"),
    (95..=99, "stormy"),
];

/// The weather forecast of the Open-Meteo API.
pub struct OpenMeteo {
    /// The HTTP client of all requests.
    client: reqwest::Client,
    /// The forecast API URL.
    url: String,
}

impl OpenMeteo {
    /// The plugin on the forecast API of `config`.
    ///
    /// # Errors
    ///
    /// The HTTP client cannot be built.
    pub fn try_new(config: &WeatherConfig) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            url: config.url.clone(),
        })
    }
}

#[async_trait]
impl Plugin for OpenMeteo {
    fn tool(&self) -> Tool {
        Tool {
            name: "get_weather".to_owned(),
            description: "Gives the house weather forecast for today or tomorrow.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "when": { "type": "string", "enum": ["today", "tomorrow"] } },
                "required": ["when"],
                "additionalProperties": false,
            }),
        }
    }

    async fn fetch(
        &self,
        arguments: &Map<String, Value>,
        home: &HomeConfig,
    ) -> anyhow::Result<Value> {
        let day = match arguments.get("when").and_then(Value::as_str) {
            Some("today") => 0,
            Some("tomorrow") => 1,
            _ => anyhow::bail!("when is not today or tomorrow: {arguments:?}"),
        };
        let fahrenheit = home.unit_system.temperature == "°F";
        let unit = if fahrenheit { "fahrenheit" } else { "celsius" };
        let body: Value = self
            .client
            .get(&self.url)
            .query(&[("latitude", home.latitude), ("longitude", home.longitude)])
            .query(&[
                ("timezone", home.time_zone.as_str()),
                ("temperature_unit", unit),
                ("forecast_days", "2"),
                ("daily", DAILY),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        forecast(&body, day)
    }
}

/// The forecast of day `day` (0 today, 1 tomorrow) of the Open-Meteo reply `body`.
///
/// # Errors
///
/// A daily value is missing or `null`, or the weather code is not in the table.
#[expect(clippy::cast_possible_truncation, reason = "a float cast saturates")]
fn forecast(body: &Value, day: usize) -> anyhow::Result<Value> {
    let daily = |name: &str| &body["daily"][name][day];
    let code = daily("weather_code").as_u64();
    let condition = code.and_then(condition).context("no known weather code")?;
    let high = daily("temperature_2m_max").as_f64().context("no high")?;
    let low = daily("temperature_2m_min").as_f64().context("no low")?;
    let chance = daily("precipitation_probability_max").as_u64();
    Ok(json!({
        "condition": condition,
        "high": high.round() as i64,
        "low": low.round() as i64,
        "precipitation_chance": chance.context("no precipitation chance")?,
    }))
}

/// The word of the WMO weather code `code`.
fn condition(code: u64) -> Option<&'static str> {
    let found = CONDITIONS.iter().find(|(codes, _)| codes.contains(&code));
    found.map(|&(_, word)| word)
}

#[cfg(test)]
mod tests {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    use super::*;
    use crate::testing::fakes;

    /// A reply with two days: code 2, 71.6, 54.4 and 20, then code 61, 60.2, 50.4 and 80.
    fn body() -> Value {
        json!({ "daily": {
            "weather_code": [2, 61],
            "temperature_2m_max": [71.6, 60.2],
            "temperature_2m_min": [54.4, 50.4],
            "precipitation_probability_max": [20, 80],
        } })
    }

    /// The result of `when` in the house `home` from a mock server that answers `status` and
    /// `body`, and the requests of the mock.
    async fn fetched(
        (status, body): (u16, Value),
        when: &str,
        home: &HomeConfig,
    ) -> anyhow::Result<(anyhow::Result<Value>, Vec<Request>)> {
        let server = MockServer::start().await;
        let template = ResponseTemplate::new(status).set_body_json(body);
        Mock::given(matchers::method("GET"))
            .respond_with(template)
            .mount(&server)
            .await;
        let plugin = OpenMeteo::try_new(&WeatherConfig { url: server.uri() })?;
        let arguments = json!({ "when": when })
            .as_object()
            .cloned()
            .unwrap_or_default();
        let result = plugin.fetch(&arguments, home).await;
        Ok((result, server.received_requests().await.unwrap_or_default()))
    }

    /// The result of `when` in the reference house from a mock server that answers `reply`.
    async fn result(reply: (u16, Value), when: &str) -> anyhow::Result<Value> {
        fetched(reply, when, &fakes::house().config).await?.0
    }

    /// The query pairs of the one request for today in the house `home`.
    async fn query(home: &HomeConfig) -> anyhow::Result<Vec<String>> {
        let (result, requests) = fetched((200, body()), "today", home).await?;
        result?;
        assert_eq!(requests.len(), 1, "{requests:?}");
        let pairs = requests[0].url.query_pairs();
        Ok(pairs
            .map(|(name, value)| format!("{name}={value}"))
            .collect())
    }

    /// AC-17.3-2.
    #[tokio::test]
    async fn ac_17_3_2_request_has_location_and_daily() -> anyhow::Result<()> {
        let expected = [
            "latitude=41.88",
            "longitude=-87.63",
            "timezone=America/Chicago",
            "temperature_unit=fahrenheit",
            "forecast_days=2",
            "daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max",
        ];
        assert_eq!(query(&fakes::house().config).await?, expected);
        Ok(())
    }

    /// AC-17.3-3.
    #[tokio::test]
    async fn ac_17_3_3_celsius() -> anyhow::Result<()> {
        let mut home = fakes::house().config;
        "°C".clone_into(&mut home.unit_system.temperature);
        let query = query(&home).await?;
        assert!(
            query.contains(&"temperature_unit=celsius".to_owned()),
            "{query:?}"
        );
        Ok(())
    }

    /// AC-17.3-4.
    #[tokio::test]
    async fn ac_17_3_4_today() -> anyhow::Result<()> {
        let expected = json!({
            "condition": "partly cloudy", "high": 72, "low": 54, "precipitation_chance": 20,
        });
        assert_eq!(result((200, body()), "today").await?, expected);
        Ok(())
    }

    /// AC-17.3-5.
    #[tokio::test]
    async fn ac_17_3_5_tomorrow() -> anyhow::Result<()> {
        let expected =
            json!({ "condition": "rainy", "high": 60, "low": 50, "precipitation_chance": 80 });
        assert_eq!(result((200, body()), "tomorrow").await?, expected);
        Ok(())
    }

    /// AC-17.3-6.
    #[test]
    fn ac_17_3_6_forecast_is_pure() -> anyhow::Result<()> {
        let body = body();
        assert_eq!(forecast(&body, 1)?, forecast(&body, 1)?);
        Ok(())
    }

    /// AC-17.3-7.
    #[tokio::test]
    async fn ac_17_3_7_other_day_makes_no_request() -> anyhow::Result<()> {
        let (result, requests) = fetched((200, body()), "monday", &fakes::house().config).await?;
        assert!(result.is_err(), "{result:?}");
        assert!(requests.is_empty(), "{requests:?}");
        Ok(())
    }

    /// AC-17.3-8.
    #[tokio::test]
    async fn ac_17_3_8_bad_reply() {
        let (mut null, mut unknown) = (body(), body());
        null["daily"]["temperature_2m_max"][0] = Value::Null;
        unknown["daily"]["weather_code"][0] = json!(42);
        for reply in [(500, body()), (200, null), (200, unknown)] {
            let result = result(reply, "today").await;
            assert!(result.is_err(), "{result:?}");
        }
    }

    /// AC-17.3-9.
    #[test]
    fn ac_17_3_9_tool() -> anyhow::Result<()> {
        let tool = OpenMeteo::try_new(&WeatherConfig { url: String::new() })?.tool();
        assert_eq!(tool.name, "get_weather");
        let when = json!({ "type": "string", "enum": ["today", "tomorrow"] });
        assert_eq!(tool.parameters["properties"], json!({ "when": when }));
        assert_eq!(tool.parameters["required"], json!(["when"]));
        assert_eq!(tool.parameters["additionalProperties"], json!(false));
        Ok(())
    }
}
