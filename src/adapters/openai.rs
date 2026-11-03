//! `Reasoner` with the `OpenAI` Responses API.

use std::mem;
use std::sync::Mutex;
use std::time::Duration;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::CreateResponseArgs;
use async_openai::types::responses::ResponseFailedEvent;
use async_openai::types::responses::ResponseIncompleteEvent;
use async_openai::types::responses::ResponseRefusalDeltaEvent;
use async_openai::types::responses::ResponseStreamEvent;
use async_openai::types::responses::ResponseTextDeltaEvent;
use async_openai::types::responses::Status;
use async_trait::async_trait;
use futures::StreamExt;
use futures::TryStreamExt;
use futures::future;
use futures::stream;
use futures::stream::BoxStream;
use tokio::time::Instant;

use crate::OpenAiConfig;
use crate::domain::decide::Question;
use crate::domain::speech;
use crate::ports::Reasoner;
use crate::ports::Step;

/// The TCP keepalive interval of the pooled connections.
const KEEPALIVE: Duration = Duration::from_secs(30);

/// The input of the warm request.
const WARM_INPUT: &str = "Reply with OK.";

/// The system message of each answer. It has no date or other changing text, so `OpenAI` prompt
/// caching can reuse it.
#[must_use]
pub fn system_prompt() -> &'static str {
    "You are Jarvis, the voice assistant of a house. Answer the request with plain text to speak \
     aloud: short sentences, with no Markdown, lists or emoji."
}

/// The user message of `question`: the local time, the HA location name, the area, the entities
/// of the area and the text.
#[must_use]
pub fn user_message(question: &Question) -> String {
    let area = question
        .area
        .as_ref()
        .map_or("none", |area| area.0.as_str());
    let entities: Vec<String> = question
        .entities
        .iter()
        .map(|entity| format!("- {}: {}\n", entity.name, entity.state))
        .collect();
    format!(
        "Time: {}\nLocation: {}\nArea: {area}\nEntities:\n{}Request: {}",
        question.now.strftime("%A %Y-%m-%d %H:%M"),
        question.location,
        entities.concat(),
        question.text
    )
}

/// The steps of the answer stream `event`. A refusal is spoken as the answer. `buffer` keeps the
/// text after the last complete sentence.
///
/// # Errors
///
/// The response failed or is incomplete, or the stream gives an error.
fn steps(buffer: &mut String, event: ResponseStreamEvent) -> anyhow::Result<Vec<Step>> {
    match event {
        ResponseStreamEvent::ResponseOutputTextDelta(ResponseTextDeltaEvent { delta, .. })
        | ResponseStreamEvent::ResponseRefusalDelta(ResponseRefusalDeltaEvent { delta, .. }) => {
            let (complete, rest) = speech::sentences(buffer, &delta);
            *buffer = rest;
            Ok(complete.into_iter().map(Step::Sentence).collect())
        }
        ResponseStreamEvent::ResponseCompleted(_) => {
            let rest = mem::take(buffer);
            let last = (!rest.is_empty()).then_some(Step::Sentence(rest));
            Ok(last.into_iter().chain([Step::Done]).collect())
        }
        ResponseStreamEvent::ResponseFailed(ResponseFailedEvent { response, .. })
        | ResponseStreamEvent::ResponseIncomplete(ResponseIncompleteEvent { response, .. }) => {
            anyhow::bail!("the response is {:?}", response.status)
        }
        ResponseStreamEvent::ResponseError(error) => {
            anyhow::bail!("the response stream failed: {}", error.message)
        }
        _ => Ok(Vec::new()),
    }
}

/// A `Reasoner` on one HTTP client for the process life. Its pooled connections have no idle
/// timeout and TCP keepalive.
pub struct OpenAiReasoner {
    /// The API client on the pooled HTTP client.
    client: Client<OpenAIConfig>,
    /// The model of each request.
    model: String,
    /// The output token limit of an answer.
    max_output_tokens: u32,
    /// The time since the last request after which `warm` sends a request.
    warm_idle: Duration,
    /// The time of the last request, or `None` before the first request.
    last_request: Mutex<Option<Instant>>,
}

