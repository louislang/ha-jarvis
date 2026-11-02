//! The `handle` use case, dispatch of decisions, the announce queues and the pools task.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::FutureExt;
use rand::rngs::StdRng;
use serde_json::Map;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::domain::catalog::Catalog;
use crate::domain::decide;
use crate::domain::decide::Chosen;
use crate::domain::decide::Decision;
use crate::domain::decide::Job;
use crate::domain::decide::Request;
use crate::domain::intent;
use crate::domain::model::Prediction;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::state::ServiceCall;
use crate::domain::state_cache::StateCache;
use crate::domain::training;
use crate::ports::Classifier;
use crate::ports::Context;
use crate::ports::Handled;
use crate::ports::Home;
use crate::ports::Store;
use crate::ports::Work;

/// One change to the pools, applied by the pools task.
pub type Change = Box<dyn FnOnce(Pools) -> Pools + Send>;

/// Decides requests with System One and dispatches the decisions to the home, the announce
/// queues and the pools task.
#[derive(Clone)]
pub struct Conversation {
    /// The current pools.
    pools: Arc<ArcSwap<Pools>>,
    /// The sender of the changes to the pools task.
    changes: mpsc::Sender<Change>,
    /// The announce queues, their house and the hot cache.
    queues: Arc<Queues>,
    /// The random source of the response selection.
    rng: Arc<Mutex<StdRng>>,
    /// The System One model.
    classifier: Arc<dyn Classifier>,
    /// The intent catalog.
    catalog: Arc<Catalog>,
    /// The least top probability that System One acts on.
    threshold: f32,
}

impl Conversation {
    /// Decides with `classifier`, `catalog` and `threshold`. Dispatches to `queues`, their house
    /// and the pools task of `pools` and `changes`. Selects the responses with `rng`.
    #[must_use]
    pub fn new(
        pools: Arc<ArcSwap<Pools>>,
        changes: mpsc::Sender<Change>,
        queues: Arc<Queues>,
        rng: StdRng,
        classifier: Arc<dyn Classifier>,
        catalog: Arc<Catalog>,
        threshold: f32,
    ) -> Self {
        Self {
            pools,
            changes,
            queues,
            rng: Arc::new(Mutex::new(rng)),
            classifier,
            catalog,
            threshold,
        }
    }

    /// The reply to the transcript `text` in `context` and its work: the System One prediction
    /// of the normalized text in the area of the satellite device, its decision and its dispatch.
    ///
    /// # Errors
    ///
    /// The prediction fails. The error text is a `failure` answer.
    pub async fn handle(&self, text: &str, context: Context) -> anyhow::Result<Handled> {
        let cache = &self.queues.cache;
        let area = (context.device_id.as_deref())
            .and_then(|device| cache.borrow().area_of_device(device).cloned());
        let model_context = training::context_text(area.as_ref().map(|area| area.0.as_str()), &[]);
        let prediction = self
            .classifier
            .predict(&model_context, &intent::normalize(text))
            .await
            .map_err(|error| {
                tracing::warn!("the prediction failed: {error:#}");
                anyhow::anyhow!(self.pick("failure"))
            })?;
        let request = Request {
            transcript: text.to_owned(),
            area,
        };
        let decision = self.decide(&prediction, &request);
        Ok(self.dispatch(decision, context.satellite_id.as_deref()))
    }

    /// The decision for `prediction` of `request` in the current hot cache and pools.
    fn decide(&self, prediction: &Prediction, request: &Request) -> Decision {
        let (cache, pools) = (self.queues.cache.borrow(), self.pools.load());
        let rng = &mut self.rng.lock().expect("no lock holder panics");
        let catalog = &self.catalog;
        decide::decide(
            prediction,
            request,
            catalog,
            &cache,
            &pools,
            self.threshold,
            rng,
        )
    }

