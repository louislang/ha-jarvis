//! Generated examples, training trigger, training run, accept and swap.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use jiff::Timestamp;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::watch;
use xxhash_rust::xxh3;

use crate::Config;
use crate::TrainConfig;
use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::feedback::Interaction;
use crate::domain::intent;
use crate::domain::state_cache::StateCache;
use crate::domain::training;
use crate::domain::training::Example;
use crate::domain::training::Settings;
use crate::domain::training::TrainingSet;
use crate::ports::Clock;
use crate::ports::InteractionLog;
use crate::ports::Reasoner;
use crate::ports::Store;
use crate::ports::Trainer;

/// The generated examples of `train.generated`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Generated {
    /// The examples of each intent, by name.
    pub intents: BTreeMap<String, Paraphrases>,
    /// The examples of `unknown`.
    pub unknown: Paraphrases,
}

/// The valid generated examples of one label, with the hash of the input that gave them.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Paraphrases {
    /// The hash of the seeds of the intent, or of the intent names for `unknown`.
    pub hash: u64,
    /// The examples in `[value](slot)` markup.
    pub examples: Vec<String>,
}

/// `generated` with new examples for each intent of `catalog` whose seeds changed, and for
/// `unknown` when the intent names changed, without the intents that `catalog` does not have.
/// Saves the result to `store`. A failed `paraphrase` keeps the old examples of its label.
///
/// # Errors
///
/// The save fails.
pub async fn refresh(
    reasoner: &dyn Reasoner,
    store: &dyn Store<Generated>,
    catalog: &Catalog,
    generated: Generated,
    train: &TrainConfig,
) -> anyhow::Result<Generated> {
    let checker = Checker {
        reasoner,
        seeds: seed_texts(catalog),
    };
    let intents = checker.intents(catalog, generated.intents, train).await;
    let unknown = checker.unknown(catalog, generated.unknown, train).await;
    let generated = Generated { intents, unknown };
    store.save(&generated)?;
    Ok(generated)
}

/// One `paraphrase` request for a label.
struct Request<'a> {
    /// The label.
    target: &'a str,
    /// The unheld seed examples of each intent in the prompt.
    seeds: &'a BTreeMap<String, Vec<String>>,
    /// The slots of the label.
    slots: &'a BTreeSet<String>,
    /// The number of examples.
    n: usize,
    /// The hash of the input of the label.
    hash: u64,
}

/// Asks System Two for examples and keeps the valid ones.
struct Checker<'a> {
    /// System Two, which writes the examples.
    reasoner: &'a dyn Reasoner,
    /// The normalized texts of all seeds, held-out ones included.
    seeds: BTreeSet<String>,
}

impl Checker<'_> {
    /// The examples of each intent of `catalog`, refreshed from `old`.
    async fn intents(
        &self,
        catalog: &Catalog,
        mut old: BTreeMap<String, Paraphrases>,
        train: &TrainConfig,
    ) -> BTreeMap<String, Paraphrases> {
        let mut intents = BTreeMap::new();
        for intent in &catalog.intents {
            let unheld = intent::unheld(&intent.examples, train.holdout);
            let mut slots = BTreeSet::new();
            catalog::insert_slots(intent, &mut slots);
            let request = Request {
                target: &intent.name,
                seeds: &BTreeMap::from([(intent.name.clone(), unheld)]),
                slots: &slots,
                n: train.paraphrases,
                hash: hash(&intent.examples),
            };
            let stored = old.remove(&intent.name).unwrap_or_default();
            intents.insert(intent.name.clone(), self.refreshed(stored, request).await);
        }
        intents
    }

    /// The examples of `unknown`, refreshed from `old`.
    async fn unknown(
        &self,
        catalog: &Catalog,
        old: Paraphrases,
        train: &TrainConfig,
    ) -> Paraphrases {
        let mut seeds = BTreeMap::new();
        let mut names = Vec::new();
        for intent in &catalog.intents {
            let unheld = intent::unheld(&intent.examples, train.holdout);
            seeds.insert(intent.name.clone(), unheld);
            names.push(intent.name.clone());
        }
        let request = Request {
            target: "unknown",
            seeds: &seeds,
            slots: &BTreeSet::new(),
            n: train.unknown_paraphrases,
            hash: hash(&names),
        };
        self.refreshed(old, request).await
    }

    /// `old` when its hash is the hash of `request`, else the valid examples of `request`, or
    /// `old` when the call fails.
    async fn refreshed(&self, old: Paraphrases, request: Request<'_>) -> Paraphrases {
        if old.hash == request.hash {
            return old;
        }
        let Request {
            target,
            seeds,
            slots,
            n,
            hash,
        } = request;
        match self.reasoner.paraphrase(target, seeds, slots, n).await {
            Ok(examples) => {
                let valid = |markup: &String| intent::validate_example(markup, slots, &self.seeds);
                let examples = examples.into_iter().filter(|markup| valid(markup).is_ok());
                Paraphrases {
                    hash,
                    examples: examples.collect(),
                }
            }
            Err(error) => {
                tracing::warn!("the paraphrase of {target} failed: {error:#}");
                old
            }
        }
    }
}

