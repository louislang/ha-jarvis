//! Timer durations, the time left on a timer, and the timer table.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::num::ParseIntError;

use jiff::Timestamp;
use rand::rngs::StdRng;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use text2num::Language;
use vstd::prelude::*;

use crate::domain::decide::Chosen;
use crate::domain::decide::Decision;
use crate::domain::decide::Job;
use crate::domain::dialog;
use crate::domain::dialog::Pending;
use crate::domain::intent;
use crate::domain::model::Slot;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::select;
use crate::domain::state::Event;
use crate::domain::state::ServiceCall;
use crate::domain::state::State;
use crate::domain::state_cache::StateCache;

/// The longest timer in seconds: 24 hours.
#[verus_verify]
pub const LONGEST: u32 = 86_400;

/// The seconds of the spoken duration `text`, for example 600 for "ten minutes". `None` when a
/// word is not a number, a unit or "and", a number has no unit, or the sum is not 1 s to 24 h.
#[verus_spec(result =>
    ensures
        match result {
            Some(seconds) => 1 <= seconds <= LONGEST,
            None => true,
        },
)]
#[must_use]
pub fn duration(text: &str) -> Option<u32> {
    let words = select::words(&intent::normalize(text));
    let mut total: u32 = 0;
    let mut number = String::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= words@.len(),
        decreases words@.len() - index,
    ))]
    while index < words.len() {
        let word = words[index].as_str();
        if let Some(size) = unit(word) {
            total = total.checked_add(count(&number)?.checked_mul(size)?)?;
            number = String::new();
        } else {
            number = pending(number, word);
        }
        index += 1;
    }
    if number.is_empty() && total != 0 && total <= LONGEST {
        Some(total)
    } else {
        None
    }
}

/// `number` with `word` added after a space. An "and" with no number before it adds nothing.
#[verus_verify]
fn pending(mut number: String, word: &str) -> String {
    if word == "and" && number.is_empty() {
        return number;
    }
    if !number.is_empty() {
        number.push(' ');
    }
    number.push_str(word);
    number
}

/// The seconds of the unit `word`, or `None` when `word` is not a unit.
#[verus_verify]
fn unit(word: &str) -> Option<u32> {
    if word == "second" || word == "seconds" {
        Some(1)
    } else if word == "minute" || word == "minutes" {
        Some(60)
    } else if word == "hour" || word == "hours" {
        Some(3600)
    } else {
        None
    }
}

/// The value of the number words or digits `number`: "a" and "an" give 1.
#[verus_verify]
fn count(number: &str) -> Option<u32> {
    if number == "a" || number == "an" {
        return Some(1);
    }
    let parsed: Result<u32, ParseIntError> =
        match text2num::text2digits(number, &Language::english()) {
            Ok(digits) => digits.parse(),
            Err(_) => number.parse(),
        };
    parsed.ok()
}

/// `seconds` as spoken hours, minutes and seconds with digits, for example "4 minutes and 12
/// seconds". The seconds are left out at 1 hour or more.
#[verus_verify]
#[must_use]
pub fn spoken(seconds: u32) -> String {
    let hours = seconds / 3600;
    let minutes = seconds % 3600 / 60;
    if hours > 0 {
        joined(&part(hours, "hour"), minutes, "minute")
    } else if minutes > 0 {
        joined(&part(minutes, "minute"), seconds % 60, "second")
    } else {
        part(seconds, "second")
    }
}

/// `first`, then "and" with `count` and `unit` when `count` is not 0.
#[verus_verify]
fn joined(first: &str, count: u32, unit: &str) -> String {
    if count == 0 {
        first.to_owned()
    } else {
        let second = part(count, unit);
        format!("{first} and {second}")
    }
}

/// `count` and `unit`, with "s" when `count` is not 1.
#[verus_verify]
fn part(count: u32, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// The seconds left on the HA timer helper `state` at `now`: to `finishes_at`, rounded up, when it
/// is `active`, and its `remaining` time when it is `paused`. `None` for another state, or for a
/// missing or unclear attribute.
#[verus_spec(result =>
    ensures
        !(state.state@ == "active"@ || state.state@ == "paused"@) ==> result is None,
        state.state@ == "paused"@ ==> match result {
            Some(total) => paused_left(state.attributes, total),
            None => true,
        },
)]
#[must_use]
pub fn left(state: &State, now: Timestamp) -> Option<u32> {
    if state.state.as_str() == "active" {
        let end: Timestamp = state
            .attributes
            .get("finishes_at")?
            .as_str()?
            .parse()
            .ok()?;
        until(end.as_nanosecond().checked_sub(now.as_nanosecond())?)
    } else if state.state.as_str() == "paused" {
        clock(state.attributes.get("remaining")?.as_str()?)
    } else {
        None
    }
}

/// The whole seconds of `nanoseconds`, rounded up: 0 when it is not positive, `None` above
/// `u32::MAX`.
#[verus_verify]
fn until(nanoseconds: i128) -> Option<u32> {
    if nanoseconds <= 0 {
        return Some(0);
    }
    u32::try_from((nanoseconds - 1) / 1_000_000_000 + 1).ok()
}

/// The seconds of the time `text`, `H:MM:SS`, or `None` when it does not parse.
#[verus_spec(result =>
    ensures
        match result {
            Some(total) => is_clock(text@, total),
            None => true,
        },
)]
fn clock(text: &str) -> Option<u32> {
    use std::str::FromStr;

    let (hours, rest) = text.split_once(':')?;
    let (minutes, seconds) = rest.split_once(':')?;
    let hour = u32::from_str(hours).ok()?;
    let minute = u32::from_str(minutes).ok()?;
    let second = u32::from_str(seconds).ok()?;
    if minute >= 60 || second >= 60 {
        return None;
    }
    let total = hour.checked_mul(3600)?.checked_add(minute * 60 + second)?;
    proof! {
        assert(text@ =~= hours@ + seq![':'] + minutes@ + seq![':'] + seconds@);
        assert(clock_of(text@, hours@, minutes@, seconds@, total));
    }
    Some(total)
}

/// What a timer request does.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TimerAction {
    /// Sets a new timer.
    Set,
    /// Cancels a timer.
    Cancel,
    /// Cancels the newest timer.
    CancelLast,
    /// Pauses a timer.
    Pause,
    /// Resumes a paused timer.
    Resume,
    /// Tells the time left.
    Left,
}

/// A timer request of System One.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct TimerRequest {
    /// The intent name.
    pub intent: String,
    /// What the request does.
    pub action: TimerAction,
    /// The `timer` slot: the name of the timer.
    pub timer: Option<String>,
    /// The `duration` slot.
    pub duration: Option<String>,
    /// The transcript of the request.
    pub transcript: String,
}

/// One entry of the timer table.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Timer {
    /// The satellite that set the timer.
    pub satellite: Option<String>,
    /// The normalized name, or `None` for a generic timer.
    pub name: Option<String>,
    /// The duration slot of the set request, for example "ten minutes".
    pub duration: String,
    /// The order of the set request: a later timer has a higher order.
    pub order: u64,
    /// True while the start call of the entry runs.
    pub starting: bool,
}

// Not derived: the derived `clone` has no `ensures`, so Verus cannot see that a copy is equal.
#[verus_verify]
impl Clone for Timer {
    /// A copy of the entry.
    #[verus_spec(copy =>
        ensures
            copy == *self,
    )]
    fn clone(&self) -> Self {
        Timer {
            satellite: self.satellite.clone(),
            name: self.name.clone(),
            duration: self.duration.clone(),
            order: self.order,
            starting: self.starting,
        }
    }
}

/// The timer table.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Timers {
    /// The entry of each helper, by entity ID.
    pub timers: BTreeMap<String, Timer>,
    /// The order of the next timer.
    pub next: u64,
}

/// A timer change in HA, in the order of HA.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum TimerInput {
    /// A state change of a timer helper, or `TimerEnded`.
    Event(Event),
    /// The timer helper states of one `get_states` result.
    Snapshot(Vec<State>),
}

/// The plan of a timer request.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// Start `helper` for `seconds`.
    Start {
        /// The entity ID of the helper.
        helper: String,
        /// The duration of the timer.
        seconds: u32,
    },
    /// The request has no duration.
    NoDuration,
    /// The duration does not read.
    Unclear,
    /// A timer with the same name runs.
    Taken,
    /// No helper is free.
    Full,
    /// Call `service` on `helper`.
    Act {
        /// The entity ID of the helper.
        helper: String,
        /// The service of the domain `timer`: `cancel`, `pause` or `start`.
        service: String,
        /// A copy of the entry of `helper`.
        timer: Timer,
    },
    /// Two or more timers fit the request.
    Several {
        /// The candidates, in `order`.
        timers: Vec<Candidate>,
    },
    /// The time left on the timers that fit the request.
    Left {
        /// The timers with their time left, in `order`.
        timers: Vec<Remaining>,
    },
    /// No timer fits the request.
    Missing,
}

/// A timer and its time left.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Remaining {
    /// A copy of the entry.
    pub timer: Timer,
    /// The seconds left.
    pub left: u32,
}

