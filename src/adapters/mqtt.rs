//! The status entity of Jarvis in HA through MQTT discovery, with a retained state and a last
//! will.

use std::time::Duration;

use anyhow::Context;
use rumqttc::AsyncClient;
use rumqttc::Event;
use rumqttc::EventLoop;
use rumqttc::LastWill;
use rumqttc::MqttOptions;
use rumqttc::Outgoing;
use rumqttc::Packet;
use rumqttc::QoS;
use serde_json::json;
use tokio::sync::Notify;
use tokio::sync::watch;

use crate::ports::State;
use crate::ports::Status;

/// The topic of the status.
const STATE_TOPIC: &str = "jarvis/status";

/// The topic of the discovery message.
const CONFIG_TOPIC: &str = "homeassistant/sensor/jarvis/status/config";

/// The wait after a connection error.
const RETRY: Duration = Duration::from_secs(5);

/// The options of the broker at `url` (`mqtt://host:port`) with `username` and `password`: the
/// client ID `jarvis`, a keep-alive of 30 s and the last will `offline`.
///
/// # Errors
///
/// `url` is not valid.
pub fn options(url: &str, username: &str, password: &str) -> anyhow::Result<MqttOptions> {
    let mut options = MqttOptions::parse_url(format!("{url}?client_id=jarvis"))
        .with_context(|| format!("mqtt.url {url} is not valid"))?;
    let offline = Status {
        state: State::Offline,
        message: None,
    };
    let will = LastWill::new(
        STATE_TOPIC,
        json!(offline).to_string(),
        QoS::AtLeastOnce,
        true,
    );
    options
        .set_credentials(username, password)
        .set_keep_alive(Duration::from_secs(30))
        .set_last_will(will);
    Ok(options)
}

/// Publishes the discovery message and the status of `status` at each connection with `options`,
/// and each change of `status`. When the sender of `status` closes, publishes the last status,
/// sends `DISCONNECT` and ends.
pub async fn publish(options: MqttOptions, status: watch::Receiver<Status>) {
    let (client, events) = AsyncClient::new(options, 10);
    let connected = Notify::new();
    tokio::join!(
        poll(events, &connected),
        follow(&client, status, &connected)
    );
}

/// Polls `events` until `DISCONNECT` is sent, and notifies `connected` at each `ConnAck`. After a
/// connection error, waits [`RETRY`].
async fn poll(mut events: EventLoop, connected: &Notify) {
    loop {
        match events.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => connected.notify_one(),
            Ok(Event::Outgoing(Outgoing::Disconnect)) => return,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!("MQTT connection error: {error}");
                tokio::time::sleep(RETRY).await;
            }
        }
    }
}

/// Publishes the discovery message and the status at each notice of `connected`, and the status
/// at each change of `status`, with `client`. When the sender closes, publishes the last status
/// and sends `DISCONNECT`.
async fn follow(client: &AsyncClient, mut status: watch::Receiver<Status>, connected: &Notify) {
    loop {
        let closed = tokio::select! {
            () = connected.notified() => {
                send(client, CONFIG_TOPIC, discovery()).await;
                false
            }
            changed = status.changed() => changed.is_err(),
        };
        let current = json!(*status.borrow_and_update());
        send(client, STATE_TOPIC, current).await;
        if closed {
            if let Err(error) = client.disconnect().await {
                tracing::warn!("cannot disconnect from MQTT: {error}");
            }
            return;
        }
    }
}

/// Publishes `payload` on `topic` with `QoS` 1 and retain. A full request queue delays the
/// publish until the event loop takes requests again, so no status or discovery is lost.
async fn send(client: &AsyncClient, topic: &str, payload: serde_json::Value) {
    let payload = payload.to_string();
    if let Err(error) = client.publish(topic, QoS::AtLeastOnce, true, payload).await {
        tracing::warn!("cannot publish {topic}: {error}");
    }
}

