//! Dispatch of decisions, the announce queues and the pools task.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::domain::decide::Chosen;
use crate::domain::decide::Decision;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::state_cache::StateCache;
use crate::ports::Home;
use crate::ports::Store;

/// Background work that the caller starts after it sends the reply.
pub type Work = BoxFuture<'static, anyhow::Result<()>>;

/// One change to the pools, applied by the pools task.
pub type Change = Box<dyn FnOnce(Pools) -> Pools + Send>;

/// The reply to a decision and the work after it.
#[must_use]
pub struct Handled {
    /// The text to speak.
    pub text: String,
    /// The background work, if any.
    pub after: Option<Work>,
}

/// Dispatches decisions to the home and the pools task.
pub struct Conversation {
    /// The house.
    home: Arc<dyn Home>,
    /// The sender of the changes to the pools task.
    changes: mpsc::Sender<Change>,
}

impl Conversation {
    /// Dispatches to `home` and to the pools task of `changes`.
    #[must_use]
    pub fn new(home: Arc<dyn Home>, changes: mpsc::Sender<Change>) -> Self {
        Self { home, changes }
    }

    /// The reply of `decision` and its service call as background work. Records the use of the
    /// reply.
    pub fn dispatch(&self, decision: Decision) -> Handled {
        let (chosen, call) = match decision {
            Decision::Reply { reply, call } => (reply, call),
            Decision::Defer { filler, .. } => (filler, None),
            Decision::Fallback { reply } => (reply, None),
        };
        let Chosen { text, pool, index } = chosen;
        // The task holds the receiver until every sender drops, so the send fails only after a panic.
        let _ = self.changes.send(Box::new(move |pools| {
            responses::record_use(pools, &pool, index)
        }));
        let after = call.map(|call| {
            let home = Arc::clone(&self.home);
            async move {
                home.call(call).await?;
                Ok(())
            }
            .boxed()
        });
        Handled { text, after }
    }
}

/// The announce queue and the generation of one satellite.
#[derive(Default)]
struct Queue {
    /// The generation of the satellite.
    generation: AtomicU64,
    /// The turn to announce. Its waiters are the items of the queue, in order.
    turn: tokio::sync::Mutex<()>,
}

/// The announce queue of each satellite.
pub struct Queues {
    /// The house.
    home: Arc<dyn Home>,
    /// The hot cache.
    cache: watch::Receiver<StateCache>,
    /// The wait after the satellite becomes idle.
    margin: Duration,
    /// The deadline for the satellite to become idle.
    idle_wait: Duration,
    /// The queue of each satellite entity.
    satellites: Mutex<BTreeMap<String, Arc<Queue>>>,
}

impl Queues {
    /// Queues that announce on `home` `margin` after `cache` shows the satellite idle, and drop
    /// an item when the satellite is not idle within `idle_wait`.
    #[must_use]
    pub fn new(
        home: Arc<dyn Home>,
        cache: watch::Receiver<StateCache>,
        margin: Duration,
        idle_wait: Duration,
    ) -> Self {
        Self {
            home,
            cache,
            margin,
            idle_wait,
            satellites: Mutex::default(),
        }
    }

    /// The queue of `satellite`.
    fn queue(&self, satellite: &str) -> Arc<Queue> {
        let mut satellites = self.satellites.lock().expect("no lock holder panics");
        Arc::clone(satellites.entry(satellite.to_owned()).or_default())
    }

    /// Increases the generation of `satellite` and returns it.
    pub fn next_generation(&self, satellite: &str) -> u64 {
        self.queue(satellite)
            .generation
            .fetch_add(1, Ordering::SeqCst)
            + 1
    }

    /// Announces `text` on `satellite` after the earlier items of its queue, `margin` after the
    /// satellite is idle. Drops it when the satellite is not idle within `idle_wait`, or when
    /// the generation of the satellite is no longer `generation`.
    ///
    /// # Errors
    ///
    /// The announce fails.
    pub async fn announce(
        &self,
        satellite: &str,
        generation: u64,
        text: &str,
    ) -> anyhow::Result<()> {
        let queue = self.queue(satellite);
        let _turn = queue.turn.lock().await;
        let mut cache = self.cache.clone();
        let idle = cache.wait_for(|cache| {
            cache
                .state(satellite)
                .is_some_and(|state| state.state == "idle")
        });
        if !matches!(tokio::time::timeout(self.idle_wait, idle).await, Ok(Ok(_))) {
            tracing::warn!("{satellite} is not idle, so the announce of {text:?} is dropped");
            return Ok(());
        }
        tokio::time::sleep(self.margin).await;
        if queue.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        self.home.announce(satellite, text).await
    }
}