/// The plan of the `Set` request `request` from `satellite`, and the table after it.
#[verus_spec((planned, table) =>
    requires
        unique(timers.timers@),
    ensures
        unique(table.timers@),
        match planned {
            Plan::Start { helper, .. } => cached_free(*cache, timers.timers@, helper)
                && table.timers@.contains_key(helper) && table.timers@[helper].starting
                && table.timers@.remove(helper) == timers.timers@,
            Plan::Full => none_free(*cache, timers.timers@) && table == timers,
            _ => table == timers,
        },
)]
#[must_use]
pub fn set(
    request: &TimerRequest,
    timers: Timers,
    cache: &StateCache,
    satellite: Option<String>,
) -> (Plan, Timers) {
    let Some(spoken) = &request.duration else {
        return (Plan::NoDuration, timers);
    };
    let Some(seconds) = duration(spoken) else {
        return (Plan::Unclear, timers);
    };
    let name = name_of(request.timer.as_ref());
    if matches!(&name, Some(name) if taken(&timers, name)) {
        return (Plan::Taken, timers);
    }
    let Some(helper) = free(&timers, cache) else {
        return (Plan::Full, timers);
    };
    let timer = Timer {
        satellite,
        name,
        duration: spoken.clone(),
        order: timers.next,
        starting: true,
    };
    let mut table = timers;
    table.timers.insert(helper.clone(), timer);
    table.next = table.next.saturating_add(1);
    proof! {
        assert(table.timers@.remove(helper) =~= timers.timers@);
    }
    (Plan::Start { helper, seconds }, table)
}

/// The normalized `timer` slot `slot`, or `None` when it is missing or empty.
#[verus_verify]
fn name_of(slot: Option<&String>) -> Option<String> {
    let name = intent::normalize(slot?);
    if name.is_empty() { None } else { Some(name) }
}

/// True when an entry of `timers` has the name `name`.
#[verus_spec(found =>
    ensures
        !found ==> nameless(timers.timers@, name@),
)]
fn taken(timers: &Timers, name: &str) -> bool {
    let entries = timers.timers.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            entries_of(timers.timers@, all),
            forall|index: int| 0 <= index < entry_iter.index() ==> !named(*#[trigger] all[index].1, name@),
    ))]
    for (_, timer) in entries {
        if matches!(&timer.name, Some(own) if own.as_str() == name) {
            return true;
        }
    }
    proof! {
        assert forall|helper: String| #[trigger] timers.timers@.contains_key(helper)
            implies !named(timers.timers@[helper], name@) by {
            let index = choose|index: int| 0 <= index < all.len() && all[index] == (&helper, &timers.timers@[helper]);
            assert(*all[index].1 == timers.timers@[helper]);
        }
    }
    false
}

/// The entity ID of the first free helper of `cache`, in entity ID order.
#[verus_spec(found =>
    ensures
        match found {
            Some(helper) => cached_free(*cache, timers.timers@, helper),
            None => none_free(*cache, timers.timers@),
        },
)]
fn free(timers: &Timers, cache: &StateCache) -> Option<String> {
    let entries = cache.states.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            crate::domain::speaker::ordered(cache.states@, all),
            forall|index: int| 0 <= index < entry_iter.index() ==> !free_state(**#[trigger] all[index].1, timers.timers@),
    ))]
    for (_, state) in entries {
        if is_free(state, timers) {
            proof! {
                assert(cache.states@.contains_key(*all[entry_iter.index() as int].0));
            }
            return Some(state.entity_id.clone());
        }
    }
    proof! {
        assert forall|key: std::sync::Arc<str>| #[trigger] cache.states@.contains_key(key)
            implies !free_state(*cache.states@[key], timers.timers@) by {
            let index = choose|index: int| 0 <= index < all.len() && all[index] == (&key, &cache.states@[key]);
            assert(*all[index].1 == cache.states@[key]);
        }
    }
    None
}

/// True when `state` is a timer helper that is `idle` and has no entry in `timers`.
#[verus_spec(found =>
    ensures
        found == free_state(*state, timers.timers@),
)]
fn is_free(state: &State, timers: &Timers) -> bool {
    let helper = match state.entity_id.strip_prefix("timer.jarvis_") {
        Some(_) => {
            proof! {
                assert(state.entity_id@.subrange(0, "timer.jarvis_"@.len() as int) =~= "timer.jarvis_"@);
            }
            true
        }
        None => false,
    };
    helper && state.state.as_str() == "idle" && !timers.timers.contains_key(&state.entity_id)
}

/// A timer that an action can act on.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// The entity ID of the helper.
    pub helper: String,
    /// A copy of the entry.
    pub timer: Timer,
}

/// The entries of `timers` that `action` can act on, by the HA state of their helper in `cache`,
/// in `order`.
#[verus_spec(found =>
    ensures
        exists|kept: Seq<Candidate>| #[trigger] candidates_of(kept, timers.timers@, *cache, action)
            && crate::domain::contracts::sorted_stable(kept, found@, by_order()),
        forall|index: int| 0 <= index < found@.len()
            ==> entry_of(timers.timers@, *cache, action, #[trigger] found@[index]),
)]
#[must_use]
pub fn candidates(action: TimerAction, timers: &Timers, cache: &StateCache) -> Vec<Candidate> {
    let mut found = matching(action, timers, cache);
    proof_decl! {
        let ghost kept = found@;
    }
    proof! {
        assert(crate::domain::contracts::total(by_order()));
    }
    found.sort_by(ascending);
    proof! {
        assert(crate::domain::contracts::gives(ascending, by_order()));
        let places = choose|places: Seq<int>| crate::domain::contracts::placed(kept, found@, by_order(), places);
        assert forall|index: int| 0 <= index < found@.len()
            implies entry_of(timers.timers@, *cache, action, #[trigger] found@[index]) by {
            assert(found@[index] == kept[places[index]]);
        }
    }
    found
}

/// The entries of `timers` that `action` can act on, in entity ID order.
#[verus_spec(found =>
    ensures
        candidates_of(found@, timers.timers@, *cache, action),
)]
fn matching(action: TimerAction, timers: &Timers, cache: &StateCache) -> Vec<Candidate> {
    let mut found = Vec::new();
    let entries = timers.timers.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            listing(timers.timers@, all),
            kept_before(found@, timers.timers@, *cache, action, all, entry_iter.index()),
    ))]
    for (helper, timer) in entries {
        proof_decl! {
            let ghost before = found@;
        }
        if fits(action, cache, helper) {
            found.push(Candidate {
                helper: helper.clone(),
                timer: timer.clone(),
            });
        }
        proof! {
            lemma_kept(timers.timers@, *cache, action, all, before, found@, entry_iter.index());
        }
    }
    proof! {
        assert forall|helper: String| #[trigger] timers.timers@.contains_key(helper)
            && in_state(*cache, helper@, action) implies listed(found@, helper) by {
            let index = index_of(timers.timers@, all, helper);
        }
    }
    found
}

/// True when `cache` shows the helper `helper` in a state that `action` acts on.
#[verus_spec(found =>
    ensures
        found == in_state(*cache, helper@, action),
)]
#[must_use]
pub fn fits(action: TimerAction, cache: &StateCache, helper: &str) -> bool {
    let Some(state) = cache.states.get(helper) else {
        return false;
    };
    let state = state.state.as_str();
    match action {
        TimerAction::Set => false,
        TimerAction::Pause => state == "active",
        TimerAction::Resume => state == "paused",
        _ => state == "active" || state == "paused",
    }
}

/// The order of `first` and `second` by the order of their timers.
#[verus_spec(ordering =>
    ensures
        ordering == by_order()(*first, *second),
)]
fn ascending(first: &Candidate, second: &Candidate) -> Ordering {
    first.timer.order.cmp(&second.timer.order)
}

/// True when `cache` shows the helper `helper` `idle`.
#[verus_spec(found =>
    ensures
        found == crate::domain::speaker::shows(*cache, helper@, "idle"@),
)]
fn idle(cache: &StateCache, helper: &str) -> bool {
    matches!(cache.states.get(helper), Some(state) if state.state.as_str() == "idle")
}

/// `timers` without the entry of `helper` when it starts and has `order`: a failed start call.
#[verus_spec(table =>
    ensures
        table.timers@ == if starts(timers.timers@, helper@)
            && timers.timers@[crate::domain::contracts::string_key(helper@)].order == order {
            timers.timers@.remove(crate::domain::contracts::string_key(helper@))
        } else {
            timers.timers@
        },
)]
#[must_use]
pub fn forget(timers: Timers, helper: &str, order: u64) -> Timers {
    proof! {
        broadcast use
            crate::domain::contracts::axiom_string_contains_borrowed_key,
            crate::domain::contracts::axiom_string_maps_borrowed_key;
    }
    let mut table = timers;
    if matches!(table.timers.get(helper), Some(timer) if timer.starting && timer.order == order) {
        table.timers.remove(helper);
    }
    table
}

/// The entry of `helper`, and `timers` without it: HA ended the helper. An entry that starts stays,
/// because its start call decides it.
#[verus_spec((entry, table) =>
    ensures
        table.timers@ == if starts(timers.timers@, helper@) {
            timers.timers@
        } else {
            timers.timers@.remove(crate::domain::contracts::string_key(helper@))
        },
        starts(timers.timers@, helper@) ==> entry is None,
)]
#[must_use]
pub fn ended(timers: Timers, helper: &str) -> (Option<Timer>, Timers) {
    proof! {
        broadcast use
            crate::domain::contracts::axiom_string_contains_borrowed_key,
            crate::domain::contracts::axiom_string_maps_borrowed_key;
    }
    if matches!(timers.timers.get(helper), Some(timer) if timer.starting) {
        return (None, timers);
    }
    let mut table = timers;
    let entry = table.timers.remove(helper);
    (entry, table)
}

