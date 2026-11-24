//! The timer use case: the timer table, its store and the timer helpers of HA.

use std::mem;
use std::sync::Arc;
use std::sync::Mutex;

use jiff::Timestamp;
use rand::rngs::StdRng;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::domain::decide::Decision;
use crate::domain::responses::Pools;
use crate::domain::state::Event;
use crate::domain::state_cache::StateCache;
use crate::domain::timers;
use crate::domain::timers::TimerInput;
use crate::domain::timers::TimerRequest;
use crate::domain::timers::Timers;
use crate::ports::TimerStore;

/// The timer table and the view of the timer helpers, which one lock guards.
struct Desk {
    /// The timer table.
    timers: Timers,
    /// The timer helper states, in the order of HA.
    view: StateCache,
}

impl Desk {
    /// The table after `input`, which also changes the view.
    fn after(&mut self, input: TimerInput) -> Timers {
        match input {
            TimerInput::Event(Event::TimerEnded { entity_id, .. }) => {
                timers::ended(self.timers.clone(), &entity_id).1
            }
            TimerInput::Event(event) => {
                let Event::StateChanged { entity_id, .. } = &event else {
                    return self.timers.clone();
                };
                let helper = entity_id.clone();
                self.view = mem::take(&mut self.view).apply(event);
                let table = self.timers.clone();
                let state = self.view.state(&helper);
                state.map_or(table.clone(), |state| timers::running(table, state))
            }
            TimerInput::Snapshot(states) => {
                let states = states
                    .into_iter()
                    .map(|state| (Arc::from(state.entity_id.as_str()), Arc::new(state)));
                self.view = StateCache {
                    states: states.collect(),
                    ..StateCache::default()
                };
                timers::reconcile(&self.timers, &self.view)
            }
        }
    }
}

/// Plans the timer requests on the timer table, keeps the table in its store and follows the
/// timer helpers of HA.
pub struct TimerDesk {
    /// The table and the view.
    desk: tokio::sync::Mutex<Desk>,
    /// The saved table.
    store: Arc<dyn TimerStore>,
    /// The least Jaro-Winkler similarity of a spoken name to a label: `dialog.choice_similarity`.
    similarity: f64,
}

impl TimerDesk {
    /// A desk with the table of `store`, whose start calls ended with the last run, and an empty
    /// view. Logs a failed load and starts with an empty table.
    pub async fn new(store: Arc<dyn TimerStore>, similarity: f64) -> Self {
        let loaded = store.load().await.unwrap_or_else(|error| {
            tracing::warn!("the timer table did not load: {error:#}");
            Timers::default()
        });
        Self {
            desk: tokio::sync::Mutex::new(Desk {
                timers: timers::restored(&loaded),
                view: StateCache::default(),
            }),
            store,
            similarity,
        }
    }

    /// The decision for `request` from `satellite` at `now`, with a response of `pools` that `rng`
    /// selects. Plans on the view and keeps the new table under the lock, so two requests cannot
    /// take the same helper.
    ///
    /// # Panics
    ///
    /// A holder of the lock of `rng` panicked.
    pub async fn decide(
        &self,
        request: &TimerRequest,
        satellite: Option<String>,
        now: Timestamp,
        pools: &Pools,
        rng: &Mutex<StdRng>,
    ) -> Decision {
        let mut desk = self.desk.lock().await;
        let timers = desk.timers.clone();
        let (plan, table) =
            timers::plan(request, timers, &desk.view, now, satellite, self.similarity);
        self.keep(&mut desk, table).await;
        let rng = &mut rng.lock().expect("no lock holder panics");
        timers::answer(&plan, request, pools, rng)
    }

    /// Applies each input of `events` to the view and the table, until the sender drops.
    pub async fn run(&self, mut events: UnboundedReceiver<TimerInput>) {
        while let Some(input) = events.recv().await {
            let mut desk = self.desk.lock().await;
            let table = desk.after(input);
            self.keep(&mut desk, table).await;
        }
    }

