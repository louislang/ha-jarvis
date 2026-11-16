//! Hot cache: load, apply and queries.

use std::collections::BTreeMap;
use std::sync::Arc;

use vstd::prelude::*;

use crate::domain::select;
use crate::domain::state::Area;
use crate::domain::state::AreaId;
use crate::domain::state::Device;
use crate::domain::state::Entity;
use crate::domain::state::EntityId;
use crate::domain::state::Event;
use crate::domain::state::HomeConfig;
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
    /// The HA configuration, after the first load.
    pub config: Option<HomeConfig>,
}

/// The mute switches of the satellites.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct MuteSwitches {
    /// The mute switches whose state is `off`.
    pub off: Vec<EntityId>,
    /// The satellites without a mute switch.
    pub missing: Vec<EntityId>,
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
            config: Some(snapshot.config),
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

    /// The HA configuration of the snapshot, after the first load.
    #[must_use]
    pub fn config(&self) -> Option<&HomeConfig> {
        self.config.as_ref()
    }

    /// The state of `entity_id`.
    #[must_use]
    pub fn state(&self, entity_id: &str) -> Option<&State> {
        self.states.get(entity_id).map(|state| &**state)
    }

    /// The area of the device `device_id`.
    #[verus_spec(area =>
        ensures
            owned(area) == device_area(self.devices@, device_id@),
    )]
    #[must_use]
    pub fn area_of_device(&self, device_id: &str) -> Option<&AreaId> {
        self.devices.get(device_id)?.area_id.as_ref()
    }

    /// The area of `entity_id`, else the area of its device.
    #[verus_spec(area =>
        ensures
            forall|index: int| #[trigger] first(self.entities@, entity_id@, index)
                ==> owned(area) == entity_area(*self, self.entities@[index]),
    )]
    #[must_use]
    pub fn area_of_entity(&self, entity_id: &str) -> Option<&AreaId> {
        let mut index = 0;
        #[cfg_attr(verus_only, verus_spec(
            invariant
                index <= self.entities@.len(),
                forall|earlier: int| 0 <= earlier < index
                    ==> #[trigger] self.entities@[earlier].entity_id.0@ != entity_id@,
            decreases self.entities@.len() - index,
        ))]
        while index < self.entities.len() {
            let entity = &self.entities[index];
            if entity.entity_id.0.as_str() == entity_id {
                return self.entity_area(entity);
            }
            index += 1;
        }
        None
    }

    /// The area of `entity`, else the area of its device.
    #[verus_spec(area =>
        ensures
            owned(area) == entity_area(*self, *entity),
    )]
    #[must_use]
    pub fn entity_area<'cache>(&'cache self, entity: &'cache Entity) -> Option<&'cache AreaId> {
        match &entity.area_id {
            Some(area) => Some(area),
            None => self.area_of_device(entity.device_id.as_ref()?),
        }
    }

    /// The IDs of the entities in `area` and `domain`, with `device_class` if given.
    #[verus_spec(found =>
        ensures
            forall|position: int| 0 <= position < found@.len() ==> chosen(
                *self, *#[trigger] found@[position], self.entities@.len() as int,
                *area, domain@, optional(device_class)),
    )]
    #[must_use]
    pub fn entities(
        &self,
        area: &AreaId,
        domain: &str,
        device_class: Option<&str>,
    ) -> Vec<&EntityId> {
        let mut found: Vec<&EntityId> = Vec::new();
        let mut index = 0;
        #[cfg_attr(verus_only, verus_spec(
            invariant
                index <= self.entities@.len(),
                forall|position: int| 0 <= position < found@.len() ==> chosen(
                    *self, *#[trigger] found@[position], index as int,
                    *area, domain@, optional(device_class)),
            decreases self.entities@.len() - index,
        ))]
        while index < self.entities.len() {
            let entity = &self.entities[index];
            if self.selects(entity, area, domain, device_class) {
                found.push(&entity.entity_id);
            }
            index += 1;
        }
        found
    }

    /// True when `entity` is in `area` and `domain`, with `device_class` if given.
    #[verus_spec(selects =>
        ensures
            selects == selected(*self, *entity, *area, domain@, optional(device_class)),
    )]
    fn selects(
        &self,
        entity: &Entity,
        area: &AreaId,
        domain: &str,
        device_class: Option<&str>,
    ) -> bool {
        Self::in_domain(&entity.entity_id.0, domain)
            && matches!(self.entity_area(entity), Some(found) if found.0.as_str() == area.0.as_str())
            && match device_class {
                Some(class) => self.device_class(&entity.entity_id.0) == Some(class),
                None => true,
            }
    }

    /// True when the entity ID `entity_id` is `domain`, a dot and an object ID.
    #[verus_spec(found =>
        ensures
            found == in_domain(entity_id@, domain@),
    )]
    #[must_use]
    pub fn in_domain(entity_id: &str, domain: &str) -> bool {
        let id = entity_id.as_bytes();
        let prefix = domain.as_bytes();
        proof! {
            if prefix.len() < id.len() {
                assert(id@.take(prefix@.len() as int) =~= prefix@ <==> id.starts_with(prefix));
            }
        }
        prefix.len() < id.len() && id.starts_with(prefix) && id[prefix.len()] == b'.'
    }

    /// The `device_class` attribute of the state of `entity_id`.
    #[verus_spec(class =>
        ensures
            optional(class) == device_class(*self, entity_id@),
    )]
    #[must_use]
    pub fn device_class(&self, entity_id: &str) -> Option<&str> {
        self.states
            .get(entity_id)?
            .attributes
            .get("device_class")?
            .as_str()
    }

    /// The area whose name or alias is `text`, without case sensitivity.
    #[verus_spec(area =>
        ensures
            match area {
                Some(id) => exists|index: int| 0 <= index < self.areas@.len()
                    && *id == #[trigger] self.areas@[index].area_id
                    && named(self.areas@[index], crate::domain::contracts::lowercase(text@)),
                None => forall|index: int| 0 <= index < self.areas@.len()
                    ==> !named(#[trigger] self.areas@[index], crate::domain::contracts::lowercase(text@)),
            },
    )]
    #[must_use]
    pub fn area_by_name(&self, text: &str) -> Option<&AreaId> {
        let lower = text.to_lowercase();
        let mut index = 0;
        #[cfg_attr(verus_only, verus_spec(
            invariant
                index <= self.areas@.len(),
                lower@ == crate::domain::contracts::lowercase(text@),
                forall|earlier: int| 0 <= earlier < index
                    ==> !named(#[trigger] self.areas@[earlier], lower@),
            decreases self.areas@.len() - index,
        ))]
        while index < self.areas.len() {
            let area = &self.areas[index];
            if has_name(area, &lower) {
                return Some(&area.area_id);
            }
            index += 1;
        }
        None
    }
}