/// `timers` with `starting` cleared on the entry of the helper of `state` when `state` is `active`
/// or `paused`.
#[verus_spec(table =>
    ensures
        table.next == timers.next,
        table.timers@ == if acts_on(TimerAction::Cancel, state.state@)
            && timers.timers@.contains_key(state.entity_id) {
            timers.timers@.insert(state.entity_id, started(timers.timers@[state.entity_id]))
        } else {
            timers.timers@
        },
)]
#[must_use]
pub fn running(timers: Timers, state: &State) -> Timers {
    let mut table = timers;
    let now = state.state.as_str();
    if now != "active" && now != "paused" {
        return table;
    }
    if let Some(mut entry) = table.timers.remove(&state.entity_id) {
        entry.starting = false;
        table.timers.insert(state.entity_id.clone(), entry);
    }
    table
}

/// `timers` with `starting` cleared on each entry: no start call survives a restart.
#[verus_spec(table =>
    ensures
        table.next == timers.next,
        forall|helper: String| #[trigger] table.timers@.contains_key(helper)
            == timers.timers@.contains_key(helper),
        forall|helper: String| #[trigger] table.timers@.contains_key(helper)
            ==> table.timers@[helper] == started(timers.timers@[helper]),
)]
#[must_use]
pub fn restored(timers: &Timers) -> Timers {
    let mut table = BTreeMap::new();
    let entries = timers.timers.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            listing(timers.timers@, all),
            restored_before(table@, timers.timers@, all, entry_iter.index()),
    ))]
    for (helper, timer) in entries {
        let mut entry = timer.clone();
        entry.starting = false;
        table.insert(helper.clone(), entry);
    }
    proof! {
        assert forall|helper: String| #[trigger] timers.timers@.contains_key(helper)
            implies table@.contains_key(helper) by {
            let index = index_of(timers.timers@, all, helper);
        }
    }
    Timers {
        timers: table,
        next: timers.next,
    }
}

/// `timers` after a reconnect: without each entry that does not start and whose helper `cache`
/// shows `idle`, and with `starting` cleared on each entry whose helper `cache` shows `active` or
/// `paused`.
#[verus_spec(table =>
    ensures
        forall|helper: String| #[trigger] table.timers@.contains_key(helper)
            == (timers.timers@.contains_key(helper) && (timers.timers@[helper].starting
                || !crate::domain::speaker::shows(*cache, helper@, "idle"@))),
        forall|helper: String| #[trigger] table.timers@.contains_key(helper)
            ==> table.timers@[helper] == settled(timers.timers@[helper], *cache, helper),
)]
#[must_use]
pub fn reconcile(timers: &Timers, cache: &StateCache) -> Timers {
    let mut table = timers.timers.clone();
    let entries = timers.timers.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            listing(timers.timers@, all),
            reconciled(table@, timers.timers@, *cache, all, entry_iter.index()),
    ))]
    for (helper, timer) in entries {
        if timer.starting {
            // `Cancel` acts on an `active` or `paused` helper.
            if fits(TimerAction::Cancel, cache, helper) {
                let mut entry = timer.clone();
                entry.starting = false;
                table.insert(helper.clone(), entry);
            }
        } else if idle(cache, helper) {
            table.remove(helper);
        }
    }
    proof! {
        assert forall|helper: String| #[trigger] timers.timers@.contains_key(helper)
            implies reconciled_entry(table@, timers.timers@, *cache, helper) by {
            let index = index_of(timers.timers@, all, helper);
        }
    }
    Timers {
        timers: table,
        next: timers.next,
    }
}

/// The plan of the timer request `request` from `satellite` at `now`, and the table after it.
/// `similarity` is the least Jaro-Winkler similarity of a spoken name to a label.
#[verus_spec((planned, table) =>
    requires
        unique(timers.timers@),
    ensures
        !(request.action is Set) ==> match planned {
            Plan::Act { helper, .. } => timers.timers@.contains_key(helper)
                && in_state(*cache, helper@, request.action)
                && table.timers@ == if removes(request.action) {
                    timers.timers@.remove(helper)
                } else {
                    timers.timers@
                },
            Plan::Several { timers: several } => several@.len() >= 2 && table == timers,
            _ => table == timers,
        },
)]
#[must_use]
pub fn plan(
    request: &TimerRequest,
    timers: Timers,
    cache: &StateCache,
    now: Timestamp,
    satellite: Option<String>,
    similarity: f64,
) -> (Plan, Timers) {
    let action = request.action;
    if matches!(action, TimerAction::Set) {
        return set(request, timers, cache, satellite);
    }
    let mut found = candidates(action, &timers, cache);
    if matches!(action, TimerAction::CancelLast) {
        let chosen = found.pop();
        return act(action, chosen, timers);
    }
    let mut kept = meant(request.timer.as_ref(), found, similarity);
    if matches!(action, TimerAction::Left) {
        let planned = if kept.is_empty() {
            Plan::Missing
        } else {
            Plan::Left {
                timers: remaining(&kept, cache, now),
            }
        };
        return (planned, timers);
    }
    if kept.len() > 1 {
        return (Plan::Several { timers: kept }, timers);
    }
    let chosen = kept.pop();
    act(action, chosen, timers)
}

/// `Act` on `chosen` with the service of `action`, or `Missing` without a candidate, and the table
/// after it. `Cancel` and `CancelLast` remove the entry of `chosen`.
#[verus_spec((planned, table) =>
    ensures
        match planned {
            Plan::Act { helper, .. } => chosen matches Some(candidate) && candidate.helper == helper
                && table.timers@ == if removes(action) {
                    timers.timers@.remove(helper)
                } else {
                    timers.timers@
                },
            _ => planned == Plan::Missing && table == timers,
        },
)]
fn act(action: TimerAction, chosen: Option<Candidate>, timers: Timers) -> (Plan, Timers) {
    let Some(candidate) = chosen else {
        return (Plan::Missing, timers);
    };
    let mut table = timers;
    let service = match action {
        TimerAction::Cancel | TimerAction::CancelLast => {
            table.timers.remove(&candidate.helper);
            "cancel"
        }
        TimerAction::Pause => "pause",
        _ => "start",
    };
    let planned = Plan::Act {
        helper: candidate.helper,
        service: service.to_owned(),
        timer: candidate.timer,
    };
    (planned, table)
}

/// The candidates of `found` that the `timer` slot `slot` means: all of them without a name, else
/// the newest one with the label that `dialog::fill` gives for the name.
#[verus_spec(kept =>
    ensures
        within(kept@, found@),
)]
fn meant(slot: Option<&String>, found: Vec<Candidate>, similarity: f64) -> Vec<Candidate> {
    let Some(name) = slot else {
        return found;
    };
    let labels = labels(&found);
    let mut kept = Vec::new();
    let Some(label) = dialog::fill(name, &labels, similarity) else {
        return kept;
    };
    let mut rest = found;
    let mut index = labels.len();
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= labels@.len(),
            labels@.len() == rest@.len(),
            rest@ == found@,
            kept@.len() == 0,
        decreases index,
    ))]
    while index > 0 {
        index -= 1;
        if labels[index].as_str() == label.as_str() {
            kept.push(rest.swap_remove(index));
            return kept;
        }
    }
    kept
}

/// The label of each candidate of `found`: its name, else its duration.
#[verus_spec(labels =>
    ensures
        labelled(labels@, found@),
)]
fn labels(found: &[Candidate]) -> Vec<String> {
    let mut labels = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= found@.len(),
            labels@.len() == index,
            forall|earlier: int| 0 <= earlier < index
                ==> labels@[earlier] == label(#[trigger] found@[earlier].timer),
        decreases found@.len() - index,
    ))]
    while index < found.len() {
        let timer = &found[index].timer;
        let label = match &timer.name {
            Some(name) => name.clone(),
            None => timer.duration.clone(),
        };
        labels.push(label);
        index += 1;
    }
    labels
}

/// Each candidate of `found` whose helper `cache` shows with a time left at `now`, with that time.
#[verus_verify]
fn remaining(found: &[Candidate], cache: &StateCache, now: Timestamp) -> Vec<Remaining> {
    let mut timers = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= found@.len(),
        decreases found@.len() - index,
    ))]
    while index < found.len() {
        let candidate = &found[index];
        let seconds = match cache.states.get(candidate.helper.as_str()) {
            Some(state) => left(state, now),
            None => None,
        };
        if let Some(seconds) = seconds {
            timers.push(Remaining {
                timer: candidate.timer.clone(),
                left: seconds,
            });
        }
        index += 1;
    }
    timers
}

