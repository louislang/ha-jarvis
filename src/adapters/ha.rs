//! `Home` over the HA WebSocket API.

use std::collections::HashMap;
use std::io;
use std::pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Weak;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use futures::SinkExt;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::future;
use futures::future::AbortHandle;
use futures::future::Abortable;
use futures::lock::Mutex as AsyncMutex;
use futures::stream;
use futures::stream::BoxStream;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
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
use crate::domain::state::Context;
use crate::domain::state::Event;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;
use crate::domain::state::State;
use crate::ports::Home;

/// The sender of the events of all connections.
type Events = mpsc::UnboundedSender<Event>;

/// The sender of the result of one request.
type ResultSender = oneshot::Sender<anyhow::Result<Value>>;

/// The HA events that the adapter subscribes to.
const EVENT_TYPES: [&str; 4] = [
    "area_registry_updated",
    "device_registry_updated",
    "entity_registry_updated",
    "state_changed",
];

/// `Home` over the HA WebSocket API.
pub struct HaHome<S> {
    /// The send half of the socket and the next request ID.
    writer: AsyncMutex<Writer<S>>,
    /// The result sender of each request that waits, by request ID.
    pending: Mutex<HashMap<u64, ResultSender>>,
    /// The events, until [`HaHome::events`] takes them.
    events: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
    /// The deadline of each request except an announce.
    deadline: Duration,
    /// The deadline of an announce, which HA answers after the satellite plays the audio.
    announce_deadline: Duration,
    /// Stops the connection task when the adapter drops.
    task: AbortHandle,
}

impl<S> Drop for HaHome<S> {
    fn drop(&mut self) {
        self.task.abort();
    }
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
    /// with `token`, serves the connection and connects again with backoff after each disconnect.
    pub fn start<C, F>(connector: C, token: &str, config: &HaConfig) -> Arc<Self>
    where
        C: Fn() -> F + Send + 'static,
        F: Future<Output = io::Result<S>> + Send + 'static,
    {
        let (task, registration) = AbortHandle::new_pair();
        let (sender, receiver) = mpsc::unbounded();
        let home = Arc::new(Self {
            writer: AsyncMutex::new(Writer {
                sink: None,
                next_id: 1,
            }),
            pending: Mutex::new(HashMap::new()),
            events: Mutex::new(Some(receiver)),
            deadline: Duration::from_millis(config.request_timeout_ms),
            announce_deadline: Duration::from_secs(config.announce_timeout_s),
            task,
        });
        // A weak reference, so that the task does not keep the adapter alive.
        let weak = Arc::downgrade(&home);
        let run = Self::run(weak, connector, token.to_owned(), config.clone(), sender);
        tokio::spawn(Abortable::new(run, registration));
        home
    }

    /// Connects and serves each connection until it fails, then waits for the backoff delay.
    async fn run<C, F>(
        home: Weak<Self>,
        connector: C,
        token: String,
        config: HaConfig,
        events: Events,
    ) where
        C: Fn() -> F,
        F: Future<Output = io::Result<S>>,
    {
        let ping = Duration::from_secs(config.ping_interval_s);
        let mut attempt: u32 = 0;
        loop {
            if let Ok(socket) = connect(connector(), &token, &config).await {
                attempt = 0;
                Self::serve(&home, socket, &events, ping).await;
            }
            time::sleep(delay(attempt)).await;
            attempt = attempt.saturating_add(1);
        }
    }