impl OpenAiReasoner {
    /// A reasoner of `config` with the `OpenAI` API key `key`.
    ///
    /// # Errors
    ///
    /// The HTTP client cannot be built.
    pub fn try_new(config: &OpenAiConfig, key: &str) -> anyhow::Result<Self> {
        Self::try_with_api(config, OpenAIConfig::new().with_api_key(key))
    }

    /// A reasoner of `config` on the API of `api`.
    ///
    /// # Errors
    ///
    /// The HTTP client cannot be built.
    fn try_with_api(config: &OpenAiConfig, api: OpenAIConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .pool_idle_timeout(None)
            .tcp_keepalive(KEEPALIVE)
            .build()?;
        Ok(Self {
            client: Client::build(http, api),
            model: config.model.clone(),
            max_output_tokens: config.max_output_tokens,
            warm_idle: Duration::from_secs(config.warm_idle_s),
            last_request: Mutex::new(None),
        })
    }

    /// Records a request now when the last request is `idle` or more ago, or when there is no
    /// last request. True when it records.
    ///
    /// # Errors
    ///
    /// An earlier record panicked.
    fn record_when_idle(&self, idle: Duration) -> anyhow::Result<bool> {
        let mut last_request = self
            .last_request
            .lock()
            .map_err(|_poisoned| anyhow::anyhow!("an earlier record panicked"))?;
        let now = Instant::now();
        if last_request.is_some_and(|last| now.duration_since(last) < idle) {
            return Ok(false);
        }
        *last_request = Some(now);
        Ok(true)
    }
}

#[async_trait]
impl Reasoner for OpenAiReasoner {
    async fn warm(&self) -> anyhow::Result<()> {
        if !self.record_when_idle(self.warm_idle)? {
            return Ok(());
        }
        let request = CreateResponseArgs::default()
            .model(&self.model)
            .input(WARM_INPUT)
            .max_output_tokens(self.max_output_tokens)
            .store(false)
            .build()?;
        let response = self.client.responses().create(request).await?;
        anyhow::ensure!(
            response.status != Status::Incomplete,
            "the response is incomplete: {:?}",
            response.incomplete_details
        );
        Ok(())
    }

    fn answer(&self, question: &Question) -> BoxStream<'static, anyhow::Result<Step>> {
        let request = CreateResponseArgs::default()
            .model(&self.model)
            .instructions(system_prompt())
            .input(user_message(question))
            .max_output_tokens(self.max_output_tokens)
            .store(false)
            .build();
        let recorded = self.record_when_idle(Duration::ZERO);
        let client = self.client.clone();
        let events = async move {
            recorded?;
            anyhow::Ok(client.responses().create_stream(request?).await?)
        };
        let events = stream::once(events)
            .map_ok(|events| events.map_err(anyhow::Error::from))
            .try_flatten();
        let steps = events.scan(String::new(), |buffer, event| {
            future::ready(Some(event.and_then(|event| steps(buffer, event))))
        });
        steps
            .map_ok(|steps| stream::iter(steps.into_iter().map(Ok)))
            .try_flatten()
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use anyhow::Context as _;
    use serde_json::Value;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    use crate::Config;
    use crate::domain::decide;
    use crate::domain::state::AreaId;
    use crate::domain::state_cache::StateCache;
    use crate::testing::fakes::house;