/// The decision for `plan` of `request`: a reply from the pool of the intent or of the plan with
/// the call of `Start` or `Act`, a question for a missing slot, or a System Two job for `Unclear`
/// and for a pool that does not render.
#[verus_spec(decision =>
    requires
        responses::filled(pools@),
        responses::timer_pooled(pools@),
        pools@.contains_key(crate::domain::contracts::string_key(request.intent@)),
        pools@.contains_key(crate::domain::contracts::string_key("filler"@)),
    ensures
        decision matches Decision::Reply { call: Some(made), .. } ==> timer_call(*plan, made),
        plan is Taken || plan is Full || plan is Missing || plan is Left
            ==> decision matches Decision::Reply { call: None, .. }
            || crate::domain::decide::deferred(decision, request.transcript),
        plan is Unclear ==> crate::domain::decide::deferred(decision, request.transcript),
        plan is NoDuration ==> crate::domain::decide::deferred(decision, request.transcript)
            || (slot_asked(decision, "duration"@) && asked_choices(decision).len() == 0),
        plan matches Plan::Several { timers } ==> crate::domain::decide::deferred(decision, request.transcript)
            || (slot_asked(decision, "timer"@) && labelled(asked_choices(decision), timers@)),
)]
#[must_use]
pub fn answer(plan: &Plan, request: &TimerRequest, pools: &Pools, rng: &mut StdRng) -> Decision {
    let context = context(plan, request);
    let decision = match plan {
        Plan::Unclear => None,
        Plan::Several { timers } => {
            let pending = awaiting(request, "timer", labels(timers));
            asked(pools, "ask_timer", &context, pending, rng)
        }
        Plan::NoDuration => {
            let pending = awaiting(request, "duration", Vec::new());
            asked(pools, "ask_duration", &context, pending, rng)
        }
        _ => replied(plan, request, &context, pools, rng),
    };
    match decision {
        Some(decision) => decision,
        None => Decision::Defer {
            filler: pick(pools, "filler", rng),
            job: Job::Llm(request.transcript.clone()),
        },
    }
}

/// The reply from the pool of the intent of `request` or of `plan`, with the call of `Start` or
/// `Act`, or `None` when the pool does not render with `context`.
#[verus_spec(decision =>
    requires
        responses::filled(pools@),
        responses::timer_pooled(pools@),
        pools@.contains_key(crate::domain::contracts::string_key(request.intent@)),
    ensures
        decision is None || (decision matches Some(Decision::Reply { call, .. })
            && (call matches Some(made) ==> timer_call(*plan, made))),
)]
fn replied(
    plan: &Plan,
    request: &TimerRequest,
    context: &serde_json::Map<String, Value>,
    pools: &Pools,
    rng: &mut StdRng,
) -> Option<Decision> {
    let intent = request.intent.as_str();
    let (pool, call) = match plan {
        Plan::Start { helper, seconds } => {
            let mut data = serde_json::Map::new();
            data.insert("duration".to_owned(), Value::from(*seconds));
            (intent, Some(call(helper, "start", data)))
        }
        Plan::Act {
            helper, service, ..
        } => (intent, Some(call(helper, service, serde_json::Map::new()))),
        Plan::Taken => ("timer_taken", None),
        Plan::Full => ("timer_full", None),
        Plan::Missing => ("timer_missing", None),
        _ => (intent, None),
    };
    let reply = rendered(pools, pool, context, rng)?;
    Some(Decision::Reply { reply, call })
}

/// The call of the timer service `service` on `helper` with `data`.
#[verus_spec(made =>
    ensures
        made.domain@ == "timer"@ && made.service@ == service@,
        made.target@.len() == 1 && made.target@[0]@ == helper@,
)]
fn call(helper: &str, service: &str, data: serde_json::Map<String, Value>) -> ServiceCall {
    ServiceCall {
        domain: "timer".to_owned(),
        service: service.to_owned(),
        target: vec![helper.to_owned()],
        data,
        return_response: false,
    }
}

/// The question from the pool `name` rendered with `context`, with `pending`, or `None` when the
/// pool does not render.
#[verus_spec(decision =>
    requires
        responses::filled(pools@),
        pools@.contains_key(crate::domain::contracts::string_key(name@)),
    ensures
        decision is None || (decision matches Some(Decision::Ask { pending: waiting, .. }) && waiting == pending),
)]
fn asked(
    pools: &Pools,
    name: &str,
    context: &serde_json::Map<String, Value>,
    pending: Pending,
    rng: &mut StdRng,
) -> Option<Decision> {
    let question = rendered(pools, name, context, rng)?;
    Some(Decision::Ask { question, pending })
}

/// The dialog of `request` that waits for the slot `missing` with `choices`, with the `timer` slot
/// of `request` as its slot.
#[verus_spec(waiting =>
    ensures
        waiting matches Pending::Slot { missing: asked, choices: offered, .. }
            && asked@ == missing@ && offered == choices,
)]
fn awaiting(request: &TimerRequest, missing: &str, choices: Vec<String>) -> Pending {
    let mut slots = Vec::new();
    if let Some(name) = &request.timer {
        slots.push(Slot {
            name: "timer".to_owned(),
            value: name.clone(),
        });
    }
    Pending::Slot {
        intent: request.intent.clone(),
        slots,
        missing: missing.to_owned(),
        choices,
        transcript: request.transcript.clone(),
    }
}

/// The template context of `plan`: `timer` and `duration` of the entry of `Act`, else of
/// `request`, and the `timers` of `Left` and `Several`.
#[verus_verify]
fn context(plan: &Plan, request: &TimerRequest) -> serde_json::Map<String, Value> {
    let mut context = match plan {
        Plan::Act { timer, .. } => described(timer.name.as_ref(), Some(&timer.duration), None),
        _ => described(request.timer.as_ref(), request.duration.as_ref(), None),
    };
    let mut timers = Vec::new();
    match plan {
        Plan::Left { timers: found } => {
            for remaining in found {
                let timer = &remaining.timer;
                let left = Some(remaining.left);
                let entry = described(timer.name.as_ref(), Some(&timer.duration), left);
                timers.push(Value::from(entry));
            }
        }
        Plan::Several { timers: found } => {
            for candidate in found {
                let timer = &candidate.timer;
                let entry = described(timer.name.as_ref(), Some(&timer.duration), None);
                timers.push(Value::from(entry));
            }
        }
        _ => return context,
    }
    context.insert("timers".to_owned(), Value::from(timers));
    context
}

/// The variables `timer` and `duration` with `name` and `duration`, else an empty string, and
/// `left` as spoken text when given.
#[verus_verify]
fn described(
    name: Option<&String>,
    duration: Option<&String>,
    left: Option<u32>,
) -> serde_json::Map<String, Value> {
    let mut variables = serde_json::Map::new();
    variables.insert("timer".to_owned(), Value::from(text(name)));
    variables.insert("duration".to_owned(), Value::from(text(duration)));
    if let Some(seconds) = left {
        variables.insert("left".to_owned(), Value::from(spoken(seconds)));
    }
    variables
}

/// A copy of `value`, or an empty string.
#[verus_verify]
fn text(value: Option<&String>) -> String {
    match value {
        Some(value) => value.clone(),
        None => String::new(),
    }
}

/// A random response of the pool `name` rendered with `context`, or `None` when it does not render.
#[verus_spec(
    requires
        responses::filled(pools@),
        pools@.contains_key(crate::domain::contracts::string_key(name@)),
)]
fn rendered(
    pools: &Pools,
    name: &str,
    context: &serde_json::Map<String, Value>,
    rng: &mut StdRng,
) -> Option<Chosen> {
    let mut chosen = pick(pools, name, rng);
    chosen.text = responses::render(chosen.text.as_str(), context).ok()?;
    Some(chosen)
}

/// A random response of the pool `name`, with its template as the text.
#[verus_spec(
    requires
        responses::filled(pools@),
        pools@.contains_key(crate::domain::contracts::string_key(name@)),
)]
fn pick(pools: &Pools, name: &str, rng: &mut StdRng) -> Chosen {
    proof! {
        broadcast use
            crate::domain::contracts::axiom_string_contains_borrowed_key,
            crate::domain::contracts::axiom_string_maps_borrowed_key;
    }
    let pool = pools.get(name).expect("the pool exists");
    let index = responses::select(pool, rng, None);
    Chosen {
        text: pool.responses[index].text.clone(),
        pool: name.to_owned(),
        index,
    }
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_char_pattern,
    crate::domain::contracts::axiom_str_pattern,
    crate::domain::contracts::axiom_string_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
    crate::domain::contracts::axiom_string_borrow,
    crate::domain::contracts::axiom_string_removes_borrowed_key,
};

/// True when `text` is `hours`, a colon, `minutes`, a colon and `seconds`, with the minutes and the
/// seconds less than 60, and `total` is the seconds of that time.
pub open spec fn clock_of(text: Seq<char>, hours: Seq<char>, minutes: Seq<char>, seconds: Seq<char>, total: u32) -> bool {
    &&& text == hours + seq![':'] + minutes + seq![':'] + seconds
    &&& crate::domain::contracts::natural(hours) matches Some(h)
        && crate::domain::contracts::natural(minutes) matches Some(m)
        && crate::domain::contracts::natural(seconds) matches Some(s)
        && m < 60 && s < 60 && total == h * 3600 + m * 60 + s
}

/// True when `text` is a time `H:MM:SS` of `total` seconds.
pub open spec fn is_clock(text: Seq<char>, total: u32) -> bool {
    exists|hours: Seq<char>, minutes: Seq<char>, seconds: Seq<char>| #[trigger] clock_of(text, hours, minutes, seconds, total)
}

/// True when the `remaining` attribute of `attributes` is a time `H:MM:SS` of `total` seconds.
pub open spec fn paused_left(attributes: serde_json::Map<String, serde_json::Value>, total: u32) -> bool {
    crate::domain::contracts::attribute(attributes, "remaining") matches Some(value)
        && crate::domain::contracts::text(value) matches Some(remaining)
        && is_clock(remaining, total)
}

/// True when `timer` has the name `name`.
pub open spec fn named(timer: Timer, name: Seq<char>) -> bool {
    timer.name matches Some(own) && own@ == name
}

