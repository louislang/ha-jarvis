//! Generated examples, training trigger, training run, accept and swap.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jiff::SignedDuration;
use jiff::Timestamp;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use tokio::sync::watch;
use xxhash_rust::xxh3;

use crate::Config;
use crate::CurrentClassifier;
use crate::TrainConfig;
use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::feedback::Interaction;
use crate::domain::intent;
use crate::domain::state::EntityId;
use crate::domain::state::ServiceCall;
use crate::domain::state_cache;
use crate::domain::state_cache::StateCache;
use crate::domain::training;
use crate::domain::training::Example;
use crate::domain::training::Metrics;
use crate::domain::training::Settings;
use crate::domain::training::TrainingSet;
use crate::domain::training::TrainingState;
use crate::ports::Classifier;
use crate::ports::Clock;
use crate::ports::Home;
use crate::ports::InteractionLog;
use crate::ports::ModelLoader;
use crate::ports::Models;
use crate::ports::Reasoner;
use crate::ports::Store;
use crate::ports::Trainer;

/// The run name of the fixture model.
pub const FIXTURE: &str = "fixture";

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
    /// The training state of `train.state`.
    pub state: Arc<dyn Store<TrainingState>>,
    /// The log with the marks.
    pub log: Arc<dyn InteractionLog>,
    /// The local trainer.
    pub trainer: Arc<dyn Trainer>,
    /// The `RunPod` trainer, which trains after an error of the local trainer.
    pub runpod: Option<Arc<dyn Trainer>>,
    /// Opens the new model folder.
    pub loader: Arc<dyn ModelLoader>,
    /// The classifier of the conversation.
    pub classifier: Arc<CurrentClassifier>,
    /// The intents.
    pub catalog: Arc<Catalog>,
    /// The hot cache, which gives the area names.
    pub cache: watch::Receiver<StateCache>,
    /// The time of the start of a run.
    pub clock: Arc<dyn Clock>,
    /// The configuration.
    pub config: Config,
    /// The house, which mutes the satellites during a local run.
    pub home: Arc<dyn Home>,
    /// The model sessions of Jarvis, which a local run frees.
    pub models: Arc<dyn Models>,
}