    /// A response object with `status`.
    fn response(status: &str) -> Value {
        json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 0,
            "model": "gpt-4.1-nano",
            "output": [],
            "status": status,
        })
    }

    /// A mock `OpenAI` server that answers each response request with `status`, and a reasoner of
    /// `jarvis.toml` on it.
    async fn mock(status: &str) -> anyhow::Result<(MockServer, OpenAiReasoner)> {
        serve(ResponseTemplate::new(200).set_body_json(response(status))).await
    }

    /// A mock `OpenAI` server that answers each response request with `template`, and a reasoner
    /// of `jarvis.toml` on it.
    async fn serve(template: ResponseTemplate) -> anyhow::Result<(MockServer, OpenAiReasoner)> {
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/responses"))
            .respond_with(template)
            .mount(&server)
            .await;
        let config = Config::parse(include_str!("../../jarvis.toml"))?.openai;
        let api = OpenAIConfig::new()
            .with_api_key("key")
            .with_api_base(server.uri());
        Ok((server, OpenAiReasoner::try_with_api(&config, api)?))
    }

    /// The number of requests that `server` received.
    async fn requests(server: &MockServer) -> anyhow::Result<usize> {
        let requests = server.received_requests().await;
        Ok(requests.context("the server records no requests")?.len())
    }

    /// The number of requests of a first `warm` and of a second `warm` `idle_s` seconds later.
    async fn warm_twice(idle_s: u64) -> anyhow::Result<(usize, usize)> {
        let (server, reasoner) = mock("completed").await?;
        reasoner.warm().await?;
        let first = requests(&server).await?;
        tokio::time::advance(Duration::from_secs(idle_s)).await;
        reasoner.warm().await?;
        Ok((first, requests(&server).await? - first))
    }

    /// AC-7.1-1.
    #[tokio::test(start_paused = true)]
    async fn ac_7_1_1_first_warm_sends_one_request() -> anyhow::Result<()> {
        assert_eq!(warm_twice(0).await?.0, 1);
        Ok(())
    }

    /// AC-7.1-2.
    #[tokio::test(start_paused = true)]
    async fn ac_7_1_2_warm_within_warm_idle_s_sends_no_request() -> anyhow::Result<()> {
        assert_eq!(warm_twice(59).await?.1, 0);
        Ok(())
    }

    /// AC-7.1-3.
    #[tokio::test(start_paused = true)]
    async fn ac_7_1_3_warm_after_warm_idle_s_sends_one_request() -> anyhow::Result<()> {
        assert_eq!(warm_twice(60).await?.1, 1);
        Ok(())
    }

    /// AC-7.1-5.
    #[tokio::test(start_paused = true)]
    async fn ac_7_1_5_incomplete_response_is_an_error() -> anyhow::Result<()> {
        let (_server, reasoner) = mock("incomplete").await?;
        let error = reasoner.warm().await.err().context("warm succeeded")?;
        assert!(
            format!("{error:#}").contains("incomplete"),
            "the error is {error:#}"
        );
        Ok(())
    }

    /// The question "what is on" from `area` of the reference house on Monday 2026-10-26 08:00,
    /// the local time in `America/Chicago`.
    fn question(area: Option<&str>) -> Question {
        let house = house();
        let location = house.config.location_name.clone();
        let cache = StateCache::load(house);
        let area = area.map(|area| AreaId(area.to_owned()));
        let now = jiff::civil::date(2026, 10, 26).at(8, 0, 0, 0);
        decide::question("what is on", area.as_ref(), &location, now, &cache)
    }

    /// The user message of `question(area)`.
    fn asked(area: Option<&str>) -> String {
        user_message(&question(area))
    }

    /// The steps of an answer that a mock `OpenAI` server streams as `deltas` of the event `kind`.
    async fn answered(kind: &str, deltas: &[&str]) -> anyhow::Result<Vec<Step>> {
        let texts = deltas.iter().map(|delta| {
            json!({
                "type": kind,
                "sequence_number": 0,
                "item_id": "msg_1",
                "output_index": 0,
                "content_index": 0,
                "delta": delta,
            })
        });
        let completed = json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": response("completed"),
        });
        let events: Vec<String> = texts
            .chain([completed])
            .map(|event| format!("data: {event}\n\n"))
            .collect();
        let template =
            ResponseTemplate::new(200).set_body_raw(events.concat(), "text/event-stream");
        let (_server, reasoner) = serve(template).await?;
        reasoner
            .answer(&question(Some("kitchen")))
            .try_collect()
            .await
    }

    /// An answer within `warm_idle_s` of the next `warm` counts as the last request.
    #[tokio::test(start_paused = true)]
    async fn answer_records_the_last_request() -> anyhow::Result<()> {
        let (server, reasoner) = mock("completed").await?;
        reasoner.warm().await?;
        tokio::time::advance(Duration::from_secs(59)).await;
        let _steps: Vec<anyhow::Result<Step>> = reasoner.answer(&question(None)).collect().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        reasoner.warm().await?;
        assert_eq!(requests(&server).await?, 2);
        Ok(())
    }

    /// The event type of a text delta.
    const TEXT: &str = "response.output_text.delta";

    /// A refusal is the answer text.
    #[tokio::test(start_paused = true)]
    async fn refusal_is_spoken() -> anyhow::Result<()> {
        let steps = answered("response.refusal.delta", &["I can't help."]).await?;
        assert_eq!(
            steps,
            [Step::Sentence("I can't help.".to_owned()), Step::Done]
        );
        Ok(())
    }

    /// AC-7.3-2.
    #[tokio::test(start_paused = true)]
    async fn ac_7_3_2_rest_is_the_last_sentence_at_the_end() -> anyhow::Result<()> {
        let steps = answered(TEXT, &["Hello there. It", " is"]).await?;
        assert_eq!(steps[1..], [Step::Sentence("It is".to_owned()), Step::Done]);
        Ok(())
    }

    /// AC-7.3-4.
    #[tokio::test(start_paused = true)]
    async fn ac_7_3_4_two_sentences_give_sentence_sentence_done() -> anyhow::Result<()> {
        let steps = answered(TEXT, &["Hello there.", " How are", " you?"]).await?;
        assert_eq!(
            steps,
            [
                Step::Sentence("Hello there.".to_owned()),
                Step::Sentence("How are you?".to_owned()),
                Step::Done,
            ]
        );
        Ok(())
    }

    /// The entity lines of `message`.
    fn entity_lines(message: &str) -> Vec<&str> {
        message
            .lines()
            .filter(|line| line.starts_with("- "))
            .collect()
    }

    /// AC-7.2-1.
    #[test]
    fn ac_7_2_1_system_message_is_equal_for_two_areas() {
        let kitchen = (system_prompt(), asked(Some("kitchen")));
        let office = (system_prompt(), asked(Some("office")));
        assert_ne!(kitchen.1, office.1, "the user messages are equal");
        assert_eq!(kitchen.0.as_bytes(), office.0.as_bytes());
    }

    /// AC-7.2-2.
    #[test]
    fn ac_7_2_2_kitchen_user_message_has_only_kitchen_entities() {
        assert_eq!(
            entity_lines(&asked(Some("kitchen"))),
            [
                "- Carson Kitchen Media Player: idle",
                "- Thermostat: heat",
                "- Thermostat Current Temperature: 63.5",
                "- Thermostat Motion: on",
                "- Overhead Light: off",
                "- Strawberry Lamp: on",
            ]
        );
    }

    /// AC-7.2-3.
    #[test]
    fn ac_7_2_3_no_area_has_no_entities() {
        let message = asked(None);
        assert!(entity_lines(&message).is_empty(), "{message}");
    }

    /// AC-7.2-4.
    #[test]
    fn ac_7_2_4_user_message_has_local_time_and_location() {
        let message = asked(Some("kitchen"));
        assert!(message.contains("Monday 2026-10-26 08:00"), "{message}");
        assert!(message.contains(&house().config.location_name), "{message}");
    }

    /// AC-7.2-5.
    #[test]
    fn ac_7_2_5_system_prompt_asks_for_plain_spoken_text() {
        let prompt = system_prompt().to_lowercase();
        for wanted in ["plain text", "speak", "markdown", "lists", "emoji"] {
            assert!(prompt.contains(wanted), "{wanted:?} is not in {prompt:?}");
        }
    }
}