    /// Makes `table` the table of `desk`, and saves it when it changed. Logs a failed save.
    async fn keep(&self, desk: &mut Desk, table: Timers) {
        if table == desk.timers {
            return;
        }
        desk.timers = table;
        if let Err(error) = self.store.save(&desk.timers).await {
            tracing::warn!("the timer table did not save: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rand::SeedableRng;

    use super::*;
    use crate::domain::state::ServiceCall;
    use crate::domain::state::TimerEnd;
    use crate::domain::timers::Timer;
    use crate::domain::timers::TimerAction;
    use crate::testing::fakes::FakeTimerStore;
    use crate::testing::fakes::timer_state;

    /// The Kitchen satellite.
    const KITCHEN: &str = "assist_satellite.kitchen";

    /// The first timer helper.
    const J1: &str = "timer.jarvis_1";

    /// The second timer helper.
    const J2: &str = "timer.jarvis_2";

    /// A desk on a `FakeTimerStore`.
    struct Fixture {
        /// The desk.
        desk: TimerDesk,
        /// The store of the desk.
        store: Arc<FakeTimerStore>,
    }

    impl Fixture {
        /// A new desk on a store with `entries`.
        async fn new(entries: &[(&str, Timer)]) -> Self {
            let store = Arc::new(FakeTimerStore::default());
            for (helper, timer) in entries {
                let mut saved = store.timers.lock().expect("no test panics");
                saved.timers.insert((*helper).to_owned(), timer.clone());
            }
            Self {
                desk: TimerDesk::new(Arc::clone(&store) as _, 0.85).await,
                store,
            }
        }

        /// A new desk on an empty store whose view shows `timer.jarvis_1` `idle`.
        async fn free() -> Self {
            let fixture = Self::new(&[]).await;
            fixture.feed(vec![snapshot(&[(J1, "idle")])]).await;
            fixture
        }

        /// The reply from the pools of `responses.toml` and the call of the request `intent` with
        /// `action` and the `timer` slot `name` from `satellite`, with "five minutes" as the
        /// duration of a `Set`.
        async fn ask(
            &self,
            (intent, action): (&str, TimerAction),
            name: Option<&str>,
            satellite: &str,
        ) -> (String, Option<ServiceCall>) {
            let request = TimerRequest {
                intent: intent.to_owned(),
                action,
                timer: name.map(str::to_owned),
                duration: Some("five minutes".to_owned()),
                transcript: intent.to_owned(),
            };
            let now: Timestamp = "2026-11-22T12:00:00Z".parse().expect("a timestamp");
            let satellite = Some(satellite.to_owned());
            let catalog = crate::load_catalog(Path::new("intents.toml")).expect("a catalog");
            let pools = crate::load_pools(Path::new("responses.toml"), &catalog).expect("pools");
            let rng = Mutex::new(StdRng::seed_from_u64(1));
            let decision = self.desk.decide(&request, satellite, now, &pools, &rng);
            let Decision::Reply { reply, call } = decision.await else {
                panic!("not a reply");
            };
            (reply.text, call)
        }

        /// The reply to `timer_set` for `name` from the Kitchen satellite.
        async fn set(&self, name: &str) -> (String, Option<ServiceCall>) {
            self.ask(("timer_set", TimerAction::Set), Some(name), KITCHEN)
                .await
        }

        /// The reply to `timer_left` for `name` from `satellite`.
        async fn left(&self, name: Option<&str>, satellite: &str) -> String {
            self.ask(("timer_left", TimerAction::Left), name, satellite)
                .await
                .0
        }

        /// Runs the desk on `inputs`.
        async fn feed(&self, inputs: Vec<TimerInput>) {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            for input in inputs {
                sender.send(input).expect("the receiver is open");
            }
            drop(sender);
            self.desk.run(receiver).await;
        }

        /// The saved entry of `helper`.
        fn saved(&self, helper: &str) -> Option<Timer> {
            let saved = self.store.timers.lock().expect("no test panics");
            saved.timers.get(helper).cloned()
        }
    }

    /// The entry `name` with the order `order` from the Kitchen satellite.
    fn entry(name: &str, order: u64, starting: bool) -> Timer {
        Timer {
            satellite: Some(KITCHEN.to_owned()),
            name: Some(name.to_owned()),
            duration: "five minutes".to_owned(),
            order,
            starting,
        }
    }

    /// The state change of `helper` to `value`.
    fn changed(helper: &str, value: &str) -> TimerInput {
        let new = timer_state(helper, value);
        TimerInput::Event(Event::StateChanged {
            entity_id: helper.to_owned(),
            old: None,
            context: new.context.clone(),
            new: Some(Box::new(new)),
        })
    }

    /// The snapshot with each helper of `states` in its state.
    fn snapshot(states: &[(&str, &str)]) -> TimerInput {
        let states = states
            .iter()
            .map(|(helper, value)| timer_state(helper, value));
        TimerInput::Snapshot(states.collect())
    }

    /// The `Cancelled` end of `timer.jarvis_1`.
    fn cancelled() -> TimerInput {
        TimerInput::Event(Event::TimerEnded {
            entity_id: J1.to_owned(),
            end: TimerEnd::Cancelled,
        })
    }

    /// AC-19.9b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_1_cancel_in_ha_leaves_the_table() {
        let fixture = Fixture::free().await;
        fixture.set("chicken").await;
        fixture.feed(vec![changed(J1, "active")]).await;
        assert_eq!(fixture.saved(J1), Some(entry("chicken", 0, false)));
        fixture.feed(vec![changed(J1, "idle"), cancelled()]).await;
        let reply = fixture.left(Some("chicken"), KITCHEN).await;
        assert_eq!(reply, "There is no chicken timer.");
        assert_eq!(fixture.saved(J1), None);
    }

    /// AC-19.9b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_2_restart_drops_idle_entries() {
        let chicken = entry("chicken", 0, false);
        let entries = [(J1, chicken.clone()), (J2, entry("pasta", 1, false))];
        let fixture = Fixture::new(&entries).await;
        fixture
            .feed(vec![snapshot(&[(J1, "active"), (J2, "idle")])])
            .await;
        let reply = fixture.left(None, KITCHEN).await;
        assert_eq!(reply, "The chicken timer has 5 minutes left.");
        assert_eq!(
            (fixture.saved(J1), fixture.saved(J2)),
            (Some(chicken), None)
        );
    }

    /// AC-19.9b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_3_one_free_helper_for_two_requests() {
        let fixture = Fixture::free().await;
        let (first, second) = tokio::join!(fixture.set("chicken"), fixture.set("pasta"));
        let service = first.1.map(|call| call.service);
        assert_eq!((service, second.1), (Some("start".to_owned()), None));
        assert_eq!(second.0, "All timers are in use.");
    }

