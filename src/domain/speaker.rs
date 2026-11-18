//! Speaker identification: discovery of the person trackers and the satellite presence sensors,
//! and the people and presence signals.

use serde::Deserialize;
use serde_json::Value;
use vstd::prelude::*;

use crate::domain::state::Entity;
use crate::domain::state::EntityId;
use crate::domain::state::State;
use crate::domain::state_cache::StateCache;

/// The area and distance sensors of a phone or watch.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tracker {
    /// The sensor whose state is the area of the device.
    #[serde(rename = "area")]
    pub area_sensor: EntityId,
    /// The sensor of the distance to the nearest satellite, in m.
    #[serde(rename = "distance")]
    pub distance_sensor: EntityId,
}

/// A person of the house: a `person` entity of HA.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    /// The `person` entity.
    pub entity: EntityId,
    /// The `friendly_name` attribute, else the entity ID.
    pub name: String,
}

/// The trackers of `person`: `configured` when set, else the area and distance sensors of each
/// device in the `device_trackers` attribute of the `person` entity.
#[verus_spec(found =>
    ensures
        configured matches Some(set) ==> found == set,
        configured is None ==> forall|position: int| 0 <= position < found@.len()
            ==> #[trigger] tracked(*cache, found@[position]),
)]
#[must_use]
pub fn trackers(
    cache: &StateCache,
    person: &EntityId,
    configured: Option<Vec<Tracker>>,
) -> Vec<Tracker> {
    if let Some(set) = configured {
        return set;
    }
    let mut found = Vec::new();
    let Some(devices) = device_trackers(cache, person.0.as_str()) else {
        return found;
    };
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= devices@.len(),
            forall|position: int| 0 <= position < found@.len()
                ==> #[trigger] tracked(*cache, found@[position]),
        decreases devices@.len() - index,
    ))]
    while index < devices.len() {
        if let Some(tracker) = tracker_of(cache, &devices[index]) {
            found.push(tracker);
        }
        index += 1;
    }
    found
}

/// The items of the `device_trackers` attribute of the state of `person`.
#[verus_verify]
fn device_trackers<'cache>(cache: &'cache StateCache, person: &str) -> Option<&'cache Vec<Value>> {
    let trackers = cache.states.get(person)?.attributes.get("device_trackers");
    trackers?.as_array()
}

/// The tracker of the device of the `device_tracker` entity in `item`.
#[verus_spec(found =>
    ensures
        found matches Some(tracker) ==> tracked(*cache, tracker),
)]
fn tracker_of(cache: &StateCache, item: &Value) -> Option<Tracker> {
    let id = item.as_str()?;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(decreases cache.entities@.len() - index))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if entity.entity_id.0.as_str() == id {
            let device = entity.device_id.as_ref()?.as_str();
            let (area, distance) = sensors_on(cache, device)?;
            let tracker = Tracker {
                area_sensor: EntityId(area.0.clone()),
                distance_sensor: EntityId(distance.0.clone()),
            };
            proof! {
                assert(paired(*cache, tracker, device@));
            }
            return Some(tracker);
        }
        index += 1;
    }
    None
}

/// The last sensor on `device` whose state is an area name and the last with the unit `m`.
#[verus_spec(found =>
    ensures
        found matches Some((area, distance)) ==> sensor(*cache, *area, device@)
            && shows_area(*cache, area.0@) && sensor(*cache, *distance, device@)
            && in_meters(*cache, distance.0@),
)]
fn sensors_on<'cache>(
    cache: &'cache StateCache,
    device: &str,
) -> Option<(&'cache EntityId, &'cache EntityId)> {
    let mut area: Option<&EntityId> = None;
    let mut distance: Option<&EntityId> = None;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            area matches Some(id) ==> sensor(*cache, *id, device@) && shows_area(*cache, id.0@),
            distance matches Some(id) ==> sensor(*cache, *id, device@) && in_meters(*cache, id.0@),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_area_sensor(cache, entity, device) {
            area = Some(&entity.entity_id);
        }
        if is_distance_sensor(cache, entity, device) {
            distance = Some(&entity.entity_id);
        }
        index += 1;
    }
    Some((area?, distance?))
}

