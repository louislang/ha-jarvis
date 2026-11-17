//! Hot cache sync, response backfill, the request habit store, the learned intent check, the daily
//! log prune and the hotword refresh.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::mem;
use std::ptr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::mpsc;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::StreamExt;
use jiff::SignedDuration;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Deserialize;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::BackfillConfig;
use crate::Config;
use crate::LearnConfig;
use crate::app::conversation::Change;
use crate::app::conversation::Conversation;
use crate::app::speech::Speech;
use crate::domain::catalog::Catalog;
use crate::domain::feedback::Deferred;
use crate::domain::habits::RequestHabits;
use crate::domain::learn;
use crate::domain::learn::Cluster;
use crate::domain::learn::Learned;
use crate::domain::learn::LearnedIntent;
use crate::domain::learn::LearnedKind;
use crate::domain::learn::LearnedStatus;
use crate::domain::learn::Point;
use crate::domain::responses;
use crate::domain::responses::Pool;
use crate::domain::responses::Pools;
use crate::domain::speech;
use crate::domain::state::Event;
use crate::domain::state::ServiceCall;
use crate::domain::state_cache::StateCache;
use crate::ports::Clock;
use crate::ports::Home;
use crate::ports::InteractionLog;
use crate::ports::Reasoner;
use crate::ports::SpeechToText;
use crate::ports::Store;
use crate::ports::Tool;

/// The wait before the first retry of a failed snapshot.
const FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest wait between two snapshot retries.
const MAX_RETRY: Duration = Duration::from_secs(30);

/// The time between two prunes of the interaction log.
const PRUNE_INTERVAL: Duration = Duration::from_hours(24);

/// The time between two hotword refreshes.
const HOTWORDS_REFRESH: Duration = Duration::from_hours(1);

/// The media types of the library names that are hotwords, in the order of their calls.
const LIBRARY_TYPES: [&str; 3] = ["playlist", "artist", "album"];

/// Keeps `cache` equal to the states and the registries of `home` until the event stream ends,
/// from the first `Reconnected`. Publishes each new cache to the receivers of `cache`.
pub async fn sync(home: &dyn Home, cache: &watch::Sender<StateCache>) {
    let mut events = home.events();
    while let Some(event) = events.next().await {
        match event {
            Event::StateChanged { .. } => {
                cache.send_modify(|cache| *cache = mem::take(cache).apply(event));
            }
            Event::RegistryUpdated | Event::Reconnected => load(home, cache).await,
        }
    }
}

/// Replaces `cache` with a new snapshot of `home`. After each failed snapshot, logs the error,
/// keeps the old cache and tries again after a wait of 1 s, doubled each time, at most 30 s.
async fn load(home: &dyn Home, cache: &watch::Sender<StateCache>) {
    let mut wait = FIRST_RETRY;
    loop {
        match home.snapshot().await {
            Ok(snapshot) => {
                cache.send_replace(StateCache::load(snapshot));
                return;
            }
            Err(error) => tracing::warn!("the snapshot failed: {error:#}"),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(MAX_RETRY);
    }
}

/// Records response uses, and adds new responses to a pool that a use leaves with too few unused
/// responses.
pub struct Backfill {
    /// The limits of a backfill.
    config: BackfillConfig,
    /// System Two, which writes the candidates.
    reasoner: Arc<dyn Reasoner>,
    /// The sender of the changes to the pools task.
    changes: mpsc::Sender<Change>,
    /// The pre-render of the new responses.
    speech: Arc<Speech>,
    /// The live catalog. Only an intent pool can use `area`.
    catalog: Arc<ArcSwap<Catalog>>,
    /// The hot cache, which has the area names of the pre-render.
    cache: watch::Receiver<StateCache>,
    /// The pools with a backfill in progress.
    running: Mutex<HashSet<String>>,
}

impl Backfill {
    /// Backfills with the limits of `config` and the candidates of `reasoner`, pre-renders with
    /// `speech` for the areas of `cache`, and sends the uses and the merges on `changes`.
    #[must_use]
    pub fn new(
        config: BackfillConfig,
        reasoner: Arc<dyn Reasoner>,
        changes: mpsc::Sender<Change>,
        speech: Arc<Speech>,
        catalog: Arc<ArcSwap<Catalog>>,
        cache: watch::Receiver<StateCache>,
    ) -> Self {
        Self {
            config,
            reasoner,
            changes,
            speech,
            catalog,
            cache,
            running: Mutex::default(),
        }
    }

    /// Sends the use of the response at `index` of the pool `pool` to the pools task. The task
    /// then starts a backfill of the pool if it needs one and none runs.
    pub fn record_use(self: &Arc<Self>, pool: String, index: usize) {
        let backfill = Arc::clone(self);
        self.send(Box::new(move |pools| {
            let pools = responses::record_use(pools, &pool, index);
            backfill.start(&pools, pool);
            pools
        }));
    }

    /// Sends `change` to the pools task.
    fn send(&self, change: Change) {
        // The task holds the receiver until every sender drops, so the send fails only after a
        // panic.
        let _ = self.changes.send(change);
    }

    /// Spawns the backfill of the pool `name` of `pools` when it needs one and none runs.
    fn start(self: Arc<Self>, pools: &Pools, name: String) {
        let Some(pool) = pools.get(&name) else {
            return;
        };
        let needed = responses::needs_backfill(pool, self.config.min_fresh, self.config.max_pool);
        // `insert` is false while a backfill of the pool runs: the `start` guard of the model.
        if !needed || !self.running().insert(name.clone()) {
            return;
        }
        let pool = pool.clone();
        tokio::spawn(async move {
            match self.candidates(&name, &pool).await {
                Ok(new) => self.send_merge(name, new),
                Err(error) => {
                    tracing::warn!("the backfill of {name} failed: {error:#}");
                    self.running().remove(&name);
                }
            }
        });
    }

    /// The pools with a backfill in progress.
    fn running(&self) -> MutexGuard<'_, HashSet<String>> {
        self.running.lock().expect("no lock holder panics")
    }

    /// The new responses of the pool `name`, which is `pool`, that System Two writes and that are
    /// valid, after their pre-render.
    ///
    /// # Errors
    ///
    /// `generate` or the pre-render fails.
    async fn candidates(&self, name: &str, pool: &Pool) -> anyhow::Result<Vec<String>> {
        let variables = self.variables(name);
        let existing = texts(pool);
        let generated = self
            .reasoner
            .generate(name, &existing, &variables, self.config.batch);
        let new = self.valid(pool, generated.await?, &variables);
        self.prerender(name, &new).await?;
        Ok(new)
    }

    /// Synthesizes the responses `new` of the pool `name` for the area names of the hot cache.
    ///
    /// # Errors
    ///
    /// A synthesis fails.
    async fn prerender(&self, name: &str, new: &[String]) -> anyhow::Result<()> {
        let added = Pools::from([(name.to_owned(), pool(new))]);
        let areas = area_names(&self.cache.borrow());
        self.speech.prerender(&added, &areas).await
    }

    /// The variables that the responses of the pool `name` can use: `area` for an intent pool.
    fn variables(&self, name: &str) -> BTreeSet<String> {
        let catalog = self.catalog.load();
        let intent = catalog.intents.iter().any(|intent| intent.name == name);
        BTreeSet::from_iter(intent.then(|| "area".to_owned()))
    }

    /// Each of `candidates` that is valid with `variables` against `pool` and the candidates
    /// before it, up to `max_pool` responses in all.
    fn valid(
        &self,
        pool: &Pool,
        candidates: Vec<String>,
        variables: &BTreeSet<String>,
    ) -> Vec<String> {
        let (mut checked, max_chars) = (pool.clone(), self.config.max_chars);
        for candidate in candidates {
            if responses::validate(&candidate, &checked, variables, max_chars).is_ok() {
                checked = responses::merge(checked, &[candidate], self.config.max_pool);
            }
        }
        texts(&checked).split_off(pool.responses.len())
    }

    /// Sends the merge of `new` into the pool `name` to the pools task, which then ends the
    /// backfill of the pool.
    fn send_merge(self: &Arc<Self>, name: String, new: Vec<String>) {
        let (backfill, max_pool) = (Arc::clone(self), self.config.max_pool);
        self.send(Box::new(move |mut pools| {
            if let Some(pool) = pools.remove(&name) {
                pools.insert(name.clone(), responses::merge(pool, &new, max_pool));
            }
            // A use that the task applies after this change sees the new responses.
            backfill.running().remove(&name);
            pools
        }));
    }
}

/// The name of each area of `cache`.
#[must_use]
pub fn area_names(cache: &StateCache) -> Vec<String> {
    let names = cache.areas.iter().map(|area| area.name.clone());
    names.collect()
}

/// The text of each response of `pool`.
fn texts(pool: &Pool) -> Vec<String> {
    let texts = pool.responses.iter().map(|response| response.text.clone());
    texts.collect()
}

/// The request habit model, which `handle` records into and the habit task saves.
pub struct Habits {
    /// The model. No code holds the lock across an `await`.
    model: Mutex<RequestHabits>,
    /// The days in which a score halves.
    half_life_days: f64,
    /// The hot cache, which has the HA time zone.
    cache: watch::Receiver<StateCache>,
}

impl Habits {
    /// The habits of `model`, whose scores halve each `half_life_days`, in the HA time zone of
    /// `cache`.
    #[must_use]
    pub fn new(
        model: RequestHabits,
        half_life_days: f64,
        cache: watch::Receiver<StateCache>,
    ) -> Self {
        Self {
            model: Mutex::new(model),
            half_life_days,
            cache,
        }
    }

    /// Records one request of `key` at `time`. Without the HA time zone, or with a poisoned lock,
    /// the request does not count.
    pub fn observe(&self, key: String, time: Timestamp) {
        let Some(zone) = self.zone() else {
            return;
        };
        if let Some(mut model) = self.lock() {
            *model = mem::take(&mut *model).observe(key, time, &zone, self.half_life_days);
        }
    }

    /// A copy of the model, or `None` when the lock is poisoned.
    #[must_use]
    pub fn current(&self) -> Option<RequestHabits> {
        self.lock().map(|model| model.clone())
    }

    /// The model without the scores that faded by the local date of `time`, or the whole model
    /// without the HA time zone. `None` when the lock is poisoned.
    fn pruned(&self, time: Timestamp) -> Option<RequestHabits> {
        let model = self.current()?;
        Some(match self.zone() {
            Some(zone) => model.prune(zone.to_datetime(time).date(), self.half_life_days),
            None => model,
        })
    }

    /// The HA time zone of the hot cache.
    fn zone(&self) -> Option<TimeZone> {
        let cache = self.cache.borrow();
        TimeZone::get(&cache.config.as_ref()?.time_zone).ok()
    }