    /// AC-19.9b-4.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_4_time_left_from_another_satellite() {
        let fixture = Fixture::free().await;
        fixture.set("chicken").await;
        fixture.feed(vec![changed(J1, "active")]).await;
        let office = "assist_satellite.office";
        let reply = fixture.left(Some("chicken"), office).await;
        assert_eq!(reply, "The chicken timer has 5 minutes left.");
    }

    /// AC-19.9b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_5_late_cancel_keeps_the_new_timer() {
        let fixture = Fixture::free().await;
        fixture.set("chicken").await;
        fixture.feed(vec![changed(J1, "active")]).await;
        let cancel = ("timer_cancel", TimerAction::Cancel);
        let (reply, _) = fixture.ask(cancel, Some("chicken"), KITCHEN).await;
        assert_eq!(reply, "I cancelled the chicken timer.");
        fixture.feed(vec![changed(J1, "idle")]).await;
        let (_, call) = fixture.set("pasta").await;
        assert_eq!(call.map(|call| call.target), Some(vec![J1.to_owned()]));
        fixture.feed(vec![cancelled()]).await;
        assert_eq!(fixture.saved(J1), Some(entry("pasta", 1, true)));
        fixture.feed(vec![changed(J1, "active")]).await;
        let reply = fixture.left(Some("pasta"), KITCHEN).await;
        assert_eq!(reply, "The pasta timer has 5 minutes left.");
    }

    /// AC-19.9b-6.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_6_old_snapshot_keeps_the_new_timer() {
        let fixture = Fixture::free().await;
        fixture.set("pasta").await;
        fixture.feed(vec![snapshot(&[(J1, "idle")])]).await;
        assert_eq!(fixture.saved(J1), Some(entry("pasta", 0, true)));
    }

    /// AC-19.9b-7.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_7_restart_ends_the_start_calls() {
        let entries = [
            (J1, entry("chicken", 0, true)),
            (J2, entry("pasta", 1, true)),
        ];
        let fixture = Fixture::new(&entries).await;
        fixture
            .feed(vec![snapshot(&[(J1, "idle"), (J2, "active")])])
            .await;
        let pasta = Some(entry("pasta", 1, false));
        assert_eq!((fixture.saved(J1), fixture.saved(J2)), (None, pasta));
    }

    /// AC-19.9b-8.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_8_saved_name_is_taken() {
        let fixture = Fixture::new(&[(J1, entry("chicken", 0, false))]).await;
        let (reply, call) = fixture.set("chicken").await;
        assert_eq!(reply, "There is already a chicken timer.");
        assert_eq!(call, None);
    }
}
