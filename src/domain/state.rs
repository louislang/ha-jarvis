//! Home data: snapshot, states, registries, events and service calls.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

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

/// Tests of the reference house snapshot.
#[cfg(test)]
mod tests {
    use super::*;

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
}