    /// The model, or `None` after a `warn` event when a holder of the lock panicked.
    fn lock(&self) -> Option<MutexGuard<'_, RequestHabits>> {
        let locked = self.model.lock();
        if locked.is_err() {
            tracing::warn!("the habit model is poisoned");
        }
        locked.ok()
    }
}

/// The habit task, its habits and its stop.
pub struct HabitsTask {
    /// The habits that the task saves.
    pub habits: Arc<Habits>,
    /// Stops the task after one more save when it sends or drops.
    pub stop: oneshot::Sender<()>,
    /// The task.
    pub task: JoinHandle<()>,
}

/// Starts the habit task. Each `interval`, and once more when its stop sends or drops, it saves
/// to `store` the model of `habits` without the scores that faded by the `clock` time.
#[must_use]
pub fn start_habits(
    habits: Arc<Habits>,
    store: Arc<dyn Store<RequestHabits>>,
    clock: Arc<dyn Clock>,
    interval: Duration,
) -> HabitsTask {
    let (stop, mut stopped) = oneshot::channel();
    let saved = Arc::clone(&habits);
    let task = tokio::spawn(async move {
        loop {
            let stopping = tokio::select! {
                () = tokio::time::sleep(interval) => false,
                _ = &mut stopped => true,
            };
            if let Some(model) = saved.pruned(clock.now()) {
                let store = Arc::clone(&store);
                // A failed save keeps the last saved file. The next save writes the whole model.
                let _ = tokio::task::spawn_blocking(move || store.save(&model)).await;
            }
            if stopping {
                return;
            }
        }
    });
    HabitsTask { habits, stop, task }
}

/// The learned intent check: the clusters of the System Two requests of the log and the
/// proposals of System Two for them, saved to the learned intents file through the pools task.
#[derive(Clone)]
pub struct Learning {
    /// The `[learn]` section.
    pub config: LearnConfig,
    /// The log of the System Two requests.
    pub log: Arc<dyn InteractionLog>,
    /// System Two.
    pub reasoner: Arc<dyn Reasoner>,
    /// The intent catalog.
    pub catalog: Arc<Catalog>,
    /// The plugin tools.
    pub tools: Vec<Tool>,
    /// The activation, with the learned intents file, its pools task and the time of the window.
    pub activation: Activation,
    /// The request habits, which get the times of the clusters.
    pub habits: Arc<Habits>,
}

impl Learning {
    /// Saves an entry for each cluster of the System Two requests of the window that no learned
    /// entry covers. Logs a failed check.
    pub async fn check(&self) {
        if let Err(error) = self.try_check().await {
            tracing::warn!("the learned intent check failed: {error:#}");
        }
    }

    /// The check of [`Learning::check`].
    ///
    /// # Errors
    ///
    /// `deferred`, `embed`, `propose` or the load of the file fails.
    async fn try_check(&self) -> anyhow::Result<()> {
        let points = self.points().await?;
        let mut learned = self.activation.load().await?.intents;
        let distance = self.config.cluster_distance;
        let mut habits: BTreeMap<String, Vec<Timestamp>> = BTreeMap::new();
        for cluster in learn::clusters(&points, distance, self.config.min_cluster) {
            if let Some((name, times)) = self.learn(cluster, &mut learned).await? {
                habits.entry(name).or_default().extend(times);
            }
        }
        self.record(habits);
        Ok(())
    }

    /// The name of the covering entry of `cluster` in `learned` with the times of the cluster,
    /// or the same for the entry of a new proposal, which it saves and adds to `learned`. Gives
    /// `None` when the entry gets no request habits.
    ///
    /// # Errors
    ///
    /// `propose` fails.
    async fn learn(
        &self,
        cluster: Cluster,
        learned: &mut Vec<LearnedIntent>,
    ) -> anyhow::Result<Option<(String, Vec<Timestamp>)>> {
        let distance = self.config.cluster_distance;
        let times = cluster.times.clone();
        if let Some(name) = learn::covering(&cluster, learned, distance) {
            // `covering` borrows the name of the matched entry, and an invalid entry can reuse
            // the name of another, so the match is by address.
            let matched = |entry: &&LearnedIntent| ptr::eq(entry.name.as_str(), name);
            let entry = learned.iter().find(matched);
            return Ok(entry.and_then(|entry| habit(entry, times)));
        }
        let entry = self.entry(cluster, learned).await?;
        self.save(entry.clone());
        let found = habit(&entry, times);
        if entry.status == LearnedStatus::Active {
            self.activation.activate(entry.clone()).await;
        }
        learned.push(entry);
        Ok(found)
    }

    /// Observes the times of each key of `habits` in time order, so that each day counts.
    fn record(&self, habits: BTreeMap<String, Vec<Timestamp>>) {
        for (name, mut times) in habits {
            times.sort();
            for time in times {
                self.habits.observe(name.clone(), time);
            }
        }
    }

    /// The System Two requests of the window with their embeddings.
    ///
    /// # Errors
    ///
    /// `deferred` or `embed` fails.
    async fn points(&self) -> anyhow::Result<Vec<Point>> {
        let window = SignedDuration::from_hours(24 * i64::try_from(self.config.window_days)?);
        let now = self.activation.clock.now();
        let requests = self.log.deferred(now - window).await?;
        let texts: Vec<String> = requests
            .iter()
            .map(|request| request.text.clone())
            .collect();
        let embeddings = self.reasoner.embed(&texts).await?;
        let point = |(request, embedding): (Deferred, Vec<f32>)| Point {
            text: request.text,
            time: request.time,
            spoken: request.spoken,
            embedding,
        };
        Ok(requests.into_iter().zip(embeddings).map(point).collect())
    }

    /// The entry of the proposal of System Two for `cluster`, with its `initial_status`, or with
    /// `none` after a `warn` event when it is not valid.
    ///
    /// # Errors
    ///
    /// `propose` fails.
    async fn entry(
        &self,
        cluster: Cluster,
        learned: &[LearnedIntent],
    ) -> anyhow::Result<LearnedIntent> {
        let entries = learned.iter().map(|entry| entry.name.clone());
        let catalog = self
            .catalog
            .intents
            .iter()
            .map(|intent| intent.name.clone());
        let names: Vec<String> = catalog.chain(entries).collect();
        let proposal = self
            .reasoner
            .propose(&cluster.texts, &names, &self.tools)
            .await?;
        let plugins: BTreeSet<String> = self.tools.iter().map(|tool| tool.name.clone()).collect();
        let valid = learn::validate_proposal(proposal.clone(), &self.catalog, learned, &plugins);
        let status = match valid {
            Ok(valid) => learn::initial_status(&valid),
            Err(error) => {
                tracing::warn!("the proposal {} is not valid: {error:?}", proposal.name);
                LearnedStatus::None
            }
        };
        Ok(LearnedIntent {
            status,
            texts: cluster.texts,
            centroid: cluster.centroid,
            ..proposal
        })
    }

    /// Sends the pools task a change that adds `entry` to the learned intents file.
    fn save(&self, entry: LearnedIntent) {
        let store = Arc::clone(&self.activation.store);
        let change: Change = Box::new(move |pools| {
            if let Err(error) = add(store.as_ref(), entry) {
                tracing::warn!("the learned intents file did not change: {error:#}");
            }
            pools
        });
        self.activation.send(change);
    }
}

/// Gives active learned entries their pools, audio, live catalog intents and activation times.
#[derive(Clone)]
pub struct Activation {
    /// The learned intents file.
    pub store: Arc<dyn Store<Learned>>,
    /// The sender of the pools task, which also writes the file.
    pub changes: mpsc::Sender<Change>,
    /// The pre-render of the pools.
    pub speech: Arc<Speech>,
    /// The hot cache, which has the area names of the pre-render.
    pub cache: watch::Receiver<StateCache>,
    /// The catalog of `intents.toml` and the active learned intents.
    pub catalog: Arc<ArcSwap<Catalog>>,
    /// The time of the activation.
    pub clock: Arc<dyn Clock>,
}

impl Activation {
    /// Activates each `active` entry of the file without `activated`.
    pub async fn resume(&self) {
        // The first check loads the file too, and logs a failed load.
        let learned = self.load().await.unwrap_or_default().intents;
        for entry in learned {
            if entry.status == LearnedStatus::Active && entry.activated.is_none() {
                self.activate(entry).await;
            }
        }
    }

    /// Adds and pre-renders the pool of `entry` when it has an intent, then adds the intent to the
    /// live catalog and sets `activated`. A failed pre-render logs and leaves `activated` unset.
    pub async fn activate(&self, entry: LearnedIntent) {
        if let Err(error) = self.add_pool(&entry).await {
            return tracing::warn!("the pre-render of {} failed: {error:#}", entry.name);
        }
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        let now = self.clock.now();
        self.send(Box::new(move |pools| {
            catalog.rcu(|current| learn::live(Catalog::clone(current), vec![entry.clone()]));
            if let Err(error) = activated(store.as_ref(), &entry.name, now) {
                tracing::warn!("the activation of {} was not saved: {error:#}", entry.name);
            }
            pools
        }));
    }

    /// When `entry` has an intent, sends the pools task a change that adds its pool when no pool
    /// has its name, then pre-renders the pool for the areas of the hot cache. Fails when the
    /// pre-render fails.
    pub(crate) async fn add_pool(&self, entry: &LearnedIntent) -> anyhow::Result<()> {
        if learn::intent(entry.clone()).is_none() {
            return Ok(());
        }
        let (name, added) = (entry.name.clone(), pool(&entry.responses));
        let pools = Pools::from([(name.clone(), added.clone())]);
        self.send(Box::new(move |mut current| {
            current.entry(name).or_insert(added);
            current
        }));
        let areas = area_names(&self.cache.borrow());
        self.speech.prerender(&pools, &areas).await
    }

    /// The learned intents file, or an empty one when it does not exist. Fails when it cannot load.
    async fn load(&self) -> anyhow::Result<Learned> {
        let store = Arc::clone(&self.store);
        let saved = tokio::task::spawn_blocking(move || store.load()).await??;
        Ok(saved.unwrap_or_default())
    }

    /// Sends `change` to the pools task.
    fn send(&self, change: Change) {
        // Without the pools task, Jarvis stops, and the next start checks and activates again.
        let _ = self.changes.send(change);
    }
}

/// The name of `entry` with `times` when the entry is `pending`, `active` or `requested`. The
/// name of a `none` or `rejected` entry can be invalid or never become an intent.
fn habit(entry: &LearnedIntent, times: Vec<Timestamp>) -> Option<(String, Vec<Timestamp>)> {
    use LearnedStatus::Active;
    use LearnedStatus::Pending;
    use LearnedStatus::Requested;
    let counted = matches!(entry.status, Pending | Active | Requested);
    counted.then(|| (entry.name.clone(), times))
}

