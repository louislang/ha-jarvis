//! The timer use case: the timer table, its store, the timer helpers of HA and the rings.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::mem;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use jiff::Timestamp;
use rand::rngs::StdRng;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::domain::decide::Decision;
use crate::domain::responses::Pools;
use crate::domain::state::Event;
use crate::domain::state::State;
use crate::domain::state::TimerEnd;
use crate::domain::state_cache::StateCache;
use crate::domain::timers;
use crate::domain::timers::Candidate;
use crate::domain::timers::Mirror;
use crate::domain::timers::MirrorChange;
use crate::domain::timers::Plan;
use crate::domain::timers::Timer;
use crate::domain::timers::TimerAction;
use crate::domain::timers::TimerInput;
use crate::domain::timers::TimerRequest;
use crate::domain::timers::Timers;
use crate::ports::TimerStore;

/// The time between the starts of two rings.
pub const RING_INTERVAL: Duration = Duration::from_secs(15);

/// The most rings of one timer.
pub const RINGS: u32 = 8;

/// The time after the end of a timer from which it no longer rings.
pub const RING_FOR: Duration = Duration::from_secs(120);

/// The announces of the end of a timer on the satellite that set it.
#[derive(Clone)]
pub struct Ring {
    /// The satellite that set the timer.
    pub satellite: String,
    /// The number of the ring. A new finish on the helper gives a new number.
    pub number: u64,
    /// The ended entry.
    pub timer: Timer,
    /// The time of the end.
    pub start: Instant,
}

/// The timer table, the view of the timer helpers and the rings, which one lock guards.
struct Desk {
    /// The timer table.
    timers: Timers,
    /// The timer helper states, in the order of HA.
    view: StateCache,
    /// The run count of each helper: the new runs that the view showed.
    runs: BTreeMap<String, u64>,
    /// The satellite of the mirror of each helper.
    mirrors: BTreeMap<String, String>,
    /// The ring of the last finished timer of each helper.
    rings: BTreeMap<String, Ring>,
    /// The number of the last ring.
    rung: u64,
}

impl Desk {
    /// The table after `input`, which also changes the view, the helper and the ring that a
    /// finish starts, and the mirror commands of the input.
    fn after(&mut self, input: TimerInput) -> (Timers, Option<(String, Ring)>, Vec<Mirror>) {
        match input {
            TimerInput::Event(Event::TimerEnded { entity_id, end }) => {
                let (entry, table) = timers::ended(self.timers.clone(), &entity_id);
                let finished = entry.filter(|_| end == TimerEnd::Finished);
                let ring = finished.and_then(|timer| self.ring(entity_id, timer));
                (table, ring, Vec::new())
            }
            TimerInput::Event(Event::StateChanged {
                entity_id,
                old,
                new,
                ..
            }) if entity_id.starts_with("assist_satellite.") => {
                let mirrors = self.returned(&entity_id, old.as_deref(), new.as_deref());
                (self.timers.clone(), None, mirrors)
            }
            TimerInput::Event(event) => {
                let (table, mirror) = self.changed(event);
                (table, None, mirror.into_iter().collect())
            }
            TimerInput::Snapshot(states) => {
                let (table, mirrors) = self.snapshot(states);
                (table, None, mirrors)
            }
        }
    }

    /// The `Start` of each mirror of `satellite` whose helper the view shows running, in entity ID
    /// order, when the satellite changes from `old` `unavailable` to `new`: its restart dropped
    /// its Assist timers.
    fn returned(&self, satellite: &str, old: Option<&State>, new: Option<&State>) -> Vec<Mirror> {
        let away = |state: &State| state.state == "unavailable";
        if !old.is_some_and(away) || new.is_none_or(away) {
            return Vec::new();
        }
        let shown = self
            .mirrors
            .iter()
            .filter(|(_, mirrored)| *mirrored == satellite);
        let started = shown.filter_map(|(helper, _)| {
            let state = self.view.state(helper).filter(|state| is_running(state))?;
            Some(Mirror {
                helper: helper.clone(),
                satellite: satellite.to_owned(),
                change: MirrorChange::Start(state.clone()),
            })
        });
        started.collect()
    }

    /// The table after the snapshot `states`, which replaces the view, and its mirror commands.
    fn snapshot(&mut self, states: Vec<State>) -> (Timers, Vec<Mirror>) {
        let renewed = self.count(&states);
        let states = states
            .into_iter()
            .map(|state| (Arc::from(state.entity_id.as_str()), Arc::new(state)));
        self.view = StateCache {
            states: states.collect(),
            ..StateCache::default()
        };
        let table = timers::reconcile(&self.timers, &self.view);
        let mirrors = self.shown(&renewed, &table);
        (table, mirrors)
    }

    /// The new ring of `timer` on `helper`, which replaces the ring of the helper, when the timer
    /// has a satellite.
    fn ring(&mut self, helper: String, timer: Timer) -> Option<(String, Ring)> {
        let satellite = timer.satellite.clone()?;
        self.rung += 1;
        let ring = Ring {
            satellite,
            number: self.rung,
            timer,
            start: Instant::now(),
        };
        self.rings.insert(helper.clone(), ring.clone());
        Some((helper, ring))
    }

    /// The helper and the ended entry of each ring, in the order of the entries.
    fn ringing(&self) -> Vec<Candidate> {
        let rings = self.rings.iter().map(|(helper, ring)| Candidate {
            helper: helper.clone(),
            timer: ring.timer.clone(),
        });
        let mut rings: Vec<Candidate> = rings.collect();
        rings.sort_by_key(|ring| ring.timer.order);
        rings
    }