/// True when `entity` is in `domain` on the device `device`.
#[verus_spec(found =>
    ensures
        found == on(*entity, domain@, device@),
)]
fn is_on(entity: &Entity, domain: &str, device: &str) -> bool {
    StateCache::in_domain(entity.entity_id.0.as_str(), domain)
        && matches!(&entity.device_id, Some(owner) if owner.as_str() == device)
}

/// True when `entity` is a sensor on `device` whose state is the name or an alias of an area.
#[verus_spec(found =>
    ensures
        found == (on(*entity, "sensor"@, device@) && shows_area(*cache, entity.entity_id.0@)),
)]
fn is_area_sensor(cache: &StateCache, entity: &Entity, device: &str) -> bool {
    is_on(entity, "sensor", device)
        && matches!(cache.states.get(entity.entity_id.0.as_str()), Some(state)
            if cache.area_by_name(state.state.as_str()).is_some())
}

/// True when `entity` is a sensor on `device` with the unit `m`.
#[verus_spec(found =>
    ensures
        found == (on(*entity, "sensor"@, device@) && in_meters(*cache, entity.entity_id.0@)),
)]
fn is_distance_sensor(cache: &StateCache, entity: &Entity, device: &str) -> bool {
    is_on(entity, "sensor", device)
        && matches!(cache.states.get(entity.entity_id.0.as_str()), Some(state)
            if matches!(state.attributes.get("unit_of_measurement"), Some(unit)
                if unit.as_str() == Some("m")))
}

/// The presence sensors of the satellite device `device`: `configured` when set, else its
/// `binary_sensor` entities with the device class `occupancy`, `presence` or `motion`.
#[verus_spec(found =>
    ensures
        configured matches Some(set) ==> found == set,
        configured is None ==> presence_list(*cache, device@, found@, cache.entities@.len() as int),
)]
#[must_use]
pub fn presence_sensors(
    cache: &StateCache,
    device: &str,
    configured: Option<Vec<EntityId>>,
) -> Vec<EntityId> {
    if let Some(set) = configured {
        return set;
    }
    let mut found = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            presence_list(*cache, device@, found@, index as int),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_presence(cache, entity, device) {
            found.push(EntityId(entity.entity_id.0.clone()));
        }
        index += 1;
    }
    found
}

/// True when `entity` is a `binary_sensor` on `device` with the device class `occupancy`,
/// `presence` or `motion`.
#[verus_spec(found =>
    ensures
        found == presence(*cache, *entity, device@),
)]
fn is_presence(cache: &StateCache, entity: &Entity, device: &str) -> bool {
    is_on(entity, "binary_sensor", device)
        && matches!(cache.device_class(entity.entity_id.0.as_str()), Some(class)
            if class == "occupancy" || class == "presence" || class == "motion")
}

/// The people of the house: the `person` entities of `cache`, in entity ID order.
#[verus_spec(found =>
    ensures
        people_in(*cache, found@, None),
)]
#[must_use]
pub fn people(cache: &StateCache) -> Vec<Person> {
    persons_with(cache, None)
}

/// The people of the house whose state is `home`, in entity ID order.
#[verus_spec(found =>
    ensures
        people_in(*cache, found@, Some("home"@)),
)]
#[must_use]
pub fn home(cache: &StateCache) -> Vec<Person> {
    persons_with(cache, Some("home"))
}

/// The `person` entities of `cache` in entity ID order, with the state `wanted` when it is `Some`.
#[verus_spec(found =>
    ensures
        people_in(*cache, found@, crate::domain::state_cache::optional(wanted)),
)]
fn persons_with(cache: &StateCache, wanted: Option<&str>) -> Vec<Person> {
    let mut found = Vec::new();
    let entries = cache.states.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            ordered(cache.states@, all),
            named(found@) == persons(all.take(entry_iter.index()), crate::domain::state_cache::optional(wanted)),
    ))]
    for (_, state) in entries {
        proof! {
            assert(all.take(entry_iter.index() + 1).drop_last() =~= all.take(entry_iter.index()));
        }
        if keeps(state, wanted) {
            found.push(person_of(state));
        }
        proof! {
            assert(named(found@) =~= persons(all.take(entry_iter.index() + 1), crate::domain::state_cache::optional(wanted)));
        }
    }
    proof! {
        assert(all.take(all.len() as int) =~= all);
    }
    found
}

