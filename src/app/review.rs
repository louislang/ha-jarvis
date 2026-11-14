//! The review use case: the logged interactions, their audio and their marks.

use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::speech::Audio;
use crate::ports::Filter;
use crate::ports::InteractionLog;
use crate::ports::Page;
use crate::ports::Review;

/// The review of the logged interactions. A mark reaches the log.
pub struct Reviewer {
    /// The log of the interactions.
    log: Arc<dyn InteractionLog>,
    /// The names that a correction can have: `unknown` and the intent names.
    names: Vec<String>,
}

impl Reviewer {
    /// The review of `log` with the intent names of `catalog`.
    #[must_use]
    pub fn new(log: Arc<dyn InteractionLog>, catalog: &Catalog) -> Self {
        Self {
            log,
            names: catalog::labels(catalog),
        }
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
        let id = mark.interaction;
        if self.log.get(id).await?.is_none() {
            anyhow::bail!("no interaction has the ID {id}");
        }
        self.log.mark(mark).await
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
        let review = Reviewer::new(Arc::clone(&log) as Arc<dyn InteractionLog>, &catalog());
        (review, log)
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
}
