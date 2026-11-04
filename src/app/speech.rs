//! The `listen` and `speak` use cases, the audio cache and the pre-render of the pool responses.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::Map;
use serde_json::Value;

use crate::domain::catalog;
use crate::domain::intent;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::speech;
use crate::domain::speech::Audio;
use crate::domain::speech::CommitPolicy;
use crate::domain::training;
use crate::ports::Classifier;
use crate::ports::Listening;
use crate::ports::Partial;
use crate::ports::Reasoner;
use crate::ports::SpeechToText;
use crate::ports::SttStream;
use crate::ports::TextToSpeech;

/// The number of samples in one millisecond of 16 kHz audio.
const SAMPLES_PER_MS: u64 = 16;

/// Starts listen sessions that warm System Two, predict on each changed first-pass partial,
/// commit by the policy and give the second-pass transcript.
pub struct Listener {
    /// The two passes of each session.
    stt: Arc<dyn SpeechToText>,
    /// The speculative prediction of each changed partial.
    classifier: Arc<dyn Classifier>,
    /// System Two, warmed at the start of each session.
    reasoner: Arc<dyn Reasoner>,
    /// The silence and length limits of an utterance.
    policy: CommitPolicy,
    /// The lowest top probability of a confident partial: `model.commit_threshold`.
    threshold: f32,
}

impl Listener {
    /// Listens with `stt` and `classifier`, warms `reasoner`, and commits by `policy` and
    /// `threshold`.
    #[must_use]
    pub fn new(
        stt: Arc<dyn SpeechToText>,
        classifier: Arc<dyn Classifier>,
        reasoner: Arc<dyn Reasoner>,
        policy: CommitPolicy,
        threshold: f32,
    ) -> Self {
        Self {
            stt,
            classifier,
            reasoner,
            policy,
            threshold,
        }
    }

    /// A session on a new first-pass stream, with no text. Starts a warm of System Two in the
    /// background.
    ///
    /// # Errors
    ///
    /// The first pass cannot start.
    pub fn listen(&self) -> anyhow::Result<Box<dyn Listening>> {
        let stream = self.stt.stream()?;
        let reasoner = Arc::clone(&self.reasoner);
        tokio::spawn(async move {
            if let Err(error) = reasoner.warm().await {
                tracing::warn!("the warm of System Two failed: {error:#}");
            }
        });
        Ok(Box::new(Session {
            stream: Arc::new(Mutex::new(stream)),
            stt: Arc::clone(&self.stt),
            classifier: Arc::clone(&self.classifier),
            policy: self.policy.clone(),
            threshold: self.threshold,
            text: String::new(),
            confident: false,
            samples: Vec::new(),
        }))
    }
}

/// The state of one listen session.
struct Session {
    /// The first pass of the utterance. A blocking thread decodes with it.
    stream: Arc<Mutex<Box<dyn SttStream>>>,
    /// The second pass of the utterance.
    stt: Arc<dyn SpeechToText>,
    /// The speculative prediction of each changed partial.
    classifier: Arc<dyn Classifier>,
    /// The silence and length limits of the utterance.
    policy: CommitPolicy,
    /// The lowest top probability of a confident partial.
    threshold: f32,
    /// The text of the last changed, non-empty partial.
    text: String,
    /// True when the prediction of `text` is a confident command.
    confident: bool,
    /// The pushed samples.
    samples: Vec<i16>,
}

impl Session {
    /// Keeps `text` and its confidence when it is a changed, non-empty text.
    async fn update(&mut self, text: &str) {
        if text != self.text && !text.is_empty() {
            self.confident = confident(&*self.classifier, self.threshold, text).await;
            text.clone_into(&mut self.text);
        }
    }

    /// True when `partial` ends the utterance by the policy.
    fn commits(&self, partial: &Partial) -> bool {
        let silence_ms = partial
            .voice_end_ms
            .map(|voice_end_ms| partial.vad_ms.saturating_sub(voice_end_ms));
        let has_text = !self.text.is_empty();
        let audio_ms = self.audio_ms();
        speech::should_commit(silence_ms, has_text, audio_ms, self.confident, &self.policy)
    }

    /// The length of the pushed samples in ms.
    fn audio_ms(&self) -> u64 {
        self.samples.len() as u64 / SAMPLES_PER_MS
    }