/// The discovery message of the entity `sensor.jarvis_status`.
fn discovery() -> serde_json::Value {
    json!({
        "name": "Status",
        "unique_id": "jarvis_status",
        "device": { "identifiers": ["jarvis"], "name": "Jarvis" },
        "device_class": "enum",
        "options": ["online", "training", "restarting", "offline"],
        "state_topic": STATE_TOPIC,
        "value_template": "{{ value_json.state }}",
        "json_attributes_topic": STATE_TOPIC,
    })
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::task::JoinHandle;

    use super::*;
    use crate::testing::fakes;
    use crate::testing::fakes::MqttMessage;
    use crate::testing::fakes::MqttSubscriber;

    /// The status `state` with no message.
    fn status(state: State) -> Status {
        Status {
            state,
            message: None,
        }
    }

    /// The JSON form of the status `state` with no message.
    fn payload(state: &str) -> serde_json::Value {
        json!({ "state": state, "message": null })
    }

    /// Starts the adapter on a new broker with the status `state`. Gives the broker, the sender of
    /// the status, the adapter task and a client of `jarvis/status` that got `state`.
    async fn connected(
        state: State,
    ) -> anyhow::Result<(
        SocketAddr,
        watch::Sender<Status>,
        JoinHandle<()>,
        MqttSubscriber,
    )> {
        let broker = fakes::mqtt_broker()?;
        let (sender, receiver) = watch::channel(status(state));
        let options = options(&format!("mqtt://{broker}"), "jarvis", "secret")?;
        let task = tokio::spawn(publish(options, receiver));
        let mut client = MqttSubscriber::new(broker, "early", "jarvis/status")?;
        let first = client.next().await?.payload;
        anyhow::ensure!(first == json!(status(state)), "the first status is {first}");
        Ok((broker, sender, task, client))
    }

    /// The first message of a new client of the topic `jarvis/status` on `broker`.
    async fn retained(broker: SocketAddr) -> anyhow::Result<MqttMessage> {
        MqttSubscriber::new(broker, "late", "jarvis/status")?
            .next()
            .await
    }

    /// AC-12.9c-1.
    #[tokio::test]
    async fn ac_12_9c_1_a_new_client_gets_the_discovery_and_the_state() -> anyhow::Result<()> {
        let (broker, _sender, _task, _client) = connected(State::Online).await?;
        let mut late = MqttSubscriber::new(broker, "late", "#")?;
        let mut messages = vec![late.next().await?, late.next().await?];
        messages.sort_by(|left, right| left.topic.cmp(&right.topic));
        let [config, state] = messages.as_slice() else {
            anyhow::bail!("not two messages: {messages:?}");
        };
        assert_eq!(config.topic, "homeassistant/sensor/jarvis/status/config");
        assert_eq!(config.payload["unique_id"], "jarvis_status");
        assert_eq!(config.payload["state_topic"], "jarvis/status");
        let options = json!(["online", "training", "restarting", "offline"]);
        assert_eq!(config.payload["options"], options);
        assert!(config.retain && state.retain, "not retained: {messages:?}");
        assert_eq!(state.payload, payload("online"));
        Ok(())
    }

    /// AC-12.9c-2.
    #[tokio::test]
    async fn ac_12_9c_2_a_change_publishes_the_state_with_retain() -> anyhow::Result<()> {
        let (broker, sender, _task, mut client) = connected(State::Online).await?;
        sender.send_replace(status(State::Training));
        assert_eq!(client.next().await?.payload, payload("training"));
        let message = retained(broker).await?;
        assert_eq!(
            (message.payload, message.retain),
            (payload("training"), true)
        );
        Ok(())
    }

    /// AC-12.9c-3.
    #[tokio::test]
    async fn ac_12_9c_3_an_abort_gives_the_last_will() -> anyhow::Result<()> {
        let (_broker, _sender, task, mut client) = connected(State::Online).await?;
        task.abort();
        assert_eq!(client.next().await?.payload, payload("offline"));
        Ok(())
    }

    /// AC-12.9c-4.
    #[tokio::test]
    async fn ac_12_9c_4_a_closed_sender_ends_with_the_last_status() -> anyhow::Result<()> {
        let (broker, sender, task, _client) = connected(State::Restarting).await?;
        drop(sender);
        task.await?;
        assert_eq!(retained(broker).await?.payload, payload("restarting"));
        Ok(())
    }
}
