//! The Wyoming TCP server. It calls `Assistant`.

use std::pin;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio_util::task::TaskTracker;

use crate::domain::speech;
use crate::domain::speech::Audio;
use crate::ports::Assistant;
use crate::ports::Context;
use crate::ports::Handled;
use crate::ports::Info;
use crate::ports::Listening;

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
    /// The data in the header line. The data after the header adds to it.
    #[serde(default)]
    data: Map<String, Value>,
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

/// The data of a `synthesize` event.
#[derive(Deserialize)]
struct Synthesize {
    /// The text to speak.
    text: String,
}

/// The `audio-start` data: the format of the audio.
#[derive(Deserialize, PartialEq)]
struct Format {
    /// The samples per second.
    rate: u32,
    /// The bytes of each sample.
    width: u32,
    /// The number of channels.
    channels: u32,
}

/// The only audio format that the first pass accepts: 16 kHz, 16-bit, mono.
const FORMAT: Format = Format {
    rate: 16_000,
    width: 2,
    channels: 1,
};

/// The number of samples in each `audio-chunk` that the server sends.
const CHUNK_SAMPLES: usize = 1024;

/// Accepts connections on `listener` until `shutdown` completes. Serves each one in its own
/// task on `tracker`.
///
/// # Errors
///
/// A connection cannot be accepted.
pub async fn serve(
    listener: TcpListener,
    assistant: Arc<dyn Assistant>,
    tracker: &TaskTracker,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let mut shutdown = pin::pin!(shutdown);
    loop {
        let (stream, _) = tokio::select! {
            accepted = listener.accept() => accepted?,
            () = &mut shutdown => return Ok(()),
        };
        // Else Nagle's algorithm holds the first `audio-chunk` until the client acknowledges
        // `audio-start`.
        stream.set_nodelay(true)?;
        let (assistant, tasks) = (Arc::clone(&assistant), tracker.clone());
        tracker.spawn(async move {
            if let Err(error) = connection(stream, assistant.as_ref(), &tasks).await {
                tracing::warn!("Wyoming connection closed: {error:#}");
            }
        });
    }
}

/// Answers the events of one connection until the client closes it. Runs the work of each
/// reply on `tracker`.
///
/// # Errors
///
/// The connection fails, or a header is not valid JSON.
async fn connection(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    assistant: &dyn Assistant,
    tracker: &TaskTracker,
) -> anyhow::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    let mut session = None;
    while stream.read_line(&mut line).await? > 0 {
        let (
            Header {
                event_type, data, ..
            },
            payload,
        ) = event(&mut stream, &line).await?;
        line.clear();
        let answered = match event_type.as_str() {
            "describe" => write(&mut stream, "info", &info_data(&assistant.info()), &[]).await,
            "transcript" => transcript(&mut stream, assistant, data, tracker).await,
            "synthesize" => synthesize(&mut stream, assistant, data).await,
            other => stt(&mut stream, assistant, &mut session, other, data, &payload).await,
        };
        answered?;
    }
    Ok(())
}

/// Feeds the event `event_type` with `data` and `payload` to `session`. Gives the transcript at
/// a commit and at `audio-stop`. `transcribe` starts a new session. The other STT events act
/// only in an open session, and other events do nothing.
///
/// # Errors
///
/// The session cannot start, the audio is not 16 kHz 16-bit mono, or the session fails.
async fn listen(
    assistant: &dyn Assistant,
    session: &mut Option<Box<dyn Listening>>,
    event_type: &str,
    data: Map<String, Value>,
    payload: &[u8],
) -> anyhow::Result<Option<String>> {
    if event_type == "transcribe" {
        *session = Some(assistant.listen()?);
        return Ok(None);
    }
    let Some(mut listening) = session.take() else {
        return Ok(None);
    };
    let result = match event_type {
        "audio-start" => check_format(data).map(|()| None),
        "audio-chunk" => listening.push(samples(payload)).await,
        "audio-stop" => return listening.finish().await.map(Some),
        _ => Ok(None),
    };
    *session = Some(listening);
    result
}

/// Checks that the `audio-start` data `data` gives 16 kHz 16-bit mono audio.
///
/// # Errors
///
/// The data has no format, or another format.
fn check_format(data: Map<String, Value>) -> anyhow::Result<()> {
    let format: Format = serde_json::from_value(Value::Object(data))?;
    anyhow::ensure!(format == FORMAT, "the audio is not 16 kHz 16-bit mono");
    Ok(())
}