    /// The table after the state change `event` of a helper, which also changes the view and
    /// counts a new run of the helper, and its mirror command.
    fn changed(&mut self, event: Event) -> (Timers, Option<Mirror>) {
        let Event::StateChanged { entity_id, .. } = &event else {
            return (self.timers.clone(), None);
        };
        let helper = entity_id.clone();
        let old = self.view.state(&helper).cloned();
        self.view = mem::take(&mut self.view).apply(event);
        let table = self.timers.clone();
        let Some(state) = self.view.state(&helper).cloned() else {
            return (table, self.cancel(&helper, None));
        };
        let mirror = if is_new_run(old.as_ref(), &state) {
            *self.runs.entry(helper.clone()).or_default() += 1;
            self.start(&helper, &table, &state)
        } else {
            self.moved(&helper, old.as_ref(), &state)
        };
        (timers::running(table, &state), mirror)
    }

    /// The mirror command of `helper`, a helper of `mirrors`, for its change from `old` to `new`
    /// in the same run: `Pause`, `Start` for a resume, or `Cancel` when it no longer runs.
    fn moved(&mut self, helper: &str, old: Option<&State>, new: &State) -> Option<Mirror> {
        let satellite = self.mirrors.get(helper)?.clone();
        let change = match (old.map(|old| old.state.as_str()), new.state.as_str()) {
            (Some("active"), "paused") => MirrorChange::Pause,
            (Some("paused"), "active") => MirrorChange::Start(new.clone()),
            (_, "active" | "paused") => return None,
            _ => return self.cancel(helper, None),
        };
        Some(Mirror {
            helper: helper.to_owned(),
            satellite,
            change,
        })
    }

    /// The `Start` of the mirror of `helper` in `state` on the satellite of its entry in `table`,
    /// which the helper keeps in `mirrors`. None for an entry without a satellite.
    fn start(&mut self, helper: &str, table: &Timers, state: &State) -> Option<Mirror> {
        let satellite = table.timers.get(helper)?.satellite.clone()?;
        self.mirrors.insert(helper.to_owned(), satellite.clone());
        Some(Mirror {
            helper: helper.to_owned(),
            satellite,
            change: MirrorChange::Start(state.clone()),
        })
    }

    /// The `Cancel` of the mirror of `helper` on its satellite in `mirrors`, else on `satellite`.
    /// The helper leaves `mirrors`.
    fn cancel(&mut self, helper: &str, satellite: Option<String>) -> Option<Mirror> {
        let satellite = self.mirrors.remove(helper).or(satellite)?;
        Some(Mirror {
            helper: helper.to_owned(),
            satellite,
            change: MirrorChange::Cancel,
        })
    }

    /// The mirror commands of a snapshot in the view, before `table` replaces the table, at most
    /// one for each helper, in entity ID order: `Start` for each new run of `renewed`, and
    /// `Cancel` for each helper that does not run.
    fn shown(&mut self, renewed: &BTreeSet<String>, table: &Timers) -> Vec<Mirror> {
        let shown = self.view.states.keys().map(ToString::to_string);
        let mut helpers: BTreeSet<String> = shown.collect();
        helpers.extend(self.mirrors.keys().cloned());
        helpers.extend(self.timers.timers.keys().cloned());
        let mut mirrors = Vec::new();
        for helper in helpers {
            let state = self.view.state(&helper).filter(|state| is_running(state));
            let mirror = match state.cloned() {
                Some(state) if renewed.contains(&helper) => self.start(&helper, table, &state),
                Some(_) => None,
                None => {
                    let removed = self.timers.timers.get(&helper);
                    let removed = removed.filter(|_| !table.timers.contains_key(&helper));
                    let satellite = removed.and_then(|entry| entry.satellite.clone());
                    self.cancel(&helper, satellite)
                }
            };
            mirrors.extend(mirror);
        }
        mirrors
    }

    /// Counts a new run of each helper of the snapshot `states` that starts a new run, or whose
    /// `last_updated` differs from the view. The helpers with a new run.
    fn count(&mut self, states: &[State]) -> BTreeSet<String> {
        let mut renewed = BTreeSet::new();
        for state in states {
            let old = self.view.state(&state.entity_id);
            // A snapshot cannot show continuity, so each change of the helper is a new run.
            let moved = old.is_none_or(|old| old.last_updated != state.last_updated);
            if moved || is_new_run(old, state) {
                *self.runs.entry(state.entity_id.clone()).or_default() += 1;
                renewed.insert(state.entity_id.clone());
            }
        }
        renewed
    }
}

/// True when `new` starts a new run of a helper whose state was `old`: `old` is not `active` or
/// `paused`, and `new` is.
fn is_new_run(old: Option<&State>, new: &State) -> bool {
    is_running(new) && !old.is_some_and(is_running)
}