    /// The changes in HA, in order, from the first subscription. Only the first call gets them.
    pub fn events(&self) -> BoxStream<'static, Event> {
        let receiver = self.events.lock().ok().and_then(|mut events| events.take());
        stream::iter(receiver).flatten().boxed()
    }

    /// Delivers each message, subscribes and pings every `ping` until the connection fails, then
    /// fails each pending request.
    async fn serve(home: &Weak<Self>, socket: WebSocketStream<S>, events: &Events, ping: Duration) {
        let (sink, stream) = socket.split();
        let Some(adapter) = home.upgrade() else {
            return;
        };
        adapter.writer.lock().await.sink = Some(sink);
        drop(adapter);
        let read = pin::pin!(Self::read(home, stream, events));
        // The subscriptions and the pings need the read loop for their results.
        future::select(read, pin::pin!(Self::control(home, events, ping))).await;
        let Some(adapter) = home.upgrade() else {
            return;
        };
        // The writer lock stops new requests until `pending` is empty.
        let mut writer = adapter.writer.lock().await;
        writer.sink = None;
        for (_, sender) in adapter.pending().drain() {
            sender.send(Err(disconnected())).ok();
        }
    }

    /// Delivers each message of `stream` until the connection closes.
    async fn read(home: &Weak<Self>, mut stream: SplitStream<WebSocketStream<S>>, events: &Events) {
        while let Some(Ok(message)) = stream.next().await {
            if let (Message::Text(text), Some(adapter)) = (message, home.upgrade()) {
                adapter.deliver(&text, events);
            }
        }
    }

    /// Subscribes to [`EVENT_TYPES`], gives `Reconnected`, then pings every `interval`.
    ///
    /// # Errors
    ///
    /// A subscription or a ping fails.
    async fn control(home: &Weak<Self>, events: &Events, interval: Duration) -> anyhow::Result<()> {
        let subscriptions = EVENT_TYPES.map(|kind| {
            Self::send(
                home,
                json!({ "type": "subscribe_events", "event_type": kind }),
            )
        });
        future::try_join_all(subscriptions).await?;
        events.unbounded_send(Event::Reconnected).ok();
        loop {
            time::sleep(interval).await;
            Self::send(home, json!({ "type": "ping" })).await?;
        }
    }

    /// Sends `command` with [`HaHome::request`] while the adapter exists.
    async fn send(home: &Weak<Self>, command: Value) -> anyhow::Result<Value> {
        home.upgrade()
            .ok_or_else(disconnected)?
            .request(command)
            .await
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

    /// Calls a service. Gives the response data when `call.return_response` is true.
    ///
    /// # Errors
    ///
    /// A connection, HA or timeout error.
    pub async fn call(&self, call: ServiceCall) -> anyhow::Result<Option<Value>> {
        let command = json!({
            "type": "call_service", "domain": call.domain, "service": call.service,
            "target": { "entity_id": call.target }, "service_data": call.data,
            "return_response": call.return_response,
        });
        let mut result = self.request(command).await?;
        let response = || result.get_mut("response").map_or(Value::Null, Value::take);
        Ok(call.return_response.then(response))
    }

    /// Speaks `text` on the satellite entity `satellite` with `assist_satellite.announce` and no
    /// preannounce sound. HA returns after the satellite plays the audio.
    ///
    /// # Errors
    ///
    /// A connection, HA or timeout error.
    pub async fn announce(&self, satellite: &str, text: &str) -> anyhow::Result<()> {
        let command = json!({
            "type": "call_service", "domain": "assist_satellite", "service": "announce",
            "target": { "entity_id": [satellite] },
            "service_data": { "message": text, "preannounce": false },
        });
        self.request_within(command, self.announce_deadline).await?;
        Ok(())
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
    async fn request(&self, command: Value) -> anyhow::Result<Value> {
        self.request_within(command, self.deadline).await
    }

    /// Sends the JSON object `command` with the next ID and waits `deadline` for its result.
    async fn request_within(
        &self,
        mut command: Value,
        deadline: Duration,
    ) -> anyhow::Result<Value> {
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
        time::timeout(deadline, exchange)
            .await
            .map_err(|_| anyhow::anyhow!("timeout error: no result within {deadline:?}"))?
    }

    /// Sends a result or a `pong` to its pending request and an event to `events`. Ignores
    /// other messages and IDs.
    fn deliver(&self, text: &str, events: &Events) {
        let (id, result) = match serde_json::from_str(text) {
            Ok(Incoming::Result {
                id,
                success: true,
                result,
                ..
            }) => (id, Ok(result)),
            Ok(Incoming::Result { id, error, .. }) => {
                (id, Err(anyhow::anyhow!("HA error: {error}")))
            }
            Ok(Incoming::Pong { id }) => (id, Ok(Value::Null)),
            Ok(Incoming::Event { event }) => {
                events.unbounded_send(event.into()).ok();
                return;
            }
            Err(_) => return,
        };
        let Some(sender) = self.pending().remove(&id) else {
            return;
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

#[async_trait]
impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> Home for HaHome<S> {
    async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        HaHome::snapshot(self).await
    }

    fn events(&self) -> BoxStream<'static, Event> {
        HaHome::events(self)
    }

    async fn call(&self, call: ServiceCall) -> anyhow::Result<Option<Value>> {
        HaHome::call(self, call).await
    }

    async fn announce(&self, satellite: &str, text: &str) -> anyhow::Result<()> {
        HaHome::announce(self, satellite, text).await
    }
}

/// The wait before connection attempt `attempt + 1`: 100 ms, doubled each attempt, at most 2 s.
fn delay(attempt: u32) -> Duration {
    Duration::from_millis(100 << attempt.min(5)).min(Duration::from_secs(2))
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

/// A message from HA after the authentication.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Incoming {
    /// The result of a command.
    Result {
        /// The request ID.
        id: u64,
        /// True when HA ran the command.
        success: bool,
        /// The result of the command.
        #[serde(default)]
        result: Value,
        /// The code and the message of the error when `success` is false.
        #[serde(default)]
        error: Value,
    },
    /// The answer to a `ping`.
    Pong {
        /// The request ID of the `ping`.
        id: u64,
    },
    /// An event of a subscription.
    Event {
        /// The event.
        event: HaEvent,
    },
}

/// An HA event of one of [`EVENT_TYPES`].
#[derive(Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
enum HaEvent {
    /// The state of one entity changed.
    StateChanged {
        /// The entity and its states.
        data: StateChange,
        /// The cause of the change.
        context: Context,
    },
    /// An area, device or entity registry changed.
    #[serde(
        rename = "area_registry_updated",
        alias = "device_registry_updated",
        alias = "entity_registry_updated"
    )]
    RegistryUpdated,
}