    /// The second-pass text of the pushed samples, else the first-pass text. A second-pass error
    /// gives the first-pass text. Logs the text, its pass and the audio length.
    ///
    /// # Errors
    ///
    /// The second pass panicked.
    async fn transcript(&self) -> anyhow::Result<String> {
        let (stt, samples) = (Arc::clone(&self.stt), self.samples.clone());
        let (text, pass) =
            match tokio::task::spawn_blocking(move || stt.transcribe(&samples)).await? {
                Ok(Some(text)) => (text, "second"),
                Ok(None) => (self.text.clone(), "first"),
                Err(error) => {
                    tracing::warn!("the second pass failed: {error:#}");
                    (self.text.clone(), "first")
                }
            };
        tracing::info!(
            transcript = text,
            pass,
            audio_ms = self.audio_ms(),
            "transcript"
        );
        Ok(text)
    }
}

/// Decodes `samples` with `stream` and gives its partial.
///
/// # Errors
///
/// An earlier decode panicked.
fn accept(stream: &Mutex<Box<dyn SttStream>>, samples: &[i16]) -> anyhow::Result<Partial> {
    let mut stream = stream
        .lock()
        .map_err(|_poisoned| anyhow::anyhow!("an earlier decode panicked"))?;
    stream.accept(samples);
    Ok(stream.partial())
}

/// True when the top label of the `classifier` prediction of `text` in no context is not
/// `unknown` and its probability is `threshold` or more. False when the prediction fails.
async fn confident(classifier: &dyn Classifier, threshold: f32, text: &str) -> bool {
    let context = training::context_text(None, &[]);
    match classifier.predict(&context, &intent::normalize(text)).await {
        Ok(prediction) => prediction
            .ranked
            .first()
            .is_some_and(|(label, probability)| label != "unknown" && *probability >= threshold),
        Err(error) => {
            tracing::warn!("the speculative prediction failed: {error:#}");
            false
        }
    }
}

#[async_trait]
impl Listening for Session {
    async fn push(&mut self, samples: Vec<i16>) -> anyhow::Result<Option<String>> {
        self.samples.extend_from_slice(&samples);
        let stream = Arc::clone(&self.stream);
        let partial = tokio::task::spawn_blocking(move || accept(&stream, &samples)).await??;
        self.update(&partial.text).await;
        if !self.commits(&partial) {
            return Ok(None);
        }
        Ok(Some(self.transcript().await?))
    }

    async fn finish(self: Box<Self>) -> anyhow::Result<String> {
        self.transcript().await
    }
}

/// Speaks texts from the audio cache, or with TTS when a text is not in the cache.
pub struct Speech {
    /// The synthesis of the texts that are not in the cache.
    tts: Arc<dyn TextToSpeech>,
    /// The audio of each pre-rendered text.
    cache: DashMap<String, Arc<Audio>>,
}

impl Speech {
    /// Speaks with `tts` and an empty cache.
    #[must_use]
    pub fn new(tts: Arc<dyn TextToSpeech>) -> Self {
        Self {
            tts,
            cache: DashMap::new(),
        }
    }

    /// The audio of `text`: the cached audio, else a new synthesis that the cache does not keep.
    ///
    /// # Errors
    ///
    /// The text is not in the cache and the synthesis fails.
    pub async fn speak(&self, text: &str) -> anyhow::Result<Arc<Audio>> {
        if let Some(audio) = self.cache.get(text) {
            return Ok(Arc::clone(&audio));
        }
        Ok(Arc::new(self.tts.synth(text).await?))
    }

    /// Synthesizes and caches each pool response whose only variable is `area`, once for each
    /// area name in `areas`.
    ///
    /// # Errors
    ///
    /// A template does not parse or render, or a synthesis fails.
    pub async fn prerender(&self, pools: &Pools, areas: &[String]) -> anyhow::Result<()> {
        for text in texts(pools, areas)? {
            let audio = self.tts.synth(&text).await?;
            self.cache.insert(text, Arc::new(audio));
        }
        Ok(())
    }
}

