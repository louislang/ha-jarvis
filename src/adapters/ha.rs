//! `Home` over the HA WebSocket API.

use std::io;
use std::time::Duration;

use anyhow::Context;
use futures::SinkExt;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::time;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::HaConfig;

/// `Home` over the HA WebSocket API.
pub struct HaHome<S> {
    /// The authenticated socket. Held to keep the connection open.
    _socket: WebSocketStream<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> HaHome<S> {
    /// Opens a transport with `connector` and authenticates with `token`.
    ///
    /// # Errors
    ///
    /// A connection, authentication, protocol or timeout error.
    pub async fn connect<F>(
        connector: impl FnOnce() -> F,
        token: &str,
        config: &HaConfig,
    ) -> anyhow::Result<Self>
    where
        F: Future<Output = io::Result<S>>,
    {
        let deadline = Duration::from_millis(config.request_timeout_ms);
        let handshake = authenticate(connector, token, &config.url);
        let socket = time::timeout(deadline, handshake)
            .await
            .map_err(|_| anyhow::anyhow!("timeout error: no auth_ok within {deadline:?}"))??;
        Ok(Self { _socket: socket })
    }
}

/// The type of an HA message.
#[derive(Deserialize)]
struct Envelope {
    /// For example `auth_ok`.
    #[serde(rename = "type")]
    kind: String,
}

/// Opens the WebSocket at `url` over the transport of `connector` and authenticates.
async fn authenticate<S, F>(
    connector: impl FnOnce() -> F,
    token: &str,
    url: &str,
) -> anyhow::Result<WebSocketStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: Future<Output = io::Result<S>>,
{
    let transport = connector().await.context("connection error")?;
    let (mut socket, _) = tokio_tungstenite::client_async(url, transport)
        .await
        .context("connection error")?;
    let required = receive_type(&mut socket).await?;
    anyhow::ensure!(required == "auth_required", unexpected(&required));
    let auth = json!({ "type": "auth", "access_token": token }).to_string();
    socket
        .send(Message::text(auth))
        .await
        .context("connection error")?;
    match receive_type(&mut socket).await?.as_str() {
        "auth_ok" => Ok(socket),
        "auth_invalid" => anyhow::bail!("authentication error: HA rejected the token"),
        other => anyhow::bail!(unexpected(other)),
    }
}

/// Receives the next message and gives its type.
async fn receive_type<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> anyhow::Result<String> {
    match socket.next().await {
        Some(Ok(Message::Text(text))) => Ok(serde_json::from_str::<Envelope>(&text)?.kind),
        Some(Ok(Message::Close(_))) | None => anyhow::bail!("connection error: HA closed"),
        Some(Ok(frame)) => anyhow::bail!("protocol error: unexpected frame {frame:?}"),
        Some(Err(error)) => Err(error).context("connection error"),
    }
}

/// The error for a message of type `kind` that the handshake does not expect.
fn unexpected(kind: &str) -> String {
    format!("protocol error: unexpected message {kind}")
}

/// Tests of the HA adapter with `FakeHaServer`.
#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Value;
    use tokio::io::DuplexStream;

    /// The only token that `FakeHaServer` accepts.
    const TOKEN: &str = "abc";

    /// The HA WebSocket API on the server end of a `tokio::io::duplex` pair.
    struct FakeHaServer {
        /// The server end of the WebSocket.
        socket: WebSocketStream<DuplexStream>,
    }

    impl FakeHaServer {
        /// Sends a message of type `kind`.
        async fn send(&mut self, kind: &str) {
            let text = json!({ "type": kind }).to_string();
            self.socket
                .send(Message::text(text))
                .await
                .expect("the client is connected");
        }

        /// Receives the next message.
        async fn receive(&mut self) -> Value {
            let message = self.socket.next().await;
            let text = message
                .expect("the client is connected")
                .expect("the frame is valid")
                .into_text()
                .expect("the frame is text");
            serde_json::from_str(&text).expect("the message is JSON")
        }

        /// Sends `auth_required` and receives `auth`. Gives true when it has `TOKEN`.
        async fn receive_auth(&mut self) -> bool {
            self.send("auth_required").await;
            self.receive().await["access_token"] == TOKEN
        }

        /// Answers `auth` with `auth_ok` or `auth_invalid`, then waits until the client closes.
        async fn authenticate(mut self) {
            let reply = if self.receive_auth().await {
                "auth_ok"
            } else {
                "auth_invalid"
            };
            self.send(reply).await;
            self.wait_close().await;
        }

        /// Waits until the client closes.
        async fn wait_close(mut self) {
            while let Some(Ok(_)) = self.socket.next().await {}
        }
    }

    /// The `[ha]` section of `jarvis.toml`.
    fn config() -> HaConfig {
        crate::Config::parse(include_str!("../../jarvis.toml"))
            .expect("jarvis.toml is valid")
            .ha
    }

    /// Connects with `token` to a `FakeHaServer` that `server` runs.
    async fn connect<F>(
        token: &str,
        server: impl FnOnce(FakeHaServer) -> F + Send + 'static,
    ) -> anyhow::Result<HaHome<DuplexStream>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (client, server_end) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let socket = tokio_tungstenite::accept_async(server_end)
                .await
                .expect("the WebSocket handshake succeeds");
            server(FakeHaServer { socket }).await;
        });
        HaHome::connect(|| async { Ok(client) }, token, &config()).await
    }

    /// Returns the text of the error that `connect` gives.
    async fn connect_error<F>(
        token: &str,
        server: impl FnOnce(FakeHaServer) -> F + Send + 'static,
    ) -> String
    where
        F: Future<Output = ()> + Send + 'static,
    {
        match connect(token, server).await {
            Ok(_) => panic!("connect succeeded"),
            Err(error) => format!("{error:#}"),
        }
    }

    /// AC-2.2-1.
    #[tokio::test(start_paused = true)]
    async fn ac_2_2_1_correct_token_connects() -> anyhow::Result<()> {
        connect(TOKEN, FakeHaServer::authenticate).await?;
        Ok(())
    }

    /// AC-2.2-2.
    #[tokio::test(start_paused = true)]
    async fn ac_2_2_2_incorrect_token_is_authentication_error() {
        let error = connect_error("wrong", FakeHaServer::authenticate).await;
        assert!(
            error.contains("authentication"),
            "not an authentication error: {error}"
        );
    }

    /// AC-2.2-3.
    #[tokio::test(start_paused = true)]
    async fn ac_2_2_3_close_before_auth_required_is_connection_error() {
        let error = connect_error(TOKEN, |server| async { drop(server) }).await;
        assert!(
            error.contains("connection"),
            "not a connection error: {error}"
        );
    }

    /// AC-2.2-4.
    #[tokio::test(start_paused = true)]
    async fn ac_2_2_4_other_reply_is_protocol_error_with_type() {
        let error = connect_error(TOKEN, |mut server| async move {
            server.receive_auth().await;
            server.send("result").await;
            server.wait_close().await;
        })
        .await;
        assert!(
            error.contains("protocol") && error.contains("result"),
            "not a protocol error with the type: {error}"
        );
    }

    /// AC-2.2-5.
    #[tokio::test(start_paused = true)]
    async fn ac_2_2_5_no_auth_ok_is_timeout_error() {
        let error = connect_error(TOKEN, |mut server| async move {
            server.receive_auth().await;
            server.wait_close().await;
        })
        .await;
        assert!(error.contains("timeout"), "not a timeout error: {error}");
    }
}
