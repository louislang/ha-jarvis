//! Hot cache sync.

use std::future;
use std::mem;

use futures::StreamExt;
use tokio::sync::watch;

use crate::domain::state::Event;
use crate::domain::state_cache::StateCache;
use crate::ports::Home;

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

/// Replaces `cache` with a new snapshot of `home`.
async fn load(home: &dyn Home, cache: &watch::Sender<StateCache>) -> anyhow::Result<()> {
    let snapshot = home.snapshot().await?;
    cache.send_replace(StateCache::load(snapshot));
    Ok(())
}

/// Tests of the sync use case and its conformance to `cache_sync.qnt`.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
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
    use crate::domain::state::Snapshot;
    use crate::domain::state::State;
    use crate::testing::fakes::FakeHome;
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
}
