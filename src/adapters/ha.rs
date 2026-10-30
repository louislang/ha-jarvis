//! `Home` over the HA WebSocket API.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Duration;

use anyhow::Context;
use futures::SinkExt;
use futures::StreamExt;
use futures::channel::oneshot;
use futures::lock::Mutex as AsyncMutex;
use futures::stream::SplitSink;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::time;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::HaConfig;
use crate::domain::state::Snapshot;

/// The sender of the result of one request.
type ResultSender = oneshot::Sender<anyhow::Result<Value>>;

/// `Home` over the HA WebSocket API.
pub struct HaHome<S> {
    /// The send half of the socket and the next request ID.
    writer: AsyncMutex<Writer<S>>,
    /// The result sender of each request that waits, by request ID.
    pending: Mutex<HashMap<u64, ResultSender>>,
    /// The deadline of each request.
    deadline: Duration,
}

/// The send half and the next ID, under one lock: HA refuses an ID that does not increase.
struct Writer<S> {
    /// The send half of the socket. `None` while HA is disconnected.
    sink: Option<SplitSink<WebSocketStream<S>, Message>>,
    /// The ID of the next request.
    next_id: u64,
}

/// Removes a request ID from `pending` when the request ends, also by timeout.
struct Pending<'home> {
    /// The pending map.
    map: &'home Mutex<HashMap<u64, ResultSender>>,
    /// The request ID.
    id: u64,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.map.lock() {
            map.remove(&self.id);
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> HaHome<S> {
    /// Returns at once. A background task opens a transport with `connector`, authenticates
    /// with `token` and reads the connection until it closes.
    pub fn start<F>(connector: impl FnOnce() -> F, token: &str, config: &HaConfig) -> Arc<Self>
    where
        F: Future<Output = io::Result<S>> + Send + 'static,
    {
        let home = Arc::new(Self {
            writer: AsyncMutex::new(Writer {
                sink: None,
                next_id: 1,
            }),
            pending: Mutex::new(HashMap::new()),
            deadline: Duration::from_millis(config.request_timeout_ms),
        });
        let (transport, token, config) = (connector(), token.to_owned(), config.clone());
        let background = Arc::clone(&home);
        tokio::spawn(async move {
            if let Ok(socket) = connect(transport, &token, &config).await {
                background.serve(socket).await;
            }
        });
        home
    }

    /// Delivers each message until the connection closes, then fails each pending request.
    async fn serve(&self, socket: WebSocketStream<S>) {
        let (sink, mut stream) = socket.split();
        self.writer.lock().await.sink = Some(sink);
        while let Some(Ok(message)) = stream.next().await {
            if let Message::Text(text) = message {
                self.deliver(&text);
            }
        }
        // The writer lock stops new requests until `pending` is empty.
        let mut writer = self.writer.lock().await;
        writer.sink = None;
        for (_, sender) in self.pending().drain() {
            sender.send(Err(disconnected())).ok();
        }
    }

    /// Loads the states, the registries, the configuration and the user of the token.
    ///
    /// # Errors
    ///
    /// A connection, protocol, HA or timeout error.
    pub async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let (states, areas, devices, mut entities, config, user) = futures::try_join!(
            self.request(json!({ "type": "get_states" })),
            self.request(json!({ "type": "config/area_registry/list" })),
            self.request(json!({ "type": "config/device_registry/list" })),
            self.request(json!({ "type": "config/entity_registry/list" })),
            self.request(json!({ "type": "get_config" })),
            self.request(json!({ "type": "auth/current_user" })),
        )?;
        self.complete(&mut entities, &states).await?;
        let snapshot = json!({
            "states": states, "areas": areas, "devices": devices,
            "entities": entities, "config": config, "user_id": user["id"],
        });
        serde_json::from_value(snapshot).context("protocol error: the snapshot is not valid")
    }

    /// Completes each listed entity with its entry of `get_entries` and the names of `states`.
    async fn complete(&self, entities: &mut Value, states: &Value) -> anyhow::Result<()> {
        let listed = entities.as_array_mut().context("protocol error")?;
        let ids: Vec<&Value> = listed.iter().map(|entity| &entity["entity_id"]).collect();
        let command = json!({ "type": "config/entity_registry/get_entries", "entity_ids": ids });
        let entries = self.request(command).await?;
        let names: HashMap<&Value, &Value> = states
            .as_array()
            .into_iter()
            .flatten()
            .map(|state| (&state["entity_id"], &state["attributes"]["friendly_name"]))
            .filter(|(_, name)| name.is_string())
            .collect();
        let objects = listed.iter_mut().filter_map(Value::as_object_mut);
        for fields in objects {
            complete_entity(fields, &entries, &names);
        }
        Ok(())
    }

    /// Sends the JSON object `command` with the next ID and waits for its result.
    async fn request(&self, mut command: Value) -> anyhow::Result<Value> {
        let exchange = async {
            let (sender, receiver) = oneshot::channel();
            let mut writer = self.writer.lock().await;
            let Writer { sink, next_id } = &mut *writer;
            let sink = sink.as_mut().ok_or_else(disconnected)?;
            let pending = Pending {
                map: &self.pending,
                id: *next_id,
            };
            *next_id += 1;
            self.pending().insert(pending.id, sender);
            command["id"] = Value::from(pending.id);
            let text = Message::text(command.to_string());
            sink.send(text).await.context("connection error")?;
            drop(writer);
            receiver.await.context("connection error")?
        };
        time::timeout(self.deadline, exchange)
            .await
            .map_err(|_| anyhow::anyhow!("timeout error: no result within {:?}", self.deadline))?
    }

    /// Sends a `result` message to its pending request. Ignores other messages and IDs.
    fn deliver(&self, text: &str) {
        let reply = match serde_json::from_str::<Reply>(text) {
            Ok(reply) if reply.kind == "result" => reply,
            _ => return,
        };
        let Some(sender) = self.pending().remove(&reply.id) else {
            return;
        };
        let result = if reply.success {
            Ok(reply.result)
        } else {
            Err(anyhow::anyhow!("HA error: {}", reply.error))
        };
        // The request can time out between the removal and the send.
        sender.send(result).ok();
    }

    /// The pending map.
    fn pending(&self) -> MutexGuard<'_, HashMap<u64, ResultSender>> {
        self.pending
            .lock()
            .expect("no code panics while it holds the pending map")
    }
}

