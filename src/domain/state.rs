//! Hot cache data and queries.

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

/// The ID of an area, for example `living_room`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct AreaId(pub String);

/// The ID of an entity, for example `light.kitchen`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct EntityId(pub String);

/// An area registry entry.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Area {
    /// For example `living_room`.
    pub area_id: AreaId,
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
    pub area_id: Option<AreaId>,
}

/// An entity registry entry.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Entity {
    /// For example `light.kitchen`.
    pub entity_id: EntityId,
    /// The name that HA shows.
    pub name: String,
    /// Other names of the entity.
    pub aliases: Vec<String>,
    /// The area of the entity. Without it, the entity is in the area of its device.
    pub area_id: Option<AreaId>,
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

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
};

/// True when the entity ID `entity_id` is `domain`, a dot and an object ID.
pub open spec fn in_domain(entity_id: Seq<char>, domain: Seq<char>) -> bool {
    let id = vstd::utf8::encode_utf8(entity_id);
    let prefix = vstd::utf8::encode_utf8(domain);
    prefix.len() < id.len() && id.take(prefix.len() as int) == prefix && id[prefix.len() as int] == b'.'
}

/// The `device_class` attribute of the state of `entity_id`.
pub open spec fn device_class(cache: crate::domain::state_cache::StateCache, entity_id: Seq<char>) -> Option<Seq<char>> {
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
    cache: crate::domain::state_cache::StateCache,
    entity: Entity,
    area: AreaId,
    domain: Seq<char>,
    class: Option<Seq<char>>,
) -> bool {
    &&& in_domain(entity.entity_id.0@, domain)
    &&& crate::domain::state_cache::entity_area(cache, entity) matches Some(found) && found.0@ == area.0@
    &&& class matches Some(wanted) ==> device_class(cache, entity.entity_id.0@) == Some(wanted)
}

/// True when `id` is the ID of a selected entity among the first `count` entities.
pub open spec fn chosen(
    cache: crate::domain::state_cache::StateCache,
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

} // verus!

/// Tests of the reference house snapshot and the hot cache.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::state_cache::StateCache;

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
            .filter(|entity| entity.entity_id.0.starts_with(prefix))
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
            .find(|state| state.entity_id == entity.entity_id.0)
            .unwrap_or_else(|| panic!("{:?} has no state", entity.entity_id))
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
            .as_ref()
            .or_else(|| device.and_then(|device| device.area_id.as_ref()))
            .map(|area| area.0.as_str())
    }

    /// The `device_class` attribute of the state of `entity`.
    fn device_class<'snapshot>(
        snapshot: &'snapshot Snapshot,
        entity: &Entity,
    ) -> Option<&'snapshot Value> {
        state_of(snapshot, entity).attributes.get("device_class")
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
                "{:?} presence sensors",
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
            assert!(entity.entity_id.0.starts_with(domain), "{entity:?}");
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
}
