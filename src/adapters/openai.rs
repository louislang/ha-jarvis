//! `Reasoner` with the `OpenAI` Responses API.

use std::collections::BTreeSet;
use std::mem;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context as _;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::CreateResponse;
use async_openai::types::responses::CreateResponseArgs;
use async_openai::types::responses::FunctionTool;
use async_openai::types::responses::FunctionToolCall;
use async_openai::types::responses::InputItem;
use async_openai::types::responses::InputParam;
use async_openai::types::responses::OutputItem;
use async_openai::types::responses::Response;
use async_openai::types::responses::ResponseCompletedEvent;
use async_openai::types::responses::ResponseFailedEvent;
use async_openai::types::responses::ResponseFormatJsonSchema;
use async_openai::types::responses::ResponseIncompleteEvent;
use async_openai::types::responses::ResponseOutputItemDoneEvent;
use async_openai::types::responses::ResponseRefusalDeltaEvent;
use async_openai::types::responses::ResponseStreamEvent;
use async_openai::types::responses::ResponseTextDeltaEvent;
use async_openai::types::responses::Status;
use async_openai::types::responses::Tool;
use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::future;
use futures::stream;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tokio::time::Instant;

use crate::OpenAiConfig;
use crate::domain::decide::Question;
use crate::domain::speech;
use crate::domain::state::ServiceCall;
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
        .map(|item| format!("- {} ({}): {}\n", item.name, item.entity_id, item.state))
        .collect();
    format!(
        "Time: {}\nLocation: {}\nArea: {area}\nEntities:\n{}Request: {}",
        question.now.strftime("%A %Y-%m-%d %H:%M"),
        question.location,
        entities.concat(),
        question.text
    )
}

/// The `generate` input: `n` new responses for the pool `pool`, unlike each of `existing`, with
/// only the template variables `variables`.
fn generate_prompt(
    pool: &str,
    existing: &[String],
    variables: &BTreeSet<String>,
    n: usize,
) -> String {
    let existing: Vec<String> = existing.iter().map(|text| format!("- {text}\n")).collect();
    let variables: Vec<&str> = variables.iter().map(String::as_str).collect();
    format!(
        "Write {n} new responses of the voice assistant Jarvis for the response pool {pool}. Each \
         response is plain text to speak aloud and differs from each existing response. A \
         response can use only these template variables, as {{{{ name }}}}: {}.\nExisting \
         responses:\n{}",
        variables.join(", "),
        existing.concat()
    )
}

/// The structured output format of `generate`: `{ "responses": [string] }`.
fn responses_format() -> ResponseFormatJsonSchema {
    ResponseFormatJsonSchema {
        description: None,
        name: "responses".to_owned(),
        schema: json!({
            "type": "object",
            "properties": {"responses": {"type": "array", "items": {"type": "string"}}},
            "required": ["responses"],
            "additionalProperties": false,
        }),
        strict: Some(true),
    }
}

/// The output of `generate`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Generated {
    /// The new responses.
    responses: Vec<String>,
}

/// The `call_service` tool: one HA service call.
fn call_service() -> Tool {
    Tool::Function(FunctionTool {
        name: "call_service".to_owned(),
        description: Some("Calls a Home Assistant service on entities of the house.".to_owned()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "domain": {"type": "string", "description": "For example light."},
                "service": {"type": "string", "description": "For example turn_on."},
                "target": {"type": "array", "items": {"type": "string"}, "description": "Entity IDs."},
                "data": {"type": "object", "description": "The service data."},
            },
            "required": ["domain", "service", "target", "data"],
        })),
        // Strict mode does not permit the free-form `data` object.
        strict: Some(false),
        ..FunctionTool::default()
    })
}

/// The arguments of a `call_service` tool call.
#[derive(Deserialize)]
struct Arguments {
    /// For example `light`.
    domain: String,
    /// For example `turn_on`.
    service: String,
    /// The target entity IDs.
    target: Vec<String>,
    /// The service data.
    data: Map<String, Value>,
}

