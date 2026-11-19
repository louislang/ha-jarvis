//! The web server of the Jarvis tab: the page, and the review, status and settings API behind a
//! bearer token.

use std::collections::BTreeMap;
use std::io;
use std::io::Cursor;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::Request;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware;
use axum::middleware::Next;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing;
use hound::SampleFormat;
use hound::WavSpec;
use hound::WavWriter;
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;

use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::feedback::Value;
use crate::domain::learn::LearnedIntent;
use crate::domain::speech::Audio;
use crate::domain::state::EntityId;
use crate::ports::Clock;
use crate::ports::Control;
use crate::ports::Filter;
use crate::ports::Page;
use crate::ports::Refusal;
use crate::ports::Review;
use crate::ports::Setting;
use crate::ports::Status;
use crate::ports::Unassigned;
use crate::ports::Unresolved;

/// The page of the Jarvis tab.
const PAGE: &str = include_str!("review.html");

/// What the routes of the web server use.
pub struct Web {
    /// The review use case.
    pub review: Arc<dyn Review>,
    /// The bearer token of each request under `api/`.
    pub token: String,
    /// The number of interactions on a page of `api/interactions`.
    pub page_size: usize,
    /// The time of each mark.
    pub clock: Arc<dyn Clock>,
    /// The status and the settings of Jarvis.
    pub control: Arc<dyn Control>,
}

/// The `filter` values of `GET api/interactions`.
#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kept {
    /// Every interaction.
    #[default]
    All,
    /// The interactions with a bad mark.
    Bad,
}

/// The query of `GET api/interactions`.
#[derive(Deserialize)]
struct ListQuery {
    /// The interactions to keep.
    #[serde(default)]
    filter: Kept,
    /// The page number, from 0.
    #[serde(default)]
    page: usize,
}

/// The `value` values of a mark.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Rating {
    /// [`Value::Good`].
    Good,
    /// [`Value::Bad`].
    Bad,
}

/// The body of `POST api/interactions/{id}/mark`.
#[derive(Deserialize)]
struct MarkBody {
    /// The rating, or `None` to remove the earlier mark.
    value: Option<Rating>,
    /// The intent that Jarvis should have selected.
    correction: Option<String>,
}

/// The body of `POST api/proposals/{name}`.
#[derive(Deserialize)]
struct ResolveBody {
    /// Approves the entry, else rejects it.
    approve: bool,
}

/// The body of `POST api/interactions/{id}/speaker`.
#[derive(Deserialize)]
struct SpeakerBody {
    /// The `person` entity of the speaker, or `None` for no speaker.
    person: Option<EntityId>,
}

/// Serves the routes of `web` on `listener` until `shutdown` completes, then waits for the open
/// requests.
///
/// # Errors
///
/// The server fails.
pub async fn serve(
    listener: TcpListener,
    web: Web,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, router(web))
        .with_graceful_shutdown(shutdown)
        .await
}

/// The routes of `web`: the page without a token, and one layer that checks the token of every
/// route under `api/`.
fn router(web: Web) -> Router {
    let web = Arc::new(web);
    let api = Router::new()
        .route("/interactions", routing::get(list))
        .route("/interactions/{id}/audio", routing::get(audio))
        .route("/interactions/{id}/mark", routing::post(mark))
        .route("/interactions/{id}/speaker", routing::post(speaker))
        .route("/people", routing::get(people))
        .route("/intents", routing::get(intents))
        .route("/proposals", routing::get(proposals))
        .route("/proposals/{name}", routing::post(resolve))
        .route("/status", routing::get(status))
        .route("/settings", routing::get(settings).post(save))
        .layer(middleware::from_fn_with_state(Arc::clone(&web), authorize));
    Router::new()
        .route("/", routing::get(Html(PAGE)))
        .nest("/api", api)
        .with_state(web)
}