    /// The reply or filler of `decision` and its service call or fetch as background work.
    /// Records the use of the reply. Increases the generation of `satellite`, the satellite
    /// entity of the request.
    pub fn dispatch(&self, decision: Decision, satellite: Option<&str>) -> Handled {
        let request = satellite
            .map(|satellite| (satellite.to_owned(), self.queues.next_generation(satellite)));
        let (chosen, after) = match decision {
            Decision::Reply { reply, call } => (reply, call.map(|call| self.after(call, request))),
            Decision::Defer {
                filler,
                job: Job::Fetch { call, pool },
            } => (filler, Some(self.fetch(call, pool, request))),
            Decision::Defer {
                filler,
                job: Job::Llm(_),
            } => (filler, None),
            Decision::Fallback { reply } => (reply, None),
        };
        let Chosen { text, pool, index } = chosen;
        record_use(&self.changes, pool, index);
        Handled { text, after }
    }

    /// Makes `call`. When it fails, logs the error, queues a `failure` answer on the satellite
    /// of `request` with the generation of `request`, and records its use.
    fn after(&self, call: ServiceCall, request: Option<(String, u64)>) -> Work {
        let conversation = self.clone();
        async move {
            let Err(error) = conversation.queues.home.call(call).await else {
                return Ok(());
            };
            tracing::warn!("the call failed: {error:#}");
            let Some((satellite, generation)) = request else {
                return Ok(());
            };
            let text = conversation.pick("failure");
            let queues = &conversation.queues;
            queues.announce(&satellite, generation, &text).await
        }
        .boxed()
    }

    /// Makes `call` while `request` is the newest of its satellite. Renders an answer of `pool`
    /// with the response data as `response`, or selects a `failure` answer when the call or the
    /// render fails. Queues the answer with the generation of `request`, or logs it without a
    /// satellite.
    fn fetch(&self, call: ServiceCall, pool: String, request: Option<(String, u64)>) -> Work {
        let conversation = self.clone();
        async move {
            if let Some((satellite, generation)) = &request
                && !conversation.queues.is_current(satellite, *generation)
            {
                return Ok(());
            }
            let answer = match conversation.queues.home.call(call).await {
                Ok(response) => conversation.render(&pool, response),
                Err(error) => Err(error),
            };
            let text = answer.unwrap_or_else(|error| {
                tracing::warn!("the fetch failed: {error:#}");
                conversation.pick("failure")
            });
            let Some((satellite, generation)) = request else {
                tracing::info!("the fetch answer is {text:?}");
                return Ok(());
            };
            let queues = &conversation.queues;
            queues.announce(&satellite, generation, &text).await
        }
        .boxed()
    }

    /// An answer of the pool `pool` rendered with `response` as `response`.
    fn render(&self, pool: &str, response: Option<Value>) -> anyhow::Result<String> {
        let template = self.pick(pool);
        let context = Map::from_iter([("response".to_owned(), response.unwrap_or_default())]);
        Ok(responses::render(&template, &context)?)
    }

    /// The text of an answer that `responses::select` picks from the current pool `pool`.
    /// Records its use.
    fn pick(&self, pool: &str) -> String {
        let (text, index) = {
            let pools = self.pools.load();
            let found = pools.get(pool).expect("valid pools have each answer pool");
            let index =
                responses::select(found, &mut self.rng.lock().expect("no lock holder panics"));
            (found.responses[index].text.clone(), index)
        };
        record_use(&self.changes, pool.to_owned(), index);
        text
    }
}

