//! Hot cache: load, apply and queries.

use std::collections::BTreeMap;
use std::sync::Arc;

use vstd::prelude::*;

use crate::domain::state::Area;
use crate::domain::state::Device;
use crate::domain::state::Entity;
use crate::domain::state::Event;
use crate::domain::state::Snapshot;
use crate::domain::state::State;

/// The hot cache: the entity states and the registries of HA.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateCache {
    /// The state of each entity, by entity ID.
    pub states: BTreeMap<Arc<str>, Arc<State>>,
    /// The area registry.
    pub areas: Arc<Vec<Area>>,
    /// The device registry.
    pub devices: Arc<BTreeMap<Arc<str>, Device>>,
    /// The entity registry.
    pub entities: Arc<Vec<Entity>>,
}

#[verus_verify]
impl StateCache {
    /// A cache with the states and the registries of `snapshot` only.
    #[verus_spec(cache =>
        ensures
            unwrapped(cache.states@) == snapshot_states(snapshot.states@),
    )]
    #[must_use]
    pub fn load(snapshot: Snapshot) -> Self {
        let mut states = BTreeMap::new();
        let mut remaining = snapshot.states;
        #[cfg_attr(verus_only, verus_spec(
            invariant
                remaining.len() <= snapshot.states.len(),
                remaining@ == snapshot.states@.take(remaining.len() as int),
                unwrapped(states@) == snapshot_states(snapshot.states@.skip(remaining.len() as int)),
            ensures
                remaining.len() == 0,
            decreases remaining.len(),
        ))]
        while let Some(state) = remaining.pop() {
            proof! {
                assert(remaining@ =~= snapshot.states@.take(remaining.len() as int));
                assert(snapshot.states@.skip(remaining.len() as int).drop_first()
                    =~= snapshot.states@.skip(remaining.len() + 1));
            }
            let key = Arc::from(state.entity_id.clone());
            let state = Arc::new(state);
            proof! {
                lemma_unwrapped_insert(states@, key, state);
            }
            states.insert(key, state);
        }
        proof! {
            assert(snapshot.states@.skip(0) =~= snapshot.states@);
        }
        Self {
            states,
            areas: Arc::new(snapshot.areas),
            devices: Arc::new(by_id(snapshot.devices)),
            entities: Arc::new(snapshot.entities),
        }
    }

    /// The cache after `event`.
    #[verus_spec(cache =>
        ensures
            applied(self, event, cache),
    )]
    #[must_use]
    pub fn apply(mut self, event: Event) -> Self {
        if let Event::StateChanged { entity_id, new, .. } = event {
            let key: Arc<str> = Arc::from(entity_id);
            match new {
                Some(new) => {
                    let newer = matches!(self.states.get(&key), Some(cached)
                        if cached.last_updated.as_nanosecond() > new.last_updated.as_nanosecond());
                    if !newer {
                        let new = Arc::new(*new);
                        proof! {
                            lemma_unwrapped_insert(self.states@, key, new);
                        }
                        self.states.insert(key, new);
                    }
                }
                None => {
                    self.states.remove(&key);
                }
            }
        }
        self
    }

    /// The state of `entity_id`.
    #[must_use]
    pub fn state(&self, entity_id: &str) -> Option<&State> {
        self.states.get(entity_id).map(|state| &**state)
    }
}

/// The devices by device ID.
#[verus_verify]
fn by_id(mut devices: Vec<Device>) -> BTreeMap<Arc<str>, Device> {
    let mut map = BTreeMap::new();
    #[cfg_attr(verus_only, verus_spec(decreases devices.len()))]
    while let Some(device) = devices.pop() {
        map.insert(Arc::from(device.id.clone()), device);
    }
    map
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
};

/// The cache key of `entity_id`.
pub open spec fn key(entity_id: String) -> Arc<str> {
    crate::domain::contracts::str_key(entity_id@)
}

/// The states that `states` point to.
pub open spec fn unwrapped(states: Map<Arc<str>, Arc<State>>) -> Map<Arc<str>, State> {
    states.map_values(|state: Arc<State>| *state)
}

/// The state of each entity in `states`. The first state of an entity wins.
pub open spec fn snapshot_states(states: Seq<State>) -> Map<Arc<str>, State>
    decreases states.len(),
{
    if states.len() == 0 {
        Map::empty()
    } else {
        snapshot_states(states.drop_first()).insert(key(states[0].entity_id), states[0])
    }
}

/// True when `later` is after `earlier`.
pub open spec fn newer(later: jiff::Timestamp, earlier: jiff::Timestamp) -> bool {
    crate::domain::contracts::nanosecond(later) > crate::domain::contracts::nanosecond(earlier)
}

/// True when `cache` is `before` after `event`.
pub open spec fn applied(before: StateCache, event: Event, cache: StateCache) -> bool {
    match event {
        Event::StateChanged { entity_id, new, .. } => {
            let key = key(entity_id);
            match new {
                Some(new) => if before.states@.contains_key(key)
                    && newer(before.states@[key].last_updated, new.last_updated) {
                    cache.states@ == before.states@
                } else {
                    unwrapped(cache.states@) == unwrapped(before.states@).insert(key, *new)
                },
                None => cache.states@ == before.states@.remove(key),
            }
        },
        _ => cache == before,
    }
}

/// Unwrapping after an insert is an insert of the unwrapped state.
pub proof fn lemma_unwrapped_insert(
    states: Map<Arc<str>, Arc<State>>,
    key: Arc<str>,
    state: Arc<State>,
)
    ensures
        unwrapped(states.insert(key, state)) == unwrapped(states).insert(key, *state),
{
    assert(unwrapped(states.insert(key, state)) =~= unwrapped(states).insert(key, *state));
}

} // verus!