/// Runs `next` only when `request` has the header `Authorization: Bearer <token>`, else gives
/// 401.
async fn authorize(State(web): State<Arc<Web>>, request: Request, next: Next) -> Response {
    let expected = format!("Bearer {}", web.token);
    let given = request.headers().get(header::AUTHORIZATION);
    if given.is_some_and(|given| given.as_bytes() == expected.as_bytes()) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// The interactions of the page and the filter of `query`.
async fn list(
    State(web): State<Arc<Web>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Interaction>>, StatusCode> {
    let filter = match query.filter {
        Kept::All => Filter::All,
        Kept::Bad => Filter::Bad,
    };
    let page = Page {
        number: query.page,
        size: web.page_size,
    };
    let interactions = web
        .review
        .list(filter, page)
        .await
        .map_err(|error| failed(&error))?;
    Ok(Json(interactions))
}

/// The audio of the interaction `id` as a WAV file, or 404 when it has none.
async fn audio(State(web): State<Arc<Web>>, Path(id): Path<u64>) -> Result<Response, StatusCode> {
    let found = web.review.audio(id).await.map_err(|error| failed(&error))?;
    let audio = found.ok_or(StatusCode::NOT_FOUND)?;
    let wav = wav(&audio).map_err(|error| failed(&error))?;
    Ok(([(header::CONTENT_TYPE, "audio/wav")], wav).into_response())
}

/// 500 for `error`, which the log gets.
fn failed(error: &anyhow::Error) -> StatusCode {
    tracing::warn!("a web request failed: {error:#}");
    StatusCode::INTERNAL_SERVER_ERROR
}

/// The 16-bit mono WAV file of `audio`.
///
/// # Errors
///
/// The encoder fails.
fn wav(audio: &Audio) -> anyhow::Result<Vec<u8>> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: audio.rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut bytes, spec)?;
    for &sample in &audio.samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(bytes.into_inner())
}

/// Marks the action of the interaction `id` with `body` at the time of the clock. Gives 400 for
/// an error of `Review::mark`.
async fn mark(
    State(web): State<Arc<Web>>,
    Path(id): Path<u64>,
    Json(body): Json<MarkBody>,
) -> StatusCode {
    let value = body.value.map(|rating| match rating {
        Rating::Good => Value::Good,
        Rating::Bad => Value::Bad,
    });
    let mark = Mark {
        interaction: id,
        value,
        correction: body.correction,
        time: web.clock.now(),
    };
    match web.review.mark(mark).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::info!("a mark was refused: {error:#}");
            StatusCode::BAD_REQUEST
        }
    }
}

/// The names that a correction can have.
async fn intents(State(web): State<Arc<Web>>) -> Json<Vec<String>> {
    Json(web.review.intents())
}

/// The `pending` learned entries.
async fn proposals(State(web): State<Arc<Web>>) -> Result<Json<Vec<LearnedIntent>>, StatusCode> {
    let proposals = web.review.proposals().map_err(|error| failed(&error))?;
    Ok(Json(proposals))
}

/// Approves or rejects the learned entry `name` with `body`. Gives 409 when no `pending` entry
/// has the name.
async fn resolve(
    State(web): State<Arc<Web>>,
    Path(name): Path<String>,
    Json(body): Json<ResolveBody>,
) -> StatusCode {
    match web.review.resolve(&name, body.approve).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(Unresolved::NotPending) => StatusCode::CONFLICT,
        Err(Unresolved::Failed(error)) => failed(&error),
    }
}

/// The people of the house as `{entity, name}`.
async fn people(State(web): State<Arc<Web>>) -> Json<Vec<serde_json::Value>> {
    let people = web.review.people().into_iter();
    Json(
        people
            .map(|person| json!({ "entity": person.entity, "name": person.name }))
            .collect(),
    )
}

/// Sets the person of `body` as the speaker of the interaction `id`. Gives 400 for a person that
/// is not one of the people, and 404 when the interaction has no embedding.
async fn speaker(
    State(web): State<Arc<Web>>,
    Path(id): Path<u64>,
    Json(body): Json<SpeakerBody>,
) -> StatusCode {
    match web.review.assign(id, body.person).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(Unassigned::NotPerson) => StatusCode::BAD_REQUEST,
        Err(Unassigned::NotFound) => StatusCode::NOT_FOUND,
        Err(Unassigned::Failed(error)) => failed(&error),
    }
}

/// The status of Jarvis.
async fn status(State(web): State<Arc<Web>>) -> Json<Status> {
    Json(web.control.status())
}

/// The fields of the settings form with their values.
async fn settings(State(web): State<Arc<Web>>) -> Result<Json<Vec<Setting>>, StatusCode> {
    let settings = web.control.settings().map_err(|error| failed(&error))?;
    Ok(Json(settings))
}