/// The pools task and the pools that it publishes.
pub struct PoolsTask {
    /// The current pools.
    pub pools: Arc<ArcSwap<Pools>>,
    /// The sender of the changes.
    pub changes: mpsc::Sender<Change>,
    /// The task. It ends when every sender drops.
    pub task: JoinHandle<()>,
}

/// Starts the pools task with the pools of `source` merged into the pools of `store`. The task
/// applies each change in order, publishes the result and saves it to `store`.
///
/// # Errors
///
/// A store cannot load, or `source` has no pools.
pub fn start_pools(
    source: &dyn Store<Pools>,
    store: Arc<dyn Store<Pools>>,
) -> anyhow::Result<PoolsTask> {
    let source = source
        .load()?
        .ok_or_else(|| anyhow::anyhow!("the source has no pools"))?;
    let saved = store.load()?.unwrap_or_default();
    let pools = Arc::new(ArcSwap::from_pointee(merge(source, &saved)));
    let (changes, receiver) = mpsc::channel::<Change>();
    let published = Arc::clone(&pools);
    // The saves block on file writes, so the task runs on a blocking thread.
    let task = tokio::task::spawn_blocking(move || {
        for change in receiver {
            let next = change(Pools::clone(&published.load()));
            published.store(Arc::new(next));
            // A failed save keeps the last saved file. The next change saves again.
            let _ = store.save(&published.load());
        }
    });
    Ok(PoolsTask {
        pools,
        changes,
        task,
    })
}

/// The pools of `source` with the use count of each response that `saved` has too.
fn merge(mut source: Pools, saved: &Pools) -> Pools {
    for (name, pool) in &mut source {
        let old = saved
            .get(name)
            .map_or(&[][..], |pool| pool.responses.as_slice());
        for response in &mut pool.responses {
            let same = old.iter().find(|other| other.text == response.text);
            response.uses = same.map_or(0, |other| other.uses);
        }
    }
    source
}