/// The pool of the responses `texts`, with no uses.
#[must_use]
pub fn pool(texts: &[String]) -> Pool {
    let empty = Pool {
        responses: Vec::new(),
        last: None,
    };
    responses::merge(empty, texts, usize::MAX)
}

/// Sets `activated` of the first `active` entry `name` of `store` to `now` when it has none.
/// Fails when `store` cannot load or save.
fn activated(store: &dyn Store<Learned>, name: &str, now: Timestamp) -> anyhow::Result<()> {
    let mut learned = store.load()?.unwrap_or_default();
    let mut entries = learned.intents.iter_mut();
    let active = entries.find(|entry| entry.name == name && entry.status == LearnedStatus::Active);
    if let Some(entry) = active {
        entry.activated.get_or_insert(now);
    }
    store.save(&learned)
}

/// Adds `entry` to the learned intents of `store`.
///
/// # Errors
///
/// `store` cannot load or save.
fn add(store: &dyn Store<Learned>, entry: LearnedIntent) -> anyhow::Result<()> {
    let mut learned = store.load()?.unwrap_or_default();
    learned.intents.push(entry);
    store.save(&learned)
}

/// Starts a task that activates the active entries without `activated` of `learning`, then runs
/// the check of `learning` at once and each `interval`.
#[must_use]
pub fn start_learning(learning: Learning, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        learning.activation.resume().await;
        let mut ticks = tokio::time::interval(interval);
        loop {
            ticks.tick().await;
            learning.check().await;
        }
    })
}

/// A prefetched answer and its time.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    /// The text.
    pub text: String,
    /// The time when the answer was complete.
    pub time: Timestamp,
}

/// The prefetch of the answers of the active prefetch intents.
pub struct Prefetch {
    /// System Two, which answers the questions.
    conversation: Conversation,
    /// The learned intents file, the pre-render and the time.
    activation: Activation,
    /// The request habits, which give the usual times and the HA time zone.
    habits: Arc<Habits>,
    /// The least decayed score of a usual time.
    min_score: f64,
    /// The minutes of the window before the hour of a usual time.
    lead_min: u64,
    /// The time of the last prefetch of each intent.
    attempts: Mutex<BTreeMap<String, Timestamp>>,
    /// The stored answer of each intent.
    answers: Mutex<BTreeMap<String, Answer>>,
}

impl Prefetch {
    /// The prefetch of `conversation` for the entries of the file of `activation`, in the windows
    /// of the usual times of `habits` with `habits.min_score` and `learn.prefetch_lead_min` of
    /// `config`.
    #[must_use]
    pub fn new(
        conversation: Conversation,
        activation: Activation,
        habits: Arc<Habits>,
        config: &Config,
    ) -> Self {
        Self {
            conversation,
            activation,
            habits,
            min_score: config.habits.min_score,
            lead_min: config.learn.prefetch_lead_min,
            attempts: Mutex::default(),
            answers: Mutex::default(),
        }
    }

    /// Prefetches the answer of each active prefetch entry of the learned intents file whose
    /// refresh is due at the time before it. Sets its attempt time first. Logs a failed load.
    pub async fn tick(&self) {
        let learned = match self.activation.load().await {
            Ok(learned) => learned.intents,
            Err(error) => return tracing::warn!("the learned intents did not load: {error:#}"),
        };
        for entry in learned {
            let now = self.activation.clock.now();
            if let Some(question) = self.due(&entry, now) {
                lock(&self.attempts).insert(entry.name.clone(), now);
                self.run(&entry.name, &question).await;
            }
        }
    }

    /// The question of `entry` when it is an active prefetch entry whose refresh is due at `now`.
    fn due(&self, entry: &LearnedIntent, now: Timestamp) -> Option<String> {
        let (LearnedStatus::Active, LearnedKind::Prefetch, Some(question), Some(fresh_min)) =
            (entry.status, entry.kind, &entry.question, entry.fresh_min)
        else {
            return None;
        };
        let zone = self.habits.zone()?;
        let today = zone.to_datetime(now).date();
        let (half_life_days, min_score) = (self.habits.half_life_days, self.min_score);
        let model = self.habits.current()?;
        let usual = model.usual_times(&entry.name, today, half_life_days, min_score);
        let attempt = lock(&self.attempts).get(&entry.name).copied();
        let due = learn::refresh_due(&usual, attempt, now, &zone, self.lead_min, fresh_min);
        due.then(|| question.clone())
    }

    /// Stores the answer to the prefetch question `question` of the intent `name` after its
    /// pre-render, as `store` does. A failed answer or pre-render logs and keeps the stored answer.
    async fn run(&self, name: &str, question: &str) {
        let speech = &self.activation.speech;
        let answer = match self.conversation.prefetch(question).await {
            Ok(Some(text)) => speech.render(&text).await.map(|()| Some(text)),
            other => other,
        };
        match answer {
            Ok(answer) => self.store(name, answer),
            Err(error) => tracing::warn!("the prefetch of {name} failed: {error:#}"),
        }
    }

    /// Stores `answer` of the intent `name` with the current time, or removes the stored answer
    /// without `answer`. Drops the audio of a replaced or removed answer that no stored answer has.
    fn store(&self, name: &str, answer: Option<String>) {
        let time = self.activation.clock.now();
        let mut answers = lock(&self.answers);
        let old = match answer {
            Some(text) => answers.insert(name.to_owned(), Answer { text, time }),
            None => answers.remove(name),
        };
        let stored = |old: &Answer| answers.values().any(|answer| answer.text == old.text);
        if let Some(old) = old.filter(|old| !stored(old)) {
            self.activation.speech.forget(&old.text);
        }
    }
}

/// Starts a task that runs the `tick` of `prefetch` at once and each minute.
#[must_use]
pub fn start_prefetch(prefetch: Prefetch) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_mins(1));
        loop {
            ticks.tick().await;
            prefetch.tick().await;
        }
    })
}

/// The value of `mutex`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("no lock holder panics")
}

/// Prunes `log` at the `clock` time once a day, the first time one day after the start. Logs a
/// failed prune.
pub async fn prune(log: Arc<dyn InteractionLog>, clock: Arc<dyn Clock>) {
    loop {
        tokio::time::sleep(PRUNE_INTERVAL).await;
        if let Err(error) = log.prune(clock.now()).await {
            tracing::warn!("the prune failed: {error:#}");
        }
    }
}

/// Starts a task that waits for the first snapshot in `cache`, then gives `stt` the hotwords of
/// `home`, `cache` and `catalog` at once and each [`HOTWORDS_REFRESH`].
pub fn start_hotwords(
    home: Arc<dyn Home>,
    mut cache: watch::Receiver<StateCache>,
    catalog: Arc<Catalog>,
    stt: Arc<dyn SpeechToText>,
) {
    tokio::spawn(async move {
        let Ok(_) = cache.wait_for(|cache| cache.config.is_some()).await else {
            return;
        };
        let mut refresh = tokio::time::interval(HOTWORDS_REFRESH);
        loop {
            refresh.tick().await;
            refresh_hotwords(home.as_ref(), &cache, &catalog, stt.as_ref()).await;
        }
    });
}

/// Gives `stt` the hotwords of the hot cache `cache` and `catalog`, with the playlist, artist and
/// album names of the Music Assistant library on `home`. Logs a failed library call and the
/// number of hotwords.
async fn refresh_hotwords(
    home: &dyn Home,
    cache: &watch::Receiver<StateCache>,
    catalog: &Catalog,
    stt: &dyn SpeechToText,
) {
    let entry = speech::music_entry(&cache.borrow()).map(str::to_owned);
    let names = match entry {
        Some(entry) => music_names(home, &entry).await,
        None => Vec::new(),
    };
    let words = speech::hotwords(&cache.borrow(), catalog, &names);
    tracing::info!("the second pass has {} hotwords", words.len());
    stt.set_hotwords(&words);
}

/// The playlist, artist and album names of the library of the Music Assistant config entry
/// `entry` on `home`. Logs a failed call.
async fn music_names(home: &dyn Home, entry: &str) -> Vec<String> {
    let mut names = Vec::new();
    for media_type in LIBRARY_TYPES {
        match library(home, entry, media_type).await {
            Ok(found) => names.extend(found),
            Err(error) => tracing::warn!("the {media_type} library call failed: {error:#}"),
        }
    }
    names
}

/// The names of the 500 most played items of `media_type` in the library of the Music
/// Assistant config entry `entry` on `home`.
///
/// # Errors
///
/// The call fails or its response does not parse.
async fn library(home: &dyn Home, entry: &str, media_type: &str) -> anyhow::Result<Vec<String>> {
    let data = serde_json::json!({ "config_entry_id": entry, "media_type": media_type,
        "limit": 500, "order_by": "play_count_desc" });
    let call = ServiceCall {
        domain: "music_assistant".to_owned(),
        service: "get_library".to_owned(),
        target: Vec::new(),
        data: serde_json::from_value(data)?,
        return_response: true,
    };
    let response = home.call(call).await?.unwrap_or_default();
    let library: Library = serde_json::from_value(response)?;
    Ok(library.items.into_iter().map(|item| item.name).collect())
}

/// The response data of a `music_assistant.get_library` call.
#[derive(Deserialize)]
struct Library {
    /// The items of the library.
    items: Vec<LibraryItem>,
}

/// One item of a library.
#[derive(Deserialize)]
struct LibraryItem {
    /// The name that Music Assistant shows.
    name: String,
}