/// True when the name or an alias of `area` is `lower` in lowercase.
#[verus_spec(found =>
    ensures
        found == named(*area, lower@),
)]
fn has_name(area: &Area, lower: &str) -> bool {
    if area.name.to_lowercase().as_str() == lower {
        return true;
    }
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= area.aliases@.len(),
            crate::domain::contracts::lowercase(area.name@) != lower@,
            forall|earlier: int| 0 <= earlier < index
                ==> crate::domain::contracts::lowercase(#[trigger] area.aliases@[earlier]@) != lower@,
        decreases area.aliases@.len() - index,
    ))]
    while index < area.aliases.len() {
        if area.aliases[index].to_lowercase().as_str() == lower {
            return true;
        }
        index += 1;
    }
    false
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

/// The mute switches in `cache` whose state is `off`, and the satellites without a mute switch.
#[verus_spec(switches =>
    ensures
        off_list(*cache, switches.off@, cache.entities@.len() as int),
        missing_list(*cache, switches.missing@, cache.entities@.len() as int),
)]
#[must_use]
pub fn mute_switches(cache: &StateCache) -> MuteSwitches {
    MuteSwitches {
        off: unmuted_switches(cache),
        missing: unswitched_satellites(cache),
    }
}

/// The mute switches in `cache` on a satellite device, with the state `off`.
#[verus_spec(off =>
    ensures
        off_list(*cache, off@, cache.entities@.len() as int),
)]
fn unmuted_switches(cache: &StateCache) -> Vec<EntityId> {
    let mut off: Vec<EntityId> = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            off_list(*cache, off@, index as int),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_unmuted(cache, entity) {
            off.push(EntityId(entity.entity_id.0.clone()));
        }
        index += 1;
    }
    off
}