/// Tests of dispatch and the pools task.
#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;
    use tokio::runtime::Runtime;
    use tokio::time::Instant;

    use super::*;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;
    use crate::domain::state::ServiceCall;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::house;

    /// The satellite entity of the tests.
    const SATELLITE: &str = "assist_satellite.carson_kitchen";

    /// The `announce.margin_ms` of the tests.
    const MARGIN: Duration = Duration::from_millis(300);

    /// The `announce.idle_wait_s` of the tests. No conformance trace runs this long.
    const IDLE_WAIT: Duration = Duration::from_secs(3600);

    /// The pool `lights_on` with the responses and use counts `lights`, and the pool `fallback`
    /// with one response used `fallback` times.
    fn pools(lights: &[(&str, u64)], fallback: u64) -> Pools {
        let pool = |texts: &[(&str, u64)]| {
            let responses = texts.iter().map(|&(text, uses)| Response {
                text: text.to_owned(),
                uses,
            });
            Pool {
                responses: responses.collect(),
                last: None,
            }
        };
        let (lights, fallback) = (pool(lights), pool(&[("Sorry.", fallback)]));
        Pools::from([
            ("lights_on".to_owned(), lights),
            ("fallback".to_owned(), fallback),
        ])
    }

    /// The pools of the tests, with no uses.
    fn unused() -> Pools {
        pools(&[("Lights on.", 0), ("Done.", 0)], 0)
    }

    /// The response at `index` of the pool `name`.
    fn chosen(name: &str, index: usize) -> Chosen {
        let text = unused()[name].responses[index].text.clone();
        let pool = name.to_owned();
        Chosen { text, pool, index }
    }

    /// A call that turns on the kitchen light.
    fn call() -> ServiceCall {
        ServiceCall {
            domain: "light".to_owned(),
            service: "turn_on".to_owned(),
            target: vec!["light.kitchen".to_owned()],
            data: serde_json::Map::new(),
            return_response: false,
        }
    }

    /// A `lights_on` reply with `call`.
    fn reply(call: Option<ServiceCall>) -> Decision {
        let reply = chosen("lights_on", 1);
        Decision::Reply { reply, call }
    }

    /// The pools task of `unused()`, its store and a conversation with `home`.
    type Started = (PoolsTask, Arc<FakeStore<Pools>>, Conversation);

    /// Starts the pools task of `unused()` and a conversation with `home`.
    fn start(home: &Arc<FakeHome>) -> anyhow::Result<Started> {
        let store = Arc::new(FakeStore::new(None));
        let task = start_pools(&FakeStore::new(Some(unused())), Arc::clone(&store) as _)?;
        let conversation = Conversation::new(Arc::clone(home) as _, task.changes.clone());
        Ok((task, store, conversation))
    }

    /// The last save of `store` after `conversation` drops and `task` ends.
    async fn last_save(started: Started) -> anyhow::Result<Pools> {
        let (task, store, conversation) = started;
        drop((conversation, task.changes));
        task.task.await?;
        Ok(store.saves().pop().expect("the task saves each change"))
    }

    /// AC-4.8-1.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_1_dispatch_returns_before_time_advances() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let gate = home.delay();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            gate.send(())
        });
        let (_task, _store, conversation) = start(&home)?;
        let start = Instant::now();
        let handled = conversation.dispatch(reply(Some(call())));
        assert_eq!(Instant::now(), start, "dispatch waited for the call");
        assert!(handled.after.is_some(), "the call is not in after");
        Ok(())
    }

    /// AC-4.8-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_2_call_only_after_start() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let after = conversation.dispatch(reply(Some(call()))).after;
        assert_eq!(home.calls(), [], "a call came before the start");
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        assert_eq!(home.calls(), [call()]);
        Ok(())
    }

    /// AC-4.8-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_3_store_receives_new_use_count() -> anyhow::Result<()> {
        let started = start(&Arc::new(FakeHome::default()))?;
        let _ = started.2.dispatch(reply(Some(call())));
        let expected = responses::record_use(unused(), "lights_on", 1);
        assert_eq!(last_save(started).await?, expected);
        Ok(())
    }

    /// AC-4.8-4.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_4_reply_without_call_makes_no_call() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let handled = conversation.dispatch(reply(None));
        assert!(handled.after.is_none(), "a reply with no call has work");
        assert_eq!(home.calls(), []);
        Ok(())
    }

    /// AC-4.8-5.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_5_fallback_has_text_and_no_after() -> anyhow::Result<()> {
        let (_task, _store, conversation) = start(&Arc::new(FakeHome::default()))?;
        let reply = chosen("fallback", 0);
        let handled = conversation.dispatch(Decision::Fallback { reply });
        assert_eq!(handled.text, "Sorry.");
        assert!(handled.after.is_none(), "a fallback has work");
        Ok(())
    }

    /// AC-4.8-6.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_6_concurrent_uses_are_all_saved() -> anyhow::Result<()> {
        let started = start(&Arc::new(FakeHome::default()))?;
        thread::scope(|scope| {
            for _ in 0..100 {
                scope.spawn(|| started.2.dispatch(reply(None)));
            }
        });
        let saved = last_save(started).await?;
        assert_eq!(saved["lights_on"].responses[1].uses, 100);
        Ok(())
    }

    /// AC-4.8-7.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_7_merge_keeps_counts_of_source_responses() -> anyhow::Result<()> {
        let source = FakeStore::new(Some(pools(&[("Lights on.", 0), ("New.", 0)], 0)));
        let saved = pools(&[("Lights on.", 3), ("Done.", 2)], 5);
        let task = start_pools(&source, Arc::new(FakeStore::new(Some(saved))))?;
        let expected = pools(&[("Lights on.", 3), ("New.", 0)], 5);
        assert_eq!(**task.pools.load(), expected);
        assert_eq!(source.saves(), [], "responses.toml changed");
        Ok(())
    }

    /// Sets the state of the satellite in `cache` to `value`.
    fn set(cache: &watch::Sender<StateCache>, value: &str) {
        cache.send_modify(|cache| {
            let state = cache.states.get_mut(SATELLITE).expect("the house has it");
            Arc::make_mut(state).state = value.to_owned();
        });
    }

    /// Queues on `home` and the hot cache of the reference house, with the satellite in the
    /// state `value`.
    fn queues(home: &Arc<FakeHome>, value: &str) -> (watch::Sender<StateCache>, Queues) {
        let (cache, receiver) = watch::channel(StateCache::load(house()));
        set(&cache, value);
        let queues = Queues::new(Arc::clone(home) as _, receiver, MARGIN, IDLE_WAIT);
        (cache, queues)
    }

    /// The texts that `home` announced, in order.
    fn announces(home: &FakeHome) -> Vec<String> {
        let calls = home.calls();
        let texts = calls
            .iter()
            .filter_map(|call| call.data.get("message")?.as_str());
        texts.map(str::to_owned).collect()
    }

    /// AC-4.9b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9b_5_busy_satellite_drops_announce() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, queues) = queues(&home, "responding");
        let generation = queues.next_generation(SATELLITE);
        let start = Instant::now();
        queues.announce(SATELLITE, generation, "Done.").await?;
        assert_eq!(Instant::now() - start, IDLE_WAIT, "the item ended early");
        assert_eq!(announces(&home), [] as [&str; 0]);
        Ok(())
    }

    /// Runs the tasks of `runtime` while its paused clock advances by `duration`. The clock
    /// advances only when every task waits.
    fn advance(runtime: &Runtime, duration: Duration) {
        runtime.block_on(async { tokio::time::sleep(duration).await });
    }

    /// The model state that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Announce {
        /// The started announces: request, sentence and the newest request at the start.
        started: Vec<(i64, i64, i64)>,
    }

    impl ModelState<AnnounceDriver> for Announce {
        /// The announces after every task waits. `NoStaleAnnounce` makes the newest request at
        /// the start the request of the announce.
        fn from_driver(driver: &AnnounceDriver) -> anyhow::Result<Self> {
            advance(&driver.runtime, Duration::from_millis(1));
            let started = announces(&driver.home).into_iter().map(|text| {
                let (request, sentence) = text.split_once(' ').expect("the text is \"r s\"");
                let request = request.parse()?;
                Ok((request, sentence.parse()?, request))
            });
            Ok(Self {
                started: started.collect::<anyhow::Result<_>>()?,
            })
        }
    }

    /// Replays `announce.qnt` against the queue of one satellite, with `FakeHome` as HA.
    struct AnnounceDriver {
        /// The paused runtime that runs the items.
        runtime: Runtime,
        /// HA.
        home: Arc<FakeHome>,
        /// The hot cache.
        cache: watch::Sender<StateCache>,
        /// The queues.
        queues: Arc<Queues>,
        /// The generation of each request.
        generations: BTreeMap<i64, u64>,
    }

    impl AnnounceDriver {
        /// A driver with an idle satellite, on a new runtime.
        fn new() -> Self {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            let runtime = runtime.enable_time().start_paused(true).build();
            let home = Arc::new(FakeHome::default());
            let (cache, queues) = queues(&home, "idle");
            Self {
                runtime: runtime.expect("the runtime builds"),
                home,
                cache,
                queues: Arc::new(queues),
                generations: BTreeMap::new(),
            }
        }
    }

    impl Driver for AnnounceDriver {
        type State = Announce;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                request(r: i64) => {
                    self.generations.insert(r, self.queues.next_generation(SATELLITE));
                    set(&self.cache, "responding");
                },
                pipelineEnd => set(&self.cache, "idle"),
                arrive(r: i64, s: i64) => {
                    // Generations start at 1, so a request that never started has an older one.
                    let generation = self.generations.get(&r).copied().unwrap_or(0);
                    let (queues, text) = (Arc::clone(&self.queues), format!("{r} {s}"));
                    self.runtime.spawn(async move { queues.announce(SATELLITE, generation, &text).await });
                },
                start => advance(&self.runtime, MARGIN),
                finish => {},
            })
        }
    }

    /// AC-4.9b-2. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/announce.qnt", max_steps = 30, seed = "1")]
    fn ac_4_9b_2_conformance() -> impl Driver {
        AnnounceDriver::new()
    }
}