/// The data of a `state_changed` event.
#[derive(Deserialize)]
struct StateChange {
    /// The entity that changed.
    entity_id: String,
    /// Absent for a new entity.
    old_state: Option<Box<State>>,
    /// Absent for a removed entity.
    new_state: Option<Box<State>>,
}

impl From<HaEvent> for Event {
    fn from(event: HaEvent) -> Self {
        match event {
            HaEvent::StateChanged { data, context } => Self::StateChanged {
                entity_id: data.entity_id,
                old: data.old_state,
                new: data.new_state,
                context,
            },
            HaEvent::RegistryUpdated => Self::RegistryUpdated,
        }
    }
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
    use tokio::time::Instant;

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

        /// Sends a failed result with the message "Entity not found." for `request`.
        async fn fail(&mut self, request: &Value) {
            let error = json!({ "code": "not_found", "message": "Entity not found." });
            let failure =
                json!({ "id": request["id"], "type": "result", "success": false, "error": error });
            self.send_json(&failure).await;
        }

        /// Sends `event` as an event of a subscription.
        async fn send_event(&mut self, event: Value) {
            self.send_json(&json!({ "id": 1, "type": "event", "event": event }))
                .await;
        }

        /// Answers the subscription to each type of [`EVENT_TYPES`].
        async fn subscribe(&mut self) {
            let mut kinds = Vec::new();
            for _ in EVENT_TYPES {
                let subscription = self.receive().await;
                assert_eq!(subscription["type"], "subscribe_events");
                kinds.push(subscription["event_type"].clone());
                self.reply(&subscription, Value::Null).await;
            }
            kinds.sort_by_key(ToString::to_string);
            assert_eq!(kinds, EVENT_TYPES.map(Value::from));
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

    /// The connections of one adapter, from the server side.
    struct Network {
        /// The virtual time of each connection attempt.
        attempts: Arc<Mutex<Vec<Instant>>>,
        /// The server end of each connection that the adapter opens.
        servers: mpsc::UnboundedReceiver<FakeHaServer>,
        /// The events of the adapter.
        events: BoxStream<'static, Event>,
    }

    impl Network {
        /// Receives the next connection and answers its token with `auth_ok`.
        async fn authenticated(&mut self) -> FakeHaServer {
            let mut server = self.servers.next().await.expect("the adapter connects");
            server.receive_auth().await;
            server.send("auth_ok").await;
            server
        }

        /// Receives the next connection and answers its token and each subscription.
        async fn accept(&mut self) -> FakeHaServer {
            let mut server = self.authenticated().await;
            server.subscribe().await;
            server
        }

        /// The virtual time of connection attempt `n`, from 0.
        fn attempt(&self, n: usize) -> Instant {
            self.attempts.lock().expect("no test panics with the lock")[n]
        }

        /// The next event.
        async fn event(&mut self) -> Event {
            self.events.next().await.expect("the events continue")
        }
    }

    /// Starts an adapter whose first `refusals` connection attempts fail.
    fn start(refusals: usize) -> (Arc<HaHome<DuplexStream>>, Network) {
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let (sender, servers) = mpsc::unbounded();
        let log = Arc::clone(&attempts);
        let connector = move || {
            let mut log = log.lock().expect("no test panics with the lock");
            log.push(Instant::now());
            let servers = sender.clone();
            let accept = move |server| async move {
                servers.unbounded_send(server).ok();
            };
            future::ready(if log.len() > refusals {
                Ok(open(accept))
            } else {
                Err(io::ErrorKind::ConnectionRefused.into())
            })
        };
        let home = HaHome::start(connector, TOKEN, &config());
        let events = home.events();
        let network = Network {
            attempts,
            servers,
            events,
        };
        (home, network)
    }

    /// Starts an adapter, accepts its connection and runs `server` on it after `Reconnected`.
    async fn ready<F>(
        server: impl FnOnce(FakeHaServer) -> F + Send + 'static,
    ) -> Arc<HaHome<DuplexStream>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (home, mut network) = start(0);
        let fake = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
        tokio::spawn(server(fake));
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
            server.fail(&request).await;
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

    /// Starts an adapter that receives `event` after `Reconnected`, and gives its next event.
    async fn next_event(event: Value) -> Event {
        let (_home, mut network) = start(0);
        let mut server = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
        server.send_event(event).await;
        network.event().await
    }

    /// AC-2.4b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4b_1_state_changed_gives_state_changed_with_context() -> anyhow::Result<()> {
        let state = house()["states"][0].clone();
        let context = json!({ "id": "01K", "user_id": "u1", "parent_id": null });
        let data =
            json!({ "entity_id": state["entity_id"], "old_state": null, "new_state": state });
        let event = json!({ "event_type": "state_changed", "data": data, "context": context });
        let expected = Event::StateChanged {
            entity_id: serde_json::from_value(state["entity_id"].clone())?,
            old: None,
            new: Some(serde_json::from_value(state)?),
            context: serde_json::from_value(context)?,
        };
        assert_eq!(next_event(event).await, expected);
        Ok(())
    }