/// True when `first` and `second` have the same name.
pub open spec fn same_name(first: Timer, second: Timer) -> bool {
    first.name matches Some(own) && named(second, own@)
}

/// True when no two entries of `timers` have the same name (`UniqueNames`).
pub open spec fn unique(timers: Map<String, Timer>) -> bool {
    forall|first: String, second: String| #![trigger timers[first], timers[second]]
        timers.contains_key(first) && timers.contains_key(second) && first != second
            ==> !same_name(timers[first], timers[second])
}

/// True when no entry of `timers` has the name `name`.
pub open spec fn nameless(timers: Map<String, Timer>, name: Seq<char>) -> bool {
    forall|helper: String| #[trigger] timers.contains_key(helper) ==> !named(timers[helper], name)
}

/// True when `all` lists each entry of `timers`.
pub open spec fn entries_of(timers: Map<String, Timer>, all: Seq<(&String, &Timer)>) -> bool {
    forall|helper: String| #[trigger] timers.contains_key(helper) ==> all.contains((&helper, &timers[helper]))
}

/// True when `state` is a timer helper that is `idle` and has no entry in `timers`.
pub open spec fn free_state(state: State, timers: Map<String, Timer>) -> bool {
    &&& "timer.jarvis_"@.is_prefix_of(state.entity_id@)
    &&& state.state@ == "idle"@
    &&& !timers.contains_key(state.entity_id)
}

/// True when `cache` has a free helper with the entity ID `helper`.
pub open spec fn cached_free(cache: StateCache, timers: Map<String, Timer>, helper: String) -> bool {
    exists|key: std::sync::Arc<str>| #[trigger] cache.states@.contains_key(key)
        && free_state(*cache.states@[key], timers) && cache.states@[key].entity_id == helper
}

/// True when `cache` has no free helper.
pub open spec fn none_free(cache: StateCache, timers: Map<String, Timer>) -> bool {
    forall|key: std::sync::Arc<str>| #[trigger] cache.states@.contains_key(key)
        ==> !free_state(*cache.states@[key], timers)
}

/// True when `action` acts on a helper in the HA state `state`.
pub open spec fn acts_on(action: TimerAction, state: Seq<char>) -> bool {
    match action {
        TimerAction::Set => false,
        TimerAction::Pause => state == "active"@,
        TimerAction::Resume => state == "paused"@,
        _ => state == "active"@ || state == "paused"@,
    }
}

/// True when `cache` shows the helper `helper` in a state that `action` acts on.
pub open spec fn in_state(cache: StateCache, helper: Seq<char>, action: TimerAction) -> bool {
    let key = crate::domain::contracts::str_key(helper);
    cache.states@.contains_key(key) && acts_on(action, cache.states@[key].state@)
}

/// True when `candidate` is an entry of `timers` with its helper, which `cache` shows in a state
/// that `action` acts on.
pub open spec fn entry_of(timers: Map<String, Timer>, cache: StateCache, action: TimerAction, candidate: Candidate) -> bool {
    &&& timers.contains_key(candidate.helper)
    &&& timers[candidate.helper] == candidate.timer
    &&& in_state(cache, candidate.helper@, action)
}

/// True when a candidate of `kept` has the helper `helper`.
pub open spec fn listed(kept: Seq<Candidate>, helper: String) -> bool {
    exists|index: int| 0 <= index < kept.len() && #[trigger] kept[index].helper == helper
}

