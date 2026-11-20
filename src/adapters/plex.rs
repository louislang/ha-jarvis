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

    /// The JSON reply of `GET <url>/<path>`.
    ///
    /// # Errors
    ///
    /// The request fails, the status is not a success, or the body is not JSON.
    async fn get(&self, path: &str) -> anyhow::Result<Value> {
        Ok(self
            .client
            .get(format!("{}/{path}", self.url))
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
        let recent = self.get("library/recentlyAdded").await?;
        let on_deck = self.get("library/onDeck").await?;
        shows(&recent, &on_deck, self.clock.now())
    }
}

#[async_trait]
impl MediaLibrary for Plex {
    async fn titles(&self) -> anyhow::Result<Vec<Title>> {
        let identity = self.get("identity").await?;
        let server = identity["MediaContainer"]["machineIdentifier"].as_str();
        let server = server.with_context(|| format!("no machineIdentifier: {identity}"))?;
        let sections = self.get("library/sections").await?;
        let mut titles = Vec::new();
        for section in items(&sections, "Directory")? {
            titles.extend(self.section_titles(section, server).await?);
        }
        Ok(titles)
    }

    async fn episode(&self, show: &str, season: u64, episode: u64) -> anyhow::Result<String> {
        let leaves = self
            .get(&format!("library/metadata/{show}/allLeaves"))
            .await?;
        let mut episodes = items(&leaves, "Metadata")?.iter();
        let found = episodes.find(|item| item["parentIndex"] == season && item["index"] == episode);
        let key = found.and_then(|item| item["ratingKey"].as_str());
        let key = key.with_context(|| format!("no season {season} episode {episode} of {show}"))?;
        Ok(key.to_owned())
    }
}

impl Plex {
    /// The titles of the Plex section `section` on the server `server`, none when its type is not
    /// a title kind.
    ///
    /// # Errors
    ///
    /// The section has no `type` or `key`, the request fails, or an item has no `title` or
    /// `ratingKey`.
    async fn section_titles(&self, section: &Value, server: &str) -> anyhow::Result<Vec<Title>> {
        let kind = section["type"].as_str();
        let kind = kind.with_context(|| format!("no type: {section}"))?;
        if !TITLE_KINDS.contains(&kind) {
            return Ok(Vec::new());
        }
        let key = section["key"].as_str();
        let key = key.with_context(|| format!("no key: {section}"))?;
        let all = self.get(&format!("library/sections/{key}/all")).await?;
        let items = items(&all, "Metadata")?.iter();
        items.map(|item| title(item, kind, server)).collect()
    }
}

/// The title of the Plex item `item` with `kind` on the server `server`.
///
/// # Errors
///
/// `item` has no `title` or no `ratingKey`.
fn title(item: &Value, kind: &str, server: &str) -> anyhow::Result<Title> {
    let name = item["title"].as_str();
    let name = name.with_context(|| format!("no title: {item}"))?;
    let key = item["ratingKey"].as_str();
    let key = key.with_context(|| format!("no ratingKey: {item}"))?;
    Ok(Title {
        name: name.to_owned(),
        kind: kind.to_owned(),
        key: key.to_owned(),
        server: server.to_owned(),
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

    /// The result of `titles` from a mock server that answers `status` with `identity`,
    /// `sections` and the bodies of `all` for their paths, and the requests of the mock.
    async fn listed(
        status: u16,
        identity: Value,
        sections: Value,
        all: Vec<(String, Value)>,
    ) -> anyhow::Result<(anyhow::Result<Vec<Title>>, Vec<Request>)> {
        let bodies = [
            ("/identity".to_owned(), identity),
            ("/library/sections".to_owned(), sections),
        ];
        let bodies = bodies.into_iter().chain(all);
        let server = server(status, bodies.collect()).await;
        let result = plex(&server.uri())?.titles().await;
        Ok((result, server.received_requests().await.unwrap_or_default()))
    }

    /// The reply of `GET /identity` with the server ID "abc123".
    fn identity() -> Value {
        json!({ "MediaContainer": { "machineIdentifier": "abc123" } })
    }

    /// The sections of AC-18.9-12.
    fn sections() -> Value {
        let kinds = [("1", "movie"), ("2", "show"), ("3", "artist")];
        let sections = kinds.map(|(key, kind)| json!({ "key": key, "type": kind, "title": key }));
        json!({ "MediaContainer": { "Directory": sections } })
    }

    /// The path of the items of section `key` and a reply with an item for each of `titles`,
    /// with the title as its `ratingKey`.
    fn section(key: &str, titles: &[&str]) -> (String, Value) {
        let items: Vec<Value> = titles
            .iter()
            .map(|title| json!({ "title": title, "ratingKey": title }))
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

    /// AC-18.19-1. The mocks answer only a GET with both headers.
    #[tokio::test]
    async fn ac_18_19_1_titles_with_keys_and_server() -> anyhow::Result<()> {
        let (result, requests) = listed(200, identity(), sections(), sections_all()).await?;
        let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
        let expected = [
            "/identity",
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
            let result = listed(status, identity(), sections, all).await?.0;
            assert!(result.is_err(), "{result:?}");
        }
        Ok(())
    }

    /// AC-18.19-2.
    #[tokio::test]
    async fn ac_18_19_2_no_server_or_key_is_an_error() -> anyhow::Result<()> {
        let anonymous = json!({ "MediaContainer": {} });
        let result = listed(200, anonymous, sections(), sections_all()).await?.0;
        assert!(result.is_err(), "{result:?}");
        let keyless = (
            String::from("/library/sections/1/all"),
            container(&[json!({ "title": "The Matrix" })]),
        );
        let keyless = vec![keyless, section("2", &[])];
        let result = listed(200, identity(), sections(), keyless).await?.0;
        assert!(result.is_err(), "{result:?}");
        Ok(())
    }

    /// The result of `episode("12", 2, 5)` from a mock server that answers `status` with the
    /// episodes `episodes` (season, episode, key), and the requests of the mock.
    async fn found(
        status: u16,
        episodes: &[(u64, u64, &str)],
    ) -> anyhow::Result<(anyhow::Result<String>, Vec<Request>)> {
        let items = episodes.iter().map(|(season, episode, key)| {
            json!({ "parentIndex": season, "index": episode, "ratingKey": key })
        });
        let items: Vec<Value> = items.collect();
        let path = "/library/metadata/12/allLeaves".to_owned();
        let server = server(status, vec![(path, container(&items))]).await;
        let result = plex(&server.uri())?.episode("12", 2, 5).await;
        Ok((result, server.received_requests().await.unwrap_or_default()))
    }

    /// AC-18.19-3. The mock answers only a GET with both headers.
    #[tokio::test]
    async fn ac_18_19_3_episode_key_by_season_and_number() -> anyhow::Result<()> {
        let episodes = [(2, 4, "1204"), (2, 5, "1205"), (3, 5, "1305")];
        let (result, requests) = found(200, &episodes).await?;
        let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
        assert_eq!(paths, ["/library/metadata/12/allLeaves"]);
        assert_eq!(result?, "1205");
        Ok(())
    }

    /// AC-18.19-4.
    #[tokio::test]
    async fn ac_18_19_4_missing_episode_is_an_error() -> anyhow::Result<()> {
        let missing = found(200, &[(2, 4, "1204"), (3, 5, "1305")]).await?.0;
        assert!(missing.is_err(), "{missing:?}");
        let refused = found(401, &[(2, 5, "1205")]).await?.0;
        assert!(refused.is_err(), "{refused:?}");
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
