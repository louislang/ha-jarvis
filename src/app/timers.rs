//! The timer use case: the timer table, its store and the timer helpers of HA.

use std::collections::BTreeMap;
use std::mem;
use std::sync::Arc;
use std::sync::Mutex;

use jiff::Timestamp;
use rand::rngs::StdRng;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::domain::decide::Decision;
use crate::domain::responses::Pools;
use crate::domain::state::Event;
use crate::domain::state::State;
use crate::domain::state_cache::StateCache;
use crate::domain::timers;
use crate::domain::timers::Plan;
use crate::domain::timers::TimerAction;
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
    /// The run count of each helper: the new runs that the view showed.
    runs: BTreeMap<String, u64>,
}

impl Desk {
    /// The table after `input`, which also changes the view.
    fn after(&mut self, input: TimerInput) -> Timers {
        match input {
            TimerInput::Event(Event::TimerEnded { entity_id, .. }) => {
                timers::ended(self.timers.clone(), &entity_id).1
            }
            TimerInput::Event(event) => self.changed(event),
            TimerInput::Snapshot(states) => {
                self.count(&states);
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

    /// The table after the state change `event` of a helper, which also changes the view and
    /// counts a new run of the helper.
    fn changed(&mut self, event: Event) -> Timers {
        let Event::StateChanged { entity_id, .. } = &event else {
            return self.timers.clone();
        };
        let helper = entity_id.clone();
        let old = self.view.state(&helper).cloned();
        self.view = mem::take(&mut self.view).apply(event);
        let table = self.timers.clone();
        let Some(state) = self.view.state(&helper) else {
            return table;
        };
        if is_new_run(old.as_ref(), state) {
            *self.runs.entry(helper).or_default() += 1;
        }
        timers::running(table, state)
    }

    /// Counts a new run of each helper of the snapshot `states` that starts a new run, or whose
    /// `last_updated` differs from the view.
    fn count(&mut self, states: &[State]) {
        for state in states {
            let old = self.view.state(&state.entity_id);
            // A snapshot cannot show continuity, so each change of the helper is a new run.
            let moved = old.is_none_or(|old| old.last_updated != state.last_updated);
            if moved || is_new_run(old, state) {
                *self.runs.entry(state.entity_id.clone()).or_default() += 1;
            }
        }
    }
}

/// True when `new` starts a new run of a helper whose state was `old`: `old` is not `active` or
/// `paused`, and `new` is.
fn is_new_run(old: Option<&State>, new: &State) -> bool {
    let running = |state: &State| matches!(state.state.as_str(), "active" | "paused");
    running(new) && !old.is_some_and(running)
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
                runs: BTreeMap::new(),
            }),
            store,
            similarity,
        }
    }

    /// The decision for `request` from `satellite` at `now`, with a response of `pools` that `rng`
    /// selects, and the guard of its call: the start call of a new entry, or the call of an `Act`
    /// with the run count of its helper. Plans on the view and keeps the new table under the lock,
    /// so two requests cannot take the same helper.
    ///
    /// # Panics
    ///
    /// A holder of the lock of `rng` panicked.
    pub async fn decide(
        self: &Arc<Self>,
        request: &TimerRequest,
        satellite: Option<String>,
        now: Timestamp,
        pools: &Pools,
        rng: &Mutex<StdRng>,
    ) -> (Decision, Option<Guard>) {
        let mut desk = self.desk.lock().await;
        let (timers, order) = (desk.timers.clone(), desk.timers.next);
        let (plan, table) =
            timers::plan(request, timers, &desk.view, now, satellite, self.similarity);
        self.keep(&mut desk, table).await;
        let guard = self.guard(&plan, request.action, order, &desk);
        let rng = &mut rng.lock().expect("no lock holder panics");
        (timers::answer(&plan, request, pools, rng), guard)
    }

    /// The guard of the call of `plan` for `action`: of the start call of the entry with `order`,
    /// or of an `Act` with the run count of its helper in `desk`.
    fn guard(
        self: &Arc<Self>,
        plan: &Plan,
        action: TimerAction,
        order: u64,
        desk: &Desk,
    ) -> Option<Guard> {
        match plan {
            Plan::Start { helper, .. } => Some(Guard::Start(StartGuard {
                desk: Arc::clone(self),
                helper: helper.clone(),
                order,
                armed: true,
            })),
            Plan::Act { helper, .. } => Some(Guard::Act {
                desk: Arc::clone(self),
                helper: helper.clone(),
                action,
                run: desk.runs.get(helper).copied(),
            }),
            _ => None,
        }
    }

    /// Removes the entry of `helper` while it starts and has `order`: its start call failed.
    async fn forget(&self, helper: &str, order: u64) {
        let mut desk = self.desk.lock().await;
        let table = timers::forget(desk.timers.clone(), helper, order);
        self.keep(&mut desk, table).await;
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

/// The call of a timer decision.
pub enum Guard {
    /// The start call of a new entry.
    Start(StartGuard),
    /// The call of an `Act` on the run of its helper.
    Act {
        /// The desk of the helper.
        desk: Arc<TimerDesk>,
        /// The helper of the call.
        helper: String,
        /// The action of the call.
        action: TimerAction,
        /// The run count of the helper in the view at the decision.
        run: Option<u64>,
    },
}

impl Guard {
    /// The result of `call`, which makes the call of the decision. A start call ends its guard.
    /// An `Act` makes `call` under the lock of the desk, and only while the view shows the helper
    /// in the same run and in a state that the action acts on. Else it makes no call.
    ///
    /// # Errors
    ///
    /// `call` fails.
    pub async fn made(self, call: impl Future<Output = anyhow::Result<()>>) -> anyhow::Result<()> {
        match self {
            Self::Start(start) => {
                let made = call.await;
                start.ended(made.is_ok()).await;
                made
            }
            Self::Act {
                desk,
                helper,
                action,
                run,
            } => {
                let held = desk.desk.lock().await;
                let same = held.runs.get(&helper).copied() == run;
                if !same || !timers::fits(action, &held.view, &helper) {
                    return Ok(());
                }
                call.await
            }
        }
    }
}

/// The start call of a new entry. A failed call, or a drop before the call ends, forgets the
/// entry, so that its helper and its name become free.
pub struct StartGuard {
    /// The desk of the entry.
    desk: Arc<TimerDesk>,
    /// The helper of the entry.
    helper: String,
    /// The order of the entry.
    order: u64,
    /// True until the call ends.
    armed: bool,
}

impl StartGuard {
    /// Ends the guard after the call, and forgets the entry when the call failed (`made` false).
    /// The guard stays armed until `forget` ends, so a drop while it waits still forgets.
    async fn ended(mut self, made: bool) {
        if !made {
            self.desk.forget(&self.helper, self.order).await;
        }
        self.armed = false;
    }
}

impl Drop for StartGuard {
    /// Forgets the entry in a new task when the call did not end.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (desk, order) = (Arc::clone(&self.desk), self.order);
        let helper = mem::take(&mut self.helper);
        tokio::spawn(async move { desk.forget(&helper, order).await });
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
        desk: Arc<TimerDesk>,
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
                desk: Arc::new(TimerDesk::new(Arc::clone(&store) as _, 0.85).await),
                store,
            }
        }

        /// A new desk on an empty store whose view shows `timer.jarvis_1` `idle`.
        async fn free() -> Self {
            let fixture = Self::new(&[]).await;
            fixture.feed(vec![snapshot(&[(J1, "idle")])]).await;
            fixture
        }

        /// The reply and the call of the request `intent` with `action` and the `timer` slot
        /// `name` from `satellite`. A start call is made.
        async fn ask(
            &self,
            (intent, action): (&str, TimerAction),
            name: Option<&str>,
            satellite: &str,
        ) -> (String, Option<ServiceCall>) {
            let (decision, guard) = self.decide((intent, action), name, satellite).await;
            if let Some(guard) = guard {
                guard.made(async { Ok(()) }).await.expect("no call fails");
            }
            let Decision::Reply { reply, call } = decision else {
                panic!("not a reply");
            };
            (reply.text, call)
        }

        /// The decision and the start guard of the request `intent` with `action` and the
        /// `timer` slot `name` from `satellite`, with "five minutes" as the duration of a `Set`.
        async fn decide(
            &self,
            (intent, action): (&str, TimerAction),
            name: Option<&str>,
            satellite: &str,
        ) -> (Decision, Option<Guard>) {
            let request = TimerRequest {
                intent: intent.to_owned(),
                action,
                timer: name.map(str::to_owned),
                duration: Some("five minutes".to_owned()),
                transcript: intent.to_owned(),
            };
            let now: Timestamp = "2026-11-22T12:00:00Z".parse().expect("a timestamp");
            let satellite = Some(satellite.to_owned());
            let (pools, rng) = (pools(), Mutex::new(StdRng::seed_from_u64(1)));
            self.desk
                .decide(&request, satellite, now, &pools, &rng)
                .await
        }

        /// A new desk whose view shows "chicken" `active` on `timer.jarvis_1`.
        async fn running() -> Self {
            let fixture = Self::free().await;
            fixture.set("chicken").await;
            fixture.feed(vec![changed(J1, "active")]).await;
            fixture
        }

        /// The guard of the call of the request `intent` with `action` for "chicken".
        async fn waiting(&self, request: (&str, TimerAction)) -> Guard {
            let (_, guard) = self.decide(request, Some("chicken"), KITCHEN).await;
            guard.expect("a guard of the call")
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

    /// The pools of `responses.toml`.
    fn pools() -> Pools {
        let catalog = crate::load_catalog(Path::new("intents.toml")).expect("a catalog");
        crate::load_pools(Path::new("responses.toml"), &catalog).expect("pools")
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

    /// The end `end` of `timer.jarvis_1`.
    fn ended(end: TimerEnd) -> TimerInput {
        TimerInput::Event(Event::TimerEnded {
            entity_id: J1.to_owned(),
            end,
        })
    }

    /// True when `guard` makes its call.
    async fn calls(guard: Guard) -> bool {
        let mut called = false;
        let call = async {
            called = true;
            Ok(())
        };
        guard.made(call).await.expect("no call fails");
        called
    }

    /// AC-19.9b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9b_1_cancel_in_ha_leaves_the_table() {
        let fixture = Fixture::free().await;
        fixture.set("chicken").await;
        fixture.feed(vec![changed(J1, "active")]).await;
        assert_eq!(fixture.saved(J1), Some(entry("chicken", 0, false)));
        fixture
            .feed(vec![changed(J1, "idle"), ended(TimerEnd::Cancelled)])
            .await;
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
        fixture.feed(vec![ended(TimerEnd::Cancelled)]).await;
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

    /// AC-19.9e-2: a failed start whose cleanup drops while it waits for the desk frees the entry.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9e_2_dropped_cleanup_frees_the_name() {
        let fixture = Fixture::free().await;
        let set = ("timer_set", TimerAction::Set);
        let (_, guard) = fixture.decide(set, Some("chicken"), KITCHEN).await;
        let held = fixture.desk.desk.lock().await;
        let failed = async { Err(anyhow::anyhow!("the call failed")) };
        let cleanup = tokio::spawn(guard.expect("a start guard").made(failed));
        tokio::task::yield_now().await;
        cleanup.abort();
        drop(held);
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        assert_eq!(fixture.saved(J1), None);
    }

    /// AC-19.9d-2: a waiting call skips a new run of its helper, from Jarvis or from HA.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9d_2_waiting_call_skips_a_new_run() {
        let pause = ("timer_pause", TimerAction::Pause);
        let cancel = ("timer_cancel", TimerAction::Cancel);
        let fixture = Fixture::running().await;
        let guard = fixture.waiting(pause).await;
        let end = vec![changed(J1, "idle"), ended(TimerEnd::Finished)];
        fixture.feed(end).await;
        let set = ("timer_set", TimerAction::Set);
        fixture
            .ask(set, Some("pasta"), "assist_satellite.office")
            .await;
        fixture.feed(vec![changed(J1, "active")]).await;
        assert!(!calls(guard).await, "a pause of the new timer");
        let fixture = Fixture::running().await;
        let guard = fixture.waiting(cancel).await;
        let restart = vec![changed(J1, "idle"), changed(J1, "active")];
        fixture.feed(restart).await;
        assert!(!calls(guard).await, "a cancel of the timer from HA");
    }

    /// AC-19.9d-2: a waiting cancel acts on its own run, also after a pause in HA.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9d_2_waiting_cancel_acts_on_its_run() {
        let cancel = ("timer_cancel", TimerAction::Cancel);
        let fixture = Fixture::running().await;
        let guard = fixture.waiting(cancel).await;
        fixture.feed(vec![changed(J1, "paused")]).await;
        assert!(calls(guard).await, "no cancel of the paused timer");
        let guard = Fixture::running().await.waiting(cancel).await;
        assert!(calls(guard).await, "no cancel");
    }

    /// AC-19.9d-2: a snapshot with another `last_updated` of the helper is a new run.
    #[tokio::test(start_paused = true)]
    async fn ac_19_9d_2_reconnect_restart_is_a_new_run() {
        let cancel = ("timer_cancel", TimerAction::Cancel);
        let fixture = Fixture::running().await;
        let guard = fixture.waiting(cancel).await;
        let mut restarted = timer_state(J1, "active");
        restarted.last_updated = "2026-11-22T12:03:00Z".parse().expect("a timestamp");
        fixture
            .feed(vec![TimerInput::Snapshot(vec![restarted])])
            .await;
        assert!(!calls(guard).await, "a cancel of the new timer");
        let fixture = Fixture::running().await;
        let guard = fixture.waiting(cancel).await;
        fixture.feed(vec![snapshot(&[(J1, "active")])]).await;
        assert!(calls(guard).await, "no cancel after the same snapshot");
    }
}