/// Saves the changed fields of `changes`. Gives 400 with the message of an invalid value, and
/// 409 while another save runs.
async fn save(
    State(web): State<Arc<Web>>,
    Json(changes): Json<BTreeMap<String, serde_json::Value>>,
) -> Response {
    let (status, message) = match web.control.save(changes) {
        Ok(()) => return StatusCode::NO_CONTENT.into_response(),
        Err(Refusal::Invalid(message)) => (StatusCode::BAD_REQUEST, message),
        Err(Refusal::Busy) => (StatusCode::CONFLICT, "Jarvis is busy".to_owned()),
    };
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::domain::learn::Learned;
    use crate::domain::learn::LearnedStatus;
    use crate::domain::speaker::Person;
    use crate::ports;
    use crate::testing::fakes;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeControl;
    use crate::testing::fakes::FakeReview;

    /// The token of the test server.
    const TOKEN: &str = "secret";

    /// The audio samples of the interaction 1.
    const SAMPLES: [i16; 4] = [0, 1000, -1000, i16::MAX];

    /// The interaction `id`, with the audio `samples` at 16 kHz when there are any.
    fn logged(id: u64, samples: &[i16]) -> anyhow::Result<(Interaction, Option<Audio>)> {
        let interaction = serde_json::from_value(json!({
            "id": id, "time": "2026-11-12T08:00:00Z", "transcript": "turn on the tv",
            "model_context": "area: office", "model": "fixture", "top_labels": [], "slots": [],
            "action": { "intent": "tv_on", "targets": [] }, "decision": "Reply",
            "service_calls": [], "responses": [], "marks": [],
        }))?;
        let audio = (!samples.is_empty()).then(|| Audio {
            samples: samples.to_vec(),
            rate: 16_000,
        });
        Ok((interaction, audio))
    }

    /// A `FakeReview` with the interaction 1 with audio, the interaction 2 without audio, the
    /// intent `tv_on` and the person `person.sam`.
    fn review() -> anyhow::Result<FakeReview> {
        Ok(FakeReview {
            people: vec![Person {
                entity: EntityId("person.sam".to_owned()),
                name: "Sam".to_owned(),
            }],
            interactions: vec![logged(2, &[])?, logged(1, &SAMPLES)?],
            intents: vec!["tv_on".to_owned(), "unknown".to_owned()],
            ..FakeReview::default()
        })
    }

    /// The control of Jarvis after the start, which accepts each save.
    const ONLINE: FakeControl = FakeControl {
        status: Status {
            state: ports::State::Online,
            message: None,
        },
        refusal: None,
    };

    /// Serves `review` and `control` on a local port and gives the URL of `api/`.
    async fn start(review: Arc<FakeReview>, control: FakeControl) -> anyhow::Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/api/", listener.local_addr()?);
        let web = Web {
            review,
            token: TOKEN.to_owned(),
            page_size: 50,
            clock: Arc::new(FakeClock(jiff::Timestamp::UNIX_EPOCH)),
            control: Arc::new(control),
        };
        tokio::spawn(serve(listener, web, std::future::pending()));
        Ok(url)
    }

    /// The answer of a server of `review()` to `GET api/<path>` with the token.
    async fn get(path: &str) -> anyhow::Result<reqwest::Response> {
        let url = format!("{}{path}", start(Arc::new(review()?), ONLINE).await?);
        let request = reqwest::Client::new().get(url).bearer_auth(TOKEN);
        Ok(request.send().await?)
    }

    /// The learned entries of a `FakeReview`, one for each status, each named after its status.
    fn proposing() -> Learned {
        let statuses = [
            LearnedStatus::Pending,
            LearnedStatus::Active,
            LearnedStatus::Rejected,
            LearnedStatus::None,
            LearnedStatus::Requested,
        ];
        let named = |status| fakes::learned(&format!("{status:?}").to_lowercase(), status);
        Learned {
            intents: statuses.map(named).to_vec(),
        }
    }

    /// The status of a mark request with `body` on the interaction 1, and the marks of `review`.
    async fn post_mark(
        review: FakeReview,
        body: serde_json::Value,
    ) -> anyhow::Result<(StatusCode, Vec<Mark>)> {
        let review = Arc::new(review);
        let url = format!(
            "{}interactions/1/mark",
            start(Arc::clone(&review), ONLINE).await?
        );
        let request = reqwest::Client::new().post(url).bearer_auth(TOKEN);
        let response = request.json(&body).send().await?;
        let marks = review.marks.lock().expect("no test panicked").clone();
        Ok((response.status(), marks))
    }

    /// AC-12.8-1.
    #[tokio::test]
    async fn ac_12_8_1_missing_or_wrong_token_gives_401() -> anyhow::Result<()> {
        let url = start(Arc::new(review()?), ONLINE).await?;
        let client = reqwest::Client::new();
        let requests = [
            client.get(format!("{url}interactions")),
            client.get(format!("{url}intents")).bearer_auth("wrong"),
            client.post(format!("{url}interactions/1/mark")),
        ];
        for request in requests {
            let response = request.send().await?;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{response:?}");
        }
        Ok(())
    }

    /// AC-12.8-2.
    #[tokio::test]
    async fn ac_12_8_2_list_gives_the_page_of_the_review() -> anyhow::Result<()> {
        let page: Vec<Interaction> = get("interactions?filter=bad&page=1").await?.json().await?;
        let logged = review()?.interactions.into_iter();
        assert_eq!(
            page,
            logged
                .map(|(interaction, _)| interaction)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    /// AC-12.8-3.
    #[tokio::test]
    async fn ac_12_8_3_audio_is_a_wav_file_of_the_samples() -> anyhow::Result<()> {
        let bytes = get("interactions/1/audio")
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let mut reader = hound::WavReader::new(bytes.as_ref())?;
        let samples: Vec<i16> = reader.samples().collect::<Result<_, _>>()?;
        assert_eq!(
            (reader.spec().sample_rate, samples),
            (16_000, SAMPLES.to_vec())
        );
        Ok(())
    }

    /// AC-12.8-4.
    #[tokio::test]
    async fn ac_12_8_4_no_audio_gives_404() -> anyhow::Result<()> {
        let status = get("interactions/2/audio").await?.status();
        assert_eq!(status, StatusCode::NOT_FOUND);
        Ok(())
    }

    /// AC-12.8-5.
    #[tokio::test]
    async fn ac_12_8_5_unknown_correction_gives_400() -> anyhow::Result<()> {
        let body = json!({ "value": "bad", "correction": "tv_onn" });
        let (status, marks) = post_mark(review()?, body).await?;
        assert_eq!((status, marks), (StatusCode::BAD_REQUEST, Vec::new()));
        Ok(())
    }

    /// AC-12.8-5. A known correction reaches the review as a mark on the action.
    #[tokio::test]
    async fn ac_12_8_5_known_correction_reaches_the_review() -> anyhow::Result<()> {
        let body = json!({ "value": "bad", "correction": "tv_on" });
        let (status, marks) = post_mark(review()?, body).await?;
        let expected = Mark {
            interaction: 1,
            value: Some(Value::Bad),
            correction: Some("tv_on".to_owned()),
            time: jiff::Timestamp::UNIX_EPOCH,
        };
        assert!(status.is_success(), "{status}");
        assert_eq!(marks, vec![expected]);
        Ok(())
    }

    /// AC-12.9-1.
    #[tokio::test]
    async fn ac_12_9_1_page_needs_no_token_and_has_the_review_elements() -> anyhow::Result<()> {
        let api = start(Arc::new(review()?), ONLINE).await?;
        let response = reqwest::get(api.trim_end_matches("api/")).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let html = response.text().await?;
        let (_, template) = html
            .split_once(r#"<template id="interaction">"#)
            .expect("the page has template#interaction");
        let elements = [
            r#"id="interactions""#,
            r#"id="filter""#,
            r#"<button class="mark-good""#,
            r#"<button class="mark-bad""#,
            r#"<select class="correction""#,
        ];
        for element in elements {
            assert!(html.contains(element), "the page has no {element}");
        }
        assert!(template.contains("<audio"), "the template has no audio");
        Ok(())
    }

    /// AC-12.9b-2.
    #[tokio::test]
    async fn ac_12_9b_2_status_gives_the_state_and_the_message() -> anyhow::Result<()> {
        let training = FakeControl {
            status: Status {
                state: ports::State::Training,
                message: Some("run 7".to_owned()),
            },
            refusal: None,
        };
        let url = format!("{}status", start(Arc::new(review()?), training).await?);
        let request = reqwest::Client::new().get(url).bearer_auth(TOKEN);
        let status: serde_json::Value = request.send().await?.json().await?;
        assert_eq!(status, json!({ "state": "training", "message": "run 7" }));
        Ok(())
    }

    /// AC-12.9b-3.
    #[tokio::test]
    async fn ac_12_9b_3_page_has_the_status_section() -> anyhow::Result<()> {
        let api = start(Arc::new(review()?), ONLINE).await?;
        let html = reqwest::get(api.trim_end_matches("api/"))
            .await?
            .text()
            .await?;
        let (_, section) = html
            .split_once(r#"<section id="status">"#)
            .expect("the page has #status");
        let (section, _) = section.split_once("</section>").expect("#status ends");
        for element in [r#"class="state""#, r#"class="message""#] {
            assert!(section.contains(element), "#status has no {element}");
        }
        Ok(())
    }

    /// The answer of a server of `control` to `POST api/settings` with a change.
    async fn post_settings(control: FakeControl) -> anyhow::Result<reqwest::Response> {
        let url = format!("{}settings", start(Arc::new(review()?), control).await?);
        let request = reqwest::Client::new().post(url).bearer_auth(TOKEN);
        Ok(request
            .json(&json!({ "asr.endpoint_ms": 700 }))
            .send()
            .await?)
    }

    /// AC-12.9e-4.
    #[tokio::test]
    async fn ac_12_9e_4_save_gives_400_with_the_refusal_or_204() -> anyhow::Result<()> {
        let message = "asr.endpoint_ms is too long";
        let refusing = FakeControl {
            refusal: Some(Refusal::Invalid(message.to_owned())),
            ..ONLINE
        };
        let refused = post_settings(refusing).await?;
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = refused.json().await?;
        assert_eq!(body, json!({ "error": message }));
        assert_eq!(
            post_settings(ONLINE).await?.status(),
            StatusCode::NO_CONTENT
        );
        Ok(())
    }

    /// AC-12.9f-3.
    #[tokio::test]
    async fn ac_12_9f_3_busy_gives_409() -> anyhow::Result<()> {
        let busy = FakeControl {
            refusal: Some(Refusal::Busy),
            ..ONLINE
        };
        assert_eq!(post_settings(busy).await?.status(), StatusCode::CONFLICT);
        Ok(())
    }

    /// AC-12.9e-5.
    #[tokio::test]
    async fn ac_12_9e_5_page_has_the_settings_form() -> anyhow::Result<()> {
        let api = start(Arc::new(review()?), ONLINE).await?;
        let html = reqwest::get(api.trim_end_matches("api/"))
            .await?
            .text()
            .await?;
        for element in [
            r#"<form id="settings""#,
            r#"<button id="save""#,
            r#"id="settings-error""#,
        ] {
            assert!(html.contains(element), "the page has no {element}");
        }
        Ok(())
    }

    /// AC-14.6-1.
    #[tokio::test]
    async fn ac_14_6_1_proposals_are_the_pending_entries() -> anyhow::Result<()> {
        let review = FakeReview {
            learned: Mutex::new(proposing()),
            ..FakeReview::default()
        };
        let url = format!("{}proposals", start(Arc::new(review), ONLINE).await?);
        let request = reqwest::Client::new().get(url).bearer_auth(TOKEN);
        let proposals: Vec<LearnedIntent> = request.send().await?.json().await?;
        assert_eq!(
            proposals,
            [fakes::learned("pending", LearnedStatus::Pending)]
        );
        Ok(())
    }

    /// AC-14.6-4.
    #[tokio::test]
    async fn ac_14_6_4_entry_that_is_not_pending_gives_409() -> anyhow::Result<()> {
        let review = Arc::new(FakeReview {
            learned: Mutex::new(proposing()),
            ..FakeReview::default()
        });
        let url = start(Arc::clone(&review), ONLINE).await?;
        let post = |name: &str, approve: bool| {
            let request = reqwest::Client::new().post(format!("{url}proposals/{name}"));
            request
                .bearer_auth(TOKEN)
                .json(&json!({ "approve": approve }))
                .send()
        };
        for name in ["active", "rejected", "none", "requested", "absent"] {
            for approve in [true, false] {
                let status = post(name, approve).await?.status();
                assert_eq!(status, StatusCode::CONFLICT, "{name} {approve}");
            }
        }
        let learned = review.learned.lock().expect("no test panicked").clone();
        assert_eq!(learned, proposing());
        Ok(())
    }

    /// AC-14.6-5.
    #[tokio::test]
    async fn ac_14_6_5_page_has_the_proposal_template() -> anyhow::Result<()> {
        let api = start(Arc::new(review()?), ONLINE).await?;
        let html = reqwest::get(api.trim_end_matches("api/")).await?;
        let html = html.text().await?;
        let (_, template) = html
            .split_once(r#"<template id="proposal">"#)
            .expect("the page has template#proposal");
        let (template, _) = template.split_once("</template>").expect("it ends");
        for element in [
            r#"class="examples""#,
            r#"class="service""#,
            r#"<button class="approve""#,
            r#"<button class="reject""#,
        ] {
            assert!(template.contains(element), "the template has no {element}");
        }
        Ok(())
    }

    /// AC-15.8c-1.
    #[tokio::test]
    async fn ac_15_8c_1_people_are_the_people_of_the_review() -> anyhow::Result<()> {
        let people: serde_json::Value = get("people").await?.json().await?;
        assert_eq!(people, json!([{ "entity": "person.sam", "name": "Sam" }]));
        Ok(())
    }

    /// The status of a speaker request with `person` on the interaction `id`, and the assigns of
    /// the review.
    async fn post_speaker(
        id: u64,
        person: serde_json::Value,
    ) -> anyhow::Result<(StatusCode, Vec<(u64, Option<EntityId>)>)> {
        let review = Arc::new(review()?);
        let url = start(Arc::clone(&review), ONLINE).await?;
        let request = reqwest::Client::new().post(format!("{url}interactions/{id}/speaker"));
        let body = json!({ "person": person });
        let response = request.bearer_auth(TOKEN).json(&body).send().await?;
        let assigns = review.assigns.lock().expect("no test panicked").clone();
        Ok((response.status(), assigns))
    }

    /// AC-15.8c-2.
    #[tokio::test]
    async fn ac_15_8c_2_speaker_assigns_the_person_or_none() -> anyhow::Result<()> {
        let sam = Some(EntityId("person.sam".to_owned()));
        let picked = post_speaker(1, json!("person.sam")).await?;
        assert_eq!(picked, (StatusCode::NO_CONTENT, vec![(1, sam)]));
        let cleared = post_speaker(1, serde_json::Value::Null).await?;
        assert_eq!(cleared, (StatusCode::NO_CONTENT, vec![(1, None)]));
        Ok(())
    }

    /// AC-15.8c-3.
    #[tokio::test]
    async fn ac_15_8c_3_unknown_person_gives_400_and_no_embedding_404() -> anyhow::Result<()> {
        let unknown = post_speaker(1, json!("person.alex")).await?;
        assert_eq!(unknown, (StatusCode::BAD_REQUEST, Vec::new()));
        let missing = post_speaker(9, json!("person.sam")).await?;
        assert_eq!(missing, (StatusCode::NOT_FOUND, Vec::new()));
        Ok(())
    }

    /// AC-15.8c-4.
    #[tokio::test]
    async fn ac_15_8c_4_interaction_template_has_the_speaker_list() -> anyhow::Result<()> {
        let api = start(Arc::new(review()?), ONLINE).await?;
        let html = reqwest::get(api.trim_end_matches("api/")).await?;
        let html = html.text().await?;
        let (_, template) = html
            .split_once(r#"<template id="interaction">"#)
            .expect("the page has template#interaction");
        let (template, _) = template.split_once("</template>").expect("it ends");
        assert!(
            template.contains(r#"<select class="speaker""#),
            "the template has no select.speaker"
        );
        Ok(())
    }

    /// The function `name` of the page script, up to its closing brace.
    fn page_function(name: &str) -> &'static str {
        let start = PAGE
            .find(&format!("  function {name}("))
            .expect("the page script has the function");
        let length = PAGE[start..].find("\n  }\n").expect("the function ends") + 4;
        &PAGE[start..start + length]
    }

    /// The standard output of `script` in Node.
    fn node_output(script: &str) -> anyhow::Result<String> {
        let file = tempfile::NamedTempFile::new()?;
        std::fs::write(file.path(), script)?;
        let output = std::process::Command::new("node")
            .arg(file.path())
            .output()?;
        assert!(output.status.success(), "node gave {}", output.status);
        Ok(String::from_utf8(output.stdout)?)
    }

    /// AC-15.8e-1.
    #[test]
    fn ac_15_8e_1_found_gives_the_name_and_the_source() -> anyhow::Result<()> {
        let script = format!(
            "{}\nconsole.log(found('Louis', 'Voice'));",
            page_function("found")
        );
        assert_eq!(node_output(&script)?.trim_end(), "Louis (voice)");
        Ok(())
    }

    /// AC-15.8e-2. The test picks each choice of a voice-identified request, and the empty
    /// choice of the template.
    #[test]
    fn ac_15_8e_2_only_a_person_or_the_empty_choice_sends_a_request() -> anyhow::Result<()> {
        let harness = r"
            const people = [{ entity: 'person.sam', name: 'Sam' }];
            const sent = [];
            const post = async (url, body) => { sent.push(body.person ?? ''); return true; };
            class Option { constructor(text, value) { this.value = value; } remove() {} }
            const choices = [''];
            const speaker = {
              append(...options) { choices.push(...options.map((option) => option.value)); },
              addEventListener(event, listener) { this.listener = listener; },
            };
            const interaction = { id: 1, satellite: 'office', speaker: 'person.sam', source: 'Voice' };
            pickSpeaker(speaker, interaction);
            (async () => {
              for (const choice of [...choices]) {
                speaker.value = choice;
                await speaker.listener();
              }
              console.log(JSON.stringify({ choices, sent }));
            })();";
        let script = format!(
            "{}\n{}\n{harness}",
            page_function("found"),
            page_function("pickSpeaker")
        );
        let output: serde_json::Value = serde_json::from_str(&node_output(&script)?)?;
        let choices = output["choices"].as_array().expect("choices is a list");
        let wanted: Vec<serde_json::Value> = choices
            .iter()
            .filter(|choice| {
                choice
                    .as_str()
                    .is_some_and(|value| value.is_empty() || value.starts_with("person."))
            })
            .cloned()
            .collect();
        assert!(
            choices.len() > wanted.len(),
            "no found choice in {choices:?}"
        );
        assert_eq!(output["sent"], serde_json::Value::Array(wanted));
        Ok(())
    }

    /// AC-15.9b-4.
    #[tokio::test]
    async fn ac_15_9b_4_settings_have_the_person_template() -> anyhow::Result<()> {
        let api = start(Arc::new(review()?), ONLINE).await?;
        let html = reqwest::get(api.trim_end_matches("api/")).await?;
        let html = html.text().await?;
        let (_, settings) = html
            .split_once(r#"<section id="configuration""#)
            .expect("the page has the settings section");
        let (settings, _) = settings.split_once("</section>").expect("it ends");
        let (_, template) = settings
            .split_once(r#"<template id="person">"#)
            .expect("the settings section has template#person");
        let (template, _) = template.split_once("</template>").expect("it ends");
        assert!(
            template.contains(r#"<input class="address""#),
            "the template has no input.address"
        );
        Ok(())
    }

    /// AC-12.9-2.
    #[test]
    fn ac_12_9_2_page_script_parses() -> anyhow::Result<()> {
        let (_, script) = PAGE.split_once("<script>").expect("the page has a script");
        let (script, _) = script.split_once("</script>").expect("the script ends");
        let file = tempfile::NamedTempFile::new()?;
        std::fs::write(file.path(), script)?;
        let status = std::process::Command::new("node")
            .arg("--check")
            .arg(file.path())
            .status()?;
        assert!(status.success(), "node --check gave {status}");
        Ok(())
    }

    /// AC-12.9-3. A quote or `=` before `/`, `http:` or `https:` starts an absolute URL in an
    /// attribute or a string.
    #[test]
    fn ac_12_9_3_page_has_only_relative_urls_and_no_token() {
        for opener in ['"', '\'', '`', '='] {
            for start in ["/", "http:", "https:"] {
                let absolute = format!("{opener}{start}");
                assert!(!PAGE.contains(&absolute), "the page contains {absolute}");
            }
        }
        assert!(!PAGE.contains("Authorization"), "the page sends a token");
    }
}