/// The error of a request while HA is disconnected.
fn disconnected() -> anyhow::Error {
    anyhow::anyhow!("connection error: HA disconnected")
}

/// Puts the aliases of `entries` into `fields`, and a name from `names` or the ID if it has none.
fn complete_entity(
    fields: &mut Map<String, Value>,
    entries: &Value,
    names: &HashMap<&Value, &Value>,
) {
    let id = fields.get("entity_id").cloned().unwrap_or_default();
    let aliases = entries[id.as_str().unwrap_or_default()]
        .get("aliases")
        .cloned();
    fields.insert("aliases".to_owned(), aliases.unwrap_or_else(|| json!([])));
    if fields.get("name").is_none_or(Value::is_null) {
        let name = names.get(&id).copied().unwrap_or(&id).clone();
        fields.insert("name".to_owned(), name);
    }
}

/// A message with a request ID.
#[derive(Deserialize)]
struct Reply {
    /// The request ID.
    id: u64,
    /// For example `result`.
    #[serde(rename = "type")]
    kind: String,
    /// True when HA ran the command.
    success: bool,
    /// The result of the command.
    #[serde(default)]
    result: Value,
    /// The code and the message of the error when `success` is false.
    #[serde(default)]
    error: Value,
}

/// The type of an HA message.
#[derive(Deserialize)]
struct Envelope {
    /// For example `auth_ok`.
    #[serde(rename = "type")]
    kind: String,
}

/// Authenticates with `token` over `transport` within `ha.request_timeout_ms`.
///
/// # Errors
///
/// A connection, authentication, protocol or timeout error.
async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
    transport: impl Future<Output = io::Result<S>>,
    token: &str,
    config: &HaConfig,
) -> anyhow::Result<WebSocketStream<S>> {
    let deadline = Duration::from_millis(config.request_timeout_ms);
    time::timeout(deadline, authenticate(transport, token, &config.url))
        .await
        .map_err(|_| anyhow::anyhow!("timeout error: no auth_ok within {deadline:?}"))?
}