/// The text in `buffer` as the last sentence, or `None` when it is empty.
fn rest(buffer: &mut String) -> Option<Step> {
    let rest = mem::take(buffer);
    (!rest.is_empty()).then_some(Step::Sentence(rest))
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
            Ok(rest(buffer).into_iter().chain([Step::Done]).collect())
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

/// One answer in progress: one response request for each round of tool calls.
struct Answer {
    /// The API client.
    client: Client<OpenAIConfig>,
    /// The request of the next response.
    request: CreateResponse,
    /// The tool outputs of the current response.
    outputs: Vec<InputItem>,
    /// The number of tool calls that the answer can still make.
    calls_left: u32,
    /// The channel of the steps to the consumer.
    steps: mpsc::UnboundedSender<anyhow::Result<Step>>,
}

impl Answer {
    /// Sends the steps of one response and runs its tool calls. True when the response
    /// completes with a tool call, so the answer continues.
    async fn round(&mut self) -> anyhow::Result<bool> {
        let request = self.request.clone();
        let mut events = self.client.responses().create_stream(request).await?;
        let mut buffer = String::new();
        let mut next = false;
        while let Some(event) = events.next().await {
            next |= self.event(&mut buffer, event?).await?;
        }
        Ok(next)
    }

    /// Sends the steps of `event` or runs its tool call. `buffer` keeps the text after the last
    /// complete sentence. True when the response completes with a tool call.
    async fn event(
        &mut self,
        buffer: &mut String,
        event: ResponseStreamEvent,
    ) -> anyhow::Result<bool> {
        match event {
            ResponseStreamEvent::ResponseOutputItemDone(ResponseOutputItemDoneEvent {
                item: OutputItem::FunctionCall(call),
                ..
            }) => self.call(call).await?,
            ResponseStreamEvent::ResponseCompleted(ResponseCompletedEvent { response, .. })
                if !self.outputs.is_empty() =>
            {
                self.request.previous_response_id = Some(response.id);
                self.request.input = InputParam::Items(mem::take(&mut self.outputs));
                self.send(rest(buffer))?;
                return Ok(true);
            }
            event => self.send(steps(buffer, event)?)?,
        }
        Ok(false)
    }

    /// Runs the tool call `call` through the consumer and keeps its output.
    async fn call(&mut self, call: FunctionToolCall) -> anyhow::Result<()> {
        let result = match self.service_call(&call.arguments) {
            Ok(service_call) => {
                let (reply, result) = oneshot::channel();
                self.send([Step::Call(service_call, reply)])?;
                result.await?
            }
            Err(error) => Err(error),
        };
        let output = match result {
            Ok(Some(response)) => response.to_string(),
            Ok(None) => "done".to_owned(),
            Err(error) => format!("error: {error:#}"),
        };
        let output =
            json!({"type": "function_call_output", "call_id": call.call_id, "output": output});
        self.outputs.push(serde_json::from_value(output)?);
        Ok(())
    }

    /// The service call of the tool call `arguments`. An error when the answer has
    /// `max_tool_calls` calls or the arguments are not valid.
    fn service_call(&mut self, arguments: &str) -> anyhow::Result<ServiceCall> {
        self.calls_left = self.calls_left.checked_sub(1).context("limit reached")?;
        let arguments: Arguments = serde_json::from_str(arguments)?;
        Ok(ServiceCall {
            domain: arguments.domain,
            service: arguments.service,
            target: arguments.target,
            data: arguments.data,
            return_response: false,
        })
    }

    /// Sends `steps` to the consumer.
    fn send(&self, steps: impl IntoIterator<Item = Step>) -> anyhow::Result<()> {
        for step in steps {
            self.steps.unbounded_send(Ok(step))?;
        }
        Ok(())
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
    /// The output token limit of a structured output.
    bulk_max_output_tokens: u32,
    /// The most tool calls of one answer.
    max_tool_calls: u32,
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
            bulk_max_output_tokens: config.bulk_max_output_tokens,
            max_tool_calls: config.max_tool_calls,
            warm_idle: Duration::from_secs(config.warm_idle_s),
            last_request: Mutex::new(None),
        })
    }

    /// The response to `request`.
    ///
    /// # Errors
    ///
    /// The request fails, or the response is incomplete.
    async fn create(&self, request: CreateResponse) -> anyhow::Result<Response> {
        let response = self.client.responses().create(request).await?;
        anyhow::ensure!(
            response.status != Status::Incomplete,
            "the response is incomplete: {:?}",
            response.incomplete_details
        );
        Ok(response)
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
        self.create(request).await?;
        Ok(())
    }

    fn answer(&self, question: &Question) -> BoxStream<'static, anyhow::Result<Step>> {
        let request = CreateResponseArgs::default()
            .model(&self.model)
            .instructions(system_prompt())
            .input(user_message(question))
            .tools([call_service()])
            .max_output_tokens(self.max_output_tokens)
            .build();
        let recorded = self.record_when_idle(Duration::ZERO);
        let client = self.client.clone();
        let calls_left = self.max_tool_calls;
        let (steps, receiver) = mpsc::unbounded();
        let errors = steps.clone();
        let run = async move {
            recorded?;
            let mut answer = Answer {
                client,
                request: request?,
                outputs: Vec::new(),
                calls_left,
                steps,
            };
            while answer.round().await? {}
            Ok(())
        };
        let run =
            stream::once(
                async move { run.await.or_else(|error| errors.unbounded_send(Err(error))) },
            );
        // The steps and the error go through one channel, so the error comes after the steps.
        stream::select(receiver, run.filter_map(|_sent| future::ready(None))).boxed()
    }

    async fn generate(
        &self,
        pool: &str,
        existing: &[String],
        variables: &BTreeSet<String>,
        n: usize,
    ) -> anyhow::Result<Vec<String>> {
        let request = CreateResponseArgs::default()
            .model(&self.model)
            .input(generate_prompt(pool, existing, variables, n))
            .text(responses_format())
            .max_output_tokens(self.bulk_max_output_tokens)
            .build()?;
        let response = self.create(request).await?;
        let text = response.output_text().context("the response has no text")?;
        // Serde also reads a struct from a JSON array, which the schema does not permit.
        let object: Map<String, Value> = serde_json::from_str(&text)?;
        let generated: Generated = serde_json::from_value(Value::Object(object))?;
        Ok(generated.responses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::TryStreamExt;
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

    /// A response stream of `events` and a completed event.
    fn stream(events: impl IntoIterator<Item = Value>) -> ResponseTemplate {
        let completed = json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": response("completed"),
        });
        let events: Vec<String> = events
            .into_iter()
            .chain([completed])
            .map(|event| format!("data: {event}\n\n"))
            .collect();
        ResponseTemplate::new(200).set_body_raw(events.concat(), "text/event-stream")
    }

    /// A response stream of `deltas` of the event `kind`.
    fn spoken(kind: &str, deltas: &[&str]) -> ResponseTemplate {
        stream(deltas.iter().map(|delta| {
            json!({
                "type": kind,
                "sequence_number": 0,
                "item_id": "msg_1",
                "output_index": 0,
                "content_index": 0,
                "delta": delta,
            })
        }))
    }

    /// A step with the reply of a call left out.
    #[derive(Debug, PartialEq)]
    enum Seen {
        /// A sentence.
        Sentence(String),
        /// A service call.
        Call(ServiceCall),
        /// The end of the answer.
        Done,
    }

    /// The steps of an answer that a mock `OpenAI` server streams as `template`, then "It is on."
    /// after a tool output, and the server. Each call gives `{"state": "on"}`.
    async fn served(template: ResponseTemplate) -> anyhow::Result<(MockServer, Vec<Seen>)> {
        let (server, reasoner) = serve(template).await?;
        Mock::given(matchers::body_string_contains("function_call_output"))
            .respond_with(spoken(TEXT, &["It is on."]))
            .with_priority(1)
            .mount(&server)
            .await;
        let mut steps = reasoner.answer(&question(Some("kitchen")));
        let mut seen = Vec::new();
        while let Some(step) = steps.try_next().await? {
            seen.push(match step {
                Step::Sentence(text) => Seen::Sentence(text),
                Step::Call(call, reply) => {
                    let sent = reply.send(Ok(Some(json!({"state": "on"}))));
                    anyhow::ensure!(sent.is_ok(), "the answer dropped the reply");
                    Seen::Call(call)
                }
                Step::Done => Seen::Done,
            });
        }
        Ok((server, seen))
    }

    /// The steps of an answer that a mock `OpenAI` server streams as `deltas` of the event `kind`.
    async fn answered(kind: &str, deltas: &[&str]) -> anyhow::Result<Vec<Seen>> {
        Ok(served(spoken(kind, deltas)).await?.1)
    }

    /// The steps of an answer whose first response has a tool call `call_<n>` for each of
    /// `arguments`, and the tool outputs of the last request.
    async fn called(arguments: &[&str]) -> anyhow::Result<(Vec<Seen>, Vec<Value>)> {
        let calls = arguments.iter().zip(1..).map(|(arguments, number)| {
            json!({
                "type": "response.output_item.done",
                "sequence_number": 0,
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "call_id": format!("call_{number}"),
                    "name": "call_service",
                    "arguments": arguments,
                },
            })
        });
        let (server, seen) = served(stream(calls)).await?;
        let requests = server.received_requests().await.context("no requests")?;
        let body: Value = requests.last().context("no request")?.body_json()?;
        let input = body["input"]
            .as_array()
            .context("the input is not a list")?;
        let outputs = input
            .iter()
            .filter(|item| item["type"] == "function_call_output");
        Ok((seen, outputs.cloned().collect()))
    }

    /// Valid `call_service` arguments.
    const LIGHT_ON: &str = r#"{"domain": "light", "service": "turn_on",
        "target": ["light.overhead"], "data": {"brightness_pct": 50}}"#;

    /// AC-7.4-1.
    #[tokio::test(start_paused = true)]
    async fn ac_7_4_1_tool_call_gives_call_step() -> anyhow::Result<()> {
        let (seen, _outputs) = called(&[LIGHT_ON]).await?;
        let call = ServiceCall {
            domain: "light".to_owned(),
            service: "turn_on".to_owned(),
            target: vec!["light.overhead".to_owned()],
            data: serde_json::from_value(json!({"brightness_pct": 50}))?,
            return_response: false,
        };
        assert_eq!(seen[0], Seen::Call(call));
        Ok(())
    }

    /// AC-7.4-2.
    #[tokio::test(start_paused = true)]
    async fn ac_7_4_2_result_goes_to_the_model_then_final_text() -> anyhow::Result<()> {
        let (seen, outputs) = called(&[LIGHT_ON]).await?;
        let output = json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": r#"{"state":"on"}"#,
        });
        assert_eq!(outputs, [output]);
        assert_eq!(
            seen[1..],
            [Seen::Sentence("It is on.".to_owned()), Seen::Done]
        );
        Ok(())
    }

    /// AC-7.4-3.
    #[tokio::test(start_paused = true)]
    async fn ac_7_4_3_invalid_arguments_give_a_tool_error() -> anyhow::Result<()> {
        let (seen, outputs) = called(&[r#"{"domain": "light"}"#]).await?;
        assert_eq!(seen, [Seen::Sentence("It is on.".to_owned()), Seen::Done]);
        let output = outputs[0]["output"].as_str().context("no output")?;
        assert!(output.starts_with("error: "), "the output is {output:?}");
        Ok(())
    }

    /// AC-7.4-4.
    #[tokio::test(start_paused = true)]
    async fn ac_7_4_4_call_after_the_limit_gives_limit_reached() -> anyhow::Result<()> {
        let (seen, outputs) = called(&[LIGHT_ON; 6]).await?;
        let calls = seen.iter().filter(|step| matches!(step, Seen::Call(_)));
        assert_eq!(calls.count(), 5);
        assert_eq!(outputs[5]["call_id"], "call_6");
        assert_eq!(outputs[5]["output"], "error: limit reached");
        Ok(())
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
            [Seen::Sentence("I can't help.".to_owned()), Seen::Done]
        );
        Ok(())
    }

    /// AC-7.3-2.
    #[tokio::test(start_paused = true)]
    async fn ac_7_3_2_rest_is_the_last_sentence_at_the_end() -> anyhow::Result<()> {
        let steps = answered(TEXT, &["Hello there. It", " is"]).await?;
        assert_eq!(steps[1..], [Seen::Sentence("It is".to_owned()), Seen::Done]);
        Ok(())
    }

    /// AC-7.3-4.
    #[tokio::test(start_paused = true)]
    async fn ac_7_3_4_two_sentences_give_sentence_sentence_done() -> anyhow::Result<()> {
        let steps = answered(TEXT, &["Hello there.", " How are", " you?"]).await?;
        assert_eq!(
            steps,
            [
                Seen::Sentence("Hello there.".to_owned()),
                Seen::Sentence("How are you?".to_owned()),
                Seen::Done,
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
                "- Carson Kitchen Media Player (media_player.carson_kitchen_media_player): idle",
                "- Thermostat (climate.thermostat): heat",
                "- Thermostat Current Temperature (sensor.thermostat_current_temperature): 63.5",
                "- Thermostat Motion (binary_sensor.thermostat_motion): on",
                "- Overhead Light (light.kitchen): off",
                "- Strawberry Lamp (light.strawberry_lamp): on",
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

    /// The variables `area` and `person`.
    fn variables() -> BTreeSet<String> {
        BTreeSet::from(["area".to_owned(), "person".to_owned()])
    }

    /// AC-8.2-1.
    #[test]
    fn ac_8_2_1_prompt_has_existing_responses_and_variables() {
        let existing = ["Done.".to_owned(), "The {{ area }} is ready.".to_owned()];
        let prompt = generate_prompt("lights_on", &existing, &variables(), 5);
        for wanted in ["Done.", "The {{ area }} is ready.", "area", "person"] {
            assert!(prompt.contains(wanted), "{wanted:?} is not in {prompt:?}");
        }
    }

    /// The result of `generate` when a mock `OpenAI` server replies with `status` and the output
    /// text `text`, and the body of the request.
    async fn generated(
        status: &str,
        text: &str,
    ) -> anyhow::Result<(anyhow::Result<Vec<String>>, Value)> {
        let mut body = response(status);
        body["output"] = json!([{
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "annotations": [], "text": text}],
        }]);
        let (server, reasoner) = serve(ResponseTemplate::new(200).set_body_json(body)).await?;
        let result = reasoner.generate("lights_on", &[], &variables(), 2).await;
        let requests = server.received_requests().await.context("no requests")?;
        Ok((result, requests[0].body_json()?))
    }

    /// AC-8.2-2.
    #[tokio::test(start_paused = true)]
    async fn ac_8_2_2_schema_reply_gives_its_responses() -> anyhow::Result<()> {
        let (result, request) = generated("completed", r#"{"responses": ["A", "B"]}"#).await?;
        assert_eq!(result?, ["A", "B"]);
        assert_eq!(request["max_output_tokens"], 8000);
        assert_eq!(request["text"]["format"]["type"], "json_schema");
        Ok(())
    }

    /// AC-8.2-3.
    #[tokio::test(start_paused = true)]
    async fn ac_8_2_3_reply_off_the_schema_is_an_error() -> anyhow::Result<()> {
        let replies = [
            ("completed", r#"{"responses": "A"}"#),
            ("completed", r#"[["A", "B"]]"#),
            ("incomplete", r#"{"responses": ["A", "B"]}"#),
        ];
        for (status, text) in replies {
            let (result, _request) = generated(status, text).await?;
            assert!(
                result.is_err(),
                "the result of {status} {text} is {result:?}"
            );
        }
        Ok(())
    }
}