impl Training {
    /// One run: trains on the examples that are not held out, and swaps the classifier when the
    /// new one is never worse on the held-out examples. Saves the start time and the current run.
    ///
    /// # Errors
    ///
    /// A step fails, or the held-out set is empty. The classifier and the state then do not change.
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
        let dir = self.train(&run_id, &upload).await?;
        let new = self.loader.load(&dir, self.config.device).await?;
        let accepted = self.never_worse(new.as_ref(), &held, &run_id).await?;
        self.finish(started, &run_id, &dir, accepted.then_some(new))
    }

    /// The model folder of `set` from the local trainer, or after its error from the `RunPod`
    /// trainer when it is set.
    ///
    /// # Errors
    ///
    /// Both trainers fail, or `restore` fails.
    async fn train(&self, run_id: &str, set: &TrainingSet) -> anyhow::Result<PathBuf> {
        let local = self.paused(run_id, set).await?;
        match (local, &self.runpod) {
            (Err(error), Some(runpod)) => {
                tracing::warn!("the local training failed, so RunPod trains: {error:#}");
                runpod.train(run_id, set).await
            }
            (local, _) => local,
        }
    }

    /// The result of the local trainer for `set`, with the satellites muted and the models
    /// released. Restores the models and unmutes the same satellites after it.
    ///
    /// # Errors
    ///
    /// `restore` fails.
    async fn paused(
        &self,
        run_id: &str,
        set: &TrainingSet,
    ) -> anyhow::Result<anyhow::Result<PathBuf>> {
        let switches = state_cache::mute_switches(&self.cache.borrow());
        for satellite in &switches.missing {
            tracing::warn!("the satellite {} has no mute switch", satellite.0);
        }
        let muted = self.switch("turn_on", switches.off).await;
        self.models.release().await;
        let local = self.trainer.train(run_id, set).await;
        let restored = self.models.restore().await;
        self.switch("turn_off", muted).await;
        restored.map(|()| local)
    }

    /// Calls `switch.<service>` for each of `switches`, and logs each failed call. Gives the
    /// switches whose call succeeded.
    async fn switch(&self, service: &str, switches: Vec<EntityId>) -> Vec<EntityId> {
        let mut done = Vec::new();
        for switch in switches {
            let call = ServiceCall {
                domain: "switch".to_owned(),
                service: service.to_owned(),
                target: vec![switch.0.clone()],
                data: Map::new(),
                return_response: false,
            };
            match self.home.call(call).await {
                Ok(_) => done.push(switch),
                Err(error) => tracing::warn!("switch.{service} of {} failed: {error:#}", switch.0),
            }
        }
        done
    }

    /// Saves the state of the run `run_id` that started at `started` and trained `dir`. Swaps in
    /// `new` and deletes the old model folders when the run gives one, else deletes `dir`.
    fn finish(
        &self,
        started: Timestamp,
        run_id: &str,
        dir: &Path,
        new: Option<Arc<dyn Classifier>>,
    ) -> anyhow::Result<()> {
        let previous = self.state.load()?.unwrap_or_default().current;
        let current = if new.is_some() {
            Some(run_id.to_owned())
        } else {
            fs::remove_dir_all(dir)?;
            previous.clone()
        };
        let last_time = started;
        self.state.save(&TrainingState { current, last_time })?;
        if let Some(new) = new {
            self.classifier.swap(run_id.to_owned(), new);
            let keep = [run_id, previous.as_deref().unwrap_or(FIXTURE)];
            if let Err(error) = prune(&self.config.model.dir, keep) {
                tracing::warn!("the delete of the old model folders failed: {error}");
            }
        }
        Ok(())
    }

    /// True when `new` of the run `run_id` is never worse than the current classifier on `held`.
    /// Logs both measures and the decision.
    async fn never_worse(
        &self,
        new: &dyn Classifier,
        held: &[Example],
        run_id: &str,
    ) -> anyhow::Result<bool> {
        let old = self.classifier.0.load_full();
        let threshold = self.config.model.threshold;
        let new_metrics = measure(new, held, threshold).await?;
        let old_metrics = measure(old.classifier.as_ref(), held, threshold).await?;
        let accepted = training::accept(new_metrics, old_metrics);
        tracing::info!(
            "run {run_id}: new {new_metrics:?}, old {old_metrics:?}, accepted {accepted}"
        );
        Ok(accepted)
    }

    /// The number of action marks after the start of the last run.
    ///
    /// # Errors
    ///
    /// The state or the log cannot be read.
    pub async fn changes(&self) -> anyhow::Result<usize> {
        let state = self.state.load()?.unwrap_or_default();
        Ok(training::changes(&state, &self.log.marked().await?))
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

/// Starts training runs, one at a time.
pub struct Trigger {
    /// The run.
    training: Arc<Training>,
    /// The wait after a failed run before the next automatic run.
    backoff: SignedDuration,
    /// True while a run is active.
    active: AtomicBool,
    /// The end time of the last failed run.
    failed: Mutex<Option<Timestamp>>,
}

impl Trigger {
    /// A trigger of `training` with no active run and no failed run.
    #[must_use]
    pub fn new(training: Arc<Training>, backoff: SignedDuration) -> Self {
        Self {
            training,
            backoff,
            active: AtomicBool::new(false),
            failed: Mutex::new(None),
        }
    }

    /// Starts a run after the failure backoff when the change count reaches `min_changes`.
    ///
    /// # Errors
    ///
    /// The change count cannot be read.
    pub async fn check(self: &Arc<Self>) -> anyhow::Result<()> {
        // A run that ends while the check reads the count cannot let a second run start.
        if !self.reserve() {
            return Ok(());
        }
        let (failed, min) = (*self.failed(), self.training.config.train.min_changes);
        let now = self.training.clock.now();
        let needed = if failed.is_some_and(|end| now.duration_since(end) < self.backoff) {
            Ok(false)
        } else {
            let changes = self.training.changes().await;
            changes.map(|count| training::should_train(count, min))
        };
        if matches!(needed, Ok(true)) {
            self.start();
        } else {
            self.active.store(false, Ordering::SeqCst);
        }
        needed.map(drop)
    }

    /// Starts a run at any change count and without the backoff.
    pub fn voice(self: &Arc<Self>) {
        if self.reserve() {
            self.start();
        }
    }

    /// True when `active` changes from false to true, the `run = Idle` guard of `training.qnt`.
    fn reserve(&self) -> bool {
        let swapped = self
            .active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
        swapped.is_ok()
    }

    /// Runs `Training::run` in a new task after `reserve`. Keeps the end time of a failed run.
    fn start(self: &Arc<Self>) {
        let trigger = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = trigger.training.run().await {
                tracing::warn!("the training run failed: {error:#}");
                *trigger.failed() = Some(trigger.training.clock.now());
            }
            trigger.active.store(false, Ordering::SeqCst);
        });
    }

    /// The end time of the last failed run.
    fn failed(&self) -> MutexGuard<'_, Option<Timestamp>> {
        self.failed.lock().expect("no lock holder panics")
    }
}

/// Starts a task that calls `check` of `trigger` each `interval`. Logs a failed check.
pub fn start_trigger(trigger: Arc<Trigger>, interval: Duration) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(interval);
        loop {
            ticks.tick().await;
            if let Err(error) = trigger.check().await {
                tracing::warn!("the training check failed: {error:#}");
            }
        }
    });
}