/// The ports and settings of a training run.
pub struct Training {
    /// System Two, which writes the generated examples.
    pub reasoner: Arc<dyn Reasoner>,
    /// The generated examples of `train.generated`.
    pub generated: Arc<dyn Store<Generated>>,
    /// The log with the marks.
    pub log: Arc<dyn InteractionLog>,
    /// The local trainer.
    pub trainer: Arc<dyn Trainer>,
    /// The intents.
    pub catalog: Arc<Catalog>,
    /// The hot cache, which gives the area names.
    pub cache: watch::Receiver<StateCache>,
    /// The time of the start of a run.
    pub clock: Arc<dyn Clock>,
    /// The configuration.
    pub config: Config,
}

impl Training {
    /// One run: trains on the examples that are not held out.
    ///
    /// # Errors
    ///
    /// A step fails, or the held-out set is empty.
    pub async fn run(&self) -> anyhow::Result<()> {
        let started = self.clock.now();
        let set = self.training_set(started).await?;
        let holdout = self.config.train.holdout;
        let (held, upload): (Vec<Example>, Vec<Example>) = set
            .examples
            .into_iter()
            .partition(|example| training::held_out(&example.text, holdout));
        anyhow::ensure!(!held.is_empty(), "the held-out set is empty");
        let upload = TrainingSet {
            examples: upload,
            ..set
        };
        let run_id = started.strftime("%Y%m%d-%H%M%S").to_string();
        self.trainer.train(&run_id, &upload).await?;
        Ok(())
    }

    /// The full training set of a run that starts at `started`, after a `refresh`.
    async fn training_set(&self, started: Timestamp) -> anyhow::Result<TrainingSet> {
        let (store, train) = (self.generated.as_ref(), &self.config.train);
        let generated = store.load()?.unwrap_or_default();
        let generated = refresh(&*self.reasoner, store, &self.catalog, generated, train).await?;
        let marked = self.marked().await?;
        let areas: Vec<String> = self
            .cache
            .borrow()
            .areas
            .iter()
            .map(|area| area.name.clone())
            .collect();
        let mut rng = StdRng::seed_from_u64(started.as_second().unsigned_abs());
        let settings = Settings {
            base_model: train.base_model.clone(),
            epochs: train.epochs,
            batch_size: train.batch_size,
        };
        let catalog = generated_catalog(&self.catalog, generated);
        let examples = training::seed_set(&catalog, &areas, &mut rng, settings.clone()).examples;
        let set =
            training::training_set(&self.catalog, examples, &marked, &areas, &mut rng, settings);
        Ok(set)
    }

