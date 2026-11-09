//! `Plugin` `get_new_shows` with the Plex server API.

use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use jiff::Timestamp;
use reqwest::header;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::PlexConfig;
use crate::domain::state::HomeConfig;
use crate::ports::Clock;
use crate::ports::Plugin;
use crate::ports::Tool;

/// The time before now in which an added item is new.
const NEW_S: i64 = 86_400;

/// The show name field of each kept type of the recently added items.
const ADDED: [(&str, &str); 3] = [
    ("episode", "grandparentTitle"),
    ("season", "parentTitle"),
    ("show", "title"),
];

/// The show name field of each kept type of the On Deck items.
const ON_DECK: [(&str, &str); 1] = [("episode", "grandparentTitle")];

/// The new shows and the shows to continue of a Plex server.
pub struct Plex {
    /// The HTTP client of all requests.
    client: reqwest::Client,
    /// The Plex server URL.
    url: String,
    /// The Plex token.
    token: String,
    /// The time that sets which added items are new.
    clock: Arc<dyn Clock>,
}

impl Plex {
    /// The plugin on the Plex server of `config` with `token` and `clock`.
    ///
    /// # Errors
    ///
    /// The HTTP client cannot be built.
    pub fn try_new(
        config: &PlexConfig,
        token: &str,
        clock: Arc<dyn Clock>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            url: config.url.clone(),
            token: token.to_owned(),
            clock,
        })
    }

    /// The JSON reply of `GET <url>/library/<path>`.
    ///
    /// # Errors
    ///
    /// The request fails, the status is not a success, or the body is not JSON.
    async fn library(&self, path: &str) -> anyhow::Result<Value> {
        Ok(self
            .client
            .get(format!("{}/library/{path}", self.url))
            .header("X-Plex-Token", &self.token)
            .header(header::ACCEPT, "application/json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[async_trait]
impl Plugin for Plex {
    fn tool(&self) -> Tool {
        Tool {
            name: "get_new_shows".to_owned(),
            description: "Gives the shows added to Plex in the last day and the shows to continue."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false,
            }),
        }
    }

    async fn fetch(
        &self,
        _arguments: &Map<String, Value>,
        _home: &HomeConfig,
    ) -> anyhow::Result<Value> {
        let recent = self.library("recentlyAdded").await?;
        let on_deck = self.library("onDeck").await?;
        shows(&recent, &on_deck, self.clock.now())
    }
}

/// The shows of the Plex reply `recent` added in the day before `now`, and the shows of the
/// Plex reply `on_deck`.
///
/// # Errors
///
/// A reply has no `MediaContainer`, or a kept item has no show name or no `addedAt`.
fn shows(recent: &Value, on_deck: &Value, now: Timestamp) -> anyhow::Result<Value> {
    let since = now.as_second() - NEW_S;
    let new = |item: &Value| {
        let added_at = item["addedAt"].as_i64();
        Ok(added_at.with_context(|| format!("no addedAt: {item}"))? >= since)
    };
    let added = names(recent, &ADDED, new)?;
    let on_deck = names(on_deck, &ON_DECK, |_| Ok(true))?;
    Ok(json!({ "added": added, "on_deck": on_deck }))
}

/// The show names, each once in the order of its first item, of the items of the Plex reply
/// `body` whose type has a name field in `fields` and that `keep` keeps.
///
/// # Errors
///
/// `body` has no `MediaContainer`, a kept item has no show name, or `keep` fails.
fn names(
    body: &Value,
    fields: &[(&str, &str)],
    keep: impl Fn(&Value) -> anyhow::Result<bool>,
) -> anyhow::Result<Vec<String>> {
    let container = body.get("MediaContainer").context("no MediaContainer")?;
    // Plex leaves out an empty `Metadata`.
    let items = container.get("Metadata").and_then(Value::as_array);
    let mut names: Vec<String> = Vec::new();
    for item in items.map_or(&[][..], Vec::as_slice) {
        let Some((_, field)) = fields.iter().find(|(kind, _)| item["type"] == *kind) else {
            continue;
        };
        let name = item[field].as_str();
        let name = name.with_context(|| format!("no {field}: {item}"))?;
        if keep(item)? && !names.iter().any(|kept| kept == name) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
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
    use crate::testing::fakes::FakeClock;

    /// The time of each fetch: 2026-11-08T12:00:00Z.
    const NOW: i64 = 1_794_139_200;

    /// A Plex reply with the items `items`.
    fn container(items: &[Value]) -> Value {
        json!({ "MediaContainer": { "size": items.len(), "Metadata": items } })
    }

    /// An item of `kind` with the show name field `field` set to `name`, added `hours` before
    /// `NOW`.
    fn item(kind: &str, field: &str, name: &str, hours: i64) -> Value {
        json!({ "type": kind, field: name, "addedAt": NOW - hours * 3600 })
    }

    /// The recently added items of AC-18.11-3.
    fn recent() -> Value {
        container(&[
            item("episode", "grandparentTitle", "Bluey", 1),
            item("season", "parentTitle", "Severance", 2),
            item("episode", "grandparentTitle", "Bluey", 3),
            item("movie", "title", "The Matrix", 1),
            item("episode", "grandparentTitle", "The Office", 25),
        ])
    }

    /// The On Deck items of AC-18.11-3.
    fn on_deck() -> Value {
        container(&[
            item("episode", "grandparentTitle", "The Office", 100),
            item("episode", "grandparentTitle", "Bluey", 1),
        ])
    }

    /// The plugin on the server at `url` with the token `secret` at `NOW`.
    fn plex(url: &str) -> anyhow::Result<Plex> {
        let config = PlexConfig {
            url: url.to_owned(),
            token_env: "PLEX_TOKEN".to_owned(),
        };
        let clock = Arc::new(FakeClock(Timestamp::from_second(NOW)?));
        Plex::try_new(&config, "secret", clock)
    }

    /// The result of `fetch` from a mock server that answers `status` with `recent` and
    /// `on_deck`, and the requests of the mock.
    async fn fetched(
        status: u16,
        recent: Value,
        on_deck: Value,
    ) -> anyhow::Result<(anyhow::Result<Value>, Vec<Request>)> {
        let server = MockServer::start().await;
        for (path, body) in [
            ("/library/recentlyAdded", recent),
            ("/library/onDeck", on_deck),
        ] {
            Mock::given(matchers::method("GET"))
                .and(matchers::path(path))
                .and(matchers::header("X-Plex-Token", "secret"))
                .and(matchers::header("Accept", "application/json"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .mount(&server)
                .await;
        }
        let result = plex(&server.uri())?
            .fetch(&Map::new(), &fakes::house().config)
            .await;
        Ok((result, server.received_requests().await.unwrap_or_default()))
    }

    /// AC-18.11-1.
    #[test]
    fn ac_18_11_1_tool() -> anyhow::Result<()> {
        let tool = plex("")?.tool();
        assert_eq!(tool.name, "get_new_shows");
        assert_eq!(tool.parameters["type"], json!("object"));
        assert_eq!(tool.parameters["properties"], json!({}));
        assert_eq!(tool.parameters["additionalProperties"], json!(false));
        Ok(())
    }

    /// AC-18.11-2. The mocks answer only a GET with both headers.
    #[tokio::test]
    async fn ac_18_11_2_requests_have_token_and_accept() -> anyhow::Result<()> {
        let (result, requests) = fetched(200, recent(), on_deck()).await?;
        result?;
        let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
        assert_eq!(paths, ["/library/recentlyAdded", "/library/onDeck"]);
        Ok(())
    }

    /// AC-18.11-3.
    #[tokio::test]
    async fn ac_18_11_3_new_and_on_deck_shows() -> anyhow::Result<()> {
        let expected =
            json!({ "added": ["Bluey", "Severance"], "on_deck": ["The Office", "Bluey"] });
        assert_eq!(fetched(200, recent(), on_deck()).await?.0?, expected);
        Ok(())
    }

    /// AC-18.11-4.
    #[tokio::test]
    async fn ac_18_11_4_no_metadata_gives_empty_lists() -> anyhow::Result<()> {
        let empty = json!({ "MediaContainer": { "size": 0 } });
        let result = fetched(200, empty.clone(), empty).await?.0?;
        assert_eq!(result, json!({ "added": [], "on_deck": [] }));
        Ok(())
    }

    /// AC-18.11-5.
    #[tokio::test]
    async fn ac_18_11_5_bad_reply() -> anyhow::Result<()> {
        let nameless = container(&[json!({ "type": "episode", "addedAt": NOW })]);
        let replies = [
            (401, recent(), on_deck()),
            (200, json!({}), on_deck()),
            (200, recent(), nameless),
        ];
        for (status, recent, on_deck) in replies {
            let result = fetched(status, recent, on_deck).await?.0;
            assert!(result.is_err(), "{result:?}");
        }
        Ok(())
    }

    /// AC-18.11-6.
    #[test]
    fn ac_18_11_6_shows_is_pure() -> anyhow::Result<()> {
        let (recent, on_deck, now) = (recent(), on_deck(), Timestamp::from_second(NOW)?);
        assert_eq!(
            shows(&recent, &on_deck, now)?,
            shows(&recent, &on_deck, now)?
        );
        Ok(())
    }
}