/// Sends the use of the response at `index` of the pool `pool` to the pools task of `changes`.
fn record_use(changes: &mpsc::Sender<Change>, pool: String, index: usize) {
    // The task holds the receiver until every sender drops, so the send fails only after a panic.
    let _ = changes.send(Box::new(move |pools| {
        responses::record_use(pools, &pool, index)
    }));
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

    /// True when the generation of `satellite` is `generation`.
    fn is_current(&self, satellite: &str, generation: u64) -> bool {
        self.queue(satellite).generation.load(Ordering::SeqCst) == generation
    }

    /// Increases the generation of `satellite` and returns it.
    pub fn next_generation(&self, satellite: &str) -> u64 {
        self.queue(satellite)
            .generation
            .fetch_add(1, Ordering::SeqCst)
            + 1
    }

    /// Announces `text` on `satellite` after the earlier items of its queue, when the satellite
    /// is still idle `margin` after it became idle. Drops it when the satellite is not idle
    /// within `idle_wait`, or when the generation of the satellite is no longer `generation`.
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
        let idle = |cache: &StateCache| {
            cache
                .state(satellite)
                .is_some_and(|state| state.state == "idle")
        };
        let deadline = tokio::time::Instant::now() + self.idle_wait;
        loop {
            let wait = tokio::time::timeout_at(deadline, cache.wait_for(idle));
            if !matches!(wait.await, Ok(Ok(_))) {
                tracing::warn!("{satellite} is not idle, so the announce of {text:?} is dropped");
                return Ok(());
            }
            tokio::time::sleep(self.margin).await;
            if idle(&cache.borrow()) {
                break;
            }
        }
        if !self.is_current(satellite, generation) {
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
    use rand::SeedableRng;
    use serde::Deserialize;
    use tokio::runtime::Runtime;
    use tokio::time::Instant;

    use super::*;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;
    use crate::domain::state::ServiceCall;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::house;

    /// The satellite entity of the tests.
    const SATELLITE: &str = "assist_satellite.carson_kitchen";

    /// The `announce.margin_ms` of the tests.
    const MARGIN: Duration = Duration::from_millis(300);

    /// The `announce.idle_wait_s` of the tests. No conformance trace runs this long.
    const IDLE_WAIT: Duration = Duration::from_secs(3600);

    /// The device of the kitchen satellite.
    const KITCHEN: &str = "98f4aacf9c4152ffe32dff212df91521";

    /// A conversation with `pools`, `changes`, `queues` and `classifier`, the catalog of
    /// `intents.toml` and the `model.threshold` of `jarvis.toml`.
    fn conversation(
        pools: Arc<ArcSwap<Pools>>,
        changes: mpsc::Sender<Change>,
        queues: Queues,
        classifier: Arc<dyn Classifier>,
    ) -> Conversation {
        let catalog = toml::from_str(include_str!("../../intents.toml")).expect("it is a catalog");
        let (rng, queues) = (StdRng::seed_from_u64(1), Arc::new(queues));
        Conversation::new(
            pools,
            changes,
            queues,
            rng,
            classifier,
            Arc::new(catalog),
            0.6,
        )
    }

    /// The pool `lights_on` with the responses and use counts `lights`, the pool `fallback` with
    /// one response used `fallback` times, and the pool `failure`.
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
            ("failure".to_owned(), pool(&[("Failed.", 0)])),
            ("filler".to_owned(), pool(&[("One moment.", 0)])),
            (
                "weather".to_owned(),
                pool(&[("It is {{ response.sky }}.", 0)]),
            ),
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
        let (pools, changes) = (Arc::clone(&task.pools), task.changes.clone());
        let classifier = Arc::new(FakeClassifier::default());
        let conversation = conversation(pools, changes, queues(home, "idle").1, classifier);
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
        let handled = conversation.dispatch(reply(Some(call())), None);
        assert_eq!(Instant::now(), start, "dispatch waited for the call");
        assert!(handled.after.is_some(), "the call is not in after");
        Ok(())
    }

    /// AC-4.8-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_2_call_only_after_start() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let after = conversation.dispatch(reply(Some(call())), None).after;
        assert_eq!(home.calls(), [], "a call came before the start");
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        assert_eq!(home.calls(), [call()]);
        Ok(())
    }

    /// AC-4.8-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_3_store_receives_new_use_count() -> anyhow::Result<()> {
        let started = start(&Arc::new(FakeHome::default()))?;
        let _ = started.2.dispatch(reply(Some(call())), None);
        let expected = responses::record_use(unused(), "lights_on", 1);
        assert_eq!(last_save(started).await?, expected);
        Ok(())
    }

    /// AC-4.8-4.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_4_reply_without_call_makes_no_call() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let handled = conversation.dispatch(reply(None), None);
        assert!(handled.after.is_none(), "a reply with no call has work");
        assert_eq!(home.calls(), []);
        Ok(())
    }

    /// AC-4.8-5.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_5_fallback_has_text_and_no_after() -> anyhow::Result<()> {
        let (_task, _store, conversation) = start(&Arc::new(FakeHome::default()))?;
        let reply = chosen("fallback", 0);
        let handled = conversation.dispatch(Decision::Fallback { reply }, None);
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
                scope.spawn(|| started.2.dispatch(reply(None), None));
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

    /// AC-4.9b-6.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9b_6_busy_during_margin_waits_again() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (cache, queues) = queues(&home, "idle");
        let generation = queues.next_generation(SATELLITE);
        let (start, busy_for) = (Instant::now(), Duration::from_secs(1));
        let busy = async {
            tokio::time::sleep(MARGIN / 3).await;
            set(&cache, "responding");
            tokio::time::sleep(busy_for).await;
            assert_eq!(
                announces(&home),
                [] as [&str; 0],
                "an announce came while busy"
            );
            set(&cache, "idle");
        };
        tokio::join!(queues.announce(SATELLITE, generation, "Done."), busy).0?;
        assert_eq!(Instant::now() - start, MARGIN / 3 + busy_for + MARGIN);
        assert_eq!(announces(&home), ["Done."]);
        Ok(())
    }

    /// A conversation on `home` and `classifier` with the queues of `queues(home, value)`, the
    /// pools `unused()` and the receiver of its changes. It has no pools task, because a running
    /// blocking task stops the paused clock.
    fn still_with(
        home: &Arc<FakeHome>,
        value: &str,
        classifier: Arc<dyn Classifier>,
    ) -> (
        watch::Sender<StateCache>,
        mpsc::Receiver<Change>,
        Conversation,
    ) {
        let (cache, queues) = queues(home, value);
        let pools = Arc::new(ArcSwap::from_pointee(unused()));
        let (changes, receiver) = mpsc::channel();
        let conversation = conversation(pools, changes, queues, classifier);
        (cache, receiver, conversation)
    }

    /// `still_with` and a classifier that gives no prediction.
    fn still(
        home: &Arc<FakeHome>,
        value: &str,
    ) -> (
        watch::Sender<StateCache>,
        mpsc::Receiver<Change>,
        Conversation,
    ) {
        still_with(home, value, Arc::new(FakeClassifier::default()))
    }

    /// The work of a dispatch of `decision` on `satellite`.
    fn work(
        conversation: &Conversation,
        decision: Decision,
        satellite: Option<&str>,
    ) -> anyhow::Result<Work> {
        let handled = conversation.dispatch(decision, satellite);
        handled.after.ok_or_else(|| anyhow::anyhow!("no after"))
    }

    /// The work of a dispatch on the satellite of a reply with `call()`, whose call fails.
    fn failed(home: &FakeHome, conversation: &Conversation) -> anyhow::Result<Work> {
        drop(home.delay());
        work(conversation, reply(Some(call())), Some(SATELLITE))
    }

    /// AC-4.9c-1.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_1_failure_announce_starts_margin_after_idle() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (cache, _changes, conversation) = still(&home, "responding");
        let (start, busy_for) = (Instant::now(), Duration::from_millis(1500));
        let after = failed(&home, &conversation)?;
        let idle = async {
            tokio::time::sleep(busy_for).await;
            set(&cache, "idle");
        };
        tokio::join!(after, idle).0?;
        assert_eq!(Instant::now() - start, busy_for + MARGIN);
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// AC-4.9c-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_2_failed_call_announces_failure_once() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) = still(&home, "idle");
        failed(&home, &conversation)?.await?;
        let calls = home.calls();
        assert_eq!(calls.len(), 2, "not one call and one announce: {calls:?}");
        assert_eq!(calls[0], call());
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// AC-4.9c-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_3_new_dispatch_drops_old_failure() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) = still(&home, "idle");
        let first = failed(&home, &conversation)?;
        let _ = conversation.dispatch(reply(None), Some(SATELLITE));
        first.await?;
        assert_eq!(announces(&home), [] as [&str; 0]);
        Ok(())
    }

    /// AC-4.9c-4.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_4_failure_answer_records_use() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, changes, conversation) = still(&home, "idle");
        failed(&home, &conversation)?.await?;
        let applied = changes
            .try_iter()
            .fold(unused(), |pools, change| change(pools));
        let reply = responses::record_use(unused(), "lights_on", 1);
        assert_eq!(applied, responses::record_use(reply, "failure", 0));
        Ok(())
    }

    /// The `weather` call with `return_response`.
    fn weather() -> ServiceCall {
        let target = vec!["weather.home".to_owned()];
        let (domain, service) = ("weather".to_owned(), "get_forecasts".to_owned());
        let (data, return_response) = (serde_json::Map::new(), true);
        ServiceCall {
            domain,
            service,
            target,
            data,
            return_response,
        }
    }

    /// The work of a dispatch on `satellite` of a `weather` fetch.
    fn fetch(conversation: &Conversation, satellite: Option<&str>) -> anyhow::Result<Work> {
        let job = Job::Fetch {
            call: weather(),
            pool: "weather".to_owned(),
        };
        let filler = chosen("filler", 0);
        work(conversation, Decision::Defer { filler, job }, satellite)
    }

    /// AC-4.9d-1.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9d_1_fetch_calls_with_return_response() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) = still(&home, "idle");
        fetch(&conversation, None)?.await?;
        assert_eq!(home.calls(), [weather()]);
        Ok(())
    }

    /// AC-4.9d-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9d_2_fetch_announces_rendered_answer_when_idle() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        home.respond(serde_json::json!({ "sky": "sunny" }));
        let (cache, _changes, conversation) = still(&home, "responding");
        let idle = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert_eq!(
                announces(&home),
                [] as [&str; 0],
                "an announce came while busy"
            );
            set(&cache, "idle");
        };
        tokio::join!(fetch(&conversation, Some(SATELLITE))?, idle).0?;
        assert_eq!(announces(&home), ["It is sunny."]);
        let preannounce = home
            .calls()
            .pop()
            .map(|call| call.data["preannounce"].clone());
        assert_eq!(preannounce, Some(serde_json::json!(false)));
        Ok(())
    }

    /// AC-4.9d-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9d_3_failed_fetch_announces_failure() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) = still(&home, "idle");
        drop(home.delay());
        fetch(&conversation, Some(SATELLITE))?.await?;
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// The result of `handle` for `text` from the satellite device `device` when the classifier
    /// gives `label` with 0.9, or fails without `label`. Also the `(context, text)` of each
    /// prediction.
    async fn handled(
        label: Option<&str>,
        text: &str,
        device: Option<&str>,
    ) -> (anyhow::Result<Handled>, Vec<(String, String)>) {
        let classifier = Arc::new(FakeClassifier::default());
        if let Some(label) = label {
            let ranked = vec![(label.to_owned(), 0.9)];
            classifier.script(Prediction {
                ranked,
                slots: Vec::new(),
            });
        }
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) =
            still_with(&home, "idle", Arc::clone(&classifier) as _);
        let context = Context {
            device_id: device.map(str::to_owned),
            ..Context::default()
        };
        let result = conversation.handle(text, context).await;
        (result, classifier.calls())
    }

    /// AC-5.3-1.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_1_lights_on_in_the_kitchen_gives_a_lights_on_reply() -> anyhow::Result<()> {
        let (handled, _) = handled(Some("lights_on"), "turn on the lights", Some(KITCHEN)).await;
        let text = handled?.text;
        let replies = &unused()["lights_on"].responses;
        assert!(
            replies.iter().any(|reply| reply.text == text),
            "{text:?} is not a lights_on reply"
        );
        Ok(())
    }

    /// AC-5.3-2.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_2_kitchen_device_gives_the_kitchen_context() {
        let (_, calls) = handled(Some("lights_on"), "turn on the lights", Some(KITCHEN)).await;
        assert_eq!(calls[0].0, "area: kitchen | habits: none");
    }

    /// AC-5.3-3.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_3_no_device_gives_no_area() {
        let (_, calls) = handled(Some("lights_on"), "turn on the lights", None).await;
        assert_eq!(calls[0].0, "area: none | habits: none");
    }

    /// AC-5.3-4.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_4_defer_gives_a_filler() -> anyhow::Result<()> {
        let (handled, _) = handled(Some("unknown"), "tell me a joke", Some(KITCHEN)).await;
        assert_eq!(handled?.text, "One moment.");
        Ok(())
    }

    /// AC-5.3-5.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_5_failed_prediction_gives_a_failure_error() {
        let (handled, _) = handled(None, "turn on the lights", Some(KITCHEN)).await;
        let error = handled.err().expect("the prediction fails");
        assert_eq!(error.to_string(), "Failed.");
    }

    /// AC-5.3-7.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_7_prediction_gets_the_normalized_text() {
        let (_, calls) = handled(Some("lights_on"), "Turn on the Lights.", Some(KITCHEN)).await;
        assert_eq!(calls[0].1, "turn on the lights");
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
