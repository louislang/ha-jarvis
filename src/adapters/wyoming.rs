//! The Wyoming TCP server. It calls `Assistant`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;

use crate::ports::Assistant;
use crate::ports::Context;
use crate::ports::Handled;
use crate::ports::Info;

/// The header line of a Wyoming event.
#[derive(Deserialize)]
struct Header {
    /// The event type.
    #[serde(rename = "type")]
    event_type: String,
    /// The number of data bytes after the header.
    #[serde(default)]
    data_length: u64,
    /// The number of payload bytes after the data.
    #[serde(default)]
    payload_length: u64,
}

/// The data of a `transcript` event.
#[derive(Deserialize)]
struct Transcript {
    /// The text of the request.
    text: String,
    /// The IDs of the request.
    #[serde(default)]
    context: Context,
}

/// Accepts connections on `listener` and serves each one in its own task.
///
/// # Errors
///
/// A connection cannot be accepted.
pub async fn serve(listener: TcpListener, assistant: Arc<dyn Assistant>) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let assistant = Arc::clone(&assistant);
        tokio::spawn(async move {
            if let Err(error) = connection(stream, assistant.as_ref()).await {
                tracing::warn!("Wyoming connection closed: {error:#}");
            }
        });
    }
}

/// Answers the events of one connection until the client closes it.
///
/// # Errors
///
/// The connection fails, or a header is not valid JSON.
async fn connection(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    assistant: &dyn Assistant,
) -> anyhow::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    while stream.read_line(&mut line).await? > 0 {
        let header: Header = serde_json::from_str(&line)?;
        line.clear();
        let data = data(&mut stream, &header).await?;
        match header.event_type.as_str() {
            "describe" => write(&mut stream, "info", &info_data(&assistant.info())).await?,
            "transcript" => transcript(&mut stream, assistant, &data).await?,
            _ => {}
        }
    }
    Ok(())
}

/// Answers the `transcript` data `data` with `handled` or `not-handled`. Starts the work of a
/// reply after the write and the flush.
///
/// # Errors
///
/// The data is not a transcript, or the write fails.
async fn transcript(
    stream: &mut (impl AsyncWrite + Unpin),
    assistant: &dyn Assistant,
    data: &[u8],
) -> anyhow::Result<()> {
    let Transcript { text, context } = serde_json::from_slice(data)?;
    match assistant.handle(&text, context).await {
        Ok(Handled { text, after }) => {
            write(stream, "handled", &json!({ "text": text })).await?;
            stream.flush().await?;
            if let Some(after) = after {
                tokio::spawn(async move {
                    if let Err(error) = after.await {
                        tracing::warn!("the work after the reply failed: {error:#}");
                    }
                });
            }
        }
        Err(error) => write(stream, "not-handled", &json!({ "text": error.to_string() })).await?,
    }
    Ok(())
}

/// Reads the data of the event of `header` from `stream`, and drops its payload.
///
/// # Errors
///
/// The read fails.
async fn data(stream: &mut (impl AsyncRead + Unpin), header: &Header) -> anyhow::Result<Vec<u8>> {
    let mut data = Vec::new();
    (&mut *stream)
        .take(header.data_length)
        .read_to_end(&mut data)
        .await?;
    let payload = &mut stream.take(header.payload_length);
    tokio::io::copy(payload, &mut tokio::io::sink()).await?;
    Ok(data)
}

/// Writes the event `event_type` with `data` to `stream`.
///
/// # Errors
///
/// The write fails.
async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    event_type: &str,
    data: &Value,
) -> anyhow::Result<()> {
    let data = serde_json::to_vec(data)?;
    let header = json!({ "type": event_type, "data_length": data.len() });
    let mut bytes = serde_json::to_vec(&header)?;
    bytes.push(b'\n');
    bytes.extend(data);
    Ok(stream.write_all(&bytes).await?)
}

/// The `info` data: one `handle` program with one English model.
fn info_data(info: &Info) -> Value {
    let artifact = json!({
        "name": info.name,
        "version": info.version,
        "attribution": { "name": "Jarvis", "url": "https://github.com/louislang/ha-jarvis" },
        "installed": true,
        "description": "Voice-controlled home automation for Home Assistant",
    });
    let mut model = artifact.clone();
    model["languages"] = json!(["en"]);
    let mut program = artifact;
    program["models"] = json!([model]);
    json!({ "handle": [program] })
}

