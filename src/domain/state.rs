//! Hot cache data and queries.

use std::collections::BTreeMap;
use std::sync::Arc;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;
use vstd::prelude::*;

/// The states, registries and configuration of HA at one time.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Snapshot {
    /// The state of each entity.
    pub states: Vec<State>,
    /// The area registry.
    pub areas: Vec<Area>,
    /// The device registry.
    pub devices: Vec<Device>,
    /// The entity registry.
    pub entities: Vec<Entity>,
    /// The HA configuration.
    pub config: HomeConfig,
    /// The HA user of the token.
    pub user_id: String,
}

/// The state of one entity.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct State {
    /// For example `light.kitchen`.
    pub entity_id: String,
    /// For example `on` or `64.0`.
    pub state: String,
    /// For example `device_class` and `unit_of_measurement`.
    pub attributes: serde_json::Map<String, Value>,
    /// Time of the last change of the state or the attributes.
    pub last_updated: Timestamp,
    /// The cause of the last change.
    pub context: Context,
}

/// The cause of a state change.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Context {
    /// Unique ID of the change.
    pub id: String,
    /// The HA user that made the change.
    pub user_id: Option<String>,
    /// The context of the automation or script that made the change.
    pub parent_id: Option<String>,
}

/// An area registry entry.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Area {
    /// For example `living_room`.
    pub area_id: String,
    /// For example `Living Room`.
    pub name: String,
    /// Other names of the area.
    pub aliases: Vec<String>,
}

/// A device registry entry.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Device {
    /// Unique ID of the device.
    pub id: String,
    /// The area of the device.
    pub area_id: Option<String>,
}

/// An entity registry entry.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Entity {
    /// For example `light.kitchen`.
    pub entity_id: String,
    /// The name that HA shows.
    pub name: String,
    /// Other names of the entity.
    pub aliases: Vec<String>,
    /// The area of the entity. Without it, the entity is in the area of its device.
    pub area_id: Option<String>,
    /// The device of the entity.
    pub device_id: Option<String>,
    /// `config` or `diagnostic`.
    pub entity_category: Option<String>,
    /// Who hid the entity.
    pub hidden_by: Option<String>,
    /// Who disabled the entity.
    pub disabled_by: Option<String>,
}

/// The HA configuration that Jarvis uses.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct HomeConfig {
    /// IANA time zone of the house.
    pub time_zone: String,
    /// Name of the house.
    pub location_name: String,
}

/// A change in HA.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The state of one entity changed.
    StateChanged {
        /// The entity that changed.
        entity_id: String,
        /// Absent for a new entity.
        old: Option<Box<State>>,
        /// Absent for a removed entity.
        new: Option<Box<State>>,
        /// The cause of the change.
        context: Context,
    },
    /// An area, device or entity registry changed.
    RegistryUpdated,
    /// The connection to HA opened again.
    Reconnected,
}

/// An HA service call.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceCall {
    /// For example `light`.
    pub domain: String,
    /// For example `turn_on`.
    pub service: String,
    /// The target entity IDs.
    pub target: Vec<String>,
    /// The service data.
    pub data: serde_json::Map<String, Value>,
    /// When true, HA returns the response data of the service.
    pub return_response: bool,
}