/// The satellites in `cache` without a device or without a mute switch on their device.
#[verus_spec(missing =>
    ensures
        missing_list(*cache, missing@, cache.entities@.len() as int),
)]
fn unswitched_satellites(cache: &StateCache) -> Vec<EntityId> {
    let mut missing: Vec<EntityId> = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            missing_list(*cache, missing@, index as int),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_unswitched(cache, entity) {
            missing.push(EntityId(entity.entity_id.0.clone()));
        }
        index += 1;
    }
    missing
}

/// True when `entity` is a mute switch on a satellite device, with the state `off`.
#[verus_spec(found =>
    ensures
        found == unmuted(*cache, *entity),
)]
fn is_unmuted(cache: &StateCache, entity: &Entity) -> bool {
    let id = entity.entity_id.0.as_str();
    is_mute_switch(id)
        && matches!(cache.states.get(id), Some(state) if state.state.as_str() == "off")
        && matches!(&entity.device_id, Some(device) if select::is_satellite(cache, device.as_str()))
}

/// True when `entity` is a satellite without a device or without a mute switch on its device.
#[verus_spec(found =>
    ensures
        found == unswitched(*cache, *entity),
)]
fn is_unswitched(cache: &StateCache, entity: &Entity) -> bool {
    StateCache::in_domain(entity.entity_id.0.as_str(), "assist_satellite")
        && !matches!(&entity.device_id, Some(device) if has_mute_switch(cache, device.as_str()))
}

/// True when the device `device` has a mute switch.
#[verus_spec(found =>
    ensures
        found == switched(*cache, device@),
)]
fn has_mute_switch(cache: &StateCache, device: &str) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> !switch_of(#[trigger] cache.entities@[earlier], device@),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_mute_switch(entity.entity_id.0.as_str())
            && matches!(&entity.device_id, Some(id) if id.as_str() == device)
        {
            return true;
        }
        index += 1;
    }
    false
}