/// Tests of the sync, backfill, habit, learned intent, prune and hotword use cases and the
/// conformance to `cache_sync.qnt` and `backfill.qnt`.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use futures::channel::oneshot;
    use jiff::Timestamp;
    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;
    use serde_json::json;
    use tokio::runtime::Runtime;
    use tokio::time::Instant;

    use super::*;
    use crate::Config;
    use crate::CurrentClassifier;
    use crate::app::conversation;
    use crate::app::conversation::Queues;
    use crate::app::conversation::Recorder;
    use crate::app::conversation::SystemTwo;
    use crate::app::speech::Utterances;
    use crate::app::training::FIXTURE;
    use crate::domain::catalog::Filter;
    use crate::domain::catalog::Target;
    use crate::domain::habits::Bucket;
    use crate::domain::habits::DayType;
    use crate::domain::learn::LearnedKind;
    use crate::domain::state::Snapshot;
    use crate::domain::state::State;
    use crate::testing::fakes;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeLog;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::FakeStt;
    use crate::testing::fakes::FakeTts;
    use crate::testing::fakes::Scripted;
    use crate::testing::fakes::SetClock;
    use crate::testing::fakes::house;
    use crate::testing::fakes::log_dispatch;

    /// The entity that the tests change.
    const KITCHEN: &str = "light.kitchen";

    /// Starts sync of a new cache with `home`, sends `Reconnected` and waits until sync waits.
    async fn start(home: FakeHome) -> (Arc<FakeHome>, watch::Receiver<StateCache>) {
        let (home, cache) = spawn(home).await;
        home.send(Event::Reconnected);
        settle().await;
        (home, cache)
    }

    /// Starts sync of a new cache with `home` and waits until it waits.
    async fn spawn(home: FakeHome) -> (Arc<FakeHome>, watch::Receiver<StateCache>) {
        let home = Arc::new(home);
        let (sender, cache) = watch::channel(StateCache::default());
        let task_home = Arc::clone(&home);
        tokio::spawn(async move { sync(task_home.as_ref(), &sender).await });
        settle().await;
        (home, cache)
    }

    /// Waits until every task waits. A paused clock advances only then.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    /// A state of `entity_id` with `value`, newer than each state of the reference house.
    fn state(entity_id: &str, value: &str) -> State {
        let mut state = house().states.swap_remove(0);
        (state.entity_id, state.state) = (entity_id.to_owned(), value.to_owned());
        state.last_updated = Timestamp::MAX;
        state
    }

    /// A change of `entity_id` to `value`.
    fn changed(entity_id: &str, value: &str) -> Event {
        let new = state(entity_id, value);
        Event::StateChanged {
            entity_id: new.entity_id.clone(),
            old: None,
            context: new.context.clone(),
            new: Some(Box::new(new)),
        }
    }

    /// AC-2.9-1.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_1_first_reconnect_loads_snapshot() {
        let (home, cache) = spawn(FakeHome::default()).await;
        assert_eq!(home.snapshots(), 0, "sync loaded before a connection");
        home.send(Event::Reconnected);
        settle().await;
        assert_eq!(*cache.borrow(), StateCache::load(house()));
    }

    /// AC-2.9-2.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_2_state_change_applies_at_once() {
        let (home, cache) = start(FakeHome::default()).await;
        let before = Instant::now();
        home.send(changed(KITCHEN, "off"));
        tokio::task::yield_now().await;
        assert_eq!(cache.borrow().state(KITCHEN), Some(&state(KITCHEN, "off")));
        assert_eq!(Instant::now(), before, "virtual time advanced");
    }

    /// AC-2.9-3.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_3_registry_update_loads_snapshot() {
        let (home, _cache) = start(FakeHome::default()).await;
        home.send(Event::RegistryUpdated);
        settle().await;
        assert_eq!(home.snapshots(), 2);
    }

    /// AC-2.9-4.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_4_reconnect_replaces_cache() {
        let (home, cache) = start(FakeHome::default()).await;
        let mut next = house();
        next.states.truncate(1);
        (next.areas, next.devices, next.entities) = (Vec::new(), Vec::new(), Vec::new());
        home.set_snapshot(next.clone());
        home.send(Event::Reconnected);
        settle().await;
        assert_eq!(*cache.borrow(), StateCache::load(next));
    }

    /// AC-2.9-6.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_6_change_during_load_applies_after() {
        let home = FakeHome::default();
        let gate = home.delay();
        let (home, cache) = start(home).await;
        home.send(changed(KITCHEN, "off"));
        settle().await;
        gate.send(()).expect("the snapshot call waits");
        settle().await;
        assert_eq!(cache.borrow().state(KITCHEN), Some(&state(KITCHEN, "off")));
    }

    /// Starts sync with `failures` failed `snapshot` calls. Checks that `snapshot` call `k + 2`
    /// starts `retries[k]` seconds after `Reconnected`, and not before.
    async fn assert_retries(failures: usize, retries: &[u64]) -> watch::Receiver<StateCache> {
        let home = FakeHome::default();
        for _ in 0..failures {
            drop(home.delay());
        }
        let (home, cache) = spawn(home).await;
        let begin = Instant::now();
        home.send(Event::Reconnected);
        for (calls, &retry) in (1..).zip(retries) {
            let at = begin + Duration::from_secs(retry);
            tokio::time::sleep_until(at - Duration::from_millis(1)).await;
            assert_eq!(
                home.snapshots(),
                calls,
                "the retry started before {retry} s"
            );
            tokio::time::sleep_until(at + Duration::from_millis(1)).await;
            assert_eq!(home.snapshots(), calls + 1, "no retry at {retry} s");
        }
        cache
    }

    /// AC-2.9-7.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_7_failed_start_loads_after_one_second() {
        let cache = assert_retries(1, &[1]).await;
        assert_eq!(*cache.borrow(), StateCache::load(house()));
    }

    /// The `habits.save_interval_s` of the tests.
    const SAVE_INTERVAL: Duration = Duration::from_secs(300);

    /// Monday 08:10 in the HA time zone of the reference house.
    fn monday() -> Timestamp {
        "2026-01-05T08:10:00-06:00".parse().expect("a time")
    }

    /// The task of empty habits of the reference house with `FakeStore` and the clock at
    /// `monday()`, and that store.
    fn habit_task() -> (HabitsTask, Arc<FakeStore<RequestHabits>>) {
        let cache = watch::channel(StateCache::load(house())).1;
        let habits = Arc::new(Habits::new(RequestHabits::default(), 14.0, cache));
        let (store, clock) = (Arc::new(FakeStore::new(None)), FakeClock(monday()));
        let saving = Arc::clone(&store) as _;
        let task = start_habits(habits, saving, Arc::new(clock), SAVE_INTERVAL);
        (task, store)
    }

    /// AC-11.4b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_11_4b_1_interval_saves_the_new_observations() {
        let (task, store) = habit_task();
        task.habits.observe("get_traffic".to_owned(), monday());
        tokio::time::sleep(SAVE_INTERVAL + Duration::from_secs(1)).await;
        let saved = store
            .saves()
            .pop()
            .map(|model| model.strengths.into_keys().collect());
        let bucket = Bucket {
            day: DayType::Weekdays,
            hour: 8,
        };
        assert_eq!(saved, Some(vec![("get_traffic".to_owned(), bucket)]));
    }

    /// AC-11.4b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_11_4b_2_stop_saves() -> anyhow::Result<()> {
        let (task, store) = habit_task();
        drop(task.stop);
        task.task.await?;
        assert_eq!(store.saves().len(), 1);
        Ok(())
    }

    /// AC-12.6b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_12_6b_2_one_day_gives_one_prune() {
        let log = Arc::new(FakeLog::default());
        let clock = Arc::new(FakeClock(Timestamp::UNIX_EPOCH));
        tokio::spawn(prune(Arc::clone(&log) as _, clock));
        tokio::time::sleep(PRUNE_INTERVAL + Duration::from_secs(1)).await;
        let prunes = log.prunes.lock().expect("no lock holder panics").clone();
        assert_eq!(prunes, [Timestamp::UNIX_EPOCH]);
    }

    /// AC-8.4-1.
    #[test]
    fn ac_8_4_1_use_starts_one_generate() {
        let mut driver = BackfillDriver::new();
        driver.record_use("a");
        assert_eq!(driver.reasoner.generated(), ["a"]);
    }

    /// AC-8.4-2.
    #[test]
    fn ac_8_4_2_running_backfill_blocks_second() {
        let mut driver = BackfillDriver::new();
        driver.record_use("a");
        driver.record_use("a");
        assert_eq!(driver.reasoner.generated(), ["a"]);
    }

    /// AC-8.4-4.
    #[test]
    fn ac_8_4_4_failed_generate_keeps_pools() {
        let mut driver = BackfillDriver::new();
        driver.record_use("a");
        let used = driver.pools.clone();
        driver.finish("a", false);
        assert_eq!(driver.pools, used);
        assert!(
            driver.backfill.running().is_empty(),
            "the pool stays in flight"
        );
    }

    /// AC-2.9-8.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_8_failed_snapshot_logs_one_warn_event() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        // The current-thread runtime polls the sync task on this thread, so it logs here too.
        let _default = tracing::dispatcher::set_default(&log_dispatch(&log)?);
        let home = FakeHome::default();
        let gate = home.delay();
        start(home).await;
        drop(gate);
        settle().await;
        let text = fs::read_to_string(log.path())?;
        let events: Vec<&str> = text
            .lines()
            .filter(|line| line.contains(" WARN "))
            .collect();
        assert_eq!(events.len(), 1, "the log is {text:?}");
        let error = oneshot::Canceled.to_string();
        assert!(
            events[0].contains(&error),
            "{error:?} is not in {:?}",
            events[0]
        );
        Ok(())
    }

    /// AC-2.9-9.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_9_retries_wait_one_two_and_four_seconds() {
        assert_retries(3, &[1, 3, 7]).await;
    }

    /// The candidates of the pre-render tests.
    const NEW: [&str; 2] = ["All set.", "There you go."];

    /// True when `speech` speaks each of `NEW` without a new synthesis of `tts`.
    async fn cached(speech: &Speech, tts: &FakeTts) -> anyhow::Result<bool> {
        let synths = tts.texts().len();
        for text in NEW {
            speech.speak(text).await?;
        }
        Ok(tts.texts().len() == synths)
    }

    /// AC-8.4b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_8_4b_1_generate_saves_and_caches_new_responses() -> anyhow::Result<()> {
        let (reasoner, tts) = (
            Arc::new(FakeReasoner::default()),
            Arc::new(FakeTts::default()),
        );
        let store = Arc::new(FakeStore::new(None));
        let task = conversation::start_pools(pools(), Arc::clone(&store) as _)?;
        let backfill = backfill(Arc::clone(&reasoner), task.changes, &tts);
        backfill.record_use("a".to_owned(), 0);
        // The pools task runs on a blocking thread, which stops the paused clock.
        while reasoner.generated().is_empty() {
            tokio::task::yield_now().await;
        }
        reasoner.release("a", Ok(NEW.map(str::to_owned).to_vec()));
        let speech = Arc::clone(&backfill.speech);
        drop(backfill);
        task.task.await?;
        let saved: Pools = store.saves().pop().expect("the task saves each change");
        assert_eq!(texts(&saved["a"])[START..], NEW);
        assert!(cached(&speech, &tts).await?, "a new response has no audio");
        Ok(())
    }

    /// AC-8.4b-3.
    #[test]
    fn ac_8_4b_3_audio_before_merge() -> anyhow::Result<()> {
        let mut driver = BackfillDriver::new();
        driver.record_use("a");
        let new = NEW.map(str::to_owned).to_vec();
        driver.reasoner.release("a", Ok(new));
        driver.runtime.block_on(async {
            // Each synthesis lets this task run, so the loop sees the merge when it arrives.
            let mut tries = 0..100;
            while driver.changes.try_recv().is_err() && tries.next().is_some() {
                tokio::task::yield_now().await;
            }
            assert!(!tries.is_empty(), "no merge arrived");
            let cached = cached(&driver.backfill.speech, &driver.tts).await?;
            assert!(cached, "the merge came before the audio");
            Ok(())
        })
    }

    /// The catalog of `intents.toml`.
    fn catalog() -> Catalog {
        toml::from_str(include_str!("../../intents.toml")).expect("it is a catalog")
    }

    /// The speaker that the hotword tests make a Music Assistant player.
    const SPEAKER: &str = "media_player.carson_kitchen";

    /// The reference house with [`SPEAKER`] as a Music Assistant player.
    fn music_house() -> StateCache {
        let mut snapshot = house();
        let mut entities = snapshot.entities.iter_mut();
        let speaker = entities.find(|entity| entity.entity_id.0 == SPEAKER);
        speaker.expect("the house has it").config_entry_id = Some("music_assistant".to_owned());
        let mut states = snapshot.states.iter_mut();
        let state = states.find(|state| state.entity_id == SPEAKER);
        let attributes = &mut state.expect("the house has it").attributes;
        attributes.insert("mass_player_type".to_owned(), json!("player"));
        StateCache::load(snapshot)
    }

    /// A `FakeHome` whose library has the playlist "Jazzy", the artist "Radiohead" and the album
    /// "OK Computer".
    fn library_home() -> FakeHome {
        let home = FakeHome::default();
        let names = [
            ("playlist", "Jazzy"),
            ("artist", "Radiohead"),
            ("album", "OK Computer"),
        ];
        for (media_type, name) in names {
            home.respond_library(media_type, json!({ "items": [{ "name": name }] }));
        }
        home
    }

    /// The calls of `home` and the hotword lists of `FakeStt` after one refresh with `cache` and
    /// the catalog of `intents.toml`.
    async fn refreshed(home: &FakeHome, cache: StateCache) -> (Vec<ServiceCall>, Vec<Vec<String>>) {
        let stt = FakeStt::default();
        refresh_hotwords(home, &watch::channel(cache).1, &catalog(), &stt).await;
        let lists = stt.hotwords.into_inner().expect("no lock holder panics");
        (home.calls(), lists)
    }

    /// One list: the hotwords of `cache` and the catalog of `intents.toml` with `music`.
    fn expected(cache: &StateCache, music: &[&str]) -> Vec<Vec<String>> {
        let music: Vec<String> = music.iter().map(|&name| name.to_owned()).collect();
        vec![speech::hotwords(cache, &catalog(), &music)]
    }

    /// AC-18.18b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_18_18b_1_three_library_calls_in_order() {
        let (calls, _) = refreshed(&FakeHome::default(), music_house()).await;
        let call = |media_type: &str| ServiceCall {
            domain: "music_assistant".to_owned(),
            service: "get_library".to_owned(),
            target: Vec::new(),
            data: serde_json::from_value(json!({ "config_entry_id": "music_assistant",
                "media_type": media_type, "limit": 500, "order_by": "play_count_desc" }))
            .expect("the data is an object"),
            return_response: true,
        };
        assert_eq!(calls, ["playlist", "artist", "album"].map(call));
    }

    /// AC-18.18b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_18_18b_2_library_names_are_hotwords() {
        let cache = music_house();
        let (_, lists) = refreshed(&library_home(), cache.clone()).await;
        assert_eq!(
            lists,
            expected(&cache, &["Jazzy", "Radiohead", "OK Computer"])
        );
    }

    /// AC-18.18b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_18_18b_3_failed_call_gives_no_names_of_its_type() {
        let (home, cache) = (library_home(), music_house());
        let playlist = home.delay();
        drop(home.delay());
        playlist.send(()).expect("the playlist call waits");
        let (_, lists) = refreshed(&home, cache.clone()).await;
        assert_eq!(lists, expected(&cache, &["Jazzy", "OK Computer"]));
    }

    /// AC-18.18b-4.
    #[tokio::test(start_paused = true)]
    async fn ac_18_18b_4_no_music_player_gives_no_calls() {
        let cache = StateCache::load(house());
        let (calls, lists) = refreshed(&library_home(), cache.clone()).await;
        assert!(calls.is_empty(), "the calls are {calls:?}");
        assert_eq!(lists, expected(&cache, &[]));
    }

    /// Renames the office fan of `cache` to "Study Fan".
    fn rename_fan(cache: &mut StateCache) {
        let mut entities = Arc::make_mut(&mut cache.entities).iter_mut();
        let fan = entities.find(|entity| entity.entity_id.0 == "fan.office_fan");
        fan.expect("the house has it").name = "Study Fan".to_owned();
    }

    /// AC-18.18b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_18_18b_5_first_snapshot_then_each_hour() {
        let (sender, cache) = watch::channel(StateCache::default());
        let stt = Arc::new(FakeStt::default());
        let (home, catalog) = (Arc::new(FakeHome::default()), Arc::new(catalog()));
        start_hotwords(home, cache, catalog, Arc::clone(&stt) as _);
        let lists = || stt.hotwords.lock().expect("no lock holder panics").clone();
        settle().await;
        assert!(lists().is_empty(), "a list before the first snapshot");
        sender.send_replace(StateCache::load(house()));
        settle().await;
        assert_eq!(lists().len(), 1);
        sender.send_modify(rename_fan);
        tokio::time::sleep(HOTWORDS_REFRESH).await;
        let lists = lists();
        assert_eq!(lists.len(), 2);
        assert!(lists[1].contains(&"Study Fan".to_owned()), "{lists:?}");
    }

    /// The model state that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct CacheSync {
        /// The value of each cached entity.
        cache: BTreeMap<String, i64>,
    }

    impl ModelState<CacheSyncDriver> for CacheSync {
        /// The cache after every task waits.
        fn from_driver(driver: &CacheSyncDriver) -> anyhow::Result<Self> {
            driver.runtime.block_on(settle());
            let synced = driver.cache.borrow();
            let values = synced
                .states
                .iter()
                .map(|(id, state)| Ok((id.to_string(), state.state.parse()?)));
            let cache: anyhow::Result<_> = values.collect();
            Ok(Self { cache: cache? })
        }
    }

    /// Replays `cache_sync.qnt` against sync, with `FakeHome` as HA.
    struct CacheSyncDriver {
        /// The paused runtime that runs sync.
        runtime: Runtime,
        /// HA.
        home: Arc<FakeHome>,
        /// The cache that sync keeps.
        cache: watch::Receiver<StateCache>,
        /// The HA state of each entity.
        states: BTreeMap<String, i64>,
        /// The changes that HA sent on the open connection and sync did not receive yet.
        queue: Option<VecDeque<Event>>,
        /// The gate of the delayed snapshot call.
        gate: Option<oneshot::Sender<()>>,
        /// A snapshot call failed and sync waits to retry it.
        retry: bool,
    }

    impl CacheSyncDriver {
        /// A driver that syncs with HA in `states`, on a new runtime.
        fn new(states: BTreeMap<String, i64>) -> Self {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            let runtime = runtime.enable_time().start_paused(true).build();
            let runtime = runtime.expect("the runtime builds");
            let (home, cache) = runtime.block_on(start(FakeHome::new(Self::snapshot(&states))));
            Self {
                runtime,
                home,
                cache,
                states,
                queue: Some(VecDeque::new()),
                gate: None,
                retry: false,
            }
        }

        /// The snapshot of the HA states `states`.
        fn snapshot(states: &BTreeMap<String, i64>) -> Snapshot {
            let mut snapshot = house();
            let states = states.iter().map(|(id, v)| state(id, &v.to_string()));
            snapshot.states = states.collect();
            snapshot
        }

        /// Starts a delayed snapshot call: the pending retry, else the load after `Reconnected`.
        fn take_snapshot(&mut self) {
            self.gate = Some(self.home.delay());
            if mem::take(&mut self.retry) {
                self.runtime
                    .block_on(async { tokio::time::sleep(MAX_RETRY).await });
            } else {
                self.home.send(Event::Reconnected);
            }
        }
    }

    impl Driver for CacheSyncDriver {
        type State = CacheSync;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init(start) => *self = Self::new(start),
                change(e: String, v: i64) => {
                    self.states.insert(e.clone(), v);
                    self.home.set_snapshot(Self::snapshot(&self.states));
                    if let Some(queue) = &mut self.queue { queue.push_back(changed(&e, &v.to_string())) }
                },
                deliver => if let Some(event) = self.queue.as_mut().and_then(VecDeque::pop_front) {
                    self.home.send(event);
                },
                disconnect => {
                    self.queue = None;
                    self.retry |= self.gate.take().is_some();
                },
                reconnect => self.queue = Some(VecDeque::new()),
                takeSnapshot => self.take_snapshot(),
                applySnapshot => self.gate.take().map(|gate| gate.send(())),
                failSnapshot => (self.gate, self.retry) = (None, true),
            })
        }
    }

    /// AC-2.9-5. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/cache_sync.qnt", max_steps = 30, seed = "1")]
    fn ac_2_9_5_conformance() -> impl Driver {
        CacheSyncDriver::new(BTreeMap::new())
    }

    /// The number of responses of each pool at the start of `backfill.qnt`: `MIN`, all unused.
    const START: usize = 3;

    /// The model state of `backfill.qnt` that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct BackfillState {
        /// 1 for each pool with a backfill in progress, else 0.
        running: BTreeMap<String, usize>,
        /// The number of merges of each pool.
        version: BTreeMap<String, usize>,
    }

    impl ModelState<BackfillDriver> for BackfillState {
        /// The in-flight set, and the merges from the new responses: each merge adds a batch.
        fn from_driver(driver: &BackfillDriver) -> anyhow::Result<Self> {
            let (running, batch) = (driver.backfill.running(), driver.backfill.config.batch);
            let flag = |name: &String| (name.clone(), usize::from(running.contains(name)));
            let merges = |(name, pool): (&String, &Pool)| {
                (name.clone(), (pool.responses.len() - START) / batch)
            };
            Ok(Self {
                running: driver.pools.keys().map(flag).collect(),
                version: driver.pools.iter().map(merges).collect(),
            })
        }
    }

    /// A backfill with `reasoner`, `changes` and `tts`, the `[backfill]` of `jarvis.toml` with no
    /// pool limit, the catalog of `intents.toml` and the hot cache of the reference house.
    fn backfill(
        reasoner: Arc<FakeReasoner>,
        changes: mpsc::Sender<Change>,
        tts: &Arc<FakeTts>,
    ) -> Arc<Backfill> {
        let mut config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
        config.backfill.max_pool = usize::MAX;
        let speech = Arc::new(Speech::new(Arc::clone(tts) as _));
        let cache = watch::channel(StateCache::load(house())).1;
        let catalog = Arc::new(ArcSwap::from_pointee(catalog()));
        let backfill = Backfill::new(config.backfill, reasoner, changes, speech, catalog, cache);
        Arc::new(backfill)
    }

    /// The pools `a` and `b`, each with `START` unused responses.
    fn pools() -> Pools {
        let texts: Vec<String> = (1..=START).map(|n| format!("Response {n}.")).collect();
        let empty = Pool {
            responses: Vec::new(),
            last: None,
        };
        let pool = responses::merge(empty, &texts, START);
        Pools::from([("a".to_owned(), pool.clone()), ("b".to_owned(), pool)])
    }

    /// Replays `backfill.qnt` against the backfill, with `FakeReasoner` as System Two and the
    /// pools `a` and `b`.
    struct BackfillDriver {
        /// The paused runtime that runs the backfills.
        runtime: Runtime,
        /// System Two, which holds each `generate` call.
        reasoner: Arc<FakeReasoner>,
        /// The use case.
        backfill: Arc<Backfill>,
        /// The TTS of its pre-render.
        tts: Arc<FakeTts>,
        /// The receiver of its changes.
        changes: mpsc::Receiver<Change>,
        /// The pools after each change.
        pools: Pools,
    }

    impl BackfillDriver {
        /// A driver at the start of the model.
        fn new() -> Self {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            let runtime = runtime.enable_time().start_paused(true).build();
            let (reasoner, tts) = (Arc::new(FakeReasoner::default()), Arc::default());
            let (changes, receiver) = mpsc::channel();
            Self {
                runtime: runtime.expect("the runtime builds"),
                backfill: backfill(Arc::clone(&reasoner), changes, &tts),
                reasoner,
                tts,
                changes: receiver,
                pools: pools(),
            }
        }

        /// Applies the changes, lets each task run until it waits and applies the new changes.
        fn run(&mut self) {
            let _context = self.runtime.enter();
            self.apply();
            self.runtime.block_on(settle());
            self.apply();
        }

        /// Applies each change that the backfill sent.
        fn apply(&mut self) {
            let changes = self.changes.try_iter();
            self.pools = changes.fold(mem::take(&mut self.pools), |pools, change| change(pools));
        }

        /// Uses an unused response of `pool`, or the first response when none is unused.
        fn record_use(&mut self, pool: &str) {
            let responses = &self.pools[pool].responses;
            let index = responses.iter().position(|response| response.uses == 0);
            self.backfill
                .record_use(pool.to_owned(), index.unwrap_or(0));
            self.run();
        }

        /// Ends the held `generate` call of `pool` with a batch of new candidates, or with an
        /// error.
        fn finish(&mut self, pool: &str, success: bool) {
            let next = self.pools[pool].responses.len();
            let texts = (next..next + self.backfill.config.batch).map(|n| format!("Reply {n}."));
            let failed = || anyhow::anyhow!("failed");
            let result = success.then(|| texts.collect()).ok_or_else(failed);
            self.reasoner.release(pool, result);
            self.run();
        }
    }

    impl Driver for BackfillDriver {
        type State = BackfillState;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                succeed(p: String) => self.finish(&p, true),
                failBackfill(p: String) => self.finish(&p, false),
                // `use` is a Rust keyword, so `switch!` cannot name it.
                _ => {
                    anyhow::ensure!(step.action_taken == "use", "unknown action {}", step.action_taken);
                    let pick = step.nondet_picks.get("p").ok_or_else(|| anyhow::anyhow!("no pool"))?;
                    self.record_use(&String::deserialize(pick.clone())?);
                },
            })
        }
    }

    /// AC-8.4-5. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/backfill.qnt", max_steps = 30, seed = "1")]
    fn ac_8_4_5_conformance() -> impl Driver {
        BackfillDriver::new()
    }

    /// The learned intent check of the tests, its file and the receiver of its changes.
    struct Learner {
        /// System Two.
        reasoner: Arc<FakeReasoner>,
        /// The log of the check.
        log: Arc<FakeLog>,
        /// The check.
        learning: Learning,
        /// The learned intents file.
        store: Arc<FakeStore<Learned>>,
        /// The receiver of the changes, in place of the pools task.
        changes: mpsc::Receiver<Change>,
        /// The pools after the changes.
        pools: Mutex<Pools>,
        /// The TTS of the pre-render.
        tts: Arc<FakeTts>,
    }

    /// The activation of `store` on `changes`, `intents.toml`, `tts` and the reference house one
    /// day after `monday()`, and empty habits with `half_life_days`.
    fn activation(
        store: Arc<dyn Store<Learned>>,
        changes: mpsc::Sender<Change>,
        tts: &Arc<FakeTts>,
        half_life_days: f64,
    ) -> (Activation, Arc<Habits>) {
        let cache = watch::channel(StateCache::load(house())).1;
        let model = RequestHabits::default();
        let habits = Arc::new(Habits::new(model, half_life_days, cache.clone()));
        let activation = Activation {
            store,
            changes,
            speech: Arc::new(Speech::new(Arc::clone(tts) as _)),
            cache,
            catalog: Arc::new(ArcSwap::from_pointee(catalog())),
            clock: Arc::new(FakeClock(monday() + SignedDuration::from_hours(24))),
        };
        (activation, habits)
    }

    impl Learner {
        /// The check of [`Learner::empty`] on a log with the requests of [`jokes`].
        fn new() -> Self {
            let learner = Self::empty();
            learner.defer(&jokes(), 0);
            learner
        }

        /// A check of the catalog of `intents.toml` and the `[learn]` of `jarvis.toml` on an
        /// empty log, one day after `monday()`.
        fn empty() -> Self {
            let (reasoner, log) = (
                Arc::new(FakeReasoner::default()),
                Arc::new(FakeLog::default()),
            );
            let config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
            let (store, (changes, receiver)) = (Arc::new(FakeStore::new(None)), mpsc::channel());
            let tts = Arc::new(FakeTts::default());
            let half_life_days = config.habits.half_life_days;
            let (activation, habits) =
                activation(Arc::clone(&store) as _, changes, &tts, half_life_days);
            let learning = Learning {
                config: config.learn,
                log: Arc::clone(&log) as _,
                reasoner: Arc::clone(&reasoner) as _,
                catalog: Arc::new(catalog()),
                tools: Vec::new(),
                activation,
                habits,
            };
            Self {
                reasoner,
                log,
                learning,
                store,
                changes: receiver,
                pools: Mutex::default(),
                tts,
            }
        }

        /// Adds the requests `texts` in the hours after `monday()`, with embeddings close to the
        /// unit vector of the dimension `axis`.
        fn defer(&self, texts: &[String], axis: usize) {
            for (hour, text) in (1_u8..).zip(texts) {
                self.log
                    .defer(text, monday() + SignedDuration::from_hours(hour.into()));
                let mut embedding = vec![0.0, 0.0, 0.01 * f32::from(hour)];
                embedding[axis] = 1.0;
                self.reasoner.script_embedding(text, embedding);
            }
        }

        /// Applies the changes of the checks and gives the learned entries.
        fn saved(&self) -> anyhow::Result<Vec<LearnedIntent>> {
            let mut pools = self.pools.lock().expect("no lock holder panics");
            for change in self.changes.try_iter() {
                *pools = change(mem::take(&mut *pools));
            }
            Ok(self.store.load()?.unwrap_or_default().intents)
        }

        /// Runs one check with `proposal` as the next proposal, applies its changes and gives
        /// the learned entries.
        async fn check(&self, proposal: LearnedIntent) -> anyhow::Result<Vec<LearnedIntent>> {
            self.reasoner.script_proposal(proposal);
            self.learning.check().await;
            self.saved()
        }
    }

    /// The texts of the cluster of the learned intent tests.
    fn jokes() -> Vec<String> {
        (1..=5).map(|n| format!("tell me joke {n}")).collect()
    }

    /// A valid proposal of `kind` with `service`.
    fn proposal(kind: LearnedKind, service: Option<&str>) -> LearnedIntent {
        let examples = ["tell me a joke", "say something funny", "make me laugh"];
        LearnedIntent {
            name: "tell_joke".to_owned(),
            centroid: Vec::new(),
            service: service.map(str::to_owned),
            kind,
            examples: examples.map(str::to_owned).to_vec(),
            responses: vec!["I told the wifi a joke. It did not get it.".to_owned()],
            plugin: None,
            target: Some(Target::House),
            entity: None,
            select: vec![Filter {
                domain: "scene".to_owned(),
                name: None,
                device_class: None,
            }],
            data: BTreeMap::new(),
            question: None,
            fresh_min: None,
            needs: (kind == LearnedKind::NewPlugin).then(|| "jokes".to_owned()),
            status: LearnedStatus::None,
            texts: Vec::new(),
            request_file: None,
            activated: None,
        }
    }

    /// The learned entries after one check that gets `proposal`.
    async fn saved(proposal: LearnedIntent) -> anyhow::Result<Vec<LearnedIntent>> {
        Learner::new().check(proposal).await
    }

    /// AC-14.5b-1.
    #[tokio::test]
    async fn ac_14_5b_1_uncovered_cluster_gets_one_proposal() -> anyhow::Result<()> {
        let learner = Learner::new();
        learner.check(proposal(LearnedKind::Command, None)).await?;
        assert_eq!(learner.reasoner.proposed(), vec![jokes()]);
        Ok(())
    }

    /// AC-14.5b-2.
    #[tokio::test]
    async fn ac_14_5b_2_proposal_without_service_is_active() -> anyhow::Result<()> {
        let entries = saved(proposal(LearnedKind::Command, None)).await?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, LearnedStatus::Active);
        assert_eq!(entries[0].texts, jokes());
        assert_eq!(entries[0].centroid.len(), 3);
        Ok(())
    }

    /// AC-14.5b-3.
    #[tokio::test]
    async fn ac_14_5b_3_proposal_with_service_is_pending() -> anyhow::Result<()> {
        let entries = saved(proposal(LearnedKind::Command, Some("scene.turn_on"))).await?;
        assert_eq!(entries[0].status, LearnedStatus::Pending);
        Ok(())
    }

    /// AC-14.5b-4.
    #[tokio::test]
    async fn ac_14_5b_4_none_is_saved_and_not_proposed_again() -> anyhow::Result<()> {
        let learner = Learner::new();
        let entries = learner.check(proposal(LearnedKind::None, None)).await?;
        assert_eq!(entries[0].status, LearnedStatus::None);
        learner.check(proposal(LearnedKind::Command, None)).await?;
        assert_eq!(learner.reasoner.proposed().len(), 1);
        Ok(())
    }

    /// AC-14.5b-5.
    #[tokio::test]
    async fn ac_14_5b_5_invalid_proposal_is_logged_and_saved_as_none() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        let _default = tracing::dispatcher::set_default(&log_dispatch(&log)?);
        let mut invalid = proposal(LearnedKind::Command, None);
        invalid.name = "unknown".to_owned();
        let learner = Learner::new();
        let entries = learner.check(invalid).await?;
        assert_eq!(entries[0].status, LearnedStatus::None);
        let text = fs::read_to_string(log.path())?;
        assert!(
            text.contains("the proposal unknown is not valid"),
            "the log is {text:?}"
        );
        learner.check(proposal(LearnedKind::Command, None)).await?;
        assert_eq!(learner.reasoner.proposed().len(), 1);
        Ok(())
    }

    /// AC-14.5b-6.
    #[tokio::test]
    async fn ac_14_5b_6_new_plugin_is_requested() -> anyhow::Result<()> {
        let entries = saved(proposal(LearnedKind::NewPlugin, None)).await?;
        assert_eq!(entries[0].status, LearnedStatus::Requested);
        assert_eq!(entries[0].needs.as_deref(), Some("jokes"));
        assert_eq!(entries[0].request_file, None);
        Ok(())
    }

    /// AC-14.5c-1.
    #[tokio::test(start_paused = true)]
    async fn ac_14_5c_1_task_checks_at_once_and_each_interval() -> anyhow::Result<()> {
        let learner = Learner::new();
        learner
            .reasoner
            .script_proposal(proposal(LearnedKind::Command, None));
        let mut greeting = proposal(LearnedKind::Command, None);
        greeting.name = "say_hello".to_owned();
        learner.reasoner.script_proposal(greeting);
        let interval = Duration::from_secs(learner.learning.config.check_interval_s);
        let _task = start_learning(learner.learning.clone(), interval);
        settle().await;
        assert_eq!(learner.reasoner.proposed(), vec![jokes()]);
        learner.saved()?;
        let greetings: Vec<String> = (1..=5).map(|n| format!("hello there {n}")).collect();
        learner.defer(&greetings, 1);
        tokio::time::sleep(interval).await;
        assert_eq!(learner.reasoner.proposed(), vec![jokes(), greetings]);
        Ok(())
    }

    /// The variant of a Quint sum type value.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Tag {
        /// The variant name.
        tag: String,
    }

    /// The model state of `learned.qnt` that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct LearnedModel {
        /// The status of each cluster.
        status: BTreeMap<String, Tag>,
    }

    impl ModelState<LearnedDriver> for LearnedModel {
        /// The status of the entry of each cluster, or `Absent` without an entry.
        fn from_driver(driver: &LearnedDriver) -> anyhow::Result<Self> {
            let entries = driver.learner.saved()?;
            let status = |cluster: &str| {
                let entry = entries.iter().find(|entry| entry.name == cluster);
                let tag = entry.map_or("Absent".to_owned(), |entry| format!("{:?}", entry.status));
                (cluster.to_owned(), Tag { tag })
            };
            Ok(Self {
                status: CLUSTERS.into_iter().map(status).collect(),
            })
        }
    }

    /// The clusters of `learned.qnt`, in the order of their embedding axes.
    const CLUSTERS: [&str; 2] = ["c1", "c2"];

    /// Replays `learned.qnt` against the learned intent check, with an entry named after each
    /// cluster.
    struct LearnedDriver {
        /// The runtime of the checks.
        runtime: Runtime,
        /// The check, its log, System Two and its file.
        learner: Learner,
        /// `HAS_SERVICE` of the model.
        service: BTreeMap<String, bool>,
        /// `NEEDS_PLUGIN` of the model.
        plugin: BTreeMap<String, bool>,
    }

    impl LearnedDriver {
        /// A driver with an empty log and file and the constants `service` and `plugin`.
        fn new(service: BTreeMap<String, bool>, plugin: BTreeMap<String, bool>) -> Self {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            let runtime = runtime.enable_time().start_paused(true).build();
            Self {
                runtime: runtime.expect("the runtime builds"),
                learner: Learner::empty(),
                service,
                plugin,
            }
        }

        /// Adds the requests of `cluster` and runs one check with its proposal: `none` when not
        /// `suitable`, else a `new_plugin`, a command with a service or a command without one.
        fn propose(&self, cluster: &str, suitable: bool) -> anyhow::Result<()> {
            let axis = CLUSTERS.iter().position(|name| *name == cluster);
            let axis = axis.ok_or_else(|| anyhow::anyhow!("no cluster {cluster}"))?;
            let texts: Vec<String> = (1..=5).map(|n| format!("{cluster} request {n}")).collect();
            self.learner.defer(&texts, axis);
            let kind = match (suitable, self.plugin[cluster]) {
                (false, _) => LearnedKind::None,
                (true, true) => LearnedKind::NewPlugin,
                (true, false) => LearnedKind::Command,
            };
            let service = self.service[cluster].then_some("scene.turn_on");
            let mut proposal = proposal(kind, service.filter(|_| kind == LearnedKind::Command));
            proposal.name = cluster.to_owned();
            self.runtime.block_on(self.learner.check(proposal))?;
            Ok(())
        }

        /// Sets the status of the entry of `cluster` as the review of sub-phase 14.6 does.
        fn resolve(&self, cluster: &str, status: LearnedStatus) -> anyhow::Result<()> {
            let mut learned = self.learner.store.load()?.unwrap_or_default();
            for entry in &mut learned.intents {
                if entry.name == cluster {
                    entry.status = status;
                }
            }
            self.learner.store.save(&learned)
        }
    }

    impl Driver for LearnedDriver {
        type State = LearnedModel;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init(service: BTreeMap<String, bool>, plugin: BTreeMap<String, bool>) => {
                    *self = Self::new(service, plugin);
                },
                propose(c: String, suitable: bool) => self.propose(&c, suitable)?,
                approve(c: String) => self.resolve(&c, LearnedStatus::Active)?,
                reject(c: String) => self.resolve(&c, LearnedStatus::Rejected)?,
                // Sub-phase 14.13 adds the plugin requests, which change no status.
                writeRequest => {},
                failRequest => {},
            })
        }
    }

    /// AC-14.5d-1. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/learned.qnt", max_steps = 30, seed = "1")]
    fn ac_14_5d_1_conformance() -> impl Driver {
        LearnedDriver::new(BTreeMap::new(), BTreeMap::new())
    }

    /// The days after Monday and the second embedding values of five requests at 07:10 on
    /// Monday, Tuesday and Wednesday along the axis [`X`].
    const DAYS: [(i64, f32); 5] = [(0, 0.0), (0, 0.0), (1, 0.0), (1, 0.0), (2, 0.0)];

    /// The habit model after each of `checks` checks with the learned entries `intents`, of a
    /// log with a request at 07:10 on each day after Monday of `requests`, with the embedding
    /// `[1, y, 0.01]` of its value `y`.
    async fn calendar_habits(
        intents: Vec<LearnedIntent>,
        requests: &[(i64, f32)],
        checks: usize,
    ) -> anyhow::Result<Vec<RequestHabits>> {
        let learner = Learner::empty();
        learner.store.save(&Learned { intents })?;
        for (index, (day, y)) in requests.iter().enumerate() {
            let text = format!("what is on the calendar {index}");
            learner
                .log
                .defer(&text, monday() + SignedDuration::from_hours(24 * day - 1));
            learner
                .reasoner
                .script_embedding(&text, vec![1.0, *y, 0.01]);
        }
        let mut models = Vec::new();
        for _ in 0..checks {
            learner.learning.check().await;
            models.push(learner.learning.habits.current().unwrap_or_default());
        }
        Ok(models)
    }

    /// AC-14.5e-1.
    #[tokio::test]
    async fn ac_14_5e_1_covered_cluster_records_its_usual_time() -> anyhow::Result<()> {
        let models = calendar_habits(vec![calendar(LearnedStatus::Active, X)], &DAYS, 1).await?;
        assert_eq!(usual(&models[0])?, vec![SEVEN]);
        Ok(())
    }

    /// The bucket of the calendar requests.
    const SEVEN: Bucket = Bucket {
        day: DayType::Weekdays,
        hour: 7,
    };

    /// The usual times of `family_calendar` in `model` on the Wednesday after `monday()`.
    fn usual(model: &RequestHabits) -> anyhow::Result<Vec<Bucket>> {
        let last = monday() + SignedDuration::from_hours(47);
        let wednesday = TimeZone::get("America/Chicago")?.to_datetime(last).date();
        Ok(model.usual_times("family_calendar", wednesday, 14.0, 2.0))
    }

    /// AC-14.5e-2.
    #[tokio::test]
    async fn ac_14_5e_2_same_times_do_not_change_the_model() -> anyhow::Result<()> {
        let models = calendar_habits(vec![calendar(LearnedStatus::Active, X)], &DAYS, 2).await?;
        assert_eq!(models[0], models[1]);
        assert_ne!(models[0], RequestHabits::default());
        Ok(())
    }

    /// The axis of the embeddings of the calendar requests.
    const X: [f32; 3] = [1.0, 0.0, 0.0];

    /// An entry `family_calendar` with `status` and `centroid`.
    fn calendar(status: LearnedStatus, centroid: [f32; 3]) -> LearnedIntent {
        let mut entry = proposal(LearnedKind::Command, None);
        entry.name = "family_calendar".to_owned();
        entry.status = status;
        entry.centroid = centroid.to_vec();
        entry
    }

    /// A cluster that a `none` entry covers records nothing under an earlier entry with the same
    /// name.
    #[tokio::test]
    async fn covering_none_entry_with_a_reused_name_records_nothing() -> anyhow::Result<()> {
        let active = calendar(LearnedStatus::Active, [0.0, 1.0, 0.0]);
        let models =
            calendar_habits(vec![active, calendar(LearnedStatus::None, X)], &DAYS, 1).await?;
        assert_eq!(models[0], RequestHabits::default());
        Ok(())
    }

    /// Two clusters that one entry covers count in time order: Monday and Wednesday in one,
    /// Tuesday in the other.
    #[tokio::test]
    async fn clusters_of_one_entry_count_in_time_order() -> anyhow::Result<()> {
        let mut requests = [(1, -0.2); 10];
        requests[..5].copy_from_slice(&[(0, 0.2), (0, 0.2), (0, 0.2), (2, 0.2), (2, 0.2)]);
        let entries = vec![calendar(LearnedStatus::Active, X)];
        let models = calendar_habits(entries, &requests, 1).await?;
        assert_eq!(usual(&models[0])?, vec![SEVEN]);
        Ok(())
    }

    /// The pools of `learner` after the changes of its checks.
    fn learned_pools(learner: &Learner) -> anyhow::Result<Pools> {
        learner.saved()?;
        Ok(learner.pools.lock().expect("no lock holder panics").clone())
    }

    /// AC-14.7-1.
    #[tokio::test]
    async fn ac_14_7_1_active_entry_gets_its_pool_and_audio() -> anyhow::Result<()> {
        let learner = Learner::new();
        let entry = proposal(LearnedKind::Command, None);
        learner.check(entry.clone()).await?;
        let pools = learned_pools(&learner)?;
        assert_eq!(texts(&pools["tell_joke"]), entry.responses);
        let synths = learner.tts.texts().len();
        learner
            .learning
            .activation
            .speech
            .speak(&entry.responses[0])
            .await?;
        assert_eq!(learner.tts.texts().len(), synths, "a response has no audio");
        Ok(())
    }

    /// AC-14.7-3.
    #[tokio::test(start_paused = true)]
    async fn ac_14_7_3_backfill_adds_responses_to_a_learned_pool() -> anyhow::Result<()> {
        let learner = Learner::new();
        learner.check(proposal(LearnedKind::Command, None)).await?;
        let (reasoner, changes) = (Arc::default(), learner.learning.activation.changes.clone());
        let backfill = backfill(Arc::clone(&reasoner), changes, &learner.tts);
        backfill.record_use("tell_joke".to_owned(), 0);
        learner.saved()?;
        settle().await;
        reasoner.release("tell_joke", Ok(NEW.map(str::to_owned).to_vec()));
        settle().await;
        assert_eq!(texts(&learned_pools(&learner)?["tell_joke"])[1..], NEW);
        Ok(())
    }

    /// AC-14.7-5. The start gives the source pools the saved pool of each active entry. A later
    /// `none` entry with the same name is not activated.
    #[tokio::test]
    async fn ac_14_7_5_restart_activates_once() -> anyhow::Result<()> {
        let learner = Learner::empty();
        let mut entry = proposal(LearnedKind::Command, None);
        entry.status = LearnedStatus::Active;
        learner.store.save(&Learned {
            intents: vec![entry.clone(), proposal(LearnedKind::Command, None)],
        })?;
        let mut started = pool(&entry.responses);
        started.responses[0].uses = 3;
        let source = Pools::from([("tell_joke".to_owned(), started)]);
        *learner.pools.lock().expect("no lock holder panics") = source.clone();
        learner.learning.activation.resume().await;
        let entries = learner.saved()?;
        assert_eq!(learned_pools(&learner)?, source);
        assert!(entries[0].activated.is_some(), "{entries:?}");
        assert_eq!(entries[1].activated, None);
        Ok(())
    }

    /// The question of the prefetch tests.
    const QUESTION: &str = "What is on the family calendar today?";

    /// The intent of the prefetch tests.
    const CALENDAR: &str = "family_calendar";

    /// Thursday 08:00 after `monday()`.
    fn thursday() -> Timestamp {
        monday() + SignedDuration::from_mins(3 * 24 * 60 - 10)
    }

    /// A prefetch on the System Two, the TTS, the habits and the file of a check, and its clock.
    struct Prefetcher {
        /// The prefetch.
        prefetch: Prefetch,
        /// The check.
        learner: Learner,
        /// The time.
        clock: Arc<SetClock>,
    }

    impl Prefetcher {
        /// The prefetch of [`prefetch_calendar`] of `names` with `jarvis.toml` at `thursday()`.
        fn new(names: &[&str]) -> anyhow::Result<Self> {
            let config = Config::parse(include_str!("../../jarvis.toml"))?;
            let learner = Learner::empty();
            prefetch_calendar(&learner, names)?;
            let clock = Arc::new(SetClock(Mutex::new(thursday())));
            let reasoner = Arc::clone(&learner.reasoner) as _;
            let mut two =
                crate::system_two(&config, reasoner, crate::registry(&config, Vec::new()));
            two.clock = Arc::clone(&clock) as _;
            let conversation = prefetch_conversation(&config, &learner, two);
            let habits = Arc::clone(&learner.learning.habits);
            let mut activation = learner.learning.activation.clone();
            activation.clock = Arc::clone(&clock) as _;
            let prefetch = Prefetch::new(conversation, activation, habits, &config);
            Ok(Self {
                prefetch,
                learner,
                clock,
            })
        }

        /// Runs one tick at `minutes` after `thursday()` with the answer `steps`.
        async fn tick(&self, minutes: i64, steps: Vec<Scripted>) {
            *lock(&self.clock.0) = thursday() + SignedDuration::from_mins(minutes);
            self.learner.reasoner.script(steps);
            self.prefetch.tick().await;
        }
    }

    /// Saves an active prefetch entry of [`QUESTION`] for each of `names`, used weekdays at 08:00.
    fn prefetch_calendar(learner: &Learner, names: &[&str]) -> anyhow::Result<()> {
        let mut intents = Vec::new();
        for name in names {
            let mut entry = proposal(LearnedKind::Prefetch, None);
            (entry.name, entry.status) = ((*name).to_owned(), LearnedStatus::Active);
            (entry.question, entry.fresh_min) = (Some(QUESTION.to_owned()), Some(30));
            for day in 0..3 {
                let time = monday() + SignedDuration::from_hours(24 * day);
                learner.learning.habits.observe((*name).to_owned(), time);
            }
            intents.push(entry);
        }
        learner.store.save(&Learned { intents })
    }

    /// A recorder with a log writer that has ended, at `clock`.
    fn ended_recorder(clock: &Arc<dyn Clock>) -> Recorder {
        Recorder {
            utterances: Arc::new(Utterances::new(Arc::clone(clock), 10)),
            writer: tokio::sync::mpsc::unbounded_channel().0,
            last_id: None,
            trigger: fakes::trigger("unused".as_ref()).0,
        }
    }

    /// A conversation of `config` on `FakeHome` and `two`, with the habits and the hot cache, the
    /// live catalog and the pre-render of the activation of `learner`.
    fn prefetch_conversation(config: &Config, learner: &Learner, two: SystemTwo) -> Conversation {
        let (activation, wait) = (&learner.learning.activation, Duration::from_secs(1));
        let (home, cache) = (Arc::new(FakeHome::default()), activation.cache.clone());
        let queues = Arc::new(Queues::new(home, cache, wait, wait, config.dialog.clone()));
        let classifier = Arc::new(FakeClassifier::default());
        let classifier = Arc::new(CurrentClassifier::new(FIXTURE.to_owned(), classifier));
        let recorder = ended_recorder(&two.clock);
        let backfill = crate::backfill(config, &two.reasoner, activation);
        let (catalog, threshold) = (Arc::clone(&activation.catalog), config.model.threshold);
        let pools = Arc::new(ArcSwap::from_pointee(Pools::new()));
        let habits = Arc::clone(&learner.learning.habits);
        Conversation::new(
            pools, backfill, queues, classifier, catalog, threshold, two, habits, recorder,
        )
    }

    /// The answer `sentence`.
    fn said(sentence: &'static str) -> Vec<Scripted> {
        vec![Scripted::Sentence(sentence), Scripted::Done]
    }

    /// AC-14.10c-1.
    #[tokio::test(start_paused = true)]
    async fn ac_14_10c_1_due_refresh_asks_the_question() -> anyhow::Result<()> {
        let prefetcher = Prefetcher::new(&[CALENDAR])?;
        prefetcher.tick(0, said("Two events.")).await;
        assert_eq!(prefetcher.learner.reasoner.questions(), [QUESTION]);
        Ok(())
    }

    /// AC-14.10c-2. A new answer also drops the audio of the old one.
    #[tokio::test(start_paused = true)]
    async fn ac_14_10c_2_answer_audio_is_cached() -> anyhow::Result<()> {
        let prefetcher = Prefetcher::new(&[CALENDAR])?;
        prefetcher.tick(0, said("Two events.")).await;
        let speech = &prefetcher.prefetch.activation.speech;
        let tts = &prefetcher.learner.tts;
        let synths = tts.texts().len();
        speech.speak("Two events.").await?;
        assert_eq!(tts.texts().len(), synths, "the answer has no audio");
        prefetcher.tick(30, said("Three events.")).await;
        speech.speak("Two events.").await?;
        assert_eq!(tts.texts().len(), synths + 2, "the old audio stays");
        Ok(())
    }

    /// AC-14.10c-3.
    #[tokio::test(start_paused = true)]
    async fn ac_14_10c_3_error_keeps_the_old_answer() -> anyhow::Result<()> {
        let prefetcher = Prefetcher::new(&[CALENDAR])?;
        prefetcher.tick(0, said("Old.")).await;
        let steps = vec![Scripted::Sentence("New."), Scripted::Error("lost")];
        prefetcher.tick(30, steps).await;
        let answer = lock(&prefetcher.prefetch.answers).get(CALENDAR).cloned();
        assert_eq!(
            answer.map(|answer| (answer.text, answer.time)),
            Some(("Old.".to_owned(), thursday()))
        );
        Ok(())
    }

    /// AC-14.10c-4.
    #[tokio::test(start_paused = true)]
    async fn ac_14_10c_4_window_gives_three_prefetches() -> anyhow::Result<()> {
        let prefetcher = Prefetcher::new(&[CALENDAR])?;
        let (mut asked, reasoner) = (Vec::new(), &prefetcher.learner.reasoner);
        for minutes in -10..=70 {
            let before = reasoner.questions().len();
            prefetcher.tick(minutes, Vec::new()).await;
            if reasoner.questions().len() > before {
                asked.push(minutes);
            }
        }
        assert_eq!(asked, [-5, 25, 55]);
        Ok(())
    }

    /// A tick from 08:59 skips its second intent when the first answer ends at 09:00.
    #[tokio::test(start_paused = true)]
    async fn each_intent_of_a_tick_reads_the_clock() -> anyhow::Result<()> {
        let prefetcher = Prefetcher::new(&[CALENDAR, "school_calendar"])?;
        let late = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            *lock(&prefetcher.clock.0) = thursday() + SignedDuration::from_hours(1);
        };
        let slow = vec![Scripted::Wait(Duration::from_secs(2)), Scripted::Done];
        tokio::join!(prefetcher.tick(59, slow), late);
        assert_eq!(prefetcher.learner.reasoner.questions(), [QUESTION]);
        Ok(())
    }

    /// A refresh keeps old audio that `school_calendar` still stores or that a pool has.
    #[tokio::test(start_paused = true)]
    async fn refresh_keeps_audio_in_use() -> anyhow::Result<()> {
        let prefetcher = Prefetcher::new(&[CALENDAR, "school_calendar"])?;
        let speech = &prefetcher.prefetch.activation.speech;
        let tts = &prefetcher.learner.tts;
        let pools = Pools::from([("hello".to_owned(), pool(&["Hello.".to_owned()]))]);
        speech.prerender(&pools, &[]).await?;
        prefetcher.learner.reasoner.script(said("Two events."));
        prefetcher.tick(-5, said("Two events.")).await;
        for (minutes, text) in [(25, "Hello."), (55, "Three events.")] {
            prefetcher.learner.reasoner.script(said(text));
            prefetcher.tick(minutes, Vec::new()).await;
        }
        let synths = tts.texts().len();
        speech.speak("Two events.").await?;
        speech.speak("Hello.").await?;
        assert_eq!(tts.texts().len(), synths, "the audio in use stays");
        Ok(())
    }
}
