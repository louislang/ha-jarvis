//! The review use case: the logged interactions, their audio and their marks, and the pending
//! learned entries.

use std::sync::Arc;
use std::sync::mpsc;

use async_trait::async_trait;
use futures::channel::oneshot;

use crate::app::conversation::Change;
use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::learn::Learned;
use crate::domain::learn::LearnedIntent;
use crate::domain::learn::LearnedStatus;
use crate::domain::speech::Audio;
use crate::ports::Filter;
use crate::ports::InteractionLog;
use crate::ports::Page;
use crate::ports::Review;
use crate::ports::Store;
use crate::ports::Unresolved;

/// The review of the logged interactions and the learned entries. A mark reaches the log, and a
/// resolve reaches the learned intents file through the pools task.
pub struct Reviewer {
    /// The log of the interactions.
    log: Arc<dyn InteractionLog>,
    /// The names that a correction can have: `unknown` and the intent names.
    names: Vec<String>,
    /// The learned intents file.
    learned: Arc<dyn Store<Learned>>,
    /// The sender of the pools task, which writes the file.
    changes: mpsc::Sender<Change>,
}

impl Reviewer {
    /// The review of `log` with the intent names of `catalog`, and of the learned intents file
    /// `learned`, which the pools task of `changes` writes.
    #[must_use]
    pub fn new(
        log: Arc<dyn InteractionLog>,
        catalog: &Catalog,
        learned: Arc<dyn Store<Learned>>,
        changes: mpsc::Sender<Change>,
    ) -> Self {
        Self {
            log,
            names: catalog::labels(catalog),
            learned,
            changes,
        }
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

#[async_trait]
impl Review for Reviewer {
    async fn list(&self, filter: Filter, page: Page) -> anyhow::Result<Vec<Interaction>> {
        self.log.list(filter, page).await
    }

    async fn audio(&self, id: u64) -> anyhow::Result<Option<Audio>> {
        self.log.audio(id).await
    }

    async fn mark(&self, mark: Mark) -> anyhow::Result<()> {
        check(&mark, &self.names)?;
        let id = mark.interaction;
        if self.log.get(id).await?.is_none() {
            anyhow::bail!("no interaction has the ID {id}");
        }
        self.log.mark(mark).await
    }

    fn intents(&self) -> Vec<String> {
        self.names.clone()
    }

    fn proposals(&self) -> anyhow::Result<Vec<LearnedIntent>> {
        Ok(pending(self.learned.load()?.unwrap_or_default()))
    }

    async fn resolve(&self, name: &str, approve: bool) -> Result<(), Unresolved> {
        let (sender, receiver) = oneshot::channel();
        let (store, name) = (Arc::clone(&self.learned), name.to_owned());
        let change: Change = Box::new(move |pools| {
            // The web request can end before the change runs.
            let _ = sender.send(resolve_saved(store.as_ref(), &name, approve));
            pools
        });
        let stopped = || Unresolved::Failed(anyhow::anyhow!("the pools task stopped"));
        self.changes.send(change).map_err(|_| stopped())?;
        receiver.await.map_err(|_| stopped())?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::thread;

    use jiff::Timestamp;

    use super::*;
    use crate::domain::feedback::Action;
    use crate::domain::feedback::Route;
    use crate::domain::feedback::Value;
    use crate::domain::learn;
    use crate::domain::learn::Cluster;
    use crate::domain::responses::Pools;
    use crate::testing::fakes;
    use crate::testing::fakes::FakeLog;
    use crate::testing::fakes::FakeStore;

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
        }
    }

    /// The catalog of `intents.toml`.
    fn catalog() -> Catalog {
        toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog")
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
        let review = Reviewer::new(Arc::clone(&log) as _, &catalog(), learned, changes);
        (review, log)
    }

    /// The learned entries and the proposals of a review after the resolve of its `pending` entry
    /// `good_night` with `approve`. A thread runs each change in place of the pools task.
    async fn resolved(approve: bool) -> anyhow::Result<(Learned, Vec<LearnedIntent>)> {
        let intents = vec![fakes::learned("good_night", LearnedStatus::Pending)];
        let store = Arc::new(FakeStore::new(Some(Learned { intents })));
        let (changes, receiver) = mpsc::channel::<Change>();
        thread::spawn(move || {
            for change in receiver {
                change(Pools::new());
            }
        });
        let log = Arc::new(FakeLog::default());
        let review = Reviewer::new(log, &catalog(), Arc::clone(&store) as _, changes);
        assert_eq!(review.proposals()?.len(), 1, "good_night is pending");
        let result = review.resolve("good_night", approve).await;
        assert!(result.is_ok(), "the resolve gave {result:?}");
        Ok((store.load()?.unwrap_or_default(), review.proposals()?))
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
        let (learned, proposals) = resolved(true).await?;
        assert_eq!(learned.intents[0].status, LearnedStatus::Active);
        assert_eq!(proposals, []);
        Ok(())
    }

    /// AC-14.6-3. The rejected entry keeps its centroid, so `covering` names it at the next
    /// check (P-14.5b-4).
    #[tokio::test]
    async fn ac_14_6_3_reject_makes_the_entry_rejected_and_covering() -> anyhow::Result<()> {
        let (learned, proposals) = resolved(false).await?;
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
}