/// Tests of the Wyoming server.
#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;
    use std::time::Duration;

    use futures::future;
    use tokio::io::DuplexStream;

    use crate::testing::fakes::FakeAssistant;
    use crate::testing::fakes::FakeWyomingClient;

    /// Starts a server with `FakeAssistant` and returns its address.
    async fn start() -> anyhow::Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(serve(listener, Arc::new(FakeAssistant::default())));
        Ok(address)
    }

    /// A client on a connection to `assistant` through an in-memory stream with a 16-byte buffer
    /// in each direction.
    fn open(assistant: &Arc<FakeAssistant>) -> FakeWyomingClient<DuplexStream> {
        let (client, server) = tokio::io::duplex(16);
        let assistant = Arc::clone(assistant);
        tokio::spawn(async move { connection(server, assistant.as_ref()).await });
        FakeWyomingClient::new(client)
    }

    /// Sends `transcript` with a text and a full context on `client`.
    async fn transcript(client: &mut FakeWyomingClient<DuplexStream>) -> anyhow::Result<()> {
        let context = json!({
            "conversation_id": "conversation-1",
            "device_id": "device-1",
            "satellite_id": "assist_satellite.kitchen",
        });
        let data = json!({ "text": "turn on the lights", "context": context });
        client.send("transcript", &data).await
    }

    /// Sends `transcript` to `assistant` and returns the answer.
    async fn answer(assistant: &Arc<FakeAssistant>) -> anyhow::Result<(String, Value)> {
        let mut client = open(assistant);
        transcript(&mut client).await?;
        Ok(client.receive().await?.expect("the server answers"))
    }

    /// Sends `describe` on `client` and returns the `handle` programs of the `info` answer.
    async fn describe(client: &mut FakeWyomingClient) -> anyhow::Result<Vec<Value>> {
        client.send("describe", &json!({})).await?;
        let (event_type, data) = client.receive().await?.expect("the server answers");
        assert_eq!(event_type, "info", "the answer is not info: {data}");
        Ok(data["handle"].as_array().cloned().unwrap_or_default())
    }

    /// AC-5.1-1.
    #[tokio::test]
    async fn ac_5_1_1_describe_gives_info_with_one_handle_program() -> anyhow::Result<()> {
        let mut client = FakeWyomingClient::connect(start().await?).await?;
        let programs = describe(&mut client).await?;
        let info = FakeAssistant::default().info();
        assert_eq!(programs.len(), 1, "programs: {programs:?}");
        assert_eq!(programs[0]["name"], info.name.as_str());
        assert_eq!(programs[0]["version"], info.version.as_str());
        assert_eq!(programs[0]["installed"], true);
        assert_eq!(programs[0]["models"][0]["languages"], json!(["en"]));
        Ok(())
    }

    /// AC-5.1-2.
    #[tokio::test]
    async fn ac_5_1_2_unknown_event_is_ignored() -> anyhow::Result<()> {
        let mut client = FakeWyomingClient::connect(start().await?).await?;
        client
            .send("no-such-event", &json!({ "text": "x" }))
            .await?;
        assert_eq!(describe(&mut client).await?.len(), 1);
        Ok(())
    }

    /// AC-5.1-3.
    #[tokio::test]
    async fn ac_5_1_3_bad_header_closes_only_its_connection() -> anyhow::Result<()> {
        let address = start().await?;
        let mut bad = FakeWyomingClient::connect(address).await?;
        let mut other = FakeWyomingClient::connect(address).await?;
        bad.send_raw(b"not json\n").await?;
        assert_eq!(bad.receive().await?, None, "the connection is open");
        assert_eq!(describe(&mut other).await?.len(), 1);
        Ok(())
    }

    /// AC-5.1-4.
    #[tokio::test]
    async fn ac_5_1_4_ten_clients_receive_info() -> anyhow::Result<()> {
        let address = start().await?;
        let connects = (0..10).map(|_| FakeWyomingClient::connect(address));
        let mut clients = future::try_join_all(connects).await?;
        let answers = future::try_join_all(clients.iter_mut().map(describe)).await?;
        assert!(
            answers.iter().all(|programs| programs.len() == 1),
            "answers: {answers:?}"
        );
        Ok(())
    }

    /// AC-5.2-1.
    #[tokio::test]
    async fn ac_5_2_1_transcript_gives_the_text_and_the_context() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        answer(&assistant).await?;
        let context = Context {
            conversation_id: Some("conversation-1".to_owned()),
            device_id: Some("device-1".to_owned()),
            satellite_id: Some("assist_satellite.kitchen".to_owned()),
        };
        let expected = vec![("turn on the lights".to_owned(), context)];
        assert_eq!(assistant.requests(), expected);
        Ok(())
    }

    /// AC-5.2-2.
    #[tokio::test]
    async fn ac_5_2_2_ok_gives_handled_with_the_text() -> anyhow::Result<()> {
        let answer = answer(&Arc::new(FakeAssistant::default())).await?;
        let expected = json!({ "text": "reply to turn on the lights" });
        assert_eq!(answer, ("handled".to_owned(), expected));
        Ok(())
    }

    /// AC-5.2-3.
    #[tokio::test]
    async fn ac_5_2_3_err_gives_not_handled_with_the_error_text() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        assistant.fail("no model");
        let answer = answer(&assistant).await?;
        let expected = json!({ "text": "no model" });
        assert_eq!(answer, ("not-handled".to_owned(), expected));
        Ok(())
    }

    /// AC-5.2-4. A virtual sleep returns only when every task waits.
    #[tokio::test(start_paused = true)]
    async fn ac_5_2_4_after_starts_only_when_handled_is_written() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        let mut client = open(&assistant);
        transcript(&mut client).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!assistant.started(), "the work started before the write");
        let (event_type, _) = client.receive().await?.expect("the server answers");
        assert_eq!(event_type, "handled");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            assistant.started(),
            "the work did not start after the write"
        );
        Ok(())
    }
}
