//! The Wyoming TCP server. It calls `Assistant`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

use crate::ports::Assistant;
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
async fn connection(stream: TcpStream, assistant: &dyn Assistant) -> anyhow::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    while stream.read_line(&mut line).await? > 0 {
        let header: Header = serde_json::from_str(&line)?;
        line.clear();
        skip(&mut stream, header.data_length).await?;
        skip(&mut stream, header.payload_length).await?;
        if header.event_type == "describe" {
            write(&mut stream, "info", &info_data(&assistant.info())).await?;
        }
    }
    Ok(())
}

/// Reads and drops the next `length` bytes of `stream`.
///
/// # Errors
///
/// The read fails.
async fn skip(stream: &mut BufReader<TcpStream>, length: u64) -> anyhow::Result<()> {
    tokio::io::copy(&mut stream.take(length), &mut tokio::io::sink()).await?;
    Ok(())
}

/// Writes the event `event_type` with `data` to `stream`.
///
/// # Errors
///
/// The write fails.
async fn write(
    stream: &mut BufReader<TcpStream>,
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

    use futures::future;

    use crate::testing::fakes::FakeAssistant;
    use crate::testing::fakes::FakeWyomingClient;

    /// Starts a server with `FakeAssistant` and returns its address.
    async fn start() -> anyhow::Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(serve(listener, Arc::new(FakeAssistant)));
        Ok(address)
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
        let info = FakeAssistant.info();
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
}
