//! Speaker identification: discovery of the person trackers.

use serde::Deserialize;
use serde_json::Value;
use vstd::prelude::*;

use crate::domain::state::Entity;
use crate::domain::state::EntityId;
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

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
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

} // verus!

/// Tests of discovery.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::state::Snapshot;
    use crate::testing::fakes::house;

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
}
