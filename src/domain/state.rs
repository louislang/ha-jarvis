//! Hot cache data and queries.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;
use vstd::prelude::*;

use crate::domain::state_cache::StateCache;

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

#[verus_verify]
impl StateCache {
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
    fn entity_area<'cache>(&'cache self, entity: &'cache Entity) -> Option<&'cache AreaId> {
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
        let id = entity.entity_id.0.as_bytes();
        let prefix = domain.as_bytes();
        let in_domain =
            prefix.len() < id.len() && id.starts_with(prefix) && id[prefix.len()] == b'.';
        proof! {
            if prefix.len() < id.len() {
                assert(id@.take(prefix@.len() as int) =~= prefix@ <==> id.starts_with(prefix));
            }
        }
        in_domain
            && matches!(self.entity_area(entity), Some(found) if found.0.as_str() == area.0.as_str())
            && match device_class {
                Some(class) => self.device_class(&entity.entity_id.0) == Some(class),
                None => true,
            }
    }

    /// The `device_class` attribute of the state of `entity_id`.
    #[verus_spec(class =>
        ensures
            optional(class) == device_class(*self, entity_id@),
    )]
    fn device_class(&self, entity_id: &str) -> Option<&str> {
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

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
};

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
pub open spec fn device_area(devices: Map<std::sync::Arc<str>, Device>, device_id: Seq<char>) -> Option<AreaId> {
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
}