/// True when `state` is `active` or `paused`.
fn is_running(state: &State) -> bool {
    matches!(state.state.as_str(), "active" | "paused")
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
                mirrors: BTreeMap::new(),
                rings: BTreeMap::new(),
                rung: 0,
            }),
            store,
            similarity,
        }
    }

    /// The decision for `request` from `satellite` at `now`, with a response of `pools` that `rng`
    /// selects, and the guard of its call: the start call of a new entry, or the call of an `Act`
    /// with the run count of its helper. Plans on the view and the rings, and keeps the new table
    /// under the lock, so two requests cannot take the same helper. Removes the ring of `Silence`.
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
        let (ringing, view) = (desk.ringing(), &desk.view);
        let (plan, table) = timers::plan(
            request,
            timers,
            ringing,
            view,
            now,
            satellite,
            self.similarity,
        );
        if let Plan::Silence { helper, .. } = &plan {
            desk.rings.remove(helper);
        }
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

    /// Applies each input of `events` to the view and the table, until the sender drops. Gives
    /// each new ring and its helper to `ring`, and each mirror command to `mirror` under the lock.
    pub async fn run(
        &self,
        mut events: UnboundedReceiver<TimerInput>,
        mut ring: impl FnMut(String, Ring),
        mut mirror: impl FnMut(Mirror),
    ) {
        while let Some(input) = events.recv().await {
            let mut desk = self.desk.lock().await;
            let (table, started, mirrors) = desk.after(input);
            self.keep(&mut desk, table).await;
            for command in mirrors {
                mirror(command);
            }
            if let Some((helper, started)) = started {
                ring(helper, started);
            }
        }
    }

    /// Removes each ring of `satellite`. True when it removed one.
    pub async fn stop(&self, satellite: &str) -> bool {
        let mut desk = self.desk.lock().await;
        let before = desk.rings.len();
        desk.rings.retain(|_, ring| ring.satellite != satellite);
        desk.rings.len() < before
    }

    /// True while a ring exists.
    pub async fn ringing(&self) -> bool {
        !self.desk.lock().await.rings.is_empty()
    }

    /// True while the desk keeps the ring `number` of `helper` and `RING_FOR` after its start has
    /// not passed.
    pub async fn rings(&self, helper: &str, number: u64) -> bool {
        let desk = self.desk.lock().await;
        let ring = desk.rings.get(helper);
        ring.is_some_and(|ring| ring.number == number && Instant::now() < ring.start + RING_FOR)
    }

    /// Removes the ring `number` of `helper`, whose task ended.
    pub async fn end(&self, helper: &str, number: u64) {
        let mut desk = self.desk.lock().await;
        desk.rings
            .retain(|key, ring| key != helper || ring.number != number);
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

    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use rand::SeedableRng;
    use serde::Deserialize;
    use tokio::runtime::Runtime;

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
            let (decision, guard) = self
                .decide((intent, action), name, "five minutes", satellite)
                .await;
            if let Some(guard) = guard {
                guard.made(async { Ok(()) }).await.expect("no call fails");
            }
            let Decision::Reply { reply, call } = decision else {
                panic!("not a reply");
            };
            (reply.text, call)
        }

        /// The decision and the start guard of the request `intent` with `action`, the `timer`
        /// slot `name` and the `duration` slot `duration` from `satellite`.
        async fn decide(
            &self,
            (intent, action): (&str, TimerAction),
            name: Option<&str>,
            duration: &str,
            satellite: &str,
        ) -> (Decision, Option<Guard>) {
            let request = TimerRequest {
                intent: intent.to_owned(),
                action,
                timer: name.map(str::to_owned),
                duration: Some(duration.to_owned()),
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
            let (_, guard) = self
                .decide(request, Some("chicken"), "five minutes", KITCHEN)
                .await;
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

        /// Runs the desk on `inputs`. The mirror commands of the desk.
        async fn feed(&self, inputs: Vec<TimerInput>) -> Vec<Mirror> {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            for input in inputs {
                sender.send(input).expect("the receiver is open");
            }
            drop(sender);
            let mut mirrors = Vec::new();
            self.desk
                .run(receiver, |_, _| (), |mirror| mirrors.push(mirror))
                .await;
            mirrors
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
        let (_, guard) = fixture
            .decide(set, Some("chicken"), "five minutes", KITCHEN)
            .await;
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

    /// The mirror command `change` of `helper` on `satellite`.
    fn mirror(helper: &str, satellite: &str, change: MirrorChange) -> Mirror {
        Mirror {
            helper: helper.to_owned(),
            satellite: satellite.to_owned(),
            change,
        }
    }

    /// The mirror start of `timer.jarvis_1` on the Kitchen satellite in `state`.
    fn started(state: State) -> Vec<Mirror> {
        vec![mirror(J1, KITCHEN, MirrorChange::Start(state))]
    }

    /// AC-19.15-1.
    #[tokio::test(start_paused = true)]
    async fn ac_19_15_1_mirror_follows_the_helper() {
        let fixture = Fixture::free().await;
        fixture.set("chicken").await;
        let active = timer_state(J1, "active");
        let start = fixture.feed(vec![changed(J1, "active")]).await;
        assert_eq!(start, started(active.clone()));
        let pause = fixture.feed(vec![changed(J1, "paused")]).await;
        assert_eq!(pause, [mirror(J1, KITCHEN, MirrorChange::Pause)]);
        let resume = fixture.feed(vec![changed(J1, "active")]).await;
        assert_eq!(resume, started(active));
        let end = vec![changed(J1, "idle"), ended(TimerEnd::Finished)];
        let cancel = fixture.feed(end).await;
        assert_eq!(cancel, [mirror(J1, KITCHEN, MirrorChange::Cancel)]);
    }

    /// AC-19.15-2.
    #[tokio::test(start_paused = true)]
    async fn ac_19_15_2_no_satellite_no_mirror() {
        let anonymous = Timer {
            satellite: None,
            ..entry("chicken", 0, true)
        };
        let fixture = Fixture::new(&[(J1, anonymous)]).await;
        fixture.feed(vec![snapshot(&[(J1, "idle")])]).await;
        let start = fixture.feed(vec![changed(J1, "active")]).await;
        assert_eq!(start, [], "a mirror of a timer without a satellite");
        let fixture = Fixture::free().await;
        let start = fixture.feed(vec![changed(J1, "active")]).await;
        assert_eq!(start, [], "a mirror of a helper from HA");
    }

    /// AC-19.15-3.
    #[tokio::test(start_paused = true)]
    async fn ac_19_15_3_snapshot_restarts_or_cancels_the_mirror() {
        let fixture = Fixture::running().await;
        let same = fixture.feed(vec![snapshot(&[(J1, "active")])]).await;
        assert_eq!(same, [], "a mirror of the same run");
        let mut restarted = timer_state(J1, "active");
        restarted.last_updated = "2026-11-22T12:03:00Z".parse().expect("a timestamp");
        let input = TimerInput::Snapshot(vec![restarted.clone()]);
        assert_eq!(fixture.feed(vec![input]).await, started(restarted));
        let idle = fixture.feed(vec![snapshot(&[(J1, "idle")])]).await;
        assert_eq!(idle, [mirror(J1, KITCHEN, MirrorChange::Cancel)]);
    }

    /// AC-19.15-4.
    #[tokio::test(start_paused = true)]
    async fn ac_19_15_4_restart_starts_and_cancels_mirrors() {
        let office = "assist_satellite.office";
        let pasta = Timer {
            satellite: Some(office.to_owned()),
            ..entry("pasta", 1, false)
        };
        let fixture = Fixture::new(&[(J1, entry("chicken", 0, false)), (J2, pasta)]).await;
        let states = [(J1, "active"), (J2, "idle")];
        let mirrors = fixture.feed(vec![snapshot(&states)]).await;
        let mut wanted = started(timer_state(J1, "active"));
        wanted.push(mirror(J2, office, MirrorChange::Cancel));
        assert_eq!(mirrors, wanted);
    }

    /// The state change of the satellite entity `satellite` from `old` to `new`.
    fn satellite(satellite: &str, old: &str, new: &str) -> TimerInput {
        let (old, new) = (timer_state(satellite, old), timer_state(satellite, new));
        TimerInput::Event(Event::StateChanged {
            entity_id: satellite.to_owned(),
            context: new.context.clone(),
            old: Some(Box::new(old)),
            new: Some(Box::new(new)),
        })
    }

    /// AC-19.17-2.
    #[tokio::test(start_paused = true)]
    async fn ac_19_17_2_satellite_restart_starts_its_mirrors() {
        let fixture = Fixture::running().await;
        let restart = fixture
            .feed(vec![satellite(KITCHEN, "unavailable", "idle")])
            .await;
        assert_eq!(restart, started(timer_state(J1, "active")));
        let listening = satellite(KITCHEN, "idle", "listening");
        let office = satellite("assist_satellite.office", "unavailable", "idle");
        let other = fixture.feed(vec![listening, office]).await;
        assert_eq!(
            other,
            [],
            "a mirror start without a restart of its satellite"
        );
    }

    /// The model helpers and the timer helpers.
    const HELPERS: [(&str, &str); 2] = [("h1", J1), ("h2", J2)];

    /// The model satellites and the satellites.
    const SATELLITES: [(&str, &str); 2] = [("s1", KITCHEN), ("s2", "assist_satellite.office")];

    /// The other name of `name` in `pairs`: the code name of a model name, or the model name of a
    /// code name.
    fn swap(pairs: [(&str, &str); 2], name: &str) -> String {
        let pair = pairs.iter().find(|pair| pair.0 == name || pair.1 == name);
        let (model, code) = pair.expect("a name of the model or the code");
        if *model == name { code } else { model }.to_string()
    }

    /// The cancel request.
    const CANCEL: (&str, TimerAction) = ("timer_cancel", TimerAction::Cancel);

    /// The pause request.
    const PAUSE: (&str, TimerAction) = ("timer_pause", TimerAction::Pause);

    /// The resume request.
    const RESUME: (&str, TimerAction) = ("timer_resume", TimerAction::Resume);

    /// The model state of `timers.qnt` that the conformance test compares.
    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct TimerModel {
        /// The HA state of each helper.
        ha: BTreeMap<String, String>,
        /// The table entry of each helper.
        timer: BTreeMap<String, Entry>,
        /// True for each helper whose start call runs.
        starting: BTreeMap<String, bool>,
        /// The rings with rings left, by helper. The desk removes a ring without rings left.
        #[serde(deserialize_with = "left_rings")]
        ring: BTreeMap<String, Bell>,
    }

    /// A table entry of the model. `owned = false` is no entry.
    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Entry {
        /// True for an entry.
        owned: bool,
        /// The satellite that set the timer.
        sat: String,
        /// The name, or `""` for a generic timer.
        name: String,
        /// The order of the set request.
        order: u64,
    }

    /// A ring of the model.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Bell {
        /// The satellite of the ring.
        sat: String,
        /// The rings left.
        left: u32,
    }

    /// The rings of the model with rings left.
    fn left_rings<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<String, Bell>, D::Error> {
        let rings: BTreeMap<String, Bell> = BTreeMap::deserialize(deserializer)?;
        Ok(rings
            .into_iter()
            .filter(|(_, bell)| bell.left > 0)
            .collect())
    }

    impl Entry {
        /// The model entry of the table entry `entry`.
        fn of(entry: Option<&Timer>) -> Self {
            entry.map_or_else(Self::default, |entry| Self {
                owned: true,
                sat: swap(SATELLITES, entry.satellite.as_deref().unwrap_or_default()),
                name: entry.name.clone().unwrap_or_default(),
                order: entry.order,
            })
        }
    }

    impl ModelState<TimerDriver> for TimerModel {
        /// The HA of the driver, and the table and the rings of the desk.
        fn from_driver(driver: &TimerDriver) -> anyhow::Result<Self> {
            let desk = driver.fixture.desk.desk.try_lock()?;
            let mut model = Self::default();
            for (helper, state) in &driver.ha {
                let (name, entry) = (swap(HELPERS, helper), desk.timers.timers.get(helper));
                model.ha.insert(name.clone(), state.clone());
                let starting = entry.is_some_and(|entry| entry.starting);
                model.starting.insert(name.clone(), starting);
                model.timer.insert(name.clone(), Entry::of(entry));
                let bell = desk.rings.get(helper).map(|ring| driver.bell(ring));
                model.ring.extend(bell.map(|bell| (name, bell)));
            }
            Ok(model)
        }
    }

    /// Replays `timers.qnt` against `TimerDesk`, as the conversation use case and as HA.
    struct TimerDriver {
        /// The paused runtime of the desk.
        runtime: Runtime,
        /// The desk and its store.
        fixture: Fixture,
        /// The HA state of each helper, which the next snapshot shows.
        ha: BTreeMap<String, String>,
        /// The rings made of each ring, by its number.
        counts: BTreeMap<u64, u32>,
        /// The mirror commands of the desk, in order.
        mirrors: Vec<Mirror>,
        /// The `last_updated` of each helper that a restart of HA changed.
        updated: BTreeMap<String, Timestamp>,
    }

    impl TimerDriver {
        /// A driver with no timers and both helpers `idle`.
        fn new() -> Self {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            let runtime = runtime.enable_time().start_paused(true).build();
            let runtime = runtime.expect("the runtime builds");
            let fixture = runtime.block_on(Fixture::new(&[]));
            let ha = [J1, J2].map(|helper| (helper.to_owned(), "idle".to_owned()));
            let mut driver = Self {
                runtime,
                fixture,
                ha: ha.into(),
                counts: BTreeMap::new(),
                mirrors: Vec::new(),
                updated: BTreeMap::new(),
            };
            driver.reconnect();
            driver
        }

        /// Gives the desk the snapshot of HA.
        fn reconnect(&mut self) {
            let states = self.ha.iter().map(|(helper, value)| {
                let mut state = timer_state(helper, value);
                let updated = self.updated.get(helper).copied();
                state.last_updated = updated.unwrap_or(state.last_updated);
                state
            });
            let snapshot = TimerInput::Snapshot(states.collect());
            self.feed(vec![snapshot]);
        }

        /// Runs the desk on `inputs`, and keeps its mirror commands.
        fn feed(&mut self, inputs: Vec<TimerInput>) {
            let mirrors = self.runtime.block_on(self.fixture.feed(inputs));
            self.mirrors.extend(mirrors);
        }

        /// The entry of the model helper `model` and its timer helper.
        fn entry(&self, model: &str) -> anyhow::Result<(String, Timer)> {
            let helper = swap(HELPERS, model);
            let desk = self.fixture.desk.desk.try_lock()?;
            let entry = desk.timers.timers.get(&helper).cloned();
            Ok((
                helper,
                entry.ok_or_else(|| anyhow::anyhow!("no entry on {model}"))?,
            ))
        }

        /// The call that the request `intent` with `action` for `name`, with `duration`, from the
        /// model satellite `model` makes, if any.
        fn ask(
            &self,
            request: (&str, TimerAction),
            name: &str,
            duration: &str,
            model: &str,
        ) -> Option<ServiceCall> {
            let satellite = swap(SATELLITES, model);
            let decide = self
                .fixture
                .decide(request, Some(name), duration, &satellite);
            let (decision, guard) = self.runtime.block_on(decide);
            let made = match guard {
                Some(Guard::Start(mut start)) => {
                    start.armed = false;
                    true
                }
                Some(act) => self.runtime.block_on(calls(act)),
                None => false,
            };
            let Decision::Reply { call, .. } = decision else {
                return None;
            };
            call.filter(|_| made)
        }

        /// The helper that a set for `name` takes: the first free one, or none for a taken name.
        fn planned(&self, name: &str) -> anyhow::Result<Option<String>> {
            let desk = self.fixture.desk.desk.try_lock()?;
            let free = |helper: &&String| {
                self.ha[*helper] == "idle" && !desk.timers.timers.contains_key(*helper)
            };
            let mut names = desk.timers.timers.values();
            let taken = names.any(|timer| timer.name.as_deref() == Some(name));
            Ok(self.ha.keys().find(free).filter(|_| !taken).cloned())
        }

        /// A set, which must start the helper that it takes for the duration of that helper.
        fn set(&self, model: &str, name: &str) -> anyhow::Result<()> {
            let helper = self.planned(name)?;
            let (duration, seconds) = if helper.as_deref() == Some(J2) {
                ("two minutes", 120)
            } else {
                ("one minute", 60)
            };
            let call = self.ask(("timer_set", TimerAction::Set), name, duration, model);
            let made = call.map(|call| {
                (
                    call.service,
                    call.target,
                    call.data.get("duration").cloned(),
                )
            });
            let wanted =
                helper.map(|helper| ("start".to_owned(), vec![helper], Some(seconds.into())));
            anyhow::ensure!(made == wanted, "the set of {name:?} called {made:?}");
            Ok(())
        }

        /// The request for the label of the timer of the model helper `model`, which must call the
        /// service of its action on that helper, then the state `state` of the helper and `end`.
        fn act(
            &mut self,
            request: (&str, TimerAction),
            model: &str,
            state: &str,
            end: Option<TimerEnd>,
        ) -> anyhow::Result<()> {
            let (helper, timer) = self.entry(model)?;
            let label = timer.name.as_ref().unwrap_or(&timer.duration);
            let call = self.ask(request, label, &timer.duration, "s1");
            let made = call.map(|call| (call.service, call.target));
            let service = match request.1 {
                TimerAction::Cancel => "cancel",
                TimerAction::Pause => "pause",
                _ => "start",
            };
            let wanted = Some((service.to_owned(), vec![helper]));
            anyhow::ensure!(made == wanted, "{} of {model} called {made:?}", request.0);
            self.change(model, state, end);
            Ok(())
        }

        /// HA changes the model helper `model` to `state`, then sends `end`.
        fn change(&mut self, model: &str, state: &str, end: Option<TimerEnd>) {
            let helper = swap(HELPERS, model);
            self.ha.insert(helper.clone(), state.to_owned());
            self.updated.remove(&helper);
            let mut inputs = vec![changed(&helper, state)];
            let end = end.map(|end| Event::TimerEnded {
                entity_id: helper,
                end,
            });
            inputs.extend(end.map(TimerInput::Event));
            self.feed(inputs);
        }

        /// HA ends the timer of the model helper `model`, then sends `end`.
        fn end(&mut self, model: &str, end: Option<TimerEnd>) {
            self.change(model, "idle", end);
        }

        /// A new desk on the saved table, with the snapshot of HA.
        fn restart(&mut self) {
            let store = Arc::clone(&self.fixture.store);
            let desk = TimerDesk::new(Arc::clone(&store) as _, 0.85);
            self.fixture = Fixture {
                desk: Arc::new(self.runtime.block_on(desk)),
                store,
            };
            // The new desk numbers its rings from 1 again.
            self.counts.clear();
            self.reconnect();
        }

        /// The model ring of the desk ring `ring`.
        fn bell(&self, ring: &Ring) -> Bell {
            let count = self.counts.get(&ring.number).copied().unwrap_or_default();
            Bell {
                sat: swap(SATELLITES, &ring.satellite),
                left: RINGS - count,
            }
        }

        /// The ring of the model helper `model`.
        fn ring(&self, model: &str) -> anyhow::Result<Ring> {
            let desk = self.fixture.desk.desk.try_lock()?;
            let ring = desk.rings.get(&swap(HELPERS, model)).cloned();
            ring.ok_or_else(|| anyhow::anyhow!("no ring on {model}"))
        }

        /// The next ring of the model helper `model`, at its time on an idle satellite.
        fn ring_once(&mut self, model: &str) -> anyhow::Result<()> {
            let (helper, ring) = (swap(HELPERS, model), self.ring(model)?);
            let count = self.counts.entry(ring.number).or_default();
            let desk = &self.fixture.desk;
            self.runtime.block_on(async {
                tokio::time::sleep_until(ring.start + RING_INTERVAL * *count).await;
                anyhow::ensure!(
                    desk.rings(&helper, ring.number).await,
                    "the ring on {model} ended"
                );
                *count += 1;
                if *count == RINGS {
                    desk.end(&helper, ring.number).await;
                }
                let rings = desk.desk.lock().await.rings.clone();
                // Each ring task ends at its next check after `RING_FOR`.
                for (helper, ring) in rings {
                    if !desk.rings(&helper, ring.number).await {
                        desk.end(&helper, ring.number).await;
                    }
                }
                anyhow::Ok(())
            })
        }

        /// The failed start call of the timer of the model helper `model`.
        fn start_failed(&self, model: &str) -> anyhow::Result<()> {
            let (helper, timer) = self.entry(model)?;
            let forget = self.fixture.desk.forget(&helper, timer.order);
            self.runtime.block_on(forget);
            Ok(())
        }

        /// "Stop" on the model satellite `model`.
        fn stop(&self, model: &str) {
            let satellite = swap(SATELLITES, model);
            self.runtime.block_on(self.fixture.desk.stop(&satellite));
        }

        /// A cancel of the ringing timer of the model helper `model` by its label.
        fn silence(&self, model: &str) -> anyhow::Result<()> {
            let Timer { name, duration, .. } = self.ring(model)?.timer;
            let label = name.unwrap_or_else(|| duration.clone());
            self.ask(CANCEL, &label, &duration, "s1");
            Ok(())
        }
    }

    impl Driver for TimerDriver {
        type State = TimerModel;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                setTimer(s: String, n: String) => self.set(&s, &n)?,
                started(h: String) => self.change(&h, "active", None),
                startFailed(h: String) => self.start_failed(&h)?,
                cancel(h: String) => self.act(CANCEL, &h, "idle", Some(TimerEnd::Cancelled))?,
                pause(h: String) => self.act(PAUSE, &h, "paused", None)?,
                resume(h: String) => self.act(RESUME, &h, "active", None)?,
                outside(h: String) => self.change(&h, "active", None),
                finish(h: String) => self.end(&h, Some(TimerEnd::Finished)),
                haCancel(h: String) => self.end(&h, Some(TimerEnd::Cancelled)),
                quietEnd(h: String) => self.end(&h, None),
                reconnect => self.reconnect(),
                restart => self.restart(),
                ringOnce(h: String) => self.ring_once(&h)?,
                stop(s: String) => self.stop(&s),
                silence(h: String) => self.silence(&h)?,
            })
        }
    }

    /// AC-19.12b-4.
    #[tokio::test(start_paused = true)]
    async fn ac_19_12b_4_cancel_silences_the_newest_ring() {
        let entries = [
            (J1, entry("chicken", 2, false)),
            (J2, entry("chicken", 1, false)),
        ];
        let fixture = Fixture::new(&entries).await;
        let finished = [J1, J2].map(|helper| {
            TimerInput::Event(Event::TimerEnded {
                entity_id: helper.to_owned(),
                end: TimerEnd::Finished,
            })
        });
        fixture.feed(finished.into()).await;
        fixture.ask(CANCEL, Some("chicken"), KITCHEN).await;
        let desk = fixture.desk.desk.lock().await;
        let rings: Vec<&String> = desk.rings.keys().collect();
        assert_eq!(rings, [J2], "the ring of the older chicken timer stays");
    }

    /// Sets a generic timer on `s1` and on `s2`, then starts `h1` and `h2`.
    fn start_both(driver: &mut TimerDriver) -> anyhow::Result<()> {
        driver.set("s1", "")?;
        driver.set("s2", "")?;
        driver.change("h1", "active", None);
        driver.change("h2", "active", None);
        Ok(())
    }

    /// AC-19.12b-5. `quint test` writes no `mbt::actionTaken`, so the test plays the steps of
    /// `ac_19_12b_3_ring_for` on the driver itself.
    #[test]
    fn ac_19_12b_5_ring_for_conformance() -> anyhow::Result<()> {
        let mut driver = TimerDriver::new();
        start_both(&mut driver)?;
        driver.end("h1", Some(TimerEnd::Finished));
        driver.end("h2", Some(TimerEnd::Finished));
        for _ in 0..RINGS {
            driver.ring_once("h2")?;
        }
        start_both(&mut driver)?;
        driver.end("h2", Some(TimerEnd::Finished));
        driver.ring_once("h2")?;
        driver.ring_once("h2")?;
        let model = TimerModel::from_driver(&driver)?;
        let rings: Vec<(&str, u32)> = model
            .ring
            .iter()
            .map(|(helper, bell)| (helper.as_str(), bell.left))
            .collect();
        assert_eq!(
            rings,
            [("h2", RINGS - 2)],
            "the ring of h1 ended at RING_FOR"
        );
        anyhow::ensure!(driver.ring_once("h1").is_err(), "h1 rings after RING_FOR");
        Ok(())
    }

    /// AC-19.12-1. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/timers.qnt", max_steps = 30, seed = "1")]
    fn ac_19_12_1_conformance() -> impl Driver {
        TimerDriver::new()
    }

    /// The model state of `mirrors.qnt` that the conformance test compares.
    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct MirrorModel {
        /// The satellite of the entry of each helper, `""` for no entry.
        entry: BTreeMap<String, String>,
        /// The satellite of the mirror of each helper, `""` for no mirror.
        mirror: BTreeMap<String, String>,
        /// The kind, the helper and the satellite of each queued call, in the order of the queue.
        #[serde(deserialize_with = "in_order")]
        calls: Vec<(String, String, String)>,
        /// The Assist timers.
        assist: BTreeSet<(String, String)>,
    }

    /// A queued call of the model.
    #[derive(Deserialize)]
    struct Call {
        /// The kind of the call.
        op: String,
        /// The helper of the mirror.
        h: String,
        /// The satellite of the mirror.
        s: String,
        /// The number of the call.
        n: u64,
    }

    /// The queued calls of the model in the order of the queue: each cancel first, else the
    /// lower number first.
    fn in_order<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(String, String, String)>, D::Error> {
        let mut calls: Vec<Call> = Vec::deserialize(deserializer)?;
        calls.sort_by_key(|call| (call.op != "cancel", call.n));
        Ok(calls
            .into_iter()
            .map(|call| (call.op, call.h, call.s))
            .collect())
    }

    /// The model satellite of `satellite`, `""` for none.
    fn model_satellite(satellite: Option<&String>) -> String {
        satellite.map_or_else(String::new, |satellite| swap(SATELLITES, satellite))
    }

    /// The model kind of the call of `change`.
    fn op(change: &MirrorChange) -> String {
        let op = match change {
            MirrorChange::Start(_) => "start",
            MirrorChange::Pause => "pause",
            MirrorChange::Cancel => "cancel",
        };
        op.to_owned()
    }

    impl ModelState<MirrorDriver> for MirrorModel {
        /// The entries and the mirrors of the desk, and the queue and the Assist timers of the
        /// driver.
        fn from_driver(driver: &MirrorDriver) -> anyhow::Result<Self> {
            let desk = driver.timers.fixture.desk.desk.try_lock()?;
            let mut model = Self {
                assist: driver.assist.clone(),
                ..Self::default()
            };
            for (name, helper) in HELPERS {
                let entry = desk.timers.timers.get(helper);
                let entry = entry.and_then(|entry| entry.satellite.as_ref());
                model.entry.insert(name.to_owned(), model_satellite(entry));
                let mirror = model_satellite(desk.mirrors.get(helper));
                model.mirror.insert(name.to_owned(), mirror);
            }
            let calls = driver.queue.iter().map(|mirror| {
                let helper = swap(HELPERS, &mirror.helper);
                (
                    op(&mirror.change),
                    helper,
                    swap(SATELLITES, &mirror.satellite),
                )
            });
            model.calls = calls.collect();
            Ok(model)
        }
    }

    /// Replays `mirrors.qnt` against `TimerDesk`, as HA and as the mirror task.
    struct MirrorDriver {
        /// The desk and HA.
        timers: TimerDriver,
        /// The queue of the mirror task.
        queue: Vec<Mirror>,
        /// The Assist timers, as pairs of a model helper and a model satellite.
        assist: BTreeSet<(String, String)>,
        /// The restarts of HA, which give the new `last_updated` of a running helper.
        restarts: i64,
    }

    impl MirrorDriver {
        /// A driver with no timers, no mirrors and both helpers `idle`.
        fn new() -> Self {
            Self {
                timers: TimerDriver::new(),
                queue: Vec::new(),
                assist: BTreeSet::new(),
                restarts: 0,
            }
        }

        /// A generic timer from the model satellite `model` on the first free helper, which then
        /// becomes `active`.
        fn start(&mut self, model: &str) -> anyhow::Result<()> {
            let helper = self.timers.planned("")?;
            let helper = helper.ok_or_else(|| anyhow::anyhow!("no free helper"))?;
            self.timers.set(model, "")?;
            self.timers.change(&swap(HELPERS, &helper), "active", None);
            Ok(())
        }

        /// HA restarts: the Assist timers go, and each running helper gets a new `last_updated`.
        fn ha_restart(&mut self) {
            self.assist.clear();
            self.restarts += 1;
            let restarted = Timestamp::from_second(self.restarts).expect("a timestamp");
            let running = self.timers.ha.iter().filter(|(_, state)| *state != "idle");
            let updated = running.map(|(helper, _)| (helper.clone(), restarted));
            self.timers.updated.extend(updated);
            self.timers.reconnect();
        }

        /// The model satellite `model` restarts: its Assist timers go, and its entity changes
        /// from `unavailable` to `idle`.
        fn sat_restart(&mut self, model: &str) {
            self.assist.retain(|(_, satellite)| satellite != model);
            let back = satellite(&swap(SATELLITES, model), "unavailable", "idle");
            self.timers.feed(vec![back]);
        }

        /// Takes the first call of the queue, and applies it to the Assist timers when it is
        /// `made`.
        fn call(&mut self, made: bool) -> anyhow::Result<()> {
            anyhow::ensure!(!self.queue.is_empty(), "no queued call");
            let mirror = self.queue.remove(0);
            let pair = (
                swap(HELPERS, &mirror.helper),
                swap(SATELLITES, &mirror.satellite),
            );
            match mirror.change {
                MirrorChange::Start(_) if made => self.assist.insert(pair),
                MirrorChange::Cancel if made => self.assist.remove(&pair),
                _ => false,
            };
            Ok(())
        }

        /// Moves the mirror commands of the desk into the queue, as the mirror task does: a
        /// `Cancel` removes the queued commands of its helper and satellite, and goes ahead of
        /// each command that is not a `Cancel`.
        fn enqueue(&mut self) {
            for mirror in mem::take(&mut self.timers.mirrors) {
                let mut at = self.queue.len();
                if mirror.change == MirrorChange::Cancel {
                    let pair = (&mirror.helper, &mirror.satellite);
                    self.queue
                        .retain(|queued| (&queued.helper, &queued.satellite) != pair);
                    let cancels = self.queue.iter();
                    at = cancels
                        .take_while(|queued| queued.change == MirrorChange::Cancel)
                        .count();
                }
                self.queue.insert(at, mirror);
            }
        }

        /// Plays the step `step` of the model.
        fn act(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                start(s: String) => self.start(&s)?,
                outside(h: String) => self.timers.change(&h, "active", None),
                pause(h: String) => self.timers.change(&h, "paused", None),
                resume(h: String) => self.timers.change(&h, "active", None),
                end(h: String) => self.timers.end(&h, Some(TimerEnd::Finished)),
                quietEnd(h: String) => self.timers.ha.insert(swap(HELPERS, &h), "idle".to_owned()),
                reconnect => self.timers.reconnect(),
                restart => {
                    self.queue.clear();
                    self.timers.restart();
                },
                haRestart => self.ha_restart(),
                satRestart(s: String) => self.sat_restart(&s),
                // The argument is `made`.
                call => self.call(true)?,
                callFails => self.call(false)?,
                slow => {},
                mirrorRing(h: String, s: String) => self.assist.remove(&(h, s)),
            })
        }
    }

    impl Driver for MirrorDriver {
        type State = MirrorModel;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            self.act(step)?;
            self.enqueue();
            Ok(())
        }
    }

    /// AC-19.18-1. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/mirrors.qnt", max_steps = 30, seed = "1")]
    fn ac_19_18_1_conformance() -> impl Driver {
        MirrorDriver::new()
    }
}
