//! `Reasoner` with the `OpenAI` Responses API.

use std::sync::Mutex;
use std::time::Duration;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::CreateResponseArgs;
use async_openai::types::responses::Status;
use async_trait::async_trait;
use tokio::time::Instant;

use crate::OpenAiConfig;
use crate::ports::Reasoner;

/// The TCP keepalive interval of the pooled connections.
const KEEPALIVE: Duration = Duration::from_secs(30);

/// The input of the warm request.
const WARM_INPUT: &str = "Reply with OK.";

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

    /// Records a request now when the last request is `warm_idle` or more ago, or when there is
    /// no last request. True when it records.
    ///
    /// # Errors
    ///
    /// An earlier record panicked.
    fn record_when_idle(&self) -> anyhow::Result<bool> {
        let mut last_request = self
            .last_request
            .lock()
            .map_err(|_poisoned| anyhow::anyhow!("an earlier record panicked"))?;
        let now = Instant::now();
        if last_request.is_some_and(|last| now.duration_since(last) < self.warm_idle) {
            return Ok(false);
        }
        *last_request = Some(now);
        Ok(true)
    }
}

#[async_trait]
impl Reasoner for OpenAiReasoner {
    async fn warm(&self) -> anyhow::Result<()> {
        if !self.record_when_idle()? {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use anyhow::Context as _;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    use crate::Config;

    /// A mock `OpenAI` server that answers each response request with `status`, and a reasoner of
    /// `jarvis.toml` on it.
    async fn mock(status: &str) -> anyhow::Result<(MockServer, OpenAiReasoner)> {
        let server = MockServer::start().await;
        let body = json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 0,
            "model": "gpt-4.1-nano",
            "output": [],
            "status": status,
        });
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
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
}