/// Opens the WebSocket at `url` over `transport` and authenticates.
async fn authenticate<S: AsyncRead + AsyncWrite + Unpin>(
    transport: impl Future<Output = io::Result<S>>,
    token: &str,
    url: &str,
) -> anyhow::Result<WebSocketStream<S>> {
    let transport = transport.await.context("connection error")?;
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

/// Receives the next message, after any `Ping` and `Pong` frames, and gives its type.
async fn receive_type<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> anyhow::Result<String> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return Ok(serde_json::from_str::<Envelope>(&text)?.kind);
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => anyhow::bail!("connection error: HA closed"),
            Some(Ok(frame)) => anyhow::bail!("protocol error: unexpected frame {frame:?}"),
            Some(Err(error)) => return Err(error).context("connection error"),
        }
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

    use futures::future;
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
        /// Sends `message` as JSON text.
        async fn send_json(&mut self, message: &Value) {
            self.socket
                .send(Message::text(message.to_string()))
                .await
                .expect("the client is connected");
        }

        /// Sends a message of type `kind`.
        async fn send(&mut self, kind: &str) {
            self.send_json(&json!({ "type": kind })).await;
        }

        /// Receives the next text message, or `None` when the client closes. Skips control frames.
        async fn request(&mut self) -> Option<Value> {
            while let Some(Ok(frame)) = self.socket.next().await {
                if let Message::Text(text) = frame {
                    return Some(serde_json::from_str(&text).expect("the message is JSON"));
                }
            }
            None
        }

        /// Receives the next text message.
        async fn receive(&mut self) -> Value {
            self.request().await.expect("the client is connected")
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
            while self.request().await.is_some() {}
        }

        /// Sends the successful result `result` for `request`.
        async fn reply(&mut self, request: &Value, result: Value) {
            let message =
                json!({ "id": request["id"], "type": "result", "success": true, "result": result });
            self.send_json(&message).await;
        }

        /// Answers each request from `house.json` as HA does, until the client closes.
        async fn serve_house(mut self) {
            let house = house();
            while let Some(request) = self.request().await {
                let result = house_result(&house, &request);
                self.reply(&request, result).await;
            }
        }
    }

    /// The reference house as JSON.
    fn house() -> Value {
        serde_json::from_str(include_str!("../testing/house.json")).expect("house.json is JSON")
    }

    /// The result that HA gives for `request` in the reference house.
    fn house_result(house: &Value, request: &Value) -> Value {
        let entities = house["entities"]
            .as_array()
            .expect("house.json has entities");
        match request["type"].as_str().expect("the request has a type") {
            "get_states" => house["states"].clone(),
            "config/area_registry/list" => house["areas"].clone(),
            "config/device_registry/list" => house["devices"].clone(),
            "config/entity_registry/list" => entities.iter().map(without_aliases).collect(),
            "config/entity_registry/get_entries" => entries(entities, &request["entity_ids"]),
            "get_config" => house["config"].clone(),
            "auth/current_user" => json!({ "id": house["user_id"], "name": "Jarvis" }),
            other => panic!("HA has no command {other}"),
        }
    }

    /// `entity` as `config/entity_registry/list` gives it: with no aliases and no name.
    fn without_aliases(entity: &Value) -> Value {
        let mut entity = entity.clone();
        if let Some(fields) = entity.as_object_mut() {
            fields.remove("aliases");
            fields.insert("name".to_owned(), Value::Null);
        }
        entity
    }

    /// The entries of `ids` by entity ID, as `config/entity_registry/get_entries` gives them.
    fn entries(entities: &[Value], ids: &Value) -> Value {
        let ids = ids.as_array().expect("the request has entity IDs");
        let entry = |id: &Value| entities.iter().find(|entity| entity["entity_id"] == *id);
        let pairs = ids
            .iter()
            .filter_map(|id| Some((id.as_str()?.to_owned(), entry(id)?.clone())));
        Value::Object(pairs.collect())
    }

    /// The `[ha]` section of `jarvis.toml`.
    fn config() -> HaConfig {
        crate::Config::parse(include_str!("../../jarvis.toml"))
            .expect("jarvis.toml is valid")
            .ha
    }

    /// Opens a transport to a `FakeHaServer` that `server` runs.
    fn open<F>(server: impl FnOnce(FakeHaServer) -> F + Send + 'static) -> DuplexStream
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
        client
    }

    /// Connects with `token` to a `FakeHaServer` that `server` runs.
    async fn connect<F>(
        token: &str,
        server: impl FnOnce(FakeHaServer) -> F + Send + 'static,
    ) -> anyhow::Result<WebSocketStream<DuplexStream>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        super::connect(async { Ok(open(server)) }, token, &config()).await
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
        connect(TOKEN, |mut server| async move {
            server
                .socket
                .send(Message::Ping(Vec::new().into()))
                .await
                .expect("the client is connected");
            server.authenticate().await;
        })
        .await?;
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

    /// Starts an adapter on a `FakeHaServer` that accepts the token, then runs `server`.
    async fn ready<F>(
        server: impl FnOnce(FakeHaServer) -> F + Send + 'static,
    ) -> Arc<HaHome<DuplexStream>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let client = open(|mut fake| async move {
            fake.receive_auth().await;
            fake.send("auth_ok").await;
            server(fake).await;
        });
        let home = HaHome::start(|| async { Ok(client) }, TOKEN, &config());
        // The paused clock advances only when every task waits, thus after `auth_ok`.
        time::sleep(Duration::from_millis(1)).await;
        home
    }

    /// An `echo` request with the number `n`.
    fn echo(n: u64) -> Value {
        json!({ "type": "echo", "n": n })
    }

    /// AC-2.3-1.
    #[tokio::test(start_paused = true)]
    async fn ac_2_3_1_concurrent_callers_receive_own_results() -> anyhow::Result<()> {
        let home = ready(|mut server| async move {
            let mut requests = Vec::new();
            for _ in 0..100 {
                requests.push(server.receive().await);
            }
            for request in requests.iter().rev() {
                server.reply(request, request["n"].clone()).await;
            }
            server.wait_close().await;
        })
        .await;
        let requests = (0..100).map(|n| home.request(echo(n)));
        let results: Vec<Value> = future::try_join_all(requests).await?;
        let expected: Vec<Value> = (0..100).map(Value::from).collect();
        assert_eq!(results, expected);
        Ok(())
    }

    /// AC-2.3-2.
    #[tokio::test(start_paused = true)]
    async fn ac_2_3_2_snapshot_is_reference_house() -> anyhow::Result<()> {
        let home = ready(FakeHaServer::serve_house).await;
        let expected: Snapshot = serde_json::from_value(house())?;
        assert_eq!(home.snapshot().await?, expected);
        Ok(())
    }

    /// AC-2.3-3.
    #[tokio::test(start_paused = true)]
    async fn ac_2_3_3_result_with_unknown_id_is_ignored() -> anyhow::Result<()> {
        let home = ready(|mut server| async move {
            while let Some(request) = server.request().await {
                let stray = json!({ "id": 0, "type": "result", "success": true, "result": -1 });
                server.send_json(&stray).await;
                server.reply(&request, request["n"].clone()).await;
            }
        })
        .await;
        assert_eq!(home.request(echo(1)).await?, json!(1));
        assert_eq!(home.request(echo(2)).await?, json!(2));
        Ok(())
    }

    /// AC-2.3-4.
    #[tokio::test(start_paused = true)]
    async fn ac_2_3_4_failure_is_error_with_ha_message() {
        let home = ready(|mut server| async move {
            let request = server.receive().await;
            let error = json!({ "code": "not_found", "message": "Entity not found." });
            let failure =
                json!({ "id": request["id"], "type": "result", "success": false, "error": error });
            server.send_json(&failure).await;
            server.wait_close().await;
        })
        .await;
        let error = home.request(echo(1)).await.expect_err("HA fails");
        assert!(format!("{error}").contains("Entity not found."), "{error}");
    }

    /// AC-2.3-5.
    #[tokio::test(start_paused = true)]
    async fn ac_2_3_5_no_result_is_timeout_error_and_not_pending() {
        let home = ready(FakeHaServer::wait_close).await;
        let error = home.request(echo(1)).await.expect_err("HA does not answer");
        assert!(format!("{error}").contains("timeout"), "{error}");
        assert!(home.pending().is_empty(), "the ID is still pending");
    }

    /// AC-2.3-6.
    #[tokio::test(start_paused = true)]
    async fn ac_2_3_6_aliases_from_get_entries() -> anyhow::Result<()> {
        let home = ready(FakeHaServer::serve_house).await;
        let snapshot = home.snapshot().await?;
        let expected: Snapshot = serde_json::from_value(house())?;
        let aliases = |snapshot: Snapshot| -> Vec<Vec<String>> {
            snapshot
                .entities
                .into_iter()
                .map(|entity| entity.aliases)
                .collect()
        };
        assert_eq!(aliases(snapshot), aliases(expected));
        Ok(())
    }

    /// AC-2.4-1.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4_1_close_with_pending_request_is_disconnect_error() {
        let home = ready(|mut server| async move {
            server.receive().await;
        })
        .await;
        let error = home.request(echo(1)).await.expect_err("HA closes");
        assert!(format!("{error}").contains("disconnected"), "{error}");
        assert!(home.pending().is_empty(), "the ID is still pending");
    }
}