/// Feeds the event `event_type` with `data` and `payload` to `session` with `listen`. Writes
/// `transcript` for a transcript, or `error` for a failure, and then ends `session`. Thus the
/// ended session ignores the rest of its audio.
///
/// # Errors
///
/// The write fails.
async fn stt(
    stream: &mut (impl AsyncWrite + Unpin),
    assistant: &dyn Assistant,
    session: &mut Option<Box<dyn Listening>>,
    event_type: &str,
    data: Map<String, Value>,
    payload: &[u8],
) -> anyhow::Result<()> {
    let (event_type, text) = match listen(assistant, session, event_type, data, payload).await {
        Ok(None) => return Ok(()),
        Ok(Some(text)) => ("transcript", text),
        Err(error) => ("error", format!("{error:#}")),
    };
    *session = None;
    write(stream, event_type, &json!({ "text": text }), &[]).await
}

/// The 16-bit little-endian samples of `payload`.
fn samples(payload: &[u8]) -> Vec<i16> {
    let (pairs, _): (&[[u8; 2]], &[u8]) = payload.as_chunks();
    pairs.iter().copied().map(i16::from_le_bytes).collect()
}

/// Answers the `transcript` data `data` with `handled` or `not-handled`. Starts the work of a
/// reply on `tracker` after the write and the flush.
///
/// # Errors
///
/// The data is not a transcript, or the write fails.
async fn transcript(
    stream: &mut (impl AsyncWrite + Unpin),
    assistant: &dyn Assistant,
    data: Map<String, Value>,
    tracker: &TaskTracker,
) -> anyhow::Result<()> {
    let Transcript { text, context } = serde_json::from_value(Value::Object(data))?;
    match assistant.handle(&text, context).await {
        Ok(Handled { text, after }) => {
            write(stream, "handled", &json!({ "text": text }), &[]).await?;
            stream.flush().await?;
            if let Some(after) = after {
                tracker.spawn(async move {
                    if let Err(error) = after.await {
                        tracing::warn!("the work after the reply failed: {error:#}");
                    }
                });
            }
        }
        Err(error) => {
            let text = error.to_string();
            write(stream, "not-handled", &json!({ "text": text }), &[]).await?;
        }
    }
    Ok(())
}

/// Answers the `synthesize` data `data` with the audio of its text, or with `error` when the
/// audio fails.
///
/// # Errors
///
/// The data is not a `synthesize` event, or the write fails.
async fn synthesize(
    stream: &mut (impl AsyncWrite + Unpin),
    assistant: &dyn Assistant,
    data: Map<String, Value>,
) -> anyhow::Result<()> {
    let Synthesize { text } = serde_json::from_value(Value::Object(data))?;
    match assistant.speak(&text).await {
        Ok(audio) => write_audio(stream, &audio).await,
        Err(error) => {
            let text = format!("{error:#}");
            write(stream, "error", &json!({ "text": text }), &[]).await
        }
    }
}

/// Writes `audio-start`, one `audio-chunk` for each [`CHUNK_SAMPLES`] samples of `audio` and
/// `audio-stop`.
///
/// # Errors
///
/// The write fails.
async fn write_audio(stream: &mut (impl AsyncWrite + Unpin), audio: &Audio) -> anyhow::Result<()> {
    let format = json!({ "rate": audio.rate, "width": 2, "channels": 1 });
    write(stream, "audio-start", &format, &[]).await?;
    let chunks = speech::chunks(audio, CHUNK_SAMPLES);
    for chunk in chunks {
        let payload: Vec<u8> = chunk
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        write(stream, "audio-chunk", &format, &payload).await?;
    }
    write(stream, "audio-stop", &json!({}), &[]).await
}

/// The event of the header line `line` and its payload. The header data is merged with the
/// data that follows the line on `stream`.
///
/// # Errors
///
/// The header is not valid JSON, the read fails, or the data is not a JSON object.
async fn event(
    stream: &mut (impl AsyncRead + Unpin),
    line: &str,
) -> anyhow::Result<(Header, Vec<u8>)> {
    let mut header: Header = serde_json::from_str(line)?;
    let bytes = read(stream, header.data_length).await?;
    let payload = read(stream, header.payload_length).await?;
    if !bytes.is_empty() {
        let more: Map<String, Value> = serde_json::from_slice(&bytes)?;
        header.data.extend(more);
    }
    Ok((header, payload))
}

/// The next `length` bytes of `stream`, or fewer at the end of the stream.
///
/// # Errors
///
/// The read fails.
async fn read(stream: &mut (impl AsyncRead + Unpin), length: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream.take(length).read_to_end(&mut bytes).await?;
    Ok(bytes)
}