/// The text of each pool response whose only variable is `area`, rendered with each area name in
/// `areas` in lowercase, as `decide` renders it.
fn texts(pools: &Pools, areas: &[String]) -> anyhow::Result<BTreeSet<String>> {
    let allowed = BTreeSet::from(["area".to_owned()]);
    let mut texts = BTreeSet::new();
    let templates = pools.values().flat_map(|pool| &pool.responses);
    for template in templates {
        if catalog::unknown_variable(&template.text, &allowed)?.is_some() {
            continue;
        }
        for area in areas {
            let context = Map::from_iter([("area".to_owned(), Value::from(area.to_lowercase()))]);
            texts.insert(responses::render(&template.text, &context)?);
        }
    }
    Ok(texts)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::domain::model::Prediction;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStt;
    use crate::testing::fakes::FakeTts;
    use crate::testing::fakes::log_dispatch;

    /// Commits 200 ms after confident text, 600 ms after other text and at 10 s of audio.
    const POLICY: CommitPolicy = CommitPolicy {
        early_commit_ms: 200,
        endpoint_ms: 600,
        max_utterance_ms: 10_000,
    };

    /// The text of the partials of the tests.
    const TEXT: &str = "turn on the lights";

    /// A partial of `text` with its voice end and its decided audio.
    fn partial(text: &str, voice_end_ms: Option<u64>, vad_ms: u64) -> Partial {
        Partial {
            text: text.to_owned(),
            voice_end_ms,
            vad_ms,
        }
    }

    /// A prediction of `label` with `probability` and no slots.
    fn prediction(label: &str, probability: f32) -> Prediction {
        Prediction {
            ranked: vec![(label.to_owned(), probability)],
            slots: Vec::new(),
        }
    }

    /// A listener on `stt` and `classifier` that warms `reasoner`.
    fn listener(
        stt: FakeStt,
        classifier: &Arc<FakeClassifier>,
        reasoner: &Arc<FakeReasoner>,
    ) -> Listener {
        let classifier = Arc::clone(classifier) as Arc<dyn Classifier>;
        let reasoner = Arc::clone(reasoner) as Arc<dyn Reasoner>;
        Listener::new(Arc::new(stt), classifier, reasoner, POLICY, 0.9)
    }

    /// A session on `stt`, with a classifier that gives `predictions`, and that classifier.
    fn session(
        stt: FakeStt,
        predictions: Vec<Prediction>,
    ) -> anyhow::Result<(Box<dyn Listening>, Arc<FakeClassifier>)> {
        let classifier = Arc::new(FakeClassifier::default());
        for prediction in predictions {
            classifier.script(prediction);
        }
        let listener = listener(stt, &classifier, &Arc::default());
        Ok((listener.listen()?, classifier))
    }

    /// AC-7.1-4.
    #[tokio::test(start_paused = true)]
    async fn ac_7_1_4_listen_warms_the_reasoner_once() -> anyhow::Result<()> {
        let reasoner = Arc::new(FakeReasoner::default());
        let listener = listener(FakeStt::default(), &Arc::default(), &reasoner);
        let _session = listener.listen()?;
        tokio::task::yield_now().await;
        assert_eq!(reasoner.warms(), 1);
        Ok(())
    }

    /// The result of each push of a 20 ms chunk, one push for each of `partials`, and the
    /// classifier.
    async fn pushes(
        partials: Vec<Partial>,
        predictions: Vec<Prediction>,
    ) -> anyhow::Result<(Vec<Option<String>>, Arc<FakeClassifier>)> {
        let count = partials.len();
        let stt = FakeStt {
            partials,
            ..FakeStt::default()
        };
        let (mut session, classifier) = session(stt, predictions)?;
        let mut results = Vec::new();
        for _ in 0..count {
            results.push(session.push(vec![0; 320]).await?);
        }
        Ok((results, classifier))
    }

    /// `count` results with no commit, then the commit of `text`.
    fn commit_after(count: usize, text: &str) -> Vec<Option<String>> {
        let mut results = vec![None; count];
        results.push(Some(text.to_owned()));
        results
    }

    /// AC-6.8-1.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_1_same_partial_predicts_once() -> anyhow::Result<()> {
        let partials = vec![partial(TEXT, Some(20), 20), partial(TEXT, Some(40), 40)];
        let predictions = vec![prediction("lights_on", 0.95), prediction("lights_on", 0.95)];
        let (_results, classifier) = pushes(partials, predictions).await?;
        assert_eq!(classifier.calls().len(), 1, "{:?}", classifier.calls());
        Ok(())
    }

    /// AC-6.8-2.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_2_confident_partial_commits_at_early_commit_ms() -> anyhow::Result<()> {
        let partials = vec![
            partial(TEXT, Some(500), 500),
            partial(TEXT, Some(500), 600),
            partial(TEXT, Some(500), 699),
            partial(TEXT, Some(500), 700),
        ];
        let (results, _) = pushes(partials, vec![prediction("lights_on", 0.95)]).await?;
        assert_eq!(results, commit_after(3, TEXT));
        Ok(())
    }

    /// AC-6.8-4.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_4_no_second_pass_returns_first_pass_text() -> anyhow::Result<()> {
        let partials = vec![partial(TEXT, Some(500), 500), partial(TEXT, Some(500), 700)];
        let (results, _) = pushes(partials, vec![prediction("lights_on", 0.95)]).await?;
        assert_eq!(results, commit_after(1, TEXT));
        Ok(())
    }

    /// The commit of a confident `TEXT` after 200 ms of silence, with the second pass
    /// `second_pass`.
    async fn commit_with(
        second_pass: Result<Option<String>, String>,
    ) -> anyhow::Result<Option<String>> {
        let stt = FakeStt {
            partials: vec![partial(TEXT, Some(500), 500), partial(TEXT, Some(500), 700)],
            second_pass,
            ..FakeStt::default()
        };
        let (mut session, _) = session(stt, vec![prediction("lights_on", 0.95)])?;
        session.push(vec![0; 320]).await?;
        session.push(vec![0; 320]).await
    }

    /// AC-6.8b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8b_1_commit_gives_the_second_pass_text() -> anyhow::Result<()> {
        let second_pass = "turn on the kitchen lights".to_owned();
        let transcript = commit_with(Ok(Some(second_pass.clone()))).await?;
        assert_eq!(transcript, Some(second_pass));
        Ok(())
    }

    /// AC-6.8b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8b_2_second_pass_error_gives_the_first_pass_text() -> anyhow::Result<()> {
        let transcript = commit_with(Err("no result".to_owned())).await?;
        assert_eq!(transcript, Some(TEXT.to_owned()));
        Ok(())
    }

    /// AC-6.11-3.
    #[tokio::test(start_paused = true)]
    async fn ac_6_11_3_transcript_logs_one_info_event() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        let second_pass = "turn on the kitchen lights".to_owned();
        commit_with(Ok(Some(second_pass.clone())))
            .with_subscriber(log_dispatch(&log)?)
            .await?;
        let text = fs::read_to_string(log.path())?;
        let events: Vec<&str> = text
            .lines()
            .filter(|line| line.contains(" INFO "))
            .collect();
        assert_eq!(events.len(), 1, "the log is {text:?}");
        for part in [second_pass.as_str(), "second"] {
            assert!(
                events[0].contains(part),
                "{part:?} is not in {:?}",
                events[0]
            );
        }
        Ok(())
    }

    /// AC-6.8-6.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_6_finish_before_commit_returns_transcript_at_once() -> anyhow::Result<()> {
        let stt = FakeStt {
            partials: vec![partial(TEXT, Some(500), 500)],
            ..FakeStt::default()
        };
        let (mut session, _) = session(stt, vec![prediction("lights_on", 0.95)])?;
        assert_eq!(session.push(vec![0; 320]).await?, None);
        let start = tokio::time::Instant::now();
        assert_eq!(session.finish().await?, TEXT);
        assert_eq!(tokio::time::Instant::now(), start, "finish waited");
        Ok(())
    }

    /// AC-6.8-7.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_7_no_voice_for_one_second_does_not_commit() -> anyhow::Result<()> {
        let partials: Vec<Partial> = (1..=50)
            .map(|chunk| partial("", None, chunk * 20))
            .collect();
        let (results, _) = pushes(partials, Vec::new()).await?;
        assert_eq!(results, vec![None; 50]);
        Ok(())
    }

    /// AC-6.8-8.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_8_voice_after_pause_moves_the_commit() -> anyhow::Result<()> {
        let partials = vec![
            partial(TEXT, Some(500), 500),
            partial(TEXT, Some(500), 650),
            partial(TEXT, Some(800), 800),
            partial(TEXT, Some(800), 999),
            partial(TEXT, Some(800), 1000),
        ];
        let (results, _) = pushes(partials, vec![prediction("lights_on", 0.95)]).await?;
        assert_eq!(results, commit_after(4, TEXT));
        Ok(())
    }

    /// AC-6.8-9.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_9_uncertain_change_commits_at_endpoint_ms() -> anyhow::Result<()> {
        let other = "turn on the lights in";
        let partials = vec![
            partial(TEXT, Some(500), 500),
            partial(other, Some(500), 600),
            partial(other, Some(500), 700),
            partial(other, Some(500), 1099),
            partial(other, Some(500), 1100),
        ];
        let predictions = vec![prediction("lights_on", 0.95), prediction("lights_on", 0.5)];
        let (results, _) = pushes(partials, predictions).await?;
        assert_eq!(results, commit_after(4, other));
        Ok(())
    }

    /// AC-6.8-11.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_11_predicts_the_normalized_partial() -> anyhow::Result<()> {
        let partials = vec![partial("Turn on the", Some(20), 20)];
        let (_results, classifier) = pushes(partials, vec![prediction("lights_on", 0.5)]).await?;
        let context = training::context_text(None, &[]);
        assert_eq!(classifier.calls(), [(context, "turn on the".to_owned())]);
        Ok(())
    }

    /// AC-6.8-12.
    #[tokio::test(start_paused = true)]
    async fn ac_6_8_12_prediction_error_commits_at_endpoint_ms() -> anyhow::Result<()> {
        let partials = vec![
            partial(TEXT, Some(500), 500),
            partial(TEXT, Some(500), 700),
            partial(TEXT, Some(500), 1099),
            partial(TEXT, Some(500), 1100),
        ];
        let (results, _) = pushes(partials, Vec::new()).await?;
        assert_eq!(results, commit_after(3, TEXT));
        Ok(())
    }

    /// A use case over a new `FakeTts`, and that fake.
    fn speech() -> (Speech, Arc<FakeTts>) {
        let tts = Arc::new(FakeTts::default());
        (Speech::new(Arc::clone(&tts) as Arc<dyn TextToSpeech>), tts)
    }

    /// The pool `lights_on` with an `area` template and a template with no variable, and the
    /// pool `temperature_query` with an `entity` template.
    fn pools() -> Pools {
        let pool = |texts: &[&str]| Pool {
            responses: texts
                .iter()
                .map(|text| Response {
                    text: (*text).to_owned(),
                    uses: 0,
                    generated: false,
                })
                .collect(),
            last: None,
        };
        let lights = pool(&["Powering up the {{ area }} lights.", "One moment."]);
        let temperature = pool(&["It is {{ entity.state }} degrees in the {{ area }}."]);
        Pools::from([
            ("lights_on".to_owned(), lights),
            ("temperature_query".to_owned(), temperature),
        ])
    }

    /// Pre-renders `pools()` for the Living Room and the Office.
    async fn prerender(speech: &Speech) -> anyhow::Result<()> {
        let areas = ["Living Room".to_owned(), "Office".to_owned()];
        speech.prerender(&pools(), &areas).await
    }

    /// AC-6.3b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_1_hit_returns_cached_audio_without_synth() -> anyhow::Result<()> {
        let (speech, tts) = speech();
        let audio = Arc::new(Audio {
            samples: vec![1, 2, 3],
            rate: 16_000,
        });
        speech
            .cache
            .insert("One moment.".to_owned(), Arc::clone(&audio));
        assert_eq!(speech.speak("One moment.").await?, audio);
        assert_eq!(tts.texts(), Vec::<String>::new(), "a hit called synth");
        Ok(())
    }

    /// AC-6.3b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_2_prerender_caches_each_area_text() -> anyhow::Result<()> {
        let (speech, _tts) = speech();
        prerender(&speech).await?;
        let keys: BTreeSet<String> = speech
            .cache
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let expected = [
            "One moment.",
            "Powering up the living room lights.",
            "Powering up the office lights.",
        ];
        assert_eq!(keys, BTreeSet::from(expected.map(str::to_owned)));
        Ok(())
    }

    /// AC-6.3b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_3_prerender_skips_other_variables() -> anyhow::Result<()> {
        let (speech, tts) = speech();
        prerender(&speech).await?;
        let texts = tts.texts();
        assert!(
            !texts.iter().any(|text| text.contains("degrees")),
            "an entity template was synthesized: {texts:?}"
        );
        Ok(())
    }

    /// AC-6.3b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_5_miss_synthesizes_without_store() -> anyhow::Result<()> {
        let (speech, tts) = speech();
        prerender(&speech).await?;
        let (size, calls) = (speech.cache.len(), tts.texts().len());
        speech.speak("It is 21 degrees in the office.").await?;
        assert_eq!(tts.texts()[calls..], ["It is 21 degrees in the office."]);
        assert_eq!(speech.cache.len(), size, "a miss changed the cache size");
        Ok(())
    }
}