/// The accuracy of `classifier` on `held` at `threshold`.
async fn measure(
    classifier: &dyn Classifier,
    held: &[Example],
    threshold: f32,
) -> anyhow::Result<Metrics> {
    let mut predictions = Vec::new();
    for example in held {
        predictions.push(classifier.predict(&example.context, &example.text).await?);
    }
    Ok(training::metrics(&predictions, held, threshold))
}

/// Deletes the folders in `dir` other than `keep`.
fn prune(dir: &Path, keep: [&str; 2]) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !keep.iter().any(|name| entry.file_name() == *name) {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
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

    use std::ptr;
    use std::sync::Mutex;
    use std::time::Duration;

    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::runtime::Runtime;
    use tokio::sync::OwnedMutexGuard;
    use tokio::task::JoinHandle;

    use crate::domain::model::Prediction;
    use crate::domain::model::Slot;
    use crate::testing::fakes;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeLog;
    use crate::testing::fakes::FakeModelLoader;
    use crate::testing::fakes::FakeModels;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::FakeTrainer;
    use crate::testing::fakes::SetClock;

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
        (config.train.holdout, config.model.dir) = (holdout, models.path().to_owned());
        let trainer = trainer_in(models);
        let training = Training {
            reasoner: Arc::new(FakeReasoner::default()),
            generated: Arc::new(FakeStore::new(None)),
            state: Arc::new(FakeStore::new(None)),
            log: Arc::new(FakeLog::default()),
            trainer: Arc::clone(&trainer) as _,
            runpod: None,
            loader: Arc::new(FakeModelLoader::default()),
            classifier: Arc::new(CurrentClassifier::new(
                FIXTURE.to_owned(),
                Arc::<FakeClassifier>::default(),
            )),
            catalog: Arc::new(catalog()),
            cache: watch::channel(StateCache::load(fakes::house())).1,
            clock: Arc::new(FakeClock(Timestamp::UNIX_EPOCH)),
            config,
            home: Arc::new(FakeHome::default()),
            models: Arc::new(FakeModels::default()),
        };
        (training, trainer)
    }

    /// A trainer that creates the model folders in `models`.
    fn trainer_in(models: &TempDir) -> Arc<FakeTrainer> {
        let dir = models.path().to_owned();
        Arc::new(FakeTrainer {
            dir,
            ..FakeTrainer::default()
        })
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
        // The run then stops at the load, which has no scripted classifier.
        let _stopped = training.run().await;
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

    /// The start of each run of `swap`, in seconds.
    const START: i64 = 1_000_000;

    /// The run ID of a run that starts at `START`.
    const RUN: &str = "19700112-134640";

    /// A training run with the old classifier `old`, and its fakes.
    struct Harness {
        /// The run.
        training: Arc<Training>,
        /// The local trainer.
        trainer: Arc<FakeTrainer>,
        /// `FakeRunPodTrainer`, the `RunPod` trainer of the run unless `edit` removes it.
        runpod: Arc<FakeTrainer>,
        /// The model loader.
        loader: Arc<FakeModelLoader>,
        /// The log.
        log: Arc<FakeLog>,
        /// The model folder.
        models: TempDir,
        /// The clock of the run.
        clock: Arc<SetClock>,
        /// The house with the mute switches.
        home: Arc<FakeHome>,
        /// The model sessions that a local run releases.
        sessions: Arc<FakeModels>,
    }

    /// The run of `harness` with the held-out share 0.1, the old classifier `old` and the start
    /// `START`.
    fn swap(old: &Arc<FakeClassifier>) -> Harness {
        edited(old, |_| ())
    }

    /// A clock at `START`.
    fn start_clock() -> Arc<SetClock> {
        let start = Timestamp::from_second(START).expect("a time");
        Arc::new(SetClock(Mutex::new(start)))
    }

    /// The run of `swap` with `FakeRunPodTrainer` after `edit`.
    fn edited(old: &Arc<FakeClassifier>, edit: impl FnOnce(&mut Training)) -> Harness {
        let models = tempfile::tempdir().expect("a temporary folder");
        let (training, trainer) = harness(0.1, &models);
        let runpod = trainer_in(&models);
        let (loader, log) = (Arc::<FakeModelLoader>::default(), Arc::<FakeLog>::default());
        let current = CurrentClassifier::new(FIXTURE.to_owned(), Arc::clone(old) as _);
        let clock = start_clock();
        let (home, sessions) = (Arc::<FakeHome>::default(), Arc::<FakeModels>::default());
        let mut training = Training {
            log: Arc::clone(&log) as _,
            loader: Arc::clone(&loader) as _,
            classifier: Arc::new(current),
            clock: Arc::clone(&clock) as _,
            runpod: Some(Arc::clone(&runpod) as _),
            home: Arc::clone(&home) as _,
            models: Arc::clone(&sessions) as _,
            ..training
        };
        edit(&mut training);
        Harness {
            training: Arc::new(training),
            trainer,
            runpod,
            loader,
            log,
            models,
            clock,
            home,
            sessions,
        }
    }

    /// A classifier that gives `lights_on` with 0.9 and `slots` for each text.
    fn classifier(slots: &[Slot]) -> Arc<FakeClassifier> {
        predicting("lights_on", slots)
    }

    /// A classifier that gives `label` with 0.9 and `slots` for each text.
    fn predicting(label: &str, slots: &[Slot]) -> Arc<FakeClassifier> {
        let classifier = FakeClassifier::default();
        for _ in 0..1000 {
            let ranked = vec![(label.to_owned(), 0.9)];
            let slots = slots.to_vec();
            classifier.script(Prediction { ranked, slots });
        }
        Arc::new(classifier)
    }

    /// A slot that no example has, so that each example with spans is wrong in `slots`.
    fn wrong_slot() -> Vec<Slot> {
        let (name, value) = ("area".to_owned(), "nowhere".to_owned());
        vec![Slot { name, value }]
    }

    /// Logs the request `id` with a good action mark at `second`.
    fn mark(log: &FakeLog, id: u64, second: i64) {
        let time = Timestamp::from_second(second).expect("a time");
        let interaction = json!({
            "id": id, "time": time, "transcript": "turn on the lights", "model_context": "",
            "model": FIXTURE, "top_labels": [], "slots": [], "service_calls": [], "responses": [],
            "action": { "intent": "lights_on", "targets": [] }, "decision": "Reply",
            "marks": [{ "interaction": id, "value": "Good", "time": time }],
        });
        let interaction = serde_json::from_value(interaction).expect("an interaction");
        let mut interactions = log.interactions.lock().expect("no test panics");
        interactions.push((interaction, None));
    }

    /// Scripts the new classifier `new` and the trainer result `result` of the next run.
    fn script(harness: &Harness, new: &Arc<FakeClassifier>, result: Result<(), &'static str>) {
        harness.loader.script(Arc::clone(new) as _);
        let mut results = harness.trainer.results.lock().expect("no test panics");
        results.push_back(result);
    }

    /// The number of `predict` calls of `classifier` in the next `predict` of the wrapper.
    async fn next_calls(harness: &Harness, classifier: &FakeClassifier) -> usize {
        let before = classifier.calls().len();
        let _ = harness.training.classifier.predict("", "lights on").await;
        classifier.calls().len() - before
    }

    /// Starts the run of `harness` in a task, and returns when the trainer has the call or the
    /// run ended.
    async fn started(harness: &Harness) -> JoinHandle<anyhow::Result<()>> {
        let training = Arc::clone(&harness.training);
        let task = tokio::spawn(async move { training.run().await });
        while calls(harness) == 0 && !task.is_finished() {
            tokio::task::yield_now().await;
        }
        task
    }

    /// AC-13.9b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9b_1_never_worse_model_is_swapped() -> anyhow::Result<()> {
        let (old, new) = (classifier(&wrong_slot()), classifier(&[]));
        let harness = swap(&old);
        script(&harness, &new, Ok(()));
        harness.training.run().await?;
        assert_eq!(
            next_calls(&harness, &new).await,
            1,
            "the new classifier predicts"
        );
        let state = harness.training.state.load()?.expect("a saved state");
        assert_eq!(state.current.as_deref(), Some(RUN));
        Ok(())
    }

    /// AC-13.9b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9b_2_worse_model_is_rejected() -> anyhow::Result<()> {
        let (old, new) = (classifier(&[]), classifier(&wrong_slot()));
        let harness = swap(&old);
        script(&harness, &new, Ok(()));
        harness.training.run().await?;
        assert_eq!(
            next_calls(&harness, &old).await,
            1,
            "the old classifier predicts"
        );
        let folder = harness.models.path().join(RUN);
        assert!(!folder.exists(), "{} exists", folder.display());
        Ok(())
    }

    /// AC-13.9b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9b_3_run_resets_the_change_count() -> anyhow::Result<()> {
        for slots in [Vec::new(), wrong_slot()] {
            let (old, new) = (classifier(&[]), classifier(&slots));
            let harness = swap(&old);
            mark(&harness.log, 1, START - 1);
            assert_eq!(harness.training.changes().await?, 1);
            script(&harness, &new, Ok(()));
            harness.training.run().await?;
            assert_eq!(harness.training.changes().await?, 0, "slots {slots:?}");
        }
        Ok(())
    }

    /// AC-13.9b-4.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9b_4_trainer_error_keeps_model_and_count() -> anyhow::Result<()> {
        let (old, new) = (classifier(&wrong_slot()), classifier(&[]));
        let harness = swap(&old);
        mark(&harness.log, 1, START - 1);
        script(&harness, &new, Err("CUDA out of memory"));
        assert!(harness.training.run().await.is_err(), "the run succeeded");
        assert_eq!(
            next_calls(&harness, &old).await,
            1,
            "the old classifier predicts"
        );
        assert_eq!(harness.training.changes().await?, 1);
        Ok(())
    }

    /// AC-13.9b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9b_5_blocked_load_does_not_delay_predict() -> anyhow::Result<()> {
        let old = classifier(&[]);
        let harness = swap(&old);
        let closed = harness.loader.gate.lock().await;
        script(&harness, &old, Ok(()));
        let task = started(&harness).await;
        tokio::task::yield_now().await;
        let before = tokio::time::Instant::now();
        assert_eq!(
            next_calls(&harness, &old).await,
            1,
            "the old classifier predicts"
        );
        assert_eq!(tokio::time::Instant::now(), before, "virtual time advanced");
        assert!(!task.is_finished(), "the run did not wait for the load");
        drop(closed);
        task.await?
    }

    /// AC-13.9b-6.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9b_6_mark_during_run_counts_for_the_next() -> anyhow::Result<()> {
        let old = classifier(&[]);
        let harness = swap(&old);
        mark(&harness.log, 1, START - 1);
        script(&harness, &old, Ok(()));
        let closed = harness.trainer.gate.lock().await;
        let task = started(&harness).await;
        mark(&harness.log, 2, START + 1);
        drop(closed);
        task.await??;
        assert_eq!(harness.training.changes().await?, 1);
        Ok(())
    }

    /// AC-13.9d-2. The conversation logs the run of the classifier that predicts (its own
    /// AC-13.9d-2 test).
    #[tokio::test(start_paused = true)]
    async fn ac_13_9d_2_swap_stores_the_new_classifier_with_its_run() -> anyhow::Result<()> {
        let (old, new) = (classifier(&wrong_slot()), classifier(&[]));
        let harness = swap(&old);
        script(&harness, &new, Ok(()));
        harness.training.run().await?;
        let current = harness.training.classifier.0.load_full();
        assert_eq!(current.run, RUN);
        let classifier = Arc::as_ptr(&current.classifier);
        assert!(
            ptr::addr_eq(classifier, Arc::as_ptr(&new)),
            "an old classifier"
        );
        Ok(())
    }

    /// The number of `train` calls of `harness`.
    fn calls(harness: &Harness) -> usize {
        harness.trainer.calls.lock().expect("no test panics").len()
    }

    /// Logs `count` good action marks before `START`.
    fn marks(harness: &Harness, count: u64) {
        for id in 0..count {
            mark(&harness.log, id, START - 1);
        }
    }

    /// A trigger of the run of `harness` with the backoff `backoff_s` in seconds.
    fn trigger(harness: &Harness, backoff_s: i64) -> Arc<Trigger> {
        let backoff = SignedDuration::from_secs(backoff_s);
        Arc::new(Trigger::new(Arc::clone(&harness.training), backoff))
    }

    /// Lets each task run until it waits.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    /// Moves the clock of `harness` forward by `seconds`.
    fn advance(harness: &Harness, seconds: i64) {
        *harness.clock.0.lock().expect("no test panics") += SignedDuration::from_secs(seconds);
    }

    /// AC-13.9c-1.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9c_1_count_below_min_changes_starts_no_run() {
        let harness = swap(&classifier(&[]));
        marks(&harness, 19);
        start_trigger(trigger(&harness, 0), Duration::from_secs(1));
        settle().await;
        assert_eq!(calls(&harness), 0, "a run started");
    }

    /// AC-13.9c-2.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9c_2_count_at_min_changes_starts_one_run() {
        let harness = swap(&classifier(&[]));
        marks(&harness, 20);
        start_trigger(trigger(&harness, 0), Duration::from_secs(1));
        settle().await;
        assert_eq!(calls(&harness), 1);
    }

    /// AC-13.9c-3.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9c_3_active_run_blocks_a_second_start() -> anyhow::Result<()> {
        let harness = swap(&classifier(&[]));
        marks(&harness, 20);
        let trigger = trigger(&harness, 0);
        let closed = harness.trainer.gate.lock().await;
        trigger.voice();
        settle().await;
        trigger.voice();
        trigger.check().await?;
        settle().await;
        assert_eq!(calls(&harness), 1, "a second run started");
        drop(closed);
        Ok(())
    }

    /// AC-13.9c-4. Each run fails, because the trainer has no scripted result.
    #[tokio::test(start_paused = true)]
    async fn ac_13_9c_4_failed_run_waits_for_the_backoff() -> anyhow::Result<()> {
        let harness = swap(&classifier(&[]));
        marks(&harness, 20);
        let trigger = trigger(&harness, 60);
        trigger.voice();
        settle().await;
        advance(&harness, 59);
        trigger.check().await?;
        settle().await;
        assert_eq!(calls(&harness), 1, "a run started before the backoff");
        advance(&harness, 1);
        trigger.check().await?;
        settle().await;
        assert_eq!(calls(&harness), 2, "no run after the backoff");
        trigger.voice();
        settle().await;
        assert_eq!(calls(&harness), 3, "the voice trigger waited");
        Ok(())
    }

    /// The run ID and the set of each `train` call of `trainer`.
    fn recorded(trainer: &FakeTrainer) -> Vec<(String, TrainingSet)> {
        trainer.calls.lock().expect("no test panics").clone()
    }

    /// AC-13.10-1.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10_1_local_error_sends_the_set_to_runpod() -> anyhow::Result<()> {
        let (old, new) = (classifier(&wrong_slot()), classifier(&[]));
        let harness = swap(&old);
        script(&harness, &new, Err("CUDA out of memory"));
        let results = &harness.runpod.results;
        results.lock().expect("no test panics").push_back(Ok(()));
        harness.training.run().await?;
        let runpod = recorded(&harness.runpod);
        assert_eq!(runpod.len(), 1, "RunPod calls");
        assert_eq!(runpod, recorded(&harness.trainer));
        Ok(())
    }

    /// AC-13.10-2.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10_2_local_success_does_not_call_runpod() -> anyhow::Result<()> {
        let old = classifier(&[]);
        let harness = swap(&old);
        script(&harness, &old, Ok(()));
        harness.training.run().await?;
        assert_eq!(recorded(&harness.runpod), [], "RunPod has a call");
        Ok(())
    }

    /// AC-13.10-3.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10_3_local_error_without_runpod_keeps_the_model() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        // The current-thread runtime polls the run task on this thread, so it logs here too.
        let _default = tracing::dispatcher::set_default(&fakes::log_dispatch(&log)?);
        let (old, new) = (classifier(&wrong_slot()), classifier(&[]));
        let harness = edited(&old, |training| training.runpod = None);
        script(&harness, &new, Err("CUDA out of memory"));
        trigger(&harness, 0).voice();
        settle().await;
        assert_eq!(
            next_calls(&harness, &old).await,
            1,
            "the old classifier predicts"
        );
        let text = fs::read_to_string(log.path())?;
        assert!(text.contains("CUDA out of memory"), "the log is {text:?}");
        Ok(())
    }

    /// The mute switch of the kitchen satellite, the only one that is `off` in the reference house.
    const KITCHEN: &str = "switch.carson_kitchen_mute_microphones";

    /// The `switch.<service>` call of `KITCHEN`.
    fn kitchen(service: &str) -> ServiceCall {
        ServiceCall {
            domain: "switch".to_owned(),
            service: service.to_owned(),
            target: vec![KITCHEN.to_owned()],
            data: Map::new(),
            return_response: false,
        }
    }

    /// The number of `WARN` lines of the log file `log` that name `name`.
    fn warns(log: &tempfile::NamedTempFile, name: &str) -> anyhow::Result<usize> {
        let text = fs::read_to_string(log.path())?;
        let lines = text.lines().filter(|line| line.contains(" WARN "));
        Ok(lines.filter(|line| line.contains(name)).count())
    }

    /// AC-13.10e-1. Checks the order while `FakeRunPodTrainer` waits after a local error.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10e_1_local_run_mutes_and_releases() -> anyhow::Result<()> {
        for result in [Ok(()), Err("CUDA out of memory")] {
            let old = classifier(&[]);
            let harness = swap(&old);
            script(&harness, &old, result);
            let runpod = &harness.runpod;
            runpod
                .results
                .lock()
                .expect("no test panics")
                .push_back(Ok(()));
            let (local, waiting) = (harness.trainer.gate.lock().await, runpod.gate.lock().await);
            let task = started(&harness).await;
            assert_eq!(harness.home.calls(), [kitchen("turn_on")]);
            assert!(harness.sessions.released(), "the models are not released");
            drop(local);
            settle().await;
            assert_eq!(harness.sessions.calls(), ["release", "restore"]);
            let unmuted = ["turn_on", "turn_off"].map(kitchen);
            assert_eq!(harness.home.calls(), unmuted);
            drop(waiting);
            task.await??;
        }
        Ok(())
    }

    /// The run of `swap` of `old` to the end with the new classifier `new`, where the switch call
    /// after `passed` calls fails.
    async fn failing(
        old: &Arc<FakeClassifier>,
        new: &Arc<FakeClassifier>,
        passed: usize,
    ) -> anyhow::Result<Harness> {
        let harness = swap(old);
        script(&harness, new, Ok(()));
        for _ in 0..passed {
            let gate = harness.home.delay();
            gate.send(())
                .map_err(|()| anyhow::anyhow!("the call ended"))?;
        }
        drop(harness.home.delay());
        harness.training.run().await?;
        Ok(harness)
    }

    /// AC-13.10e-2.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10e_2_failed_switch_call_is_logged() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        let _default = tracing::dispatcher::set_default(&fakes::log_dispatch(&log)?);
        let old = classifier(&[]);
        let harness = failing(&old, &old, 0).await?;
        assert_eq!(warns(&log, KITCHEN)?, 1, "warns after a failed turn_on");
        assert_eq!(calls(&harness), 1, "the process trainer did not run");
        assert_eq!(harness.home.calls(), [kitchen("turn_on")]);
        let (old, new) = (classifier(&wrong_slot()), classifier(&[]));
        let harness = failing(&old, &new, 1).await?;
        assert_eq!(warns(&log, KITCHEN)?, 2, "warns after a failed turn_off");
        assert_eq!(
            next_calls(&harness, &new).await,
            1,
            "the new classifier predicts"
        );
        Ok(())
    }

    /// AC-13.10e-3.
    #[tokio::test(start_paused = true)]
    async fn ac_13_10e_3_satellite_without_mute_switch_is_logged() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        let _default = tracing::dispatcher::set_default(&fakes::log_dispatch(&log)?);
        let mut house = fakes::house();
        let bedroom = "switch.carson_bedroom_mute_microphones";
        house
            .entities
            .retain(|entity| entity.entity_id.0 != bedroom);
        let old = classifier(&[]);
        let harness = edited(&old, |training| {
            training.cache = watch::channel(StateCache::load(house)).1;
        });
        script(&harness, &old, Ok(()));
        harness.training.run().await?;
        assert_eq!(warns(&log, "assist_satellite.carson_bedroom")?, 1);
        let turn_on = kitchen("turn_on");
        assert!(harness.home.calls().contains(&turn_on), "no turn_on call");
        Ok(())
    }

    /// A failed `restore` ends the run with its error after the unmute calls, with no `RunPod` call.
    #[tokio::test(start_paused = true)]
    async fn failed_restore_unmutes_and_ends_the_run() {
        let old = classifier(&[]);
        let harness = swap(&old);
        script(&harness, &old, Err("CUDA out of memory"));
        harness.sessions.fail_restore();
        let error = harness.training.run().await.expect_err("the run ends");
        assert_eq!(format!("{error:#}"), "no model");
        let turn_off = kitchen("turn_off");
        assert!(harness.home.calls().contains(&turn_off), "no turn_off call");
        assert_eq!(recorded(&harness.runpod), [], "RunPod has a call");
    }

    /// The model state of `training.qnt` that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct TrainingModel {
        /// `Idle`, `RunPod` while `FakeRunPodTrainer` has a waiting call, else `Local`.
        #[serde(rename = "runState")]
        run_state: Tag,
        /// The change count.
        changes: usize,
        /// The accuracy level of the active classifier.
        #[serde(rename = "modelAcc")]
        model_acc: usize,
        /// The release state of `FakeModels`.
        paused: bool,
    }

    /// The variant of a Quint sum type value.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Tag {
        /// The variant name.
        tag: String,
    }

    impl ModelState<TrainingDriver> for TrainingModel {
        /// The `active` flag with the waiting trainer, the change count and the level of the
        /// classifier of the wrapper, after each task runs until it waits.
        fn from_driver(driver: &TrainingDriver) -> anyhow::Result<Self> {
            driver.runtime.block_on(settle());
            let training = &driver.harness.training;
            let active = driver.trigger.active.load(Ordering::SeqCst);
            let current = training.classifier.0.load_full();
            let same = |level: &Arc<FakeClassifier>| {
                ptr::addr_eq(Arc::as_ptr(level), Arc::as_ptr(&current.classifier))
            };
            let level = driver.levels.iter().position(same);
            let runpod = recorded(&driver.harness.runpod).len() > driver.runpod_ends;
            let tag = match (active, runpod) {
                (false, _) => "Idle",
                (true, false) => "Local",
                (true, true) => "RunPod",
            };
            let tag = tag.to_owned();
            Ok(Self {
                run_state: Tag { tag },
                changes: driver.runtime.block_on(training.changes())?,
                model_acc: level.ok_or_else(|| anyhow::anyhow!("an unknown classifier"))?,
                paused: driver.harness.sessions.released(),
            })
        }
    }

    /// Replays `training.qnt` against the trigger with the backoff 0 and `min_changes` 2. Each step
    /// takes one second. Each `train` waits until a done or fail step.
    struct TrainingDriver {
        /// The paused runtime that runs the runs.
        runtime: Runtime,
        /// The run and its fakes.
        harness: Harness,
        /// The trigger of the run.
        trigger: Arc<Trigger>,
        /// The classifiers of the accuracy levels 0, 1 and 2.
        levels: [Arc<FakeClassifier>; 3],
        /// The trainer gate, which the driver holds between steps.
        gate: Option<OwnedMutexGuard<()>>,
        /// The gate of `FakeRunPodTrainer`, which the driver holds between steps.
        runpod_gate: Option<OwnedMutexGuard<()>>,
        /// The number of ended `FakeRunPodTrainer` calls.
        runpod_ends: usize,
    }

    impl TrainingDriver {
        /// A driver at `initWith(runpod)`: no run, no changes and the classifier of level 0.
        fn new(runpod: bool) -> Self {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            let runtime = runtime.enable_time().start_paused(true).build();
            let levels = [
                predicting("unknown", &wrong_slot()),
                classifier(&wrong_slot()),
                classifier(&[]),
            ];
            let harness = edited(&levels[0], |training| {
                let mut catalog = catalog();
                catalog.intents.retain(|intent| intent.name == "lights_on");
                catalog.unknown_examples.clear();
                training.catalog = Arc::new(catalog);
                // Each example is held out, so each level gives strictly higher measures.
                training.config.train.holdout = 1.0;
                training.config.train.min_changes = 2;
                if !runpod {
                    training.runpod = None;
                }
            });
            Self {
                runtime: runtime.expect("the runtime builds"),
                trigger: trigger(&harness, 0),
                gate: Arc::clone(&harness.trainer.gate).try_lock_owned().ok(),
                runpod_gate: Arc::clone(&harness.runpod.gate).try_lock_owned().ok(),
                runpod_ends: 0,
                harness,
                levels,
            }
        }

        /// Ends the waiting call of the local trainer, or of `FakeRunPodTrainer` when `runpod`,
        /// with `result`.
        fn end(&mut self, runpod: bool, result: Result<(), &'static str>) {
            let (trainer, gate) = if runpod {
                self.runpod_ends += 1;
                (&self.harness.runpod, &mut self.runpod_gate)
            } else {
                (&self.harness.trainer, &mut self.gate)
            };
            trainer
                .results
                .lock()
                .expect("no test panics")
                .push_back(result);
            *gate = None;
            self.runtime.block_on(settle());
            *gate = Arc::clone(&trainer.gate).try_lock_owned().ok();
        }
    }

    impl Driver for TrainingDriver {
        type State = TrainingModel;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            advance(&self.harness, 1);
            switch!(step {
                initLocal => *self = Self::new(false),
                initRunPod => *self = Self::new(true),
                change => {
                    let now = self.harness.training.clock.now().as_second();
                    mark(&self.harness.log, now.unsigned_abs(), now);
                },
                autoStart => self.runtime.block_on(self.trigger.check())?,
                voiceStart => {
                    let _context = self.runtime.enter();
                    self.trigger.voice();
                },
                localDone(a: usize) => {
                    self.harness.loader.script(Arc::clone(&self.levels[a]) as _);
                    self.end(false, Ok(()));
                },
                localFail => self.end(false, Err("CUDA out of memory")),
                runpodDone(a: usize) => {
                    self.harness.loader.script(Arc::clone(&self.levels[a]) as _);
                    self.end(true, Ok(()));
                },
                runpodFail => self.end(true, Err("the RunPod job failed")),
            })
        }
    }

    /// AC-13.9c-5. `quint_run` replays 100 traces by default.
    #[quint_run(
        spec = "quint/training.qnt",
        init = "initLocal",
        max_steps = 30,
        seed = "1"
    )]
    fn ac_13_9c_5_conformance() -> impl Driver {
        TrainingDriver::new(false)
    }

    /// AC-13.10-4.
    #[quint_run(
        spec = "quint/training.qnt",
        init = "initRunPod",
        max_steps = 30,
        seed = "1"
    )]
    fn ac_13_10_4_conformance() -> impl Driver {
        TrainingDriver::new(true)
    }

    /// AC-13.10e-4. Replays 100 traces from each start.
    #[test]
    fn ac_13_10e_4_conformance() -> anyhow::Result<()> {
        for (init, runpod) in [("initLocal", false), ("initRunPod", true)] {
            let gen_config = quint_connect::runner::RunConfig {
                spec: "quint/training.qnt".to_owned(),
                main: None,
                init: Some(init.to_owned()),
                step: None,
                max_samples: None,
                max_steps: Some(30),
                seed: "1".to_owned(),
            };
            let test_name = format!("ac_13_10e_4_conformance from {init}");
            let config = quint_connect::runner::Config {
                test_name,
                gen_config,
            };
            quint_connect::runner::run_test(TrainingDriver::new(runpod), config)?;
        }
        Ok(())
    }
}
