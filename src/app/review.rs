//! The review use case: the logged interactions, their audio, their marks and their speakers,
//! and the pending learned entries.

use std::sync::Arc;

use async_trait::async_trait;
use futures::channel::oneshot;

use crate::app::background::Activation;
use crate::app::conversation::Change;
use crate::domain::catalog;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::learn::Learned;
use crate::domain::learn::LearnedIntent;
use crate::domain::learn::LearnedStatus;
use crate::domain::speaker;
use crate::domain::speaker::Person;
use crate::domain::speaker::Source;
use crate::domain::speech::Audio;
use crate::domain::state::EntityId;
use crate::ports::Filter;
use crate::ports::InteractionLog;
use crate::ports::Page;
use crate::ports::Review;
use crate::ports::Store;
use crate::ports::Unassigned;
use crate::ports::Unresolved;
use crate::ports::VoiceProfiles;

/// The review of the logged interactions and the learned entries. A mark reaches the log, a
/// resolve reaches the learned intents file through the pools task, and an assign reaches the
/// voice profiles and the log.
pub struct Reviewer {
    /// The log of the interactions.
    log: Arc<dyn InteractionLog>,
    /// The activation, with the learned intents file, its pools task, the hot cache and the live
    /// catalog.
    activation: Activation,
    /// The voice profiles of the people.
    voices: Arc<dyn VoiceProfiles>,
    /// Held from the profile write to the end of the log update of an assign.
    assigning: tokio::sync::Mutex<()>,
}

impl Reviewer {
    /// The review of `log`, of the learned intents file of `activation` and of `voices`.
    #[must_use]
    pub fn new(
        log: Arc<dyn InteractionLog>,
        activation: Activation,
        voices: Arc<dyn VoiceProfiles>,
    ) -> Self {
        Self {
            log,
            activation,
            voices,
            assigning: tokio::sync::Mutex::default(),
        }
    }

    /// The result of `task` on the learned intents file, run by the pools task in order. Fails when
    /// the pools task stopped.
    async fn run<T: Send + 'static>(
        &self,
        task: impl FnOnce(&dyn Store<Learned>) -> T + Send + 'static,
    ) -> Result<T, Unresolved> {
        let (sender, receiver) = oneshot::channel();
        let store = Arc::clone(&self.activation.store);
        let change: Change = Box::new(move |pools| {
            // The web request can end before the change runs.
            let _ = sender.send(task(store.as_ref()));
            pools
        });
        let stopped = || Unresolved::Failed(anyhow::anyhow!("the pools task stopped"));
        let changes = &self.activation.changes;
        changes.send(change).map_err(|_| stopped())?;
        receiver.await.map_err(|_| stopped())
    }

    /// With `approve`, the `pending` entry `name` as `active`, after the pre-render of its pool. A
    /// failed pre-render fails before the save, so the entry stays `pending` for another approve.
    async fn prepared(&self, name: &str, approve: bool) -> Result<Vec<LearnedIntent>, Unresolved> {
        let mut pending = self.proposals().map_err(Unresolved::Failed)?;
        pending.retain(|entry| approve && entry.name == name);
        for entry in &mut pending {
            entry.status = LearnedStatus::Active;
            let added = self.activation.add_pool(entry).await;
            added.map_err(Unresolved::Failed)?;
        }
        Ok(pending)
    }
}

/// The `pending` entries of `learned`, in file order.
#[must_use]
pub fn pending(learned: Learned) -> Vec<LearnedIntent> {
    let mut intents = learned.intents;
    intents.retain(|entry| entry.status == LearnedStatus::Pending);
    intents
}

/// Sets the `pending` entry `name` of `learned` to `active` with `approve`, else to `rejected`.
///
/// # Errors
///
/// No `pending` entry has the name. `learned` does not change.
pub fn resolve(learned: &mut Learned, name: &str, approve: bool) -> Result<(), Unresolved> {
    let mut entries = learned.intents.iter_mut();
    let entry = entries
        .find(|entry| entry.name == name)
        .ok_or(Unresolved::NotPending)?;
    entry.status = match (entry.status, approve) {
        (LearnedStatus::Pending, true) => LearnedStatus::Active,
        (LearnedStatus::Pending, false) => LearnedStatus::Rejected,
        _ => return Err(Unresolved::NotPending),
    };
    Ok(())
}