    /// AC-2.4b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4b_2_entity_registry_updated_gives_registry_updated() {
        let data = json!({ "action": "update", "entity_id": "light.kitchen" });
        let context = json!({ "id": "01K", "user_id": null, "parent_id": null });
        let event =
            json!({ "event_type": "entity_registry_updated", "data": data, "context": context });
        assert_eq!(next_event(event).await, Event::RegistryUpdated);
    }

    /// AC-2.4c-1.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4c_1_close_reconnects_after_first_delay() {
        let (_home, mut network) = start(0);
        let server = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
        let closed = Instant::now();
        drop(server);
        let _server = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
        assert_eq!(network.attempt(1) - closed, Duration::from_millis(100));
    }

    /// AC-2.4c-2.
    #[test]
    fn ac_2_4c_2_delays_double_up_to_two_seconds() {
        let delays: Vec<Duration> = (0..6).map(delay).collect();
        let expected: Vec<Duration> = [100, 200, 400, 800, 1600, 2000]
            .map(Duration::from_millis)
            .to_vec();
        assert_eq!(delays, expected);
    }

    /// AC-2.4c-3.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4c_3_ready_resets_backoff() {
        let (_home, mut network) = start(3);
        let server = network.accept().await;
        let closed = Instant::now();
        drop(server);
        let _server = network.accept().await;
        assert_eq!(network.attempt(4) - closed, Duration::from_millis(100));
    }

    /// AC-2.4c-4.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4c_4_no_pong_reconnects_after_deadline() {
        let (_home, mut network) = start(0);
        let mut server = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
        let ping = server.receive().await;
        assert_eq!(ping["type"], "ping");
        server
            .send_json(&json!({ "id": ping["id"], "type": "pong" }))
            .await;
        let _next = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
        let wait = network.attempt(1) - network.attempt(0);
        assert_eq!(wait, Duration::from_millis(30_000 + 30_000 + 5000 + 100));
        drop(server);
    }

    /// AC-2.4c-5.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4c_5_start_without_server_reconnects_when_available() {
        let (_home, mut network) = start(3);
        let _server = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
    }

    /// AC-2.4c-6.
    #[tokio::test(start_paused = true)]
    async fn ac_2_4c_6_failed_subscription_reconnects_after_first_delay() {
        let (_home, mut network) = start(0);
        let mut server = network.authenticated().await;
        let subscription = server.receive().await;
        server.fail(&subscription).await;
        let failed = Instant::now();
        let _next = network.accept().await;
        assert_eq!(network.event().await, Event::Reconnected);
        assert_eq!(network.attempt(1) - failed, Duration::from_millis(100));
        drop(server);
    }

    /// A `light.turn_on` call of the kitchen light at half brightness.
    fn turn_on(return_response: bool) -> ServiceCall {
        ServiceCall {
            domain: "light".to_owned(),
            service: "turn_on".to_owned(),
            target: vec!["light.kitchen".to_owned()],
            data: Map::from_iter([("brightness_pct".to_owned(), json!(50))]),
            return_response,
        }
    }

    /// The response data of the fake service.
    fn response() -> Value {
        json!({ "light.kitchen": { "brightness": 128 } })
    }

    /// Starts an adapter whose server answers the first request as HA does after `delay`. The
    /// receiver gives that request.
    async fn answer_after(
        delay: Duration,
    ) -> (Arc<HaHome<DuplexStream>>, oneshot::Receiver<Value>) {
        let (sender, receiver) = oneshot::channel();
        let home = ready(move |mut server| async move {
            let request = server.receive().await;
            time::sleep(delay).await;
            let mut result = json!({ "context": { "id": "01K" } });
            if request["return_response"] == true {
                result["response"] = response();
            }
            server.reply(&request, result).await;
            sender.send(request).ok();
            server.wait_close().await;
        })
        .await;
        (home, receiver)
    }

    /// Runs `call` against a server that answers as HA does. Gives the request and the result.
    async fn serve_call(call: ServiceCall) -> anyhow::Result<(Value, Option<Value>)> {
        let (home, request) = answer_after(Duration::ZERO).await;
        let result = home.call(call).await?;
        Ok((request.await?, result))
    }

    /// AC-2.5-1.
    #[tokio::test(start_paused = true)]
    async fn ac_2_5_1_call_sends_call_service_with_target_and_data() -> anyhow::Result<()> {
        let (request, _) = serve_call(turn_on(false)).await?;
        let expected = json!({
            "id": request["id"], "type": "call_service", "domain": "light", "service": "turn_on",
            "target": { "entity_id": ["light.kitchen"] }, "service_data": { "brightness_pct": 50 },
            "return_response": false,
        });
        assert_eq!(request, expected);
        Ok(())
    }

    /// AC-2.5-2.
    #[tokio::test(start_paused = true)]
    async fn ac_2_5_2_return_response_gives_response_data() -> anyhow::Result<()> {
        let (_, result) = serve_call(turn_on(true)).await?;
        assert_eq!(result, Some(response()));
        Ok(())
    }

    /// AC-2.5-3.
    #[tokio::test(start_paused = true)]
    async fn ac_2_5_3_no_return_response_gives_none() -> anyhow::Result<()> {
        let (_, result) = serve_call(turn_on(false)).await?;
        assert_eq!(result, None);
        Ok(())
    }

    /// AC-4.9-5.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9_5_announce_sends_announce_for_satellite() -> anyhow::Result<()> {
        let (home, request) = answer_after(Duration::ZERO).await;
        home.announce("assist_satellite.kitchen", "It is sunny.")
            .await?;
        let request = request.await?;
        let expected = json!({
            "id": request["id"], "type": "call_service", "domain": "assist_satellite",
            "service": "announce", "target": { "entity_id": ["assist_satellite.kitchen"] },
            "service_data": { "message": "It is sunny.", "preannounce": false },
        });
        assert_eq!(request, expected);
        Ok(())
    }

    /// AC-4.9-8.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9_8_result_after_ten_seconds_is_ok() -> anyhow::Result<()> {
        let (home, _request) = answer_after(Duration::from_secs(10)).await;
        home.announce("assist_satellite.kitchen", "It is sunny.")
            .await
    }
}