/// True when `state` is a `person` entity, with the state `wanted` when it is `Some`.
#[verus_spec(found =>
    ensures
        found == kept(*state, crate::domain::state_cache::optional(wanted)),
)]
fn keeps(state: &State, wanted: Option<&str>) -> bool {
    StateCache::in_domain(state.entity_id.as_str(), "person")
        && match wanted {
            Some(value) => state.state.as_str() == value,
            None => true,
        }
}

/// The person of the `person` state `state`, with its `friendly_name` text, else its entity ID.
#[verus_spec(found =>
    ensures
        found.entity.0@ == state.entity_id@,
        found.name@ == name_of(*state),
)]
fn person_of(state: &State) -> Person {
    let name = match state.attributes.get("friendly_name") {
        Some(value) => match value.as_str() {
            Some(text) => text.to_owned(),
            None => state.entity_id.clone(),
        },
        None => state.entity_id.clone(),
    };
    Person {
        entity: EntityId(state.entity_id.clone()),
        name,
    }
}

/// True when the state of `id` is `value`.
#[verus_spec(found =>
    ensures
        found == shows(*cache, id.0@, value@),
)]
fn has_state(cache: &StateCache, id: &EntityId, value: &str) -> bool {
    matches!(cache.states.get(id.0.as_str()), Some(state) if state.state.as_str() == value)
}

/// Whether the room of the satellite device `device` is occupied: `None` without a presence
/// sensor, else true when one presence sensor is `on`. `configured` is as in `presence_sensors`.
#[verus_spec(found =>
    ensures
        configured matches Some(set) ==> occupancy(*cache, set@, found),
        configured is None ==> sensed(*cache, device@, found),
)]
#[must_use]
pub fn occupied(
    cache: &StateCache,
    device: &str,
    configured: Option<Vec<EntityId>>,
) -> Option<bool> {
    let sensors = presence_sensors(cache, device, configured);
    let mut on = false;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= sensors@.len(),
            on == on_among(*cache, sensors@, index as int),
        decreases sensors@.len() - index,
    ))]
    while index < sensors.len() {
        on = on || has_state(cache, &sensors[index], "on");
        index += 1;
    }
    let found = if sensors.is_empty() { None } else { Some(on) };
    proof! {
        assert(occupancy(*cache, sensors@, found));
    }
    found
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
    vstd::seq_lib::lemma_seq_contains_after_push,
};

/// True when `entity` is in `domain` on the device `device`.
pub open spec fn on(entity: Entity, domain: Seq<char>, device: Seq<char>) -> bool {
    &&& crate::domain::state_cache::in_domain(entity.entity_id.0@, domain)
    &&& entity.device_id matches Some(id) && id@ == device
}

/// True when `id` is a sensor of `cache` on `device`.
pub open spec fn sensor(cache: StateCache, id: EntityId, device: Seq<char>) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && #[trigger] cache.entities@[index].entity_id == id && on(cache.entities@[index], "sensor"@, device)
}