/// Resolves the entry `name` of `store` with `approve` and saves the file.
///
/// # Errors
///
/// [`resolve`] fails, or `store` cannot load or save.
fn resolve_saved(store: &dyn Store<Learned>, name: &str, approve: bool) -> Result<(), Unresolved> {
    let loaded = store.load().map_err(Unresolved::Failed)?;
    let mut learned = loaded.unwrap_or_default();
    resolve(&mut learned, name, approve)?;
    store.save(&learned).map_err(Unresolved::Failed)
}

/// Checks that the correction of `mark`, if any, is one of `names`.
///
/// # Errors
///
/// The correction is not in `names`.
pub fn check(mark: &Mark, names: &[String]) -> anyhow::Result<()> {
    match &mark.correction {
        Some(correction) if !names.contains(correction) => {
            anyhow::bail!("the correction {correction} is not an intent name")
        }
        _ => Ok(()),
    }
}

/// Checks that `person`, if any, is the entity of one of `people`.
///
/// # Errors
///
/// `person` is not one of `people`.
pub fn check_person(people: &[Person], person: Option<&EntityId>) -> Result<(), Unassigned> {
    match person {
        Some(person) if !people.iter().any(|known| known.entity == *person) => {
            Err(Unassigned::NotPerson)
        }
        _ => Ok(()),
    }
}

#[async_trait]
impl Review for Reviewer {
    async fn list(&self, filter: Filter, page: Page) -> anyhow::Result<Vec<Interaction>> {
        self.log.list(filter, page).await
    }

    async fn audio(&self, id: u64) -> anyhow::Result<Option<Audio>> {
        self.log.audio(id).await
    }

    async fn mark(&self, mark: Mark) -> anyhow::Result<()> {
        check(&mark, &self.intents())?;
        let id = mark.interaction;
        if self.log.get(id).await?.is_none() {
            anyhow::bail!("no interaction has the ID {id}");
        }
        self.log.mark(mark).await
    }

    fn intents(&self) -> Vec<String> {
        catalog::labels(&self.activation.catalog.load())
    }

    fn proposals(&self) -> anyhow::Result<Vec<LearnedIntent>> {
        Ok(pending(self.activation.store.load()?.unwrap_or_default()))
    }

    async fn resolve(&self, name: &str, approve: bool) -> Result<(), Unresolved> {
        let approved = self.prepared(name, approve).await?;
        let name = name.to_owned();
        self.run(move |store| resolve_saved(store, &name, approve))
            .await??;
        for entry in approved {
            self.activation.activate(entry).await;
        }
        // After the activation changes, `intents` has the approved entry.
        self.run(|_| ()).await
    }

    fn people(&self) -> Vec<Person> {
        speaker::people(&self.activation.cache.borrow())
    }