/// True when `kept` has each candidate of `action` once: each entry of `timers` whose helper
/// `cache` shows in a state that `action` acts on.
pub open spec fn candidates_of(kept: Seq<Candidate>, timers: Map<String, Timer>, cache: StateCache, action: TimerAction) -> bool {
    &&& forall|index: int| 0 <= index < kept.len() ==> entry_of(timers, cache, action, #[trigger] kept[index])
    &&& forall|helper: String| #[trigger] timers.contains_key(helper) && in_state(cache, helper@, action)
        ==> listed(kept, helper)
    &&& distinct(kept)
}

/// The order of two candidates by the order of their timers.
pub open spec fn by_order() -> spec_fn(Candidate, Candidate) -> Ordering {
    |first: Candidate, second: Candidate| vstd::std_specs::cmp::OrdSpec::cmp_spec(&first.timer.order, &second.timer.order)
}

/// True when `all` lists each entry of `timers` once, in increasing entity ID order.
pub open spec fn listing(timers: Map<String, Timer>, all: Seq<(&String, &Timer)>) -> bool {
    &&& forall|index: int| #![trigger all[index]] 0 <= index < all.len()
        ==> timers.contains_key(*all[index].0) && timers[*all[index].0] == *all[index].1
    &&& entries_of(timers, all)
    &&& vstd::std_specs::btree::increasing_seq(all.map_values(|entry: (&String, &Timer)| *entry.0))
}

/// True when one of the first `count` entries of `all` has the helper `helper`.
pub open spec fn seen_before(all: Seq<(&String, &Timer)>, count: int, helper: String) -> bool {
    exists|index: int| 0 <= index < count && *#[trigger] all[index].0 == helper
}

/// True when no two candidates of `kept` have the same helper.
pub open spec fn distinct(kept: Seq<Candidate>) -> bool {
    forall|first: int, second: int| #![trigger kept[first], kept[second]] 0 <= first < second < kept.len()
        ==> kept[first].helper != kept[second].helper
}

/// True when `found` has each candidate of `action` among the first `count` entries of `all` once.
pub open spec fn kept_before(found: Seq<Candidate>, timers: Map<String, Timer>, cache: StateCache, action: TimerAction, all: Seq<(&String, &Timer)>, count: int) -> bool {
    &&& forall|index: int| #![trigger found[index]] 0 <= index < found.len()
        ==> entry_of(timers, cache, action, found[index]) && seen_before(all, count, found[index].helper)
    &&& forall|index: int| #![trigger all[index]] 0 <= index < count && in_state(cache, (*all[index].0)@, action)
        ==> listed(found, *all[index].0)
    &&& distinct(found)
}

/// True when `table` has the entry of `helper` of `timers` after a reconnect with `cache`: without
/// it when it does not start and `cache` shows `helper` `idle`, else `settled`.
pub open spec fn reconciled_entry(table: Map<String, Timer>, timers: Map<String, Timer>, cache: StateCache, helper: String) -> bool {
    &&& table.contains_key(helper) == (timers[helper].starting || !crate::domain::speaker::shows(cache, helper@, "idle"@))
    &&& table.contains_key(helper) ==> table[helper] == settled(timers[helper], cache, helper)
}

/// True when `table` is `timers` after a reconnect with `cache` for the first `count` entries of
/// `all`, and each other entry is its entry of `timers`, or `settled`.
pub open spec fn reconciled(table: Map<String, Timer>, timers: Map<String, Timer>, cache: StateCache, all: Seq<(&String, &Timer)>, count: int) -> bool {
    &&& forall|helper: String| #[trigger] table.contains_key(helper) ==> timers.contains_key(helper)
        && (table[helper] == timers[helper] || table[helper] == settled(timers[helper], cache, helper))
    &&& forall|helper: String| #[trigger] timers.contains_key(helper) && !table.contains_key(helper)
        ==> !timers[helper].starting && crate::domain::speaker::shows(cache, helper@, "idle"@)
    &&& forall|index: int| #![trigger all[index]] 0 <= index < count
        ==> reconciled_entry(table, timers, cache, *all[index].0)
}

/// True when `table` has the entries of `timers` with `starting` cleared, and each of the first
/// `count` entries of `all`.
pub open spec fn restored_before(table: Map<String, Timer>, timers: Map<String, Timer>, all: Seq<(&String, &Timer)>, count: int) -> bool {
    &&& forall|helper: String| #[trigger] table.contains_key(helper)
        ==> timers.contains_key(helper) && table[helper] == started(timers[helper])
    &&& forall|index: int| #![trigger all[index]] 0 <= index < count ==> table.contains_key(*all[index].0)
}

/// True when the entry of the helper `helper` in `timers` starts.
pub open spec fn starts(timers: Map<String, Timer>, helper: Seq<char>) -> bool {
    let key = crate::domain::contracts::string_key(helper);
    timers.contains_key(key) && timers[key].starting
}

/// `timer` with `starting` cleared.
pub open spec fn started(timer: Timer) -> Timer {
    Timer {
        satellite: timer.satellite,
        name: timer.name,
        duration: timer.duration,
        order: timer.order,
        starting: false,
    }
}

/// The entry `timer` of `helper` after a reconnect with `cache`: started when it starts and `cache`
/// shows `helper` `active` or `paused`.
pub open spec fn settled(timer: Timer, cache: StateCache, helper: String) -> Timer {
    if timer.starting && in_state(cache, helper@, TimerAction::Cancel) {
        started(timer)
    } else {
        timer
    }
}

/// True when `made` is a call of the domain `timer` on the helper of `plan`: `start` for `Start`,
/// the service of `Act` for `Act`.
pub open spec fn timer_call(plan: Plan, made: ServiceCall) -> bool {
    &&& made.domain@ == "timer"@
    &&& match plan {
        Plan::Start { helper, .. } => made.target@.len() == 1 && made.target@[0]@ == helper@
            && made.service@ == "start"@,
        Plan::Act { helper, service, .. } => made.target@.len() == 1 && made.target@[0]@ == helper@
            && made.service@ == service@,
        _ => false,
    }
}

/// The label of `timer`: its name, else its duration.
pub open spec fn label(timer: Timer) -> String {
    match timer.name {
        Some(name) => name,
        None => timer.duration,
    }
}

/// True when `decision` asks for the slot `missing`.
pub open spec fn slot_asked(decision: Decision, missing: Seq<char>) -> bool {
    decision matches Decision::Ask { pending: Pending::Slot { missing: asked, .. }, .. } && asked@ == missing
}

/// The choices of the slot question `decision`.
pub open spec fn asked_choices(decision: Decision) -> Seq<String> {
    match decision {
        Decision::Ask { pending: Pending::Slot { choices, .. }, .. } => choices@,
        _ => seq![],
    }
}

/// True when `labels` has the label of each candidate of `found`, in order.
pub open spec fn labelled(labels: Seq<String>, found: Seq<Candidate>) -> bool {
    &&& labels.len() == found.len()
    &&& forall|index: int| 0 <= index < found.len() ==> labels[index] == label(#[trigger] found[index].timer)
}

/// True when `action` removes the entry that it acts on.
pub open spec fn removes(action: TimerAction) -> bool {
    action is Cancel || action is CancelLast
}

/// True when each candidate of `kept` is in `found`.
pub open spec fn within(kept: Seq<Candidate>, found: Seq<Candidate>) -> bool {
    forall|index: int| 0 <= index < kept.len() ==> found.contains(#[trigger] kept[index])
}

/// One more step of the loop of `matching`: `found` is `before`, with the entry at `index` of `all`
/// when it is a candidate.
proof fn lemma_kept(timers: Map<String, Timer>, cache: StateCache, action: TimerAction, all: Seq<(&String, &Timer)>, before: Seq<Candidate>, found: Seq<Candidate>, index: int)
    requires
        listing(timers, all),
        0 <= index < all.len(),
        kept_before(before, timers, cache, action, all, index),
        found == if in_state(cache, (*all[index].0)@, action) {
            before.push(Candidate { helper: *all[index].0, timer: *all[index].1 })
        } else {
            before
        },
    ensures
        kept_before(found, timers, cache, action, all, index + 1),
{
    let keys = all.map_values(|entry: (&String, &Timer)| *entry.0);
    reveal(vstd::laws_cmp::obeys_cmp_ord);
    reveal(vstd::laws_cmp::obeys_partial_cmp_spec_properties);
    assert(vstd::laws_cmp::obeys_cmp::<String>());
    assert forall|item: int| #![trigger before[item]] 0 <= item < before.len() implies before[item].helper != *all[index].0 by {
        let earlier = choose|earlier: int| 0 <= earlier < index && *#[trigger] all[earlier].0 == before[item].helper;
        assert(vstd::std_specs::cmp::OrdSpec::cmp_spec(&keys[earlier], &keys[index]) is Less);
    }
    assert forall|earlier: int| #![trigger all[earlier]] 0 <= earlier <= index
        && in_state(cache, (*all[earlier].0)@, action) implies listed(found, *all[earlier].0) by {
        if earlier < index {
            let item = choose|item: int| 0 <= item < before.len() && #[trigger] before[item].helper == *all[earlier].0;
            assert(found[item] == before[item]);
        } else {
            assert(found[found.len() - 1].helper == *all[earlier].0);
        }
    }
}

/// The index of the entry of `helper` in `all`.
proof fn index_of(timers: Map<String, Timer>, all: Seq<(&String, &Timer)>, helper: String) -> (index: int)
    requires
        listing(timers, all),
        timers.contains_key(helper),
    ensures
        0 <= index < all.len(),
        *all[index].0 == helper,
{
    assert(all.contains((&helper, &timers[helper])));
    choose|index: int| 0 <= index < all.len() && all[index] == (&helper, &timers[helper])
}

} // verus!

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rand::SeedableRng;
    use serde_json::Value;
    use serde_json::json;

    use super::*;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;

    #[test]
    fn ac_19_2_1_durations_in_seconds() {
        let cases = [
            ("ten minutes", 600),
            ("10 minutes", 600),
            ("five minutes", 300),
            ("an hour", 3600),
            ("one hour and twenty minutes", 4800),
            ("thirty seconds", 30),
            ("1 minute and 30 seconds", 90),
        ];
        for (text, seconds) in cases {
            assert_eq!(duration(text), Some(seconds), "{text}");
        }
    }

    #[test]
    fn ac_19_2_2_unclear_durations() {
        let cases = [
            "zero minutes",
            "twenty five hours",
            "ten",
            "minutes",
            "ten bananas",
            "half an hour",
            "",
        ];
        for text in cases {
            assert_eq!(duration(text), None, "{text}");
        }
    }

    #[test]
    fn ac_19_2_3_spoken_times() {
        let cases = [
            (45, "45 seconds"),
            (61, "1 minute and 1 second"),
            (252, "4 minutes and 12 seconds"),
            (3600, "1 hour"),
            (3900, "1 hour and 5 minutes"),
            (3661, "1 hour and 1 minute"),
            (0, "0 seconds"),
        ];
        for (seconds, text) in cases {
            assert_eq!(spoken(seconds), text, "{seconds}");
        }
    }

    /// The helper `timer.jarvis_1` with the HA state `state` and the attributes `attributes`.
    fn helper(state: &str, attributes: &Value) -> State {
        let value: Value = json!({
            "entity_id": "timer.jarvis_1",
            "state": state,
            "attributes": attributes,
            "last_updated": "2026-11-22T12:00:00Z",
            "context": { "id": "c1", "user_id": null, "parent_id": null },
        });
        serde_json::from_value(value).expect("a helper state")
    }

    /// `now` of the tests.
    fn now() -> Timestamp {
        "2026-11-22T12:00:00Z".parse().expect("a timestamp")
    }

    #[test]
    fn ac_19_3_1_active_until_finishes_at() {
        let cases = [
            ("2026-11-22T12:04:12+00:00", Some(252)),
            ("2026-11-22T12:04:11.2+00:00", Some(252)),
            ("2026-11-22T11:59:00+00:00", Some(0)),
        ];
        for (finishes_at, seconds) in cases {
            let state = helper(
                "active",
                &json!({ "finishes_at": finishes_at, "remaining": "0:10:00" }),
            );
            assert_eq!(left(&state, now()), seconds, "{finishes_at}");
        }
    }

    #[test]
    fn ac_19_3_2_paused_remaining() {
        let cases = [("0:04:12", Some(252)), ("1:00:00", Some(3600))];
        for (remaining, seconds) in cases {
            let state = helper("paused", &json!({ "remaining": remaining }));
            assert_eq!(left(&state, now()), seconds, "{remaining}");
        }
    }

    #[test]
    fn ac_19_3_3_no_time_left() {
        let cases = [
            helper("idle", &json!({ "remaining": "0:10:00" })),
            helper("active", &json!({ "remaining": "0:10:00" })),
            helper("paused", &json!({ "remaining": "0:61:00" })),
        ];
        for state in cases {
            assert_eq!(left(&state, now()), None, "{}", state.state);
        }
    }

    /// The hot cache with each entity ID of `states` in its HA state.
    fn cache(states: &[(&str, &str)]) -> StateCache {
        let mut cache = StateCache::default();
        for (id, state) in states {
            let mut entity = helper(state, &json!({}));
            entity.entity_id = (*id).to_owned();
            cache.states.insert(Arc::from(*id), Arc::new(entity));
        }
        cache
    }

    /// The `idle` helpers `timer.jarvis_1` and `timer.jarvis_2`.
    fn idle() -> StateCache {
        cache(&[("timer.jarvis_1", "idle"), ("timer.jarvis_2", "idle")])
    }

    /// A `Set` request with the slots `timer` and `duration`.
    fn request(timer: Option<&str>, duration: Option<&str>) -> TimerRequest {
        TimerRequest {
            intent: "timer_set".to_owned(),
            action: TimerAction::Set,
            timer: timer.map(str::to_owned),
            duration: duration.map(str::to_owned),
            transcript: "set a timer".to_owned(),
        }
    }

    /// The table after a `Set` request with the slots `timer` and `duration` on an empty table.
    fn one(timer: Option<&str>, duration: &str, cache: &StateCache) -> Timers {
        set(
            &request(timer, Some(duration)),
            Timers::default(),
            cache,
            None,
        )
        .1
    }

    #[test]
    fn ac_19_4_1_start_on_first_free_helper() {
        let kitchen = Some("assist_satellite.kitchen".to_owned());
        let chicken = request(Some("chicken"), Some("five minutes"));
        let (plan, timers) = set(&chicken, Timers::default(), &idle(), kitchen.clone());
        let start = Plan::Start {
            helper: "timer.jarvis_1".to_owned(),
            seconds: 300,
        };
        assert_eq!(plan, start);
        let entry = Timer {
            satellite: kitchen.clone(),
            name: Some("chicken".to_owned()),
            duration: "five minutes".to_owned(),
            order: 0,
            starting: true,
        };
        assert_eq!(timers.timers.get("timer.jarvis_1"), Some(&entry));
        let generic = request(None, Some("ten minutes"));
        let (plan, timers) = set(&generic, timers, &idle(), kitchen);
        let start = Plan::Start {
            helper: "timer.jarvis_2".to_owned(),
            seconds: 600,
        };
        assert_eq!(plan, start);
        let second = timers.timers.get("timer.jarvis_2").map(|timer| timer.order);
        assert_eq!(second, Some(1), "{timers:?}");
    }

    #[test]
    fn ac_19_4_2_name_taken() {
        let timers = one(Some("chicken"), "five minutes", &idle());
        let again = request(Some("Chicken"), Some("ten minutes"));
        let (plan, after) = set(&again, timers.clone(), &idle(), None);
        assert_eq!(plan, Plan::Taken);
        assert_eq!(after, timers);
    }

    #[test]
    fn ac_19_4_3_all_helpers_in_use() {
        let cache = cache(&[
            ("timer.jarvis_1", "idle"),
            ("timer.jarvis_2", "active"),
            ("timer.kitchen", "idle"),
        ]);
        let timers = one(None, "ten minutes", &cache);
        assert!(timers.timers.contains_key("timer.jarvis_1"), "{timers:?}");
        let (plan, after) = set(
            &request(None, Some("ten minutes")),
            timers.clone(),
            &cache,
            None,
        );
        assert_eq!(plan, Plan::Full);
        assert_eq!(after, timers);
    }

    #[test]
    fn ac_19_4_4_unclear_or_no_duration() {
        let cases = [
            (Some("ten bananas"), Plan::Unclear),
            (None, Plan::NoDuration),
        ];
        for (duration, expected) in cases {
            let planned = set(&request(None, duration), Timers::default(), &idle(), None);
            assert_eq!(planned, (expected, Timers::default()), "{duration:?}");
        }
    }

    /// An entry with the name `name` and the order `order`.
    fn entry(name: &str, order: u64) -> Timer {
        Timer {
            satellite: None,
            name: Some(name.to_owned()),
            duration: "ten minutes".to_owned(),
            order,
            starting: false,
        }
    }

    /// The table with each helper of `entries` and its entry.
    fn table(entries: &[(&str, Timer)]) -> Timers {
        let timers: BTreeMap<String, Timer> = entries
            .iter()
            .map(|(helper, timer)| ((*helper).to_owned(), timer.clone()))
            .collect();
        Timers { timers, next: 3 }
    }

    /// "chicken" on an `active` helper, "pasta" on a `paused` one and "rice" on an `idle` one.
    fn three() -> (Timers, StateCache) {
        let timers = table(&[
            ("timer.jarvis_1", entry("chicken", 1)),
            ("timer.jarvis_2", entry("pasta", 0)),
            ("timer.jarvis_3", entry("rice", 2)),
        ]);
        let cache = cache(&[
            ("timer.jarvis_1", "active"),
            ("timer.jarvis_2", "paused"),
            ("timer.jarvis_3", "idle"),
        ]);
        (timers, cache)
    }

    #[test]
    fn ac_19_4b_1_candidates_in_state_and_order() {
        let (timers, cache) = three();
        let cases = [
            (TimerAction::Cancel, vec!["pasta", "chicken"]),
            (TimerAction::CancelLast, vec!["pasta", "chicken"]),
            (TimerAction::Left, vec!["pasta", "chicken"]),
            (TimerAction::Pause, vec!["chicken"]),
            (TimerAction::Resume, vec!["pasta"]),
            (TimerAction::Set, vec![]),
        ];
        for (action, expected) in cases {
            let found = candidates(action, &timers, &cache);
            let names: Vec<&str> = found
                .iter()
                .filter_map(|candidate| candidate.timer.name.as_deref())
                .collect();
            assert_eq!(names, expected, "{action:?}");
        }
    }

    #[test]
    fn ac_19_4b_2_table_follows_ha() {
        let (timers, _) = three();
        let (gone, after) = ended(timers, "timer.jarvis_2");
        assert_eq!(gone, Some(entry("pasta", 0)));
        let rest: Vec<&String> = after.timers.keys().collect();
        assert_eq!(rest, ["timer.jarvis_1", "timer.jarvis_3"]);
    }

    /// The entry "chicken" with the order `order`, whose start call runs.
    fn starting(order: u64) -> Timer {
        Timer {
            starting: true,
            ..entry("chicken", order)
        }
    }

    /// The HA state `state` of the helper `id`.
    fn state_of(id: &str, state: &str) -> State {
        let mut found = helper(state, &json!({}));
        id.clone_into(&mut found.entity_id);
        found
    }

    #[test]
    fn ac_19_9_1_start_call_marks_the_entry() {
        let timers = one(Some("chicken"), "five minutes", &idle());
        let chicken = Timer {
            duration: "five minutes".to_owned(),
            ..entry("chicken", 0)
        };
        let start = Timer {
            starting: true,
            ..chicken.clone()
        };
        assert_eq!(timers.timers.get("timer.jarvis_1"), Some(&start));
        for state in ["active", "paused"] {
            let after = running(timers.clone(), &state_of("timer.jarvis_1", state));
            assert_eq!(after.timers.get("timer.jarvis_1"), Some(&chicken));
        }
        let after = running(timers.clone(), &state_of("timer.jarvis_1", "idle"));
        assert_eq!(after, timers);
        let both = table(&[
            ("timer.jarvis_1", starting(0)),
            ("timer.jarvis_2", entry("pasta", 1)),
        ]);
        let cleared = table(&[
            ("timer.jarvis_1", entry("chicken", 0)),
            ("timer.jarvis_2", entry("pasta", 1)),
        ]);
        assert_eq!(restored(&both), cleared);
    }

    #[test]
    fn ac_19_9_2_forget_only_its_own_start() {
        let timers = table(&[
            ("timer.jarvis_1", starting(1)),
            ("timer.jarvis_2", entry("pasta", 0)),
        ]);
        let rest = table(&[("timer.jarvis_2", entry("pasta", 0))]);
        assert_eq!(forget(timers.clone(), "timer.jarvis_1", 1), rest);
        assert_eq!(forget(timers.clone(), "timer.jarvis_1", 2), timers);
        assert_eq!(forget(timers.clone(), "timer.jarvis_2", 0), timers);
    }

    #[test]
    fn ac_19_9_3_late_end_keeps_a_new_start() {
        let timers = table(&[
            ("timer.jarvis_1", starting(1)),
            ("timer.jarvis_2", entry("pasta", 0)),
        ]);
        assert_eq!(
            ended(timers.clone(), "timer.jarvis_1"),
            (None, timers.clone())
        );
        let rest = table(&[("timer.jarvis_1", starting(1))]);
        assert_eq!(
            ended(timers, "timer.jarvis_2"),
            (Some(entry("pasta", 0)), rest)
        );
    }

    #[test]
    fn ac_19_9_4_reconnect_keeps_starts() {
        let timers = table(&[
            ("timer.jarvis_1", starting(0)),
            ("timer.jarvis_2", entry("pasta", 1)),
            ("timer.jarvis_3", starting(2)),
        ]);
        let cache = cache(&[
            ("timer.jarvis_1", "idle"),
            ("timer.jarvis_2", "idle"),
            ("timer.jarvis_3", "active"),
        ]);
        let kept = table(&[
            ("timer.jarvis_1", starting(0)),
            ("timer.jarvis_3", entry("chicken", 2)),
        ]);
        assert_eq!(reconcile(&timers, &cache), kept);
    }

    /// A generic "ten minutes" entry with the order `order`.
    fn generic(order: u64) -> Timer {
        Timer {
            name: None,
            ..entry("", order)
        }
    }

    /// The plan of a request with `action` and the `timer` slot `timer`.
    fn planned(
        action: TimerAction,
        timer: Option<&str>,
        timers: &Timers,
        cache: &StateCache,
    ) -> (Plan, Timers) {
        let request = TimerRequest {
            action,
            ..request(timer, None)
        };
        plan(&request, timers.clone(), cache, now(), None, 0.85)
    }

    /// The plan `Act` with `service` on `helper` and its entry `timer`.
    fn act(helper: &str, service: &str, timer: Timer) -> Plan {
        Plan::Act {
            helper: helper.to_owned(),
            service: service.to_owned(),
            timer,
        }
    }

    /// "chicken" with the order 0 on `timer.jarvis_1` and a generic timer with the order 1 on
    /// `timer.jarvis_2`, both `active`.
    fn two() -> (Timers, StateCache) {
        let timers = table(&[
            ("timer.jarvis_1", entry("chicken", 0)),
            ("timer.jarvis_2", generic(1)),
        ]);
        let cache = cache(&[("timer.jarvis_1", "active"), ("timer.jarvis_2", "active")]);
        (timers, cache)
    }

    #[test]
    fn ac_19_4c_1_cancel_one_or_ask() {
        let timers = table(&[("timer.jarvis_1", entry("chicken", 0))]);
        let one = cache(&[("timer.jarvis_1", "active")]);
        let (plan, after) = planned(TimerAction::Cancel, None, &timers, &one);
        assert_eq!(plan, act("timer.jarvis_1", "cancel", entry("chicken", 0)));
        assert!(after.timers.is_empty(), "{after:?}");
        let (timers, cache) = two();
        let several: Vec<Candidate> = candidates(TimerAction::Cancel, &timers, &cache);
        let names: Vec<Option<String>> = several
            .iter()
            .map(|candidate| candidate.timer.name.clone())
            .collect();
        assert_eq!(names, [Some("chicken".to_owned()), None]);
        let planned = planned(TimerAction::Cancel, None, &timers, &cache);
        assert_eq!(planned, (Plan::Several { timers: several }, timers));
    }

    #[test]
    fn ac_19_4c_2_cancel_by_name() {
        let (timers, cache) = two();
        let (plan, after) = planned(TimerAction::Cancel, Some("the chicken"), &timers, &cache);
        assert_eq!(plan, act("timer.jarvis_1", "cancel", entry("chicken", 0)));
        assert!(!after.timers.contains_key("timer.jarvis_1"), "{after:?}");
        let pizza = planned(TimerAction::Cancel, Some("pizza"), &timers, &cache);
        assert_eq!(pizza, (Plan::Missing, timers));
    }

    #[test]
    fn ac_19_4c_3_cancel_newest() {
        let (timers, cache) = two();
        let (plan, _) = planned(TimerAction::CancelLast, None, &timers, &cache);
        assert_eq!(plan, act("timer.jarvis_2", "cancel", generic(1)));
    }

    #[test]
    fn ac_19_4c_4_pause_and_resume() {
        let timers = table(&[
            ("timer.jarvis_1", entry("chicken", 1)),
            ("timer.jarvis_2", entry("pasta", 0)),
        ]);
        let cache = cache(&[("timer.jarvis_1", "active"), ("timer.jarvis_2", "paused")]);
        let pause = planned(TimerAction::Pause, None, &timers, &cache);
        let paused = act("timer.jarvis_1", "pause", entry("chicken", 1));
        assert_eq!(pause, (paused, timers.clone()));
        let resume = planned(TimerAction::Resume, None, &timers, &cache);
        let resumed = act("timer.jarvis_2", "start", entry("pasta", 0));
        assert_eq!(resume, (resumed, timers));
    }

    /// The hot cache of `two` with "chicken" `active` until 252 s after `now` and the generic timer
    /// `paused` with 10 minutes left.
    fn timed() -> StateCache {
        let chicken = json!({ "finishes_at": "2026-11-22T12:04:12+00:00" });
        let ten = json!({ "remaining": "0:10:00" });
        let mut cache = StateCache::default();
        for (id, state, attributes) in [
            ("timer.jarvis_1", "active", chicken),
            ("timer.jarvis_2", "paused", ten),
        ] {
            let mut entity = helper(state, &attributes);
            entity.entity_id = id.to_owned();
            cache.states.insert(Arc::from(id), Arc::new(entity));
        }
        cache
    }

    #[test]
    fn ac_19_4c_5_time_left() {
        let (timers, _) = two();
        let cache = timed();
        let chicken = Remaining {
            timer: entry("chicken", 0),
            left: 252,
        };
        let ten = Remaining {
            timer: generic(1),
            left: 600,
        };
        let (all, after) = planned(TimerAction::Left, None, &timers, &cache);
        assert_eq!(
            all,
            Plan::Left {
                timers: vec![chicken.clone(), ten]
            }
        );
        assert_eq!(after, timers);
        let named = planned(TimerAction::Left, Some("chicken"), &timers, &cache).0;
        assert_eq!(
            named,
            Plan::Left {
                timers: vec![chicken]
            }
        );
        let none = planned(TimerAction::Left, None, &timers, &StateCache::default());
        assert_eq!(none.0, Plan::Missing);
    }

    /// The pools of `responses.toml`, each response with no uses.
    fn pools() -> Pools {
        let file: toml::Table =
            toml::from_str(include_str!("../../responses.toml")).expect("responses.toml is TOML");
        let mut pools = Pools::new();
        for (name, pool) in file["pools"].as_table().expect("pools is a table") {
            let texts: Vec<String> = pool["responses"].clone().try_into().expect("texts");
            let responses = texts.into_iter().map(|text| Response {
                text,
                uses: 0,
                generated: false,
            });
            let responses = responses.collect();
            pools.insert(
                name.clone(),
                Pool {
                    responses,
                    last: None,
                },
            );
        }
        pools
    }

    /// The answer to `plan` for a request of `intent` with the slots `timer` and `duration`.
    fn answered(
        plan: &Plan,
        intent: &str,
        timer: Option<&str>,
        duration: Option<&str>,
    ) -> Decision {
        let request = TimerRequest {
            intent: intent.to_owned(),
            ..request(timer, duration)
        };
        answer(plan, &request, &pools(), &mut StdRng::seed_from_u64(1))
    }

    /// The text and the call of the reply `decision`.
    fn replied(decision: Decision) -> (String, Option<ServiceCall>) {
        let Decision::Reply { reply, call } = decision else {
            panic!("{decision:?} is not a reply");
        };
        (reply.text, call)
    }

    /// The text and the pending dialog of the question `decision`.
    fn asked(decision: Decision) -> (String, Pending) {
        let Decision::Ask { question, pending } = decision else {
            panic!("{decision:?} is not a question");
        };
        (question.text, pending)
    }

    /// The call of the timer service `service` on `timer.jarvis_1` with `data`.
    fn called(service: &str, data: &Value) -> ServiceCall {
        ServiceCall {
            domain: "timer".to_owned(),
            service: service.to_owned(),
            target: vec!["timer.jarvis_1".to_owned()],
            data: data.as_object().expect("data is an object").clone(),
            return_response: false,
        }
    }

    #[test]
    fn ac_19_6_1_start_replies_and_starts_the_helper() {
        let plan = Plan::Start {
            helper: "timer.jarvis_1".to_owned(),
            seconds: 300,
        };
        let decision = answered(&plan, "timer_set", Some("chicken"), Some("five minutes"));
        let text = "Your chicken timer is set for five minutes.".to_owned();
        let call = Some(called("start", &json!({ "duration": 300 })));
        assert_eq!(replied(decision), (text, call));
        let decision = answered(&plan, "timer_set", None, Some("ten minutes"));
        assert_eq!(replied(decision).0, "Timer set for ten minutes.");
    }

    #[test]
    fn ac_19_6_2_act_replies_and_calls_the_service() {
        let plan = act("timer.jarvis_1", "cancel", generic(0));
        let decision = answered(&plan, "timer_cancel", None, None);
        let text = "I cancelled the timer for ten minutes.".to_owned();
        assert_eq!(
            replied(decision),
            (text, Some(called("cancel", &json!({}))))
        );
    }

    #[test]
    fn ac_19_6_3_taken_full_and_missing_reply_with_no_call() {
        let cases = [
            (
                Plan::Taken,
                Some("chicken"),
                "There is already a chicken timer.",
            ),
            (Plan::Full, None, "All timers are in use."),
            (Plan::Missing, Some("pizza"), "There is no pizza timer."),
            (Plan::Missing, None, "No timer is running."),
        ];
        for (plan, timer, text) in cases {
            let decision = answered(&plan, "timer_cancel", timer, None);
            assert_eq!(replied(decision), (text.to_owned(), None), "{plan:?}");
        }
    }

    #[test]
    fn ac_19_6_4_unclear_defers() {
        let decision = answered(&Plan::Unclear, "timer_set", None, Some("ten bananas"));
        let deferred = matches!(&decision, Decision::Defer { job: Job::Llm(asked), .. } if asked == "set a timer");
        assert!(deferred, "{decision:?}");
    }

    /// The candidate of `timer` on `helper`.
    fn candidate(helper: &str, timer: Timer) -> Candidate {
        Candidate {
            helper: helper.to_owned(),
            timer,
        }
    }

    #[test]
    fn ac_19_6b_1_several_asks_which_timer() {
        let chicken = candidate("timer.jarvis_1", entry("chicken", 0));
        let pizza = candidate("timer.jarvis_2", entry("pizza", 1));
        let plan = Plan::Several {
            timers: vec![chicken.clone(), pizza.clone()],
        };
        let pending = Pending::Slot {
            intent: "timer_cancel".to_owned(),
            slots: Vec::new(),
            missing: "timer".to_owned(),
            choices: vec!["chicken".to_owned(), "pizza".to_owned()],
            transcript: "set a timer".to_owned(),
        };
        let text = "Which timer, the chicken timer or the pizza timer?".to_owned();
        let decision = answered(&plan, "timer_cancel", None, None);
        assert_eq!(asked(decision), (text, pending));
        let ten = candidate("timer.jarvis_3", generic(2));
        let plan = Plan::Several {
            timers: vec![chicken, pizza, ten],
        };
        let text = "Which timer, the chicken timer, the pizza timer or the timer for ten minutes?";
        assert_eq!(asked(answered(&plan, "timer_cancel", None, None)).0, text);
    }

    #[test]
    fn ac_19_6b_2_left_lists_the_spoken_times() {
        let chicken = Remaining {
            timer: entry("chicken", 0),
            left: 252,
        };
        let ten = Remaining {
            timer: generic(1),
            left: 600,
        };
        let plan = Plan::Left {
            timers: vec![chicken, ten],
        };
        let text =
            "The chicken timer has 4 minutes and 12 seconds left. The timer for ten minutes \
            has 10 minutes left."
                .to_owned();
        let decision = answered(&plan, "timer_left", None, None);
        assert_eq!(replied(decision), (text, None));
    }

    #[test]
    fn ac_19_6b_3_no_duration_asks_for_how_long() {
        let decision = answered(&Plan::NoDuration, "timer_set", Some("chicken"), None);
        let pending = Pending::Slot {
            intent: "timer_set".to_owned(),
            slots: vec![Slot {
                name: "timer".to_owned(),
                value: "chicken".to_owned(),
            }],
            missing: "duration".to_owned(),
            choices: Vec::new(),
            transcript: "set a timer".to_owned(),
        };
        assert_eq!(asked(decision), ("For how long?".to_owned(), pending));
    }
}