/// True when the name or an alias of an area of `cache` is `text` without case sensitivity.
pub open spec fn area_named(cache: StateCache, text: Seq<char>) -> bool {
    exists|index: int| 0 <= index < cache.areas@.len()
        && crate::domain::state_cache::named(#[trigger] cache.areas@[index], crate::domain::contracts::lowercase(text))
}

/// True when the state of `id` in `cache` is an area name.
pub open spec fn shows_area(cache: StateCache, id: Seq<char>) -> bool {
    let key = crate::domain::contracts::str_key(id);
    cache.states@.contains_key(key) && area_named(cache, cache.states@[key].state@)
}

/// True when the unit of the state of `id` in `cache` is `m`.
pub open spec fn in_meters(cache: StateCache, id: Seq<char>) -> bool {
    let key = crate::domain::contracts::str_key(id);
    &&& cache.states@.contains_key(key)
    &&& crate::domain::contracts::attribute(cache.states@[key].attributes, "unit_of_measurement")
        matches Some(unit) && crate::domain::contracts::text(unit) == Some("m"@)
}

/// True when the sensors of `tracker` are on `device`, the state of the area sensor is an area
/// name and the unit of the distance sensor is `m`.
pub open spec fn paired(cache: StateCache, tracker: Tracker, device: Seq<char>) -> bool {
    &&& sensor(cache, tracker.area_sensor, device) && shows_area(cache, tracker.area_sensor.0@)
    &&& sensor(cache, tracker.distance_sensor, device) && in_meters(cache, tracker.distance_sensor.0@)
}

/// True when the sensors of `tracker` are on one device of `cache`, as `paired` states.
pub open spec fn tracked(cache: StateCache, tracker: Tracker) -> bool {
    exists|device: Seq<char>| #[trigger] paired(cache, tracker, device)
}

/// True when `entity` is a `binary_sensor` on `device` with the device class `occupancy`,
/// `presence` or `motion`.
pub open spec fn presence(cache: StateCache, entity: Entity, device: Seq<char>) -> bool {
    &&& on(entity, "binary_sensor"@, device)
    &&& crate::domain::state_cache::device_class(cache, entity.entity_id.0@) matches Some(class)
        && (class == "occupancy"@ || class == "presence"@ || class == "motion"@)
}

/// True when `id` is the ID of a presence sensor on `device` among the first `count` entities.
pub open spec fn presence_among(cache: StateCache, device: Seq<char>, id: EntityId, count: int) -> bool {
    exists|index: int| 0 <= index < count
        && id == (#[trigger] cache.entities@[index]).entity_id && presence(cache, cache.entities@[index], device)
}

/// True when `found` lists exactly the presence sensors on `device` among the first `count`
/// entities.
pub open spec fn presence_list(cache: StateCache, device: Seq<char>, found: Seq<EntityId>, count: int) -> bool {
    &&& forall|position: int| 0 <= position < found.len()
        ==> presence_among(cache, device, #[trigger] found[position], count)
    &&& forall|index: int| 0 <= index < count && presence(cache, #[trigger] cache.entities@[index], device)
        ==> found.contains(cache.entities@[index].entity_id)
}

/// True when the state of `id` in `cache` is `value`.
pub open spec fn shows(cache: StateCache, id: Seq<char>, value: Seq<char>) -> bool {
    let key = crate::domain::contracts::str_key(id);
    cache.states@.contains_key(key) && cache.states@[key].state@ == value
}

/// The `friendly_name` attribute of `state` when it is text, else its entity ID.
pub open spec fn name_of(state: State) -> Seq<char> {
    match crate::domain::contracts::attribute(state.attributes, "friendly_name") {
        Some(value) => match crate::domain::contracts::text(value) {
            Some(text) => text,
            None => state.entity_id@,
        },
        None => state.entity_id@,
    }
}

/// True when `state` is a `person` entity, with the state `wanted` when it is `Some`.
pub open spec fn kept(state: State, wanted: Option<Seq<char>>) -> bool {
    &&& crate::domain::state_cache::in_domain(state.entity_id@, "person"@)
    &&& wanted matches Some(value) ==> state.state@ == value
}

/// The entity ID and the name of each state of `all` that `kept` keeps, in order.
pub open spec fn persons(all: Seq<(&std::sync::Arc<str>, &std::sync::Arc<State>)>, wanted: Option<Seq<char>>) -> Seq<(Seq<char>, Seq<char>)>
    decreases all.len(),
{
    if all.len() == 0 {
        Seq::empty()
    } else if kept(**all.last().1, wanted) {
        persons(all.drop_last(), wanted).push((all.last().1.entity_id@, name_of(**all.last().1)))
    } else {
        persons(all.drop_last(), wanted)
    }
}

/// True when `all` holds each entry of `states` once, in increasing key order.
pub open spec fn ordered(states: Map<std::sync::Arc<str>, std::sync::Arc<State>>, all: Seq<(&std::sync::Arc<str>, &std::sync::Arc<State>)>) -> bool {
    &&& forall|index: int| 0 <= index < all.len()
        ==> #[trigger] states.contains_key(*all[index].0) && states[*all[index].0] == *all[index].1
    &&& forall|key: std::sync::Arc<str>| #[trigger] states.contains_key(key) ==> all.contains((&key, &states[key]))
    &&& vstd::std_specs::btree::increasing_seq(all.map_values(|entry: (&std::sync::Arc<str>, &std::sync::Arc<State>)| *entry.0))
}

/// The entity ID and the name of each person of `found`.
pub open spec fn named(found: Seq<Person>) -> Seq<(Seq<char>, Seq<char>)> {
    found.map_values(|person: Person| (person.entity.0@, person.name@))
}

/// True when `found` is the entity ID and the name of each `person` state of `cache` with the
/// state `wanted` when it is `Some`, in entity ID order.
pub open spec fn people_in(cache: StateCache, found: Seq<Person>, wanted: Option<Seq<char>>) -> bool {
    exists|all: Seq<(&std::sync::Arc<str>, &std::sync::Arc<State>)>| #[trigger] ordered(cache.states@, all)
        && named(found) == persons(all, wanted)
}

/// True when one of the first `count` sensors of `sensors` is `on`.
pub open spec fn on_among(cache: StateCache, sensors: Seq<EntityId>, count: int) -> bool {
    exists|index: int| 0 <= index < count && shows(cache, (#[trigger] sensors[index]).0@, "on"@)
}

/// True when `found` is `None` for no sensors, else whether one of `sensors` is `on`.
pub open spec fn occupancy(cache: StateCache, sensors: Seq<EntityId>, found: Option<bool>) -> bool {
    match found {
        None => sensors.len() == 0,
        Some(on) => sensors.len() > 0 && on == on_among(cache, sensors, sensors.len() as int),
    }
}

/// True when `found` is the occupancy of the discovered presence sensors on `device`.
pub open spec fn sensed(cache: StateCache, device: Seq<char>, found: Option<bool>) -> bool {
    exists|sensors: Seq<EntityId>| presence_list(cache, device, sensors, cache.entities@.len() as int)
        && #[trigger] occupancy(cache, sensors, found)
}

} // verus!

/// Tests of discovery.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::state::Snapshot;
    use crate::testing::fakes::house;

    /// The device of the Office satellite.
    const OFFICE: &str = "1c8aff5b3943869c175c09b3c12f63d4";

    /// The added `binary_sensor` on the Office satellite device.
    const ADDED: &str = "binary_sensor.office_satellite";

    /// The entity ID `id`.
    fn id(id: &str) -> EntityId {
        EntityId(id.to_owned())
    }

    /// Adds the entity `entity_id` on `device` to `snapshot`, with the state `value` and the
    /// attributes `attributes`.
    fn add(snapshot: &mut Snapshot, entity_id: &str, device: &str, value: &str, attributes: Value) {
        let mut entity = snapshot.entities[0].clone();
        (entity.entity_id, entity.area_id) = (id(entity_id), None);
        entity.device_id = Some(device.to_owned());
        let mut state = snapshot.states[0].clone();
        (state.entity_id, state.state) = (entity_id.to_owned(), value.to_owned());
        state.attributes = serde_json::from_value(attributes).expect("an object");
        snapshot.entities.push(entity);
        snapshot.states.push(state);
    }

    /// The trackers of `person.alex` when the watch has an area sensor in the Office, a battery
    /// sensor and the distance sensor `distance`, and `configured` is the override.
    fn alex(distance: &str, configured: Option<Vec<Tracker>>) -> Vec<Tracker> {
        let mut snapshot = house();
        let listed = json!({ "device_trackers": ["device_tracker.alex_watch"] });
        add(&mut snapshot, "person.alex", "alex", "home", listed);
        let watch = [
            ("device_tracker.alex_watch", "home", json!({})),
            (
                "sensor.alex_watch_battery",
                "80",
                json!({ "unit_of_measurement": "%" }),
            ),
            ("sensor.alex_watch_area", "Office", json!({})),
            (distance, "1.2", json!({ "unit_of_measurement": "m" })),
        ];
        for (entity_id, value, attributes) in watch {
            add(&mut snapshot, entity_id, "watch", value, attributes);
        }
        trackers(&StateCache::load(snapshot), &id("person.alex"), configured)
    }

    /// The presence sensors of the Office satellite when `ADDED` has the device class `class`, and
    /// `configured` is the override.
    fn office(class: &str, configured: Option<Vec<EntityId>>) -> Vec<EntityId> {
        let mut snapshot = house();
        add(
            &mut snapshot,
            ADDED,
            OFFICE,
            "on",
            json!({ "device_class": class }),
        );
        presence_sensors(&StateCache::load(snapshot), OFFICE, configured)
    }

    /// The tracker with the sensors `area` and `distance`.
    fn tracker(area: &str, distance: &str) -> Tracker {
        Tracker {
            area_sensor: id(area),
            distance_sensor: id(distance),
        }
    }

    /// AC-15.1-1.
    #[test]
    fn ac_15_1_1_trackers_from_the_person_device_trackers() {
        let expected = tracker("sensor.alex_watch_area", "sensor.alex_watch_distance");
        assert_eq!(alex("sensor.alex_watch_distance", None), [expected]);
    }

    /// AC-15.1-2.
    #[test]
    fn ac_15_1_2_sensor_in_meters_is_the_distance() {
        let found = alex("sensor.alex_watch_nearest", None);
        let distances: Vec<&EntityId> = found.iter().map(|found| &found.distance_sensor).collect();
        assert_eq!(distances, [&id("sensor.alex_watch_nearest")]);
    }

    /// AC-15.1-3.
    #[test]
    fn ac_15_1_3_configured_trackers_replace_discovery() {
        let configured = vec![tracker(
            "sensor.alex_phone_area",
            "sensor.alex_phone_distance",
        )];
        let found = alex("sensor.alex_watch_distance", Some(configured.clone()));
        assert_eq!(found, configured);
    }

    /// AC-15.1b-1.
    #[test]
    fn ac_15_1b_1_occupancy_sensor_is_a_presence_sensor() {
        assert_eq!(office("occupancy", None), [id(ADDED)]);
    }

    /// AC-15.1b-2.
    #[test]
    fn ac_15_1b_2_sensor_without_device_class_is_not_a_presence_sensor() {
        let house = StateCache::load(house());
        assert!(
            house
                .device_class("binary_sensor.office_presence")
                .is_none()
        );
        assert_eq!(office("motion", None), [id(ADDED)]);
    }

    /// AC-15.1b-3.
    #[test]
    fn ac_15_1b_3_configured_presence_replaces_discovery() {
        let configured = vec![id("binary_sensor.office_presence")];
        assert_eq!(office("occupancy", Some(configured.clone())), configured);
    }

    /// The cache of the house with the `person` entities of `persons` (entity ID, state and
    /// attributes) instead of its own.
    fn household(persons: Vec<(&str, &str, Value)>) -> StateCache {
        let mut snapshot = house();
        snapshot
            .states
            .retain(|state| !state.entity_id.starts_with("person."));
        for (person, value, attributes) in persons {
            add(&mut snapshot, person, "phone", value, attributes);
        }
        StateCache::load(snapshot)
    }

    /// The person `entity` with the name `name`.
    fn person(entity: &str, name: &str) -> Person {
        Person {
            entity: id(entity),
            name: name.to_owned(),
        }
    }

    /// AC-15.2-1.
    #[test]
    fn ac_15_2_1_home_returns_the_people_at_home() {
        let cache = household(vec![
            ("person.alex", "home", json!({ "friendly_name": "Alex" })),
            ("person.sam", "not_home", json!({ "friendly_name": "Sam" })),
            ("person.kim", "home", json!({ "friendly_name": "Kim" })),
        ]);
        let expected = [person("person.alex", "Alex"), person("person.kim", "Kim")];
        assert_eq!(home(&cache), expected);
    }

    /// AC-15.2-7.
    #[test]
    fn ac_15_2_7_people_are_the_person_entities() {
        let cache = household(vec![
            ("person.sam", "not_home", json!({ "friendly_name": "Sam" })),
            ("person.alex", "home", json!({ "friendly_name": "Alex" })),
        ]);
        let expected = [person("person.alex", "Alex"), person("person.sam", "Sam")];
        assert_eq!(people(&cache), expected);
    }

    /// AC-15.2-8.
    #[test]
    fn ac_15_2_8_person_without_friendly_name_has_its_entity_id() {
        let cache = household(vec![("person.alex", "home", json!({}))]);
        assert_eq!(people(&cache), [person("person.alex", "person.alex")]);
    }

    /// AC-15.2-2.
    #[test]
    fn ac_15_2_2_no_presence_sensor_gives_no_occupancy() {
        let house = StateCache::load(house());
        assert_eq!(occupied(&house, OFFICE, None), None);
        let configured = vec![id("binary_sensor.office_presence")];
        assert_eq!(occupied(&house, OFFICE, Some(configured)), Some(true));
    }
}