    async fn assign(&self, id: u64, person: Option<EntityId>) -> Result<(), Unassigned> {
        check_person(&self.people(), person.as_ref())?;
        let embedding = self.log.embedding(id).await.map_err(Unassigned::Failed)?;
        let embedding = embedding.ok_or(Unassigned::NotFound)?;
        let _assigning = self.assigning.lock().await;
        let assigned = self.voices.assign(id, person.as_ref(), &embedding).await;
        assigned.map_err(Unassigned::Failed)?;
        let change = crate::ports::Change::Speaker {
            speaker: person,
            source: Some(Source::Assigned),
            embedding: None,
        };
        self.log
            .update(id, change)
            .await
            .map_err(Unassigned::Failed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::thread;

    use arc_swap::ArcSwap;
    use jiff::Timestamp;
    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;
    use tokio::runtime::Runtime;
    use tokio::sync::watch;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::app::speech::Speech;
    use crate::domain::catalog::Catalog;
    use crate::domain::feedback::Action;
    use crate::domain::feedback::Route;
    use crate::domain::feedback::Value;
    use crate::domain::learn;
    use crate::domain::learn::Cluster;
    use crate::domain::responses::Pools;
    use crate::domain::speaker::PersonSignals;
    use crate::domain::speaker::Signals;
    use crate::domain::speaker::Speaker;
    use crate::domain::state_cache::StateCache;
    use crate::testing::fakes;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeLog;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::FakeTts;
    use crate::testing::fakes::FakeVoiceProfiles;

    /// The target of the action of each interaction.
    const OFFICE: &str = "media_player.office";

    /// The interaction `id` with `transcript`, the link `link` and the action `tv_on` on
    /// `OFFICE`.
    fn interaction(id: u64, transcript: &str, link: Option<u64>) -> Interaction {
        Interaction {
            id,
            time: Timestamp::from_second(id.cast_signed()).expect("a valid second"),
            satellite: None,
            transcript: transcript.to_owned(),
            model_context: String::new(),
            model: "fixture".to_owned(),
            top_labels: Vec::new(),
            slots: Vec::new(),
            action: Action {
                intent: "tv_on".to_owned(),
                targets: vec![OFFICE.to_owned()],
            },
            decision: Route::Reply,
            service_calls: Vec::new(),
            responses: Vec::new(),
            latency_ms: None,
            link,
            marks: Vec::new(),
            speaker: None,
            source: None,
            embedded: false,
        }
    }

    /// The catalog of `intents.toml`.
    fn catalog() -> Catalog {
        toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog")
    }

    /// The activation of `store` on `changes` with the catalog of `intents.toml`, `FakeTts` and
    /// the reference house.
    fn activation(store: Arc<dyn Store<Learned>>, changes: mpsc::Sender<Change>) -> Activation {
        Activation {
            store,
            changes,
            speech: Arc::new(Speech::new(Arc::new(FakeTts::default()))),
            cache: watch::channel(StateCache::load(fakes::house())).1,
            catalog: Arc::new(ArcSwap::from_pointee(catalog())),
            clock: Arc::new(FakeClock(Timestamp::UNIX_EPOCH)),
        }
    }

    /// The review of a log with the request "turn on the tv" (1) and its dialog answer "the
    /// office" (2), and the log.
    fn review() -> (Reviewer, Arc<FakeLog>) {
        let logged = vec![
            (interaction(1, "turn on the tv", None), None),
            (interaction(2, "the office", Some(1)), None),
        ];
        let log = Arc::new(FakeLog {
            interactions: Mutex::new(logged),
            ..FakeLog::default()
        });
        let (learned, changes) = (Arc::new(FakeStore::new(None)), mpsc::channel().0);
        let voices = Arc::new(FakeVoiceProfiles::default());
        let review = Reviewer::new(Arc::clone(&log) as _, activation(learned, changes), voices);
        (review, log)
    }

    /// The review of a file with the entry `entry`, and the file. A thread runs each change in
    /// place of the pools task.
    fn reviewing(entry: LearnedIntent) -> (Reviewer, Arc<FakeStore<Learned>>) {
        let intents = vec![entry];
        let store = Arc::new(FakeStore::new(Some(Learned { intents })));
        let (changes, receiver) = mpsc::channel::<Change>();
        thread::spawn(move || {
            for change in receiver {
                change(Pools::new());
            }
        });
        let (log, voices) = (
            Arc::new(FakeLog::default()),
            Arc::new(FakeVoiceProfiles::default()),
        );
        let review = Reviewer::new(log, activation(Arc::clone(&store) as _, changes), voices);
        (review, store)
    }

    /// The learned entries, the proposals and the intents of a review after the resolve of its
    /// `pending` entry `good_night` with `approve`.
    async fn resolved(approve: bool) -> anyhow::Result<(Learned, Vec<LearnedIntent>, Vec<String>)> {
        let (review, store) = reviewing(fakes::learned("good_night", LearnedStatus::Pending));
        assert_eq!(review.proposals()?.len(), 1, "good_night is pending");
        let result = review.resolve("good_night", approve).await;
        assert!(result.is_ok(), "the resolve gave {result:?}");
        let learned = store.load()?.unwrap_or_default();
        Ok((learned, review.proposals()?, review.intents()))
    }

    /// The mark `value` with `correction` on the dialog answer.
    fn mark(value: Option<Value>, correction: &str) -> Mark {
        Mark {
            interaction: 2,
            value,
            correction: Some(correction.to_owned()),
            time: Timestamp::UNIX_EPOCH,
        }
    }

    /// The marks of the dialog answer in the log.
    async fn marks(log: &FakeLog) -> Vec<Mark> {
        let answer = log
            .get(2)
            .await
            .expect("the fake log reads")
            .expect("it has the answer");
        answer.marks
    }

    /// AC-12.7-1.
    #[tokio::test]
    async fn ac_12_7_1_mark_reaches_the_log() -> anyhow::Result<()> {
        let (review, log) = review();
        let bad = mark(Some(Value::Bad), "tv_show");
        review.mark(bad.clone()).await?;
        assert_eq!(marks(&log).await, [bad]);
        Ok(())
    }

    /// AC-12.7-2.
    #[tokio::test]
    async fn ac_12_7_2_none_removes_the_earlier_mark() -> anyhow::Result<()> {
        let (review, log) = review();
        review.mark(mark(Some(Value::Bad), "tv_show")).await?;
        assert_eq!(marks(&log).await.len(), 1, "the bad mark is logged");
        review.mark(mark(None, "tv_show")).await?;
        assert_eq!(marks(&log).await, []);
        Ok(())
    }

    /// AC-12.7-3.
    #[tokio::test]
    async fn ac_12_7_3_unknown_correction_changes_nothing() {
        let (review, log) = review();
        let result = review.mark(mark(Some(Value::Bad), "tv_shows")).await;
        assert!(
            result.is_err(),
            "tv_shows is not an intent name: {result:?}"
        );
        assert_eq!(marks(&log).await, []);
    }

    /// AC-12.7-4.
    #[test]
    fn ac_12_7_4_intents_are_the_catalog_names_and_unknown() {
        let (review, _log) = review();
        let mut expected: Vec<String> = catalog()
            .intents
            .into_iter()
            .map(|intent| intent.name)
            .collect();
        expected.push("unknown".to_owned());
        let mut intents = review.intents();
        intents.sort();
        expected.sort();
        assert_eq!(intents, expected);
    }

    /// AC-14.6-2.
    #[tokio::test]
    async fn ac_14_6_2_approve_makes_the_entry_active() -> anyhow::Result<()> {
        let (learned, proposals, _) = resolved(true).await?;
        assert_eq!(learned.intents[0].status, LearnedStatus::Active);
        assert_eq!(proposals, []);
        Ok(())
    }

    /// AC-14.6-3. The rejected entry keeps its centroid, so `covering` names it at the next
    /// check (P-14.5b-4).
    #[tokio::test]
    async fn ac_14_6_3_reject_makes_the_entry_rejected_and_covering() -> anyhow::Result<()> {
        let (learned, proposals, _) = resolved(false).await?;
        assert_eq!(learned.intents[0].status, LearnedStatus::Rejected);
        assert_eq!(proposals, []);
        let cluster = Cluster {
            texts: vec!["night night".to_owned()],
            times: Vec::new(),
            centroid: vec![1.0, 0.0],
        };
        let covering = learn::covering(&cluster, &learned.intents, 0.1);
        assert_eq!(covering, Some("good_night"));
        Ok(())
    }

    /// AC-14.7b-3.
    #[tokio::test]
    async fn ac_14_7b_3_approved_entry_is_an_intent() -> anyhow::Result<()> {
        let (_, _, intents) = resolved(true).await?;
        assert!(intents.contains(&"good_night".to_owned()), "{intents:?}");
        Ok(())
    }

    /// An approve whose pre-render fails gives an error, and the entry stays `pending`.
    #[tokio::test]
    async fn failed_activation_keeps_the_entry_pending() -> anyhow::Result<()> {
        let mut entry = fakes::learned("good_night", LearnedStatus::Pending);
        entry.responses = vec!["{{ broken".to_owned()];
        let (review, _) = reviewing(entry);
        let result = review.resolve("good_night", true).await;
        assert!(result.is_err(), "the resolve gave {result:?}");
        assert_eq!(review.proposals()?.len(), 1, "good_night is pending");
        Ok(())
    }

    /// The review of `voices` and of a log with the request "turn on the tv" (1) with an
    /// embedding and the dialog answer "the office" (2) without one, and the log.
    fn assigning(
        voices: FakeVoiceProfiles,
    ) -> (Arc<Reviewer>, Arc<FakeVoiceProfiles>, Arc<FakeLog>) {
        let logged = vec![
            (interaction(1, "turn on the tv", None), None),
            (interaction(2, "the office", Some(1)), None),
        ];
        let log = Arc::new(FakeLog {
            interactions: Mutex::new(logged),
            embeddings: Mutex::new(BTreeMap::from([(1, vec![1.0])])),
            ..FakeLog::default()
        });
        let voices = Arc::new(voices);
        let (learned, changes) = (Arc::new(FakeStore::new(None)), mpsc::channel().0);
        let activation = activation(learned, changes);
        let review = Reviewer::new(Arc::clone(&log) as _, activation, Arc::clone(&voices) as _);
        (Arc::new(review), voices, log)
    }

    /// The `person` entity `name`.
    fn person(name: &str) -> EntityId {
        EntityId(format!("person.{name}"))
    }

    /// The speaker and the source of the request in `log`.
    async fn record(log: &FakeLog) -> (Option<EntityId>, Option<Source>) {
        let request = log.get(1).await.expect("the fake log reads");
        let request = request.expect("it has the request");
        (request.speaker, request.source)
    }

    /// Starts the assign of `person.<name>` to the request on `review`.
    fn start(review: &Arc<Reviewer>, name: &str) -> JoinHandle<Result<(), Unassigned>> {
        let (review, name) = (Arc::clone(review), Some(person(name)));
        tokio::spawn(async move { review.assign(1, name).await })
    }

    /// AC-15.8b-1. The reference house has `person.ali` and `person.louis`.
    #[test]
    fn ac_15_8b_1_people_are_the_person_entities() {
        let (review, _, _) = assigning(FakeVoiceProfiles::default());
        let people = [("ali", "Ali"), ("louis", "Louis")].map(|(entity, name)| Person {
            entity: person(entity),
            name: name.to_owned(),
        });
        assert_eq!(review.people(), people);
    }

    /// AC-15.8b-2, with `person.louis` of the reference house in place of `person.sam`.
    #[tokio::test]
    async fn ac_15_8b_2_assign_moves_the_sample_and_sets_the_speaker() {
        let (review, voices, log) = assigning(FakeVoiceProfiles::default());
        let assigned = review.assign(1, Some(person("louis"))).await;
        assert!(assigned.is_ok(), "the assign gave {assigned:?}");
        let louis = Some("person.louis".to_owned());
        assert_eq!(
            *voices.picks.lock().expect("no panic"),
            BTreeMap::from([(1, louis)])
        );
        let sample = BTreeMap::from([(1, "person.louis".to_owned())]);
        assert_eq!(*voices.samples.lock().expect("no panic"), sample);
        assert_eq!(
            record(&log).await,
            (Some(person("louis")), Some(Source::Assigned))
        );
    }

    /// AC-15.8b-3.
    #[tokio::test]
    async fn ac_15_8b_3_assign_of_none_clears_the_speaker() {
        let (review, voices, log) = assigning(FakeVoiceProfiles::default());
        let assigned = review.assign(1, None).await;
        assert!(assigned.is_ok(), "the assign gave {assigned:?}");
        assert_eq!(
            *voices.picks.lock().expect("no panic"),
            BTreeMap::from([(1, None)])
        );
        assert_eq!(record(&log).await, (None, Some(Source::Assigned)));
    }

    /// AC-15.8b-4. `person.sam` is not a `person` entity of the reference house.
    #[tokio::test]
    async fn ac_15_8b_4_unknown_person_changes_nothing() {
        let (review, voices, log) = assigning(FakeVoiceProfiles::default());
        let assigned = review.assign(1, Some(person("sam"))).await;
        assert!(
            matches!(assigned, Err(Unassigned::NotPerson)),
            "{assigned:?}"
        );
        assert_eq!(*voices.picks.lock().expect("no panic"), BTreeMap::new());
        assert_eq!(*voices.samples.lock().expect("no panic"), BTreeMap::new());
        assert_eq!(record(&log).await, (None, None));
    }

    /// AC-15.8b-5.
    #[tokio::test]
    async fn ac_15_8b_5_no_embedding_is_not_found() {
        let (review, _, _) = assigning(FakeVoiceProfiles::default());
        let assigned = review.assign(2, None).await;
        assert!(
            matches!(assigned, Err(Unassigned::NotFound)),
            "{assigned:?}"
        );
    }

    /// AC-15.8b-8, with `person.ali` and `person.louis` of the reference house. The first assign
    /// waits at the gate after its pick, and the second starts during the wait.
    #[tokio::test]
    async fn ac_15_8b_8_concurrent_assigns_end_on_one_person() -> anyhow::Result<()> {
        let (open, gate) = oneshot::channel();
        let voices = FakeVoiceProfiles {
            gate: Mutex::new(Some(gate)),
            ..FakeVoiceProfiles::default()
        };
        let (review, voices, log) = assigning(voices);
        let first = start(&review, "ali");
        tokio::task::yield_now().await;
        let second = start(&review, "louis");
        tokio::task::yield_now().await;
        let _ = open.send(());
        let results = (first.await?, second.await?);
        assert!(
            results.0.is_ok() && results.1.is_ok(),
            "the assigns gave {results:?}"
        );
        let picks = voices.picks.lock().expect("no panic").clone();
        let (recorded, _) = record(&log).await;
        assert!(recorded.is_some(), "an assign sets the speaker");
        assert_eq!(picks[&1].clone().map(EntityId), recorded);
        Ok(())
    }

    /// The model state of `speaker.qnt` that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct SpeakerModel {
        /// The identified speaker.
        result: Chosen,
        /// The signal that identified `result`.
        source: Tag,
        /// The person and the source of each stored sample, by interaction ID.
        samples: BTreeMap<u64, (String, Tag)>,
        /// The last pick of each interaction ID.
        picks: BTreeMap<u64, Chosen>,
    }

    /// A Quint `Option[PERSON]`.
    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(tag = "tag", content = "value")]
    enum Chosen {
        /// A person.
        Some(String),
        /// No person, with the empty tuple of Quint.
        None(Vec<i64>),
    }