/// Writes the event `event_type` with `data` and `payload` to `stream`.
///
/// # Errors
///
/// The write fails.
async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    event_type: &str,
    data: &Value,
    payload: &[u8],
) -> anyhow::Result<()> {
    let data = serde_json::to_vec(data)?;
    let header = json!({
        "type": event_type,
        "data_length": data.len(),
        "payload_length": payload.len(),
    });
    let mut bytes = serde_json::to_vec(&header)?;
    bytes.push(b'\n');
    bytes.extend(data);
    bytes.extend_from_slice(payload);
    Ok(stream.write_all(&bytes).await?)
}

/// The `info` data: one `asr` and one `handle` program, each with one English model, and one
/// `tts` program with one English voice.
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
    let program = |key: &str| {
        let mut program = artifact.clone();
        program[key] = json!([model]);
        program
    };
    json!({
        "asr": [program("models")],
        "handle": [program("models")],
        "tts": [program("voices")],
    })
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
        let assistant = Arc::new(FakeAssistant::default());
        tokio::spawn(async move {
            serve(listener, assistant, &TaskTracker::new(), future::pending()).await
        });
        Ok(address)
    }

    /// A client on a connection to `assistant` through an in-memory stream with a 16-byte buffer
    /// in each direction.
    fn open(assistant: &Arc<FakeAssistant>) -> FakeWyomingClient<DuplexStream> {
        let (client, server) = tokio::io::duplex(16);
        let assistant = Arc::clone(assistant);
        tokio::spawn(
            async move { connection(server, assistant.as_ref(), &TaskTracker::new()).await },
        );
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

    /// Sends `transcribe` and `audio-start` at `rate` Hz on `client`.
    async fn start_audio(
        client: &mut FakeWyomingClient<DuplexStream>,
        rate: u32,
    ) -> anyhow::Result<()> {
        client
            .send("transcribe", &json!({ "language": "en" }))
            .await?;
        let format = json!({ "rate": rate, "width": 2, "channels": 1 });
        client.send("audio-start", &format).await
    }

    /// Sends `audio-chunk` with the 16-bit little-endian `samples` as its payload on `client`.
    async fn chunk(
        client: &mut FakeWyomingClient<DuplexStream>,
        samples: &[i16],
    ) -> anyhow::Result<()> {
        let payload: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let data = json!({ "rate": 16_000, "width": 2, "channels": 1 });
        let header =
            json!({ "type": "audio-chunk", "data": data, "payload_length": payload.len() });
        let mut bytes = format!("{header}\n").into_bytes();
        bytes.extend(payload);
        client.send_raw(&bytes).await
    }

    /// The next event on `client`.
    async fn next(client: &mut FakeWyomingClient<DuplexStream>) -> anyhow::Result<(String, Value)> {
        Ok(client.receive().await?.expect("the server answers"))
    }

    /// The `transcript` event with `text`.
    fn transcript_event(text: &str) -> (String, Value) {
        ("transcript".to_owned(), json!({ "text": text }))
    }

    /// AC-6.9-1.
    #[tokio::test]
    async fn ac_6_9_1_commit_sends_transcript_before_audio_stop() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        assistant.script(Some("turn on the lights"), "finished");
        let mut client = open(&assistant);
        start_audio(&mut client, 16_000).await?;
        chunk(&mut client, &[1, -2, 300]).await?;
        assert_eq!(
            next(&mut client).await?,
            transcript_event("turn on the lights")
        );
        assert_eq!(assistant.pushes(), vec![vec![1, -2, 300]]);
        Ok(())
    }

    /// AC-6.9-2.
    #[tokio::test]
    async fn ac_6_9_2_audio_stop_first_sends_the_finish_transcript() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        assistant.script(None, "finished");
        let mut client = open(&assistant);
        start_audio(&mut client, 16_000).await?;
        chunk(&mut client, &[1, 2]).await?;
        client.send("audio-stop", &json!({})).await?;
        assert_eq!(next(&mut client).await?, transcript_event("finished"));
        Ok(())
    }

    /// AC-6.9-3.
    #[tokio::test]
    async fn ac_6_9_3_audio_after_the_transcript_sends_no_second_transcript() -> anyhow::Result<()>
    {
        let assistant = Arc::new(FakeAssistant::default());
        assistant.script(Some("turn on the lights"), "finished");
        let mut client = open(&assistant);
        start_audio(&mut client, 16_000).await?;
        chunk(&mut client, &[1]).await?;
        next(&mut client).await?;
        chunk(&mut client, &[2]).await?;
        chunk(&mut client, &[3]).await?;
        client.send("audio-stop", &json!({})).await?;
        client.send("describe", &json!({})).await?;
        assert_eq!(next(&mut client).await?.0, "info");
        assert_eq!(assistant.pushes(), vec![vec![1]]);
        Ok(())
    }

    /// AC-6.9-4.
    #[tokio::test]
    async fn ac_6_9_4_describe_lists_the_asr_and_handle_programs() -> anyhow::Result<()> {
        let mut client = open(&Arc::new(FakeAssistant::default()));
        client.send("describe", &json!({})).await?;
        let (_, data) = next(&mut client).await?;
        for service in ["asr", "handle"] {
            let programs = data[service].as_array().cloned().unwrap_or_default();
            assert_eq!(programs.len(), 1, "{service} programs: {data}");
            assert_eq!(programs[0]["models"][0]["languages"], json!(["en"]));
        }
        Ok(())
    }

    /// AC-6.9-5.
    #[tokio::test]
    async fn ac_6_9_5_rate_44100_sends_error_and_no_push() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        assistant.script(None, "finished");
        let mut client = open(&assistant);
        start_audio(&mut client, 44_100).await?;
        assert_eq!(next(&mut client).await?.0, "error");
        chunk(&mut client, &[1]).await?;
        client.send("describe", &json!({})).await?;
        assert_eq!(next(&mut client).await?.0, "info");
        assert_eq!(assistant.pushes(), Vec::<Vec<i16>>::new());
        Ok(())
    }

    /// AC-6.9-6.
    #[tokio::test]
    async fn ac_6_9_6_two_sessions_send_one_transcript_each() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        assistant.script(Some("first"), "unused");
        assistant.script(None, "second");
        let mut client = open(&assistant);
        start_audio(&mut client, 16_000).await?;
        chunk(&mut client, &[1]).await?;
        assert_eq!(next(&mut client).await?, transcript_event("first"));
        start_audio(&mut client, 16_000).await?;
        chunk(&mut client, &[2]).await?;
        client.send("audio-stop", &json!({})).await?;
        assert_eq!(next(&mut client).await?, transcript_event("second"));
        Ok(())
    }

    /// AC-6.10-1. The 1500 samples of `FakeAssistant` give two chunks.
    #[tokio::test]
    async fn ac_6_10_1_synthesize_sends_audio_start_chunks_and_audio_stop() -> anyhow::Result<()> {
        let mut client = open(&Arc::new(FakeAssistant::default()));
        client
            .send("synthesize", &json!({ "text": "One moment." }))
            .await?;
        let mut events = Vec::new();
        for _ in 0..4 {
            events.push(next(&mut client).await?.0);
        }
        let expected = ["audio-start", "audio-chunk", "audio-chunk", "audio-stop"];
        assert_eq!(events, expected);
        Ok(())
    }

    /// AC-6.10-2.
    #[tokio::test]
    async fn ac_6_10_2_describe_lists_the_asr_handle_and_tts_programs() -> anyhow::Result<()> {
        let mut client = open(&Arc::new(FakeAssistant::default()));
        client.send("describe", &json!({})).await?;
        let (_, data) = next(&mut client).await?;
        let services: Vec<&String> = data
            .as_object()
            .map(|map| map.keys().collect())
            .unwrap_or_default();
        assert_eq!(services, ["asr", "handle", "tts"]);
        assert_eq!(data["tts"][0]["voices"][0]["languages"], json!(["en"]));
        Ok(())
    }

    /// A failed `speak` sends `error`.
    #[tokio::test]
    async fn failed_speak_sends_error() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        assistant.fail("no voice");
        let mut client = open(&assistant);
        client.send("synthesize", &json!({ "text": "Hi." })).await?;
        let expected = ("error".to_owned(), json!({ "text": "no voice" }));
        assert_eq!(next(&mut client).await?, expected);
        Ok(())
    }

    /// A `transcribe` without a session sends `error`.
    #[tokio::test]
    async fn failed_listen_sends_error() -> anyhow::Result<()> {
        let mut client = open(&Arc::new(FakeAssistant::default()));
        client.send("transcribe", &json!({})).await?;
        let expected = ("error".to_owned(), json!({ "text": "no scripted session" }));
        assert_eq!(next(&mut client).await?, expected);
        Ok(())
    }

    /// A transcript with its text in the header line and its context in the data block.
    #[tokio::test]
    async fn header_data_merges_with_the_data_block() -> anyhow::Result<()> {
        let assistant = Arc::new(FakeAssistant::default());
        let mut client = open(&assistant);
        let data = json!({ "context": { "device_id": "device-1" } }).to_string();
        let header =
            json!({ "type": "transcript", "data": { "text": "hi" }, "data_length": data.len() });
        client
            .send_raw(format!("{header}\n{data}").as_bytes())
            .await?;
        client.receive().await?;
        let context = Context {
            device_id: Some("device-1".to_owned()),
            ..Context::default()
        };
        assert_eq!(assistant.requests(), vec![("hi".to_owned(), context)]);
        Ok(())
    }
}