    /// The marked interactions with the first interaction of each marked dialog answer.
    async fn marked(&self) -> anyhow::Result<Vec<Interaction>> {
        let mut marked = self.log.marked().await?;
        let links: BTreeSet<u64> = marked
            .iter()
            .filter_map(|interaction| interaction.link)
            .collect();
        for id in links {
            if marked.iter().all(|interaction| interaction.id != id)
                && let Some(first) = self.log.get(id).await?
            {
                marked.push(first);
            }
        }
        Ok(marked)
    }
}

/// A copy of `catalog` whose seeds are the examples of `generated`, so that `seed_set` labels
/// them.
fn generated_catalog(catalog: &Catalog, generated: Generated) -> Catalog {
    let mut copy = catalog.clone();
    for intent in &mut copy.intents {
        let examples = generated.intents.get(&intent.name);
        intent.examples = examples
            .map(|found| found.examples.clone())
            .unwrap_or_default();
    }
    copy.unknown_examples = generated.unknown.examples;
    copy
}

/// The normalized texts of the seed examples of `catalog`.
fn seed_texts(catalog: &Catalog) -> BTreeSet<String> {
    let intents = catalog.intents.iter().flat_map(|intent| &intent.examples);
    let markups = intents.chain(&catalog.unknown_examples);
    let examples = markups.filter_map(|markup| intent::parse_example(markup).ok());
    examples
        .map(|example| intent::normalize(&example.text))
        .collect()
}