    /// The variant of a Quint sum type value.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Tag {
        /// The variant name.
        tag: String,
    }

    /// The Quint variant of `source`, or `NoSource`.
    fn tag(source: Option<Source>) -> Tag {
        let tag = source.map_or_else(|| "NoSource".to_owned(), |source| format!("{source:?}"));
        Tag { tag }
    }

    /// The model people with the people of the reference house in their place.
    const HOUSE: [(&str, &str); 2] = [("alex", "person.ali"), ("sam", "person.louis")];

    /// The house person of the model person `name`, or the model person of the house person `name`.
    fn swap(name: &str) -> String {
        let pair = HOUSE.iter().find(|pair| pair.0 == name || pair.1 == name);
        let (model, house) = pair.expect("a person of the model or the house");
        if *model == name { house } else { model }.to_string()
    }

    /// The model value of the house person `person`, if any.
    fn chosen(person: Option<&str>) -> Chosen {
        let chosen = person.map(swap);
        chosen.map_or(Chosen::None(Vec::new()), Chosen::Some)
    }

    impl ModelState<SpeakerDriver> for SpeakerModel {
        /// The speaker of the last `identify` of the request, and the samples and the picks of
        /// `FakeVoiceProfiles` with the sources of the driver.
        fn from_driver(driver: &SpeakerDriver) -> anyhow::Result<Self> {
            let speaker = driver.speaker.as_ref();
            let source = |id: &u64| tag(driver.sources.get(id).copied());
            let samples = driver.fakes.1.samples.lock().expect("no panic");
            let picks = driver.fakes.1.picks.lock().expect("no panic");
            Ok(Self {
                result: chosen(speaker.map(|speaker| speaker.person.0.as_str())),
                source: tag(speaker.map(|speaker| speaker.source)),
                samples: samples
                    .iter()
                    .map(|(id, person)| (*id, (swap(person), source(id))))
                    .collect(),
                picks: picks
                    .iter()
                    .map(|(id, person)| (*id, chosen(person.as_deref())))
                    .collect(),
            })
        }
    }

    /// The value of the model level `level`, in tenths.
    fn tenths(level: i64) -> f32 {
        f32::from(u8::try_from(level).expect("a level from 0 to 10")) / 10.0
    }

    /// The nondet pick `name` of `step`.
    fn pick<T: serde::de::DeserializeOwned>(step: &Step, name: &str) -> anyhow::Result<T> {
        let found = step.nondet_picks.get(name);
        let value = found.ok_or_else(|| anyhow::anyhow!("no pick {name}"))?;
        Ok(T::deserialize(value.clone())?)
    }

    /// The signals of the request of the picks of `step`.
    fn signals(step: &Step) -> anyhow::Result<Signals> {
        let at_home: BTreeSet<String> = pick(step, "atHome")?;
        let proximity: BTreeMap<String, i64> = pick(step, "prox")?;
        let voice: BTreeMap<String, i64> = pick(step, "v")?;
        let mut people = Vec::new();
        for (person, level) in &proximity {
            people.push(PersonSignals {
                entity: EntityId(swap(person)),
                proximity: f64::from(tenths(*level)),
                voice: tenths(voice[person]),
            });
        }
        let (known, sensor) = (pick::<bool>(step, "known")?, pick::<bool>(step, "sensor")?);
        let home = at_home.into_iter().map(|person| EntityId(swap(&person)));
        Ok(Signals {
            home: known.then(|| home.collect()),
            people,
            occupied: sensor.then_some(pick(step, "state")?),
        })
    }

    /// Replays `speaker.qnt` against `identify`, `enrolls`, the enroll of `FakeVoiceProfiles` and
    /// `Review::assign`, with the model constants in tenths: `threshold` 0.7, `margin` 0.2,
    /// `enroll_threshold` 0.5, `voice_threshold` 0.6 and `min_profile` 1. Each utterance has
    /// `min_enroll_ms` and an embedding in the log.
    #[derive(Default)]
    struct SpeakerDriver {
        /// The signals of the request, or `None` before the first request.
        signals: Option<Signals>,
        /// The speaker of the last `identify` of the request.
        speaker: Option<Speaker>,
        /// The interaction ID of the request.
        utterance: u64,
        /// The source of each sample, by interaction ID.
        sources: BTreeMap<u64, Source>,
        /// The review of the reference house.
        fakes: Fakes,
    }

    /// The review of the reference house, its voice profiles, its log and the runtime of its calls.
    struct Fakes(Arc<Reviewer>, Arc<FakeVoiceProfiles>, Arc<FakeLog>, Runtime);

    impl Default for Fakes {
        fn default() -> Self {
            let (review, voices, log) = assigning(FakeVoiceProfiles::default());
            Self(review, voices, log, Runtime::new().expect("a runtime"))
        }
    }

    impl SpeakerDriver {
        /// A new request with the signals of the picks of `step`, and its embedding in the log.
        fn set(&mut self, step: &Step) -> anyhow::Result<()> {
            (self.signals, self.speaker) = (Some(signals(step)?), None);
            self.utterance += 1;
            let mut embeddings = self.fakes.2.embeddings.lock().expect("no panic");
            embeddings.insert(self.utterance, vec![1.0]);
            Ok(())
        }

        /// Rule 5 for `person`, then the enroll of the request into `FakeVoiceProfiles`.
        fn enroll(&mut self, person: &str) -> anyhow::Result<()> {
            let (Some(signals), Some(identified)) = (&self.signals, &self.speaker) else {
                anyhow::bail!("an enroll without a speaker");
            };
            let (entity, voices) = (&identified.person, &self.fakes.1);
            anyhow::ensure!(entity.0 == swap(person), "another person");
            let samples = voices.samples.lock().expect("no panic");
            let own = samples.values().filter(|owner| **owner == entity.0);
            let enrolls = speaker::enrolls(signals, identified, 1, own.count(), 1, 1, 0.6);
            drop(samples);
            anyhow::ensure!(enrolls, "no enroll");
            let enroll = voices.enroll(entity, self.utterance, &[1.0]);
            self.fakes.3.block_on(enroll)?;
            self.sources.insert(self.utterance, identified.source);
            Ok(())
        }

        /// `Review::assign` of `chosen` to the request `back` requests before the current one.
        fn assign(&mut self, back: u64, chosen: Chosen) -> anyhow::Result<()> {
            let id = self.utterance - back;
            let person = match chosen {
                Chosen::Some(name) => Some(EntityId(swap(&name))),
                Chosen::None(_) => None,
            };
            let assigned = self.fakes.3.block_on(self.fakes.0.assign(id, person));
            assigned.map_err(|error| anyhow::anyhow!("the assign gave {error:?}"))?;
            self.sources.insert(id, Source::Assigned);
            Ok(())
        }
    }

    impl Driver for SpeakerDriver {
        type State = SpeakerModel;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::default(),
                setSignals => self.set(step)?,
                identify => {
                    let signals = self.signals.as_ref();
                    self.speaker =
                        signals.and_then(|signals| speaker::identify(signals, 0.7, 0.2, 0.5));
                },
                enroll(p: String) => self.enroll(&p)?,
                assignVoice(back: u64, chosen: Chosen) => self.assign(back, chosen)?,
            })
        }
    }

    /// AC-15.8d-2, which also checks AC-15.6d-1. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/speaker.qnt", max_steps = 30, seed = "1")]
    fn ac_15_8d_2_conformance() -> impl Driver {
        SpeakerDriver::default()
    }
}
