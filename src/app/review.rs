//! The review use case: the logged interactions, their audio and their marks.

use std::mem;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::sync::watch;

use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::feedback::Feedback;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::feedback::Rated;
use crate::domain::speech::Audio;
use crate::ports::Filter;
use crate::ports::InteractionLog;
use crate::ports::Page;
use crate::ports::Review;

/// The review of the logged interactions. A mark reaches the log and the feedback model.
pub struct Reviewer {
    /// The log of the interactions.
    log: Arc<dyn InteractionLog>,
    /// The names that a correction can have: `unknown` and the intent names.
    names: Vec<String>,
    /// The feedback model.
    feedback: watch::Sender<Feedback>,
    /// The turn to mark. The log and the feedback model then get the marks in the same order.
    marking: Mutex<()>,
}

impl Reviewer {
    /// The review of `log` with the intent names of `catalog` that applies the marks to
    /// `feedback`.
    #[must_use]
    pub fn new(
        log: Arc<dyn InteractionLog>,
        catalog: &Catalog,
        feedback: watch::Sender<Feedback>,
    ) -> Self {
        Self {
            log,
            names: catalog::labels(catalog),
            feedback,
            marking: Mutex::new(()),
        }
    }

    /// The parts of the interaction `id` that its marks rate, with the transcript of the
    /// interaction that its link names.
    async fn rated(&self, id: u64) -> anyhow::Result<Rated> {
        let Some(interaction) = self.log.get(id).await? else {
            anyhow::bail!("no interaction has the ID {id}");
        };
        let link = match interaction.link {
            Some(first) => self.log.get(first).await?.map(|first| first.transcript),
            None => None,
        };
        Ok(Rated {
            transcript: interaction.transcript,
            link,
            action: interaction.action,
            responses: interaction.responses,
        })
    }
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
        let rated = self.rated(mark.interaction).await?;
        let _turn = self.marking.lock().await;
        self.log.mark(mark.clone()).await?;
        self.feedback
            .send_modify(|feedback| *feedback = mem::take(feedback).apply(rated, mark));
        Ok(())
    }

    fn intents(&self) -> Vec<String> {
        self.names.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use jiff::Timestamp;

    use super::*;
    use crate::domain::feedback::Action;
    use crate::domain::feedback::Route;
    use crate::domain::feedback::Target;
    use crate::domain::feedback::Value;
    use crate::testing::fakes::FakeLog;

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
    /// office" (2), the log, and the receiver of the feedback model.
    fn review() -> (Reviewer, Arc<FakeLog>, watch::Receiver<Feedback>) {
        let logged = vec![
            (interaction(1, "turn on the tv", None), None),
            (interaction(2, "the office", Some(1)), None),
        ];
        let log = Arc::new(FakeLog {
            interactions: Mutex::new(logged),
            ..FakeLog::default()
        });
        let (feedback, receiver) = watch::channel(Feedback::default());
        let review = Reviewer::new(
            Arc::clone(&log) as Arc<dyn InteractionLog>,
            &catalog(),
            feedback,
        );
        (review, log, receiver)
    }

    /// The action mark `value` with `correction` on the dialog answer.
    fn mark(value: Option<Value>, correction: &str) -> Mark {
        Mark {
            interaction: 2,
            target: Target::Action,
            value,
            correction: Some(correction.to_owned()),
            time: Timestamp::UNIX_EPOCH,
        }
    }

    /// The marks of the dialog answer in the log, and the marks of the feedback model.
    async fn marks(log: &FakeLog, feedback: &watch::Receiver<Feedback>) -> (Vec<Mark>, Vec<Mark>) {
        let answer = log
            .get(2)
            .await
            .expect("the fake log reads")
            .expect("it has the answer");
        let applied = feedback
            .borrow()
            .marks
            .iter()
            .map(|marked| marked.mark.clone())
            .collect();
        (answer.marks, applied)
    }

    /// AC-12.7-1.
    #[tokio::test]
    async fn ac_12_7_1_mark_reaches_the_log_and_the_feedback_model() -> anyhow::Result<()> {
        let (review, log, feedback) = review();
        let bad = mark(Some(Value::Bad), "tv_show");
        review.mark(bad.clone()).await?;
        assert_eq!(marks(&log, &feedback).await, (vec![bad.clone()], vec![bad]));
        let targets = [OFFICE.to_owned()];
        assert!(
            feedback
                .borrow()
                .blocked("turn on the tv", "tv_on", &targets),
            "the answer mark counts for the text of the first interaction"
        );
        Ok(())
    }

    /// AC-12.7-2.
    #[tokio::test]
    async fn ac_12_7_2_none_removes_the_earlier_mark() -> anyhow::Result<()> {
        let (review, log, feedback) = review();
        review.mark(mark(Some(Value::Bad), "tv_show")).await?;
        assert_eq!(feedback.borrow().marks.len(), 1, "the bad mark is applied");
        review.mark(mark(None, "tv_show")).await?;
        assert_eq!(marks(&log, &feedback).await, (Vec::new(), Vec::new()));
        Ok(())
    }

    /// AC-12.7-3.
    #[tokio::test]
    async fn ac_12_7_3_unknown_correction_changes_nothing() {
        let (review, log, feedback) = review();
        let result = review.mark(mark(Some(Value::Bad), "tv_shows")).await;
        assert!(
            result.is_err(),
            "tv_shows is not an intent name: {result:?}"
        );
        assert_eq!(marks(&log, &feedback).await, (Vec::new(), Vec::new()));
    }

    /// AC-12.7-4.
    #[test]
    fn ac_12_7_4_intents_are_the_catalog_names_and_unknown() {
        let (review, _log, _feedback) = review();
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
}
