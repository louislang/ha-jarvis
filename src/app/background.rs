//! Hot cache sync and response backfill.

use std::collections::BTreeSet;
use std::collections::HashSet;
use std::future;
use std::mem;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::mpsc;

use futures::StreamExt;
use tokio::sync::watch;

use crate::BackfillConfig;
use crate::app::conversation::Change;
use crate::domain::catalog::Catalog;
use crate::domain::responses;
use crate::domain::responses::Pool;
use crate::domain::responses::Pools;
use crate::domain::state::Event;
use crate::domain::state_cache::StateCache;
use crate::ports::Home;
use crate::ports::Reasoner;

/// Keeps `cache` equal to the states and the registries of `home` until the event stream ends.
/// Publishes each new cache to the receivers of `cache`.
pub async fn sync(home: &dyn Home, cache: &watch::Sender<StateCache>) {
    let mut events = home.events();
    if load(home, cache).await.is_err() {
        events = events
            .skip_while(|event| future::ready(!matches!(event, Event::Reconnected)))
            .boxed();
    }
    while let Some(event) = events.next().await {
        match event {
            Event::StateChanged { .. } => {
                cache.send_modify(|cache| *cache = mem::take(cache).apply(event));
            }
            // A failed load keeps the old cache: the adapter sends `Reconnected` after it connects again.
            Event::RegistryUpdated | Event::Reconnected => load(home, cache).await.unwrap_or(()),
        }
    }
}

/// Replaces `cache` with a new snapshot of `home`. Logs a failed snapshot.
async fn load(home: &dyn Home, cache: &watch::Sender<StateCache>) -> anyhow::Result<()> {
    let snapshot = home
        .snapshot()
        .await
        .inspect_err(|error| tracing::warn!("the snapshot failed: {error:#}"))?;
    cache.send_replace(StateCache::load(snapshot));
    Ok(())
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
    /// The intent catalog. Only an intent pool can use `area`.
    catalog: Arc<Catalog>,
    /// The pools with a backfill in progress.
    running: Mutex<HashSet<String>>,
}

impl Backfill {
    /// Backfills with the limits of `config` and the candidates of `reasoner`, and sends the uses
    /// and the merges on `changes`.
    #[must_use]
    pub fn new(
        config: BackfillConfig,
        reasoner: Arc<dyn Reasoner>,
        changes: mpsc::Sender<Change>,
        catalog: Arc<Catalog>,
    ) -> Self {
        Self {
            config,
            reasoner,
            changes,
            catalog,
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
    /// valid.
    ///
    /// # Errors
    ///
    /// `generate` fails.
    async fn candidates(&self, name: &str, pool: &Pool) -> anyhow::Result<Vec<String>> {
        let variables = self.variables(name);
        let existing = texts(pool);
        let generated = self
            .reasoner
            .generate(name, &existing, &variables, self.config.batch);
        Ok(self.valid(pool, generated.await?, &variables))
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

/// The text of each response of `pool`.
fn texts(pool: &Pool) -> Vec<String> {
    let texts = pool.responses.iter().map(|response| response.text.clone());
    texts.collect()
}

/// Tests of the sync and backfill use cases and their conformance to `cache_sync.qnt` and
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
    use crate::domain::state::Snapshot;
    use crate::domain::state::State;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::house;

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

    /// AC-2.9-7.
    #[tokio::test(start_paused = true)]
    async fn ac_2_9_7_failed_start_loads_after_reconnect() {
        let home = FakeHome::default();
        drop(home.delay());
        let (home, cache) = start(home).await;
        home.send(Event::Reconnected);
        settle().await;
        assert_eq!(*cache.borrow(), StateCache::load(house()));
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
        let subscriber = tracing_subscriber::fmt().with_writer(Arc::new(log.reopen()?));
        // The current-thread runtime polls the sync task on this thread, so it logs here too.
        let _default = tracing::subscriber::set_default(subscriber.finish());
        let home = FakeHome::default();
        let gate = home.delay();
        start(home).await;
        // A parallel test can register the callsite while it has no subscriber.
        tracing::callsite::rebuild_interest_cache();
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
            }
        }

        /// The snapshot of the HA states `states`.
        fn snapshot(states: &BTreeMap<String, i64>) -> Snapshot {
            let mut snapshot = house();
            let states = states.iter().map(|(id, v)| state(id, &v.to_string()));
            snapshot.states = states.collect();
            snapshot
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
                disconnect => (self.queue, self.gate) = (None, None),
                reconnect => self.queue = Some(VecDeque::new()),
                takeSnapshot => {
                    self.gate = Some(self.home.delay());
                    self.home.send(Event::Reconnected);
                },
                applySnapshot => self.gate.take().map(|gate| gate.send(())),
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

    /// A backfill with `reasoner` and `changes`, the `[backfill]` of `jarvis.toml` with no pool
    /// limit and the catalog of `intents.toml`.
    fn backfill(reasoner: Arc<FakeReasoner>, changes: mpsc::Sender<Change>) -> Arc<Backfill> {
        let mut config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
        config.backfill.max_pool = usize::MAX;
        let catalog = toml::from_str(include_str!("../../intents.toml")).expect("it is a catalog");
        Arc::new(Backfill::new(
            config.backfill,
            reasoner,
            changes,
            Arc::new(catalog),
        ))
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
            let reasoner = Arc::new(FakeReasoner::default());
            let (changes, receiver) = mpsc::channel();
            let texts: Vec<String> = (1..=START).map(|n| format!("Response {n}.")).collect();
            let empty = Pool {
                responses: Vec::new(),
                last: None,
            };
            let pool = responses::merge(empty, &texts, START);
            Self {
                runtime: runtime.expect("the runtime builds"),
                backfill: backfill(Arc::clone(&reasoner), changes),
                reasoner,
                changes: receiver,
                pools: Pools::from([("a".to_owned(), pool.clone()), ("b".to_owned(), pool)]),
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
