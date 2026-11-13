//! Hot cache sync, response backfill, the request habit store and the daily log prune.

use std::collections::BTreeSet;
use std::collections::HashSet;
use std::mem;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::mpsc;
use std::time::Duration;

use futures::StreamExt;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::BackfillConfig;
use crate::app::conversation::Change;
use crate::app::speech::Speech;
use crate::domain::catalog::Catalog;
use crate::domain::habits::RequestHabits;
use crate::domain::responses;
use crate::domain::responses::Pool;
use crate::domain::responses::Pools;
use crate::domain::state::Event;
use crate::domain::state_cache::StateCache;
use crate::ports::Clock;
use crate::ports::Home;
use crate::ports::InteractionLog;
use crate::ports::Reasoner;
use crate::ports::Store;

/// The wait before the first retry of a failed snapshot.
const FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest wait between two snapshot retries.
const MAX_RETRY: Duration = Duration::from_secs(30);

/// The time between two prunes of the interaction log.
const PRUNE_INTERVAL: Duration = Duration::from_hours(24);

/// Keeps `cache` equal to the states and the registries of `home` until the event stream ends.
/// Publishes each new cache to the receivers of `cache`.
pub async fn sync(home: &dyn Home, cache: &watch::Sender<StateCache>) {
    let mut events = home.events();
    load(home, cache).await;
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
    /// The intent catalog. Only an intent pool can use `area`.
    catalog: Arc<Catalog>,
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
        catalog: Arc<Catalog>,
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
        let empty = Pool {
            responses: Vec::new(),
            last: None,
        };
        let added = Pools::from([(name.to_owned(), responses::merge(empty, new, usize::MAX))]);
        let areas = area_names(&self.cache.borrow());
        self.speech.prerender(&added, &areas).await
    }

    /// The variables that the responses of the pool `name` can use: `area` for an intent pool.
    fn variables(&self, name: &str) -> BTreeSet<String> {
        let intent = self
            .catalog
            .intents
            .iter()
            .any(|intent| intent.name == name);
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

/// Tests of the sync, backfill, habit and prune use cases and the conformance to `cache_sync.qnt` and
/// `backfill.qnt`.
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
    use tokio::runtime::Runtime;
    use tokio::time::Instant;

    use super::*;
    use crate::Config;
    use crate::app::conversation;
    use crate::domain::habits::Bucket;
    use crate::domain::habits::DayType;
    use crate::domain::state::Snapshot;
    use crate::domain::state::State;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeLog;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::FakeTts;
    use crate::testing::fakes::house;
    use crate::testing::fakes::log_dispatch;

    /// The entity that the tests change.
    const KITCHEN: &str = "light.kitchen";

    /// Starts sync of a new cache with `home` and waits until it waits.
    async fn start(home: FakeHome) -> (Arc<FakeHome>, watch::Receiver<StateCache>) {
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
    async fn ac_2_9_1_start_loads_snapshot() {
        let (_home, cache) = start(FakeHome::default()).await;
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
    /// starts `retries[k]` seconds after the start, and not before.
    async fn assert_retries(failures: usize, retries: &[u64]) -> watch::Receiver<StateCache> {
        let home = FakeHome::default();
        for _ in 0..failures {
            drop(home.delay());
        }
        let begin = Instant::now();
        let (home, cache) = start(home).await;
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
        let catalog = toml::from_str(include_str!("../../intents.toml")).expect("it is a catalog");
        let speech = Arc::new(Speech::new(Arc::clone(tts) as _));
        let cache = watch::channel(StateCache::load(house())).1;
        let catalog = Arc::new(catalog);
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
}