/// The hash of `texts`, stable across builds.
fn hash(texts: &[String]) -> u64 {
    xxh3::xxh3_64(texts.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::slice;

    use tempfile::TempDir;

    use crate::testing::fakes;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeLog;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::FakeTrainer;

    /// The catalog of `intents.toml`.
    fn catalog() -> Catalog {
        toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog")
    }

    /// The result of `refresh` of `catalog` and `generated` with `reasoner`.
    async fn refreshed(
        reasoner: &FakeReasoner,
        catalog: &Catalog,
        generated: Generated,
    ) -> Generated {
        let config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
        let store = FakeStore::new(None);
        let refreshed = refresh(reasoner, &store, catalog, generated, &config.train).await;
        let refreshed = refreshed.expect("the fake store saves");
        assert_eq!(
            store.saves(),
            slice::from_ref(&refreshed),
            "one save of the result"
        );
        refreshed
    }

    /// The examples of `catalog` that a first `refresh` generates.
    async fn first(catalog: &Catalog) -> Generated {
        let reasoner = FakeReasoner::default();
        let lights = Ok(vec!["turn the [den](area) lights on".to_owned()]);
        reasoner.script_paraphrase("lights_on", lights);
        let generated = refreshed(&reasoner, catalog, Generated::default()).await;
        let targets = reasoner.paraphrased();
        assert!(
            targets.contains(&"lights_on".to_owned()),
            "a first refresh generates lights_on: {targets:?}"
        );
        generated
    }

    /// The `paraphrase` targets of a `refresh` of `changed` after a first `refresh` of `intents.toml`.
    async fn targets(changed: &Catalog) -> Vec<String> {
        let generated = first(&catalog()).await;
        let reasoner = FakeReasoner::default();
        refreshed(&reasoner, changed, generated).await;
        reasoner.paraphrased()
    }

    /// `intents.toml` with a new seed example of `lights_on`.
    fn new_lights_seed() -> Catalog {
        let mut catalog = catalog();
        let lights = catalog
            .intents
            .iter_mut()
            .find(|intent| intent.name == "lights_on");
        let lights = lights.expect("intents.toml has lights_on");
        lights.examples.push("lights on please".to_owned());
        catalog
    }

    #[tokio::test]
    async fn ac_13_6_1_unchanged_seeds_give_no_call() {
        let targets = targets(&catalog()).await;

        assert!(
            !targets.contains(&"lights_on".to_owned()),
            "no paraphrase call for lights_on: {targets:?}"
        );
    }

    #[tokio::test]
    async fn ac_13_6_2_changed_seeds_give_one_call() {
        let targets = targets(&new_lights_seed()).await;

        assert_eq!(targets, ["lights_on"]);
    }

    #[tokio::test]
    async fn ac_13_6_3_changed_intent_list_regenerates_unknown() {
        let mut changed = catalog();
        changed.intents.pop();

        let targets = targets(&changed).await;

        assert_eq!(targets, ["unknown"]);
    }

    #[tokio::test]
    async fn ac_13_6_4_failed_paraphrase_keeps_examples() {
        let generated = first(&catalog()).await;
        let before = generated.intents["lights_on"].clone();
        let reasoner = FakeReasoner::default();
        reasoner.script_paraphrase("lights_on", Err("the request failed"));

        let refreshed = refreshed(&reasoner, &new_lights_seed(), generated).await;

        assert_eq!(before.examples, ["turn the [den](area) lights on"]);
        assert_eq!(refreshed.intents["lights_on"], before);
    }

    #[tokio::test]
    async fn ac_13_6_5_removed_intent_loses_examples() {
        let generated = first(&catalog()).await;
        let mut changed = catalog();
        changed.intents.retain(|intent| intent.name != "lights_on");

        let refreshed = refreshed(&FakeReasoner::default(), &changed, generated).await;

        assert!(
            !refreshed.intents.contains_key("lights_on"),
            "lights_on is gone: {:?}",
            refreshed.intents.keys()
        );
    }

    #[tokio::test]
    async fn unknown_seed_with_capitals_rejects_its_copy() {
        let mut catalog = catalog();
        catalog.unknown_examples = vec!["Tell me about Rome!".to_owned()];
        let reasoner = FakeReasoner::default();
        let examples = ["tell me about rome", "what is the capital of peru"];
        reasoner.script_paraphrase("unknown", Ok(examples.map(str::to_owned).to_vec()));

        let refreshed = refreshed(&reasoner, &catalog, Generated::default()).await;

        assert_eq!(refreshed.unknown.examples, ["what is the capital of peru"]);
    }

    /// A training run on `intents.toml` and the reference house with fakes, with the held-out
    /// share `holdout`, and its trainer, which creates the model folders in `models`.
    fn harness(holdout: f64, models: &TempDir) -> (Training, Arc<FakeTrainer>) {
        let mut config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
        config.train.holdout = holdout;
        let dir = models.path().to_owned();
        let trainer = Arc::new(FakeTrainer {
            dir,
            ..FakeTrainer::default()
        });
        let training = Training {
            reasoner: Arc::new(FakeReasoner::default()),
            generated: Arc::new(FakeStore::new(None)),
            log: Arc::new(FakeLog::default()),
            trainer: Arc::clone(&trainer) as _,
            catalog: Arc::new(catalog()),
            cache: watch::channel(StateCache::load(fakes::house())).1,
            clock: Arc::new(FakeClock(Timestamp::UNIX_EPOCH)),
            config,
        };
        (training, trainer)
    }

    /// AC-13.9-4.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9_4_upload_has_no_held_out_example() -> anyhow::Result<()> {
        let models = tempfile::tempdir()?;
        let (training, trainer) = harness(0.1, &models);
        trainer
            .results
            .lock()
            .expect("no test panics")
            .push_back(Ok(()));
        training.run().await?;
        let calls = trainer.calls.lock().expect("no test panics");
        let examples = &calls[0].1.examples;
        let held = examples
            .iter()
            .filter(|example| training::held_out(&example.text, 0.1));
        assert_eq!(held.count(), 0, "the upload has a held-out example");
        assert!(!examples.is_empty(), "the upload is empty");
        Ok(())
    }

    /// AC-13.9-13.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9_13_empty_held_out_set_stops_before_training() -> anyhow::Result<()> {
        let models = tempfile::tempdir()?;
        let (training, trainer) = harness(0.0, &models);
        let error = training.run().await.expect_err("the run stops");
        assert!(error.to_string().contains("held-out"), "{error}");
        let calls = trainer.calls.lock().expect("no test panics");
        assert!(calls.is_empty(), "the trainer has a call");
        Ok(())
    }
}
