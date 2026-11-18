//! `Plugin` `get_new_shows` and `MediaLibrary` with the Plex server API.

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
use crate::ports::MediaLibrary;
use crate::ports::Plugin;
use crate::ports::Title;
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

/// The section types whose items are titles.
const TITLE_KINDS: [&str; 2] = ["movie", "show"];

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

#[async_trait]
impl MediaLibrary for Plex {
    async fn titles(&self) -> anyhow::Result<Vec<Title>> {
        let sections = self.library("sections").await?;
        let mut titles = Vec::new();
        for section in items(&sections, "Directory")? {
            titles.extend(self.section_titles(section).await?);
        }
        Ok(titles)
    }
}

impl Plex {
    /// The titles of the Plex section `section`, none when its type is not a title kind.
    ///
    /// # Errors
    ///
    /// The section has no `type` or `key`, the request fails, or an item has no `title`.
    async fn section_titles(&self, section: &Value) -> anyhow::Result<Vec<Title>> {
        let kind = section["type"].as_str();
        let kind = kind.with_context(|| format!("no type: {section}"))?;
        if !TITLE_KINDS.contains(&kind) {
            return Ok(Vec::new());
        }
        let key = section["key"].as_str();
        let key = key.with_context(|| format!("no key: {section}"))?;
        let all = self.library(&format!("sections/{key}/all")).await?;
        let items = items(&all, "Metadata")?.iter();
        items.map(|item| title(item, kind)).collect()
    }
}

/// The title of the Plex item `item` with `kind`.
///
/// # Errors
///
/// `item` has no `title`.
fn title(item: &Value, kind: &str) -> anyhow::Result<Title> {
    let name = item["title"].as_str();
    let name = name.with_context(|| format!("no title: {item}"))?;
    Ok(Title {
        name: name.to_owned(),
        kind: kind.to_owned(),
    })
}

/// The items of the list `field` of the `MediaContainer` of the Plex reply `body`.
///
/// # Errors
///
/// `body` has no `MediaContainer`.
fn items<'body>(body: &'body Value, field: &str) -> anyhow::Result<&'body [Value]> {
    let container = body.get("MediaContainer").context("no MediaContainer")?;
    // Plex leaves out an empty list.
    let items = container.get(field).and_then(Value::as_array);
    Ok(items.map_or(&[][..], Vec::as_slice))
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
    let mut names: Vec<String> = Vec::new();
    for item in items(body, "Metadata")? {
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

    /// A mock server that answers a GET of each path of `bodies` with both headers with `status`
    /// and its body.
    async fn server(status: u16, bodies: Vec<(String, Value)>) -> MockServer {
        let server = MockServer::start().await;
        for (path, body) in bodies {
            Mock::given(matchers::method("GET"))
                .and(matchers::path(path))
                .and(matchers::header("X-Plex-Token", "secret"))
                .and(matchers::header("Accept", "application/json"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .mount(&server)
                .await;
        }
        server
    }

    /// The result of `fetch` from a mock server that answers `status` with `recent` and
    /// `on_deck`, and the requests of the mock.
    async fn fetched(
        status: u16,
        recent: Value,
        on_deck: Value,
    ) -> anyhow::Result<(anyhow::Result<Value>, Vec<Request>)> {
        let paths = ["/library/recentlyAdded", "/library/onDeck"].map(str::to_owned);
        let server = server(status, paths.into_iter().zip([recent, on_deck]).collect()).await;
        let result = plex(&server.uri())?
            .fetch(&Map::new(), &fakes::house().config)
            .await;
        Ok((result, server.received_requests().await.unwrap_or_default()))
    }

    /// The result of `titles` from a mock server that answers `status` with `sections` and the
    /// bodies of `all` for their paths, and the requests of the mock.
    async fn listed(
        status: u16,
        sections: Value,
        all: Vec<(String, Value)>,
    ) -> anyhow::Result<(anyhow::Result<Vec<Title>>, Vec<Request>)> {
        let bodies = [("/library/sections".to_owned(), sections)]
            .into_iter()
            .chain(all);
        let server = server(status, bodies.collect()).await;
        let result = plex(&server.uri())?.titles().await;
        Ok((result, server.received_requests().await.unwrap_or_default()))
    }

    /// The sections of AC-18.9-12.
    fn sections() -> Value {
        let kinds = [("1", "movie"), ("2", "show"), ("3", "artist")];
        let sections = kinds.map(|(key, kind)| json!({ "key": key, "type": kind, "title": key }));
        json!({ "MediaContainer": { "Directory": sections } })
    }

    /// The path of the items of section `key` and a reply with an item for each of `titles`.
    fn section(key: &str, titles: &[&str]) -> (String, Value) {
        let items: Vec<Value> = titles
            .iter()
            .map(|title| json!({ "title": title }))
            .collect();
        (format!("/library/sections/{key}/all"), container(&items))
    }

    /// The items of the sections of AC-18.9-12.
    fn sections_all() -> Vec<(String, Value)> {
        let movies = section("1", &["The Matrix", "Fargo"]);
        vec![
            movies,
            section("2", &["VisionQuest"]),
            section("3", &["Radiohead"]),
        ]
    }

    /// AC-18.9-12. The mocks answer only a GET with both headers.
    #[tokio::test]
    async fn ac_18_9_12_titles_of_movie_and_show_sections() -> anyhow::Result<()> {
        let (result, requests) = listed(200, sections(), sections_all()).await?;
        let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
        let expected = [
            "/library/sections",
            "/library/sections/1/all",
            "/library/sections/2/all",
        ];
        assert_eq!(paths, expected);
        let titles = [
            ("The Matrix", "movie"),
            ("Fargo", "movie"),
            ("VisionQuest", "show"),
        ];
        let titles = titles.map(|(name, kind)| fakes::title(name, kind));
        assert_eq!(result?, titles);
        Ok(())
    }

    /// AC-18.9-13.
    #[tokio::test]
    async fn ac_18_9_13_bad_titles_reply() -> anyhow::Result<()> {
        let nameless = (
            String::from("/library/sections/1/all"),
            container(&[json!({})]),
        );
        let nameless = vec![nameless, section("2", &[])];
        let replies = [
            (401, sections(), sections_all()),
            (200, json!({}), sections_all()),
            (200, sections(), nameless),
        ];
        for (status, sections, all) in replies {
            let result = listed(status, sections, all).await?.0;
            assert!(result.is_err(), "{result:?}");
        }
        Ok(())
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