/// The hot cache: the entity states and the registries of HA.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateCache {
    /// The state of each entity, by entity ID.
    pub states: BTreeMap<Arc<str>, Arc<State>>,
    /// The area registry.
    pub areas: Arc<Vec<Area>>,
    /// The device registry.
    pub devices: Arc<Vec<Device>>,
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
            devices: Arc::new(snapshot.devices),
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

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
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
pub open spec fn newer(later: Timestamp, earlier: Timestamp) -> bool {
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

/// Tests of the reference house snapshot and the hot cache.
#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;

    /// The entity that the cache tests change.
    const KITCHEN: &str = "light.kitchen";

    /// The reference house snapshot.
    fn house() -> Snapshot {
        serde_json::from_str(include_str!("../testing/house.json"))
            .expect("house.json is a snapshot")
    }

    /// The entities of `snapshot` whose entity ID starts with `prefix`.
    fn with_prefix<'snapshot>(
        snapshot: &'snapshot Snapshot,
        prefix: &str,
    ) -> Vec<&'snapshot Entity> {
        snapshot
            .entities
            .iter()
            .filter(|entity| entity.entity_id.starts_with(prefix))
            .collect()
    }

    /// The entity of `snapshot` with the name `name`.
    fn named<'snapshot>(snapshot: &'snapshot Snapshot, name: &str) -> &'snapshot Entity {
        snapshot
            .entities
            .iter()
            .find(|entity| entity.name == name)
            .unwrap_or_else(|| panic!("no entity has the name {name}"))
    }

    /// The state of `entity` in `snapshot`.
    fn state_of<'snapshot>(snapshot: &'snapshot Snapshot, entity: &Entity) -> &'snapshot State {
        snapshot
            .states
            .iter()
            .find(|state| state.entity_id == entity.entity_id)
            .unwrap_or_else(|| panic!("{} has no state", entity.entity_id))
    }

    /// The area of `entity`, else the area of its device.
    fn area_of<'snapshot>(
        snapshot: &'snapshot Snapshot,
        entity: &'snapshot Entity,
    ) -> Option<&'snapshot str> {
        let device = snapshot
            .devices
            .iter()
            .find(|device| Some(&device.id) == entity.device_id.as_ref());
        entity
            .area_id
            .as_deref()
            .or_else(|| device.and_then(|device| device.area_id.as_deref()))
    }

    /// The `device_class` attribute of the state of `entity`.
    fn device_class<'snapshot>(
        snapshot: &'snapshot Snapshot,
        entity: &Entity,
    ) -> Option<&'snapshot Value> {
        state_of(snapshot, entity).attributes.get("device_class")
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

    /// AC-2.1-1.
    #[test]
    fn ac_2_1_1_house_parses() {
        assert!(
            serde_json::from_str::<Snapshot>(include_str!("../testing/house.json")).is_ok(),
            "house.json is not a snapshot"
        );
    }

    /// AC-2.1-2.
    #[test]
    fn ac_2_1_2_areas_people_satellites_presence() {
        let house = house();
        assert_eq!(house.areas.len(), 7);
        assert_eq!(with_prefix(&house, "person.").len(), 2);
        let satellites = with_prefix(&house, "assist_satellite.");
        assert_eq!(satellites.len(), 4);
        for satellite in satellites {
            let presence: Vec<&Entity> = with_prefix(&house, "binary_sensor.")
                .into_iter()
                .filter(|entity| entity.device_id == satellite.device_id)
                .filter(|entity| entity.name.ends_with(" Presence"))
                .collect();
            assert_eq!(
                presence.len(),
                1,
                "{} presence sensors",
                satellite.entity_id
            );
            assert_eq!(device_class(&house, presence[0]), None, "{presence:?}");
        }
    }

    /// AC-2.1-3.
    #[test]
    fn ac_2_1_3_switches_fan_and_climate() {
        let house = house();
        for (name, domain) in [
            ("Garage Light", "switch."),
            ("Living Room Fan", "switch."),
            ("Office Fan", "fan."),
        ] {
            let entity = named(&house, name);
            assert!(entity.entity_id.starts_with(domain), "{entity:?}");
        }
        let climate = with_prefix(&house, "climate.");
        assert_eq!(climate.len(), 1, "{climate:?}");
        assert_eq!(area_of(&house, climate[0]), Some("kitchen"));
    }

    /// AC-2.1-4.
    #[test]
    fn ac_2_1_4_office_temperature_sensors() {
        let house = house();
        let temperature = Value::from("temperature");
        let mut states: Vec<&str> = with_prefix(&house, "sensor.")
            .into_iter()
            .filter(|entity| area_of(&house, entity) == Some("office"))
            .filter(|entity| device_class(&house, entity) == Some(&temperature))
            .map(|entity| state_of(&house, entity).state.as_str())
            .collect();
        states.sort_unstable();
        assert_eq!(states, ["64.0", "76.6"]);
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
}