/// True when `entity_id` is a `switch` entity ID that ends in `_mute_microphones`.
#[verus_spec(found =>
    ensures
        found == mute_switch(entity_id@),
)]
fn is_mute_switch(entity_id: &str) -> bool {
    let id = entity_id.as_bytes();
    let suffix = "_mute_microphones".as_bytes();
    proof! {
        if suffix.len() <= id.len() {
            assert(id@.skip(id.len() - suffix.len()) =~= suffix@ <==> id.ends_with(suffix));
        }
    }
    StateCache::in_domain(entity_id, "switch") && id.ends_with(suffix)
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
    vstd::seq_lib::lemma_seq_contains_after_push,
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

/// The value that `value` points to.
pub open spec fn owned<Item>(value: Option<&Item>) -> Option<Item> {
    match value {
        Some(item) => Some(*item),
        None => None,
    }
}

/// The text of `value`.
pub open spec fn optional<Text: View<V = Seq<char>>>(value: Option<Text>) -> Option<Seq<char>> {
    match value {
        Some(text) => Some(text@),
        None => None,
    }
}

/// The area of the device `device_id` in `devices`.
pub open spec fn device_area(devices: Map<Arc<str>, Device>, device_id: Seq<char>) -> Option<AreaId> {
    let key = crate::domain::contracts::str_key(device_id);
    if devices.contains_key(key) {
        devices[key].area_id
    } else {
        None
    }
}

/// The area of `entity`, else the area of its device.
pub open spec fn entity_area(cache: StateCache, entity: Entity) -> Option<AreaId> {
    match entity.area_id {
        Some(area) => Some(area),
        None => match entity.device_id {
            Some(device) => device_area(cache.devices@, device@),
            None => None,
        },
    }
}

/// True when `index` is the first entity of `entities` with the ID `entity_id`.
pub open spec fn first(entities: Seq<Entity>, entity_id: Seq<char>, index: int) -> bool {
    &&& 0 <= index < entities.len()
    &&& entities[index].entity_id.0@ == entity_id
    &&& forall|earlier: int| 0 <= earlier < index ==> #[trigger] entities[earlier].entity_id.0@ != entity_id
}

/// True when the entity ID `entity_id` is `domain`, a dot and an object ID.
pub open spec fn in_domain(entity_id: Seq<char>, domain: Seq<char>) -> bool {
    let id = vstd::utf8::encode_utf8(entity_id);
    let prefix = vstd::utf8::encode_utf8(domain);
    prefix.len() < id.len() && id.take(prefix.len() as int) == prefix && id[prefix.len() as int] == b'.'
}

/// The `device_class` attribute of the state of `entity_id`.
pub open spec fn device_class(cache: StateCache, entity_id: Seq<char>) -> Option<Seq<char>> {
    let key = crate::domain::contracts::str_key(entity_id);
    if cache.states@.contains_key(key) {
        match crate::domain::contracts::attribute(cache.states@[key].attributes, "device_class") {
            Some(value) => crate::domain::contracts::text(value),
            None => None,
        }
    } else {
        None
    }
}

/// True when `entity` is in `area` and `domain`, with `class` if given.
pub open spec fn selected(
    cache: StateCache,
    entity: Entity,
    area: AreaId,
    domain: Seq<char>,
    class: Option<Seq<char>>,
) -> bool {
    &&& in_domain(entity.entity_id.0@, domain)
    &&& entity_area(cache, entity) matches Some(found) && found.0@ == area.0@
    &&& class matches Some(wanted) ==> device_class(cache, entity.entity_id.0@) == Some(wanted)
}

/// True when `id` is the ID of a selected entity among the first `count` entities.
pub open spec fn chosen(
    cache: StateCache,
    id: EntityId,
    count: int,
    area: AreaId,
    domain: Seq<char>,
    class: Option<Seq<char>>,
) -> bool {
    exists|index: int| 0 <= index < count
        && id == (#[trigger] cache.entities@[index]).entity_id
        && selected(cache, cache.entities@[index], area, domain, class)
}

/// True when the name or an alias of `area` is `lower` in lowercase.
pub open spec fn named(area: Area, lower: Seq<char>) -> bool {
    crate::domain::contracts::lowercase(area.name@) == lower || aliased(area.aliases@, lower)
}

/// True when an alias in `aliases` is `lower` in lowercase.
pub open spec fn aliased(aliases: Seq<String>, lower: Seq<char>) -> bool {
    exists|index: int| 0 <= index < aliases.len()
        && crate::domain::contracts::lowercase(#[trigger] aliases[index]@) == lower
}

/// True when the entity ID `entity_id` is a `switch` entity ID that ends in `_mute_microphones`.
pub open spec fn mute_switch(entity_id: Seq<char>) -> bool {
    let id = vstd::utf8::encode_utf8(entity_id);
    let suffix = vstd::utf8::encode_utf8("_mute_microphones"@);
    &&& in_domain(entity_id, "switch"@)
    &&& suffix.len() <= id.len() && id.skip(id.len() - suffix.len()) == suffix
}

/// True when `entity` is a mute switch on a satellite device in `cache`, with the state `off`.
pub open spec fn unmuted(cache: StateCache, entity: Entity) -> bool {
    let key = crate::domain::contracts::str_key(entity.entity_id.0@);
    &&& mute_switch(entity.entity_id.0@)
    &&& cache.states@.contains_key(key) && cache.states@[key].state@ == "off"@
    &&& entity.device_id matches Some(device) && crate::domain::select::satellite(cache, device@)
}

/// True when `entity` is a mute switch of the device `device`.
pub open spec fn switch_of(entity: Entity, device: Seq<char>) -> bool {
    &&& mute_switch(entity.entity_id.0@)
    &&& entity.device_id matches Some(id) && id@ == device
}

/// True when the device `device` has a mute switch in `cache`.
pub open spec fn switched(cache: StateCache, device: Seq<char>) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && switch_of(#[trigger] cache.entities@[index], device)
}

/// True when `entity` is a satellite without a device or without a mute switch on its device.
pub open spec fn unswitched(cache: StateCache, entity: Entity) -> bool {
    &&& in_domain(entity.entity_id.0@, "assist_satellite"@)
    &&& !(entity.device_id matches Some(device) && switched(cache, device@))
}

/// True when `id` is the ID of an unmuted mute switch among the first `count` entities.
pub open spec fn off_among(cache: StateCache, id: EntityId, count: int) -> bool {
    exists|index: int| 0 <= index < count
        && id == (#[trigger] cache.entities@[index]).entity_id
        && unmuted(cache, cache.entities@[index])
}

/// True when `id` is the ID of a satellite without a mute switch among the first `count` entities.
pub open spec fn missing_among(cache: StateCache, id: EntityId, count: int) -> bool {
    exists|index: int| 0 <= index < count
        && id == (#[trigger] cache.entities@[index]).entity_id
        && unswitched(cache, cache.entities@[index])
}

/// True when `off` lists exactly the unmuted mute switches among the first `count` entities.
pub open spec fn off_list(cache: StateCache, off: Seq<EntityId>, count: int) -> bool {
    &&& forall|position: int| 0 <= position < off.len()
        ==> off_among(cache, #[trigger] off[position], count)
    &&& forall|index: int| 0 <= index < count && unmuted(cache, #[trigger] cache.entities@[index])
        ==> off.contains(cache.entities@[index].entity_id)
}

/// True when `missing` lists exactly the satellites without a mute switch among the first `count`
/// entities.
pub open spec fn missing_list(cache: StateCache, missing: Seq<EntityId>, count: int) -> bool {
    &&& forall|position: int| 0 <= position < missing.len()
        ==> missing_among(cache, #[trigger] missing[position], count)
    &&& forall|index: int| 0 <= index < count && unswitched(cache, #[trigger] cache.entities@[index])
        ==> missing.contains(cache.entities@[index].entity_id)
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

/// Tests of the hot cache.
#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;
    use crate::domain::state::Context;
    use crate::testing::fakes::house;

    /// The entity that the cache tests change.
    const KITCHEN: &str = "light.kitchen";

    /// The entity of `snapshot` with the name `name`.
    fn named<'snapshot>(snapshot: &'snapshot Snapshot, name: &str) -> &'snapshot Entity {
        snapshot
            .entities
            .iter()
            .find(|entity| entity.name == name)
            .unwrap_or_else(|| panic!("no entity has the name {name}"))
    }

    /// The state of `light.kitchen` in `snapshot`.
    fn kitchen(snapshot: &Snapshot) -> State {
        snapshot
            .states
            .iter()
            .find(|state| state.entity_id == KITCHEN)
            .cloned()
            .expect("the house has light.kitchen")
    }

    /// `state` with the value `value`, `seconds` after its last change.
    fn changed(state: &State, value: &str, seconds: i64) -> State {
        State {
            state: value.to_owned(),
            last_updated: state.last_updated + SignedDuration::from_secs(seconds),
            ..state.clone()
        }
    }

    /// A change of `light.kitchen` to `new`.
    fn kitchen_changed(new: Option<State>) -> Event {
        Event::StateChanged {
            entity_id: KITCHEN.to_owned(),
            old: None,
            new: new.map(Box::new),
            context: Context {
                id: "change".to_owned(),
                user_id: None,
                parent_id: None,
            },
        }
    }

    /// AC-2.7-1.
    #[test]
    fn ac_2_7_1_load_gives_snapshot_state() {
        let house = house();
        let kitchen = kitchen(&house);
        assert_eq!(StateCache::load(house).state(KITCHEN), Some(&kitchen));
    }

    /// AC-2.7-2.
    #[test]
    fn ac_2_7_2_apply_sets_new_state() {
        let house = house();
        let new = changed(&kitchen(&house), "off", 1);
        let cache = StateCache::load(house).apply(kitchen_changed(Some(new.clone())));
        assert_eq!(cache.state(KITCHEN), Some(&new));
    }

    /// AC-2.7-3.
    #[test]
    fn ac_2_7_3_apply_without_new_state_removes() {
        let cache = StateCache::load(house()).apply(kitchen_changed(None));
        assert_eq!(cache.state(KITCHEN), None);
    }

    /// AC-2.7-4.
    #[test]
    fn ac_2_7_4_apply_keeps_other_entities() {
        let house = house();
        let new = changed(&kitchen(&house), "off", 1);
        let before = StateCache::load(house);
        for event in [kitchen_changed(Some(new)), kitchen_changed(None)] {
            let mut after = before.clone().apply(event).states;
            let mut expected = before.states.clone();
            after.remove(KITCHEN);
            expected.remove(KITCHEN);
            assert_eq!(after, expected);
        }
    }

    /// AC-2.7-5.
    #[test]
    fn ac_2_7_5_load_replaces_all_states() {
        let first = house();
        let new = changed(&kitchen(&first), "off", 1);
        let second = Snapshot {
            states: vec![new.clone()],
            ..first.clone()
        };
        let mut cache = StateCache::load(first.clone());
        assert_eq!(cache.state(KITCHEN), Some(&kitchen(&first)));
        cache = StateCache::load(second);
        for state in &first.states {
            assert_ne!(cache.state(&state.entity_id), Some(state));
        }
        assert_eq!(cache.state(KITCHEN), Some(&new));
    }

    /// AC-2.7-6.
    #[test]
    fn ac_2_7_6_apply_keeps_newer_cached_state() {
        let house = house();
        let cached = kitchen(&house);
        let older = changed(&cached, "off", -1);
        let cache = StateCache::load(house).apply(kitchen_changed(Some(older)));
        assert_eq!(cache.state(KITCHEN), Some(&cached));
    }

    /// The area `area_id`.
    fn area(area_id: &str) -> AreaId {
        AreaId(area_id.to_owned())
    }

    /// AC-2.8-1.
    #[test]
    fn ac_2_8_1_area_of_device() {
        let cache = StateCache::load(house());
        assert_eq!(
            cache.area_of_device("6a3812757f5f0043a18115e60036f9ab"),
            Some(&area("bedroom"))
        );
    }

    /// AC-2.8-2.
    #[test]
    fn ac_2_8_2_area_of_entity_from_device() {
        let house = house();
        let sensor = "sensor.office_temperature";
        assert!(
            house
                .entities
                .iter()
                .any(|entity| entity.entity_id.0 == sensor && entity.area_id.is_none()),
            "{sensor} has an area"
        );
        assert_eq!(
            StateCache::load(house).area_of_entity(sensor),
            Some(&area("office"))
        );
    }

    /// AC-2.8-3.
    #[test]
    fn ac_2_8_3_entities_of_area_and_domain() {
        let mut house = house();
        let fan = EntityId("fan.living_room_ceiling".to_owned());
        house.entities.push(Entity {
            entity_id: fan.clone(),
            ..named(&house, "Living Room Fan").clone()
        });
        let cache = StateCache::load(house);
        assert_eq!(cache.entities(&area("living_room"), "fan", None), [&fan]);
    }

    /// AC-2.8-4.
    #[test]
    fn ac_2_8_4_entities_with_device_class() {
        let cache = StateCache::load(house());
        let motion = EntityId("binary_sensor.office_motion".to_owned());
        assert_eq!(
            cache.entities(&area("office"), "binary_sensor", Some("motion")),
            [&motion]
        );
    }

    /// AC-2.8b-1.
    #[test]
    fn ac_2_8b_1_area_by_name_or_alias() {
        let mut house = house();
        let garage = house
            .areas
            .iter_mut()
            .find(|entry| entry.area_id == area("garage"))
            .expect("the house has a garage");
        garage.aliases.push("Workshop".to_owned());
        let cache = StateCache::load(house);
        assert_eq!(
            cache.area_by_name("living ROOM"),
            Some(&area("living_room"))
        );
        assert_eq!(cache.area_by_name("WORKSHOP"), Some(&area("garage")));
    }

    /// AC-2.8b-2.
    #[test]
    fn ac_2_8b_2_area_by_unknown_name() {
        assert_eq!(StateCache::load(house()).area_by_name("attic"), None);
    }

    /// The mute switch of the Kitchen satellite, which is `off`.
    const KITCHEN_MUTE: &str = "switch.carson_kitchen_mute_microphones";

    /// The entity ID `id`.
    fn id(id: &str) -> EntityId {
        EntityId(id.to_owned())
    }

    /// AC-13.10c-1.
    #[test]
    fn ac_13_10c_1_reference_house_mute_switches() {
        assert_eq!(
            mute_switches(&StateCache::load(house())),
            MuteSwitches {
                off: vec![id(KITCHEN_MUTE)],
                missing: Vec::new(),
            }
        );
    }

    /// AC-13.10c-2.
    #[test]
    fn ac_13_10c_2_satellite_without_mute_switch() {
        let mut house = house();
        house
            .entities
            .retain(|entity| entity.entity_id.0 != "switch.carson_bedroom_mute_microphones");
        assert_eq!(
            mute_switches(&StateCache::load(house)),
            MuteSwitches {
                off: vec![id(KITCHEN_MUTE)],
                missing: vec![id("assist_satellite.carson_bedroom")],
            }
        );
    }
}
