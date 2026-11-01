//! Entity selection for an intent and an area.

use vstd::prelude::*;

use crate::domain::catalog::Filter;
use crate::domain::catalog::Intent;
use crate::domain::intent;
use crate::domain::state::AreaId;
use crate::domain::state::Entity;
use crate::domain::state::EntityId;
use crate::domain::state_cache::StateCache;

/// The IDs of the entities in `area` that match a filter of `intent`, are not excluded and are
/// not skipped.
#[verus_spec(selected =>
    ensures
        forall|position: int| 0 <= position < selected@.len()
            ==> #[trigger] picked(*intent, *cache, selected@[position]),
        intent.target is Entities && any_local(*intent, *cache, *area) ==> (forall|position: int|
            0 <= position < selected@.len()
                ==> #[trigger] picked_local(*intent, *cache, *area, selected@[position]))
            && forall|index: int| 0 <= index < cache.entities@.len()
                && plain_local(*intent, *cache, *area, #[trigger] cache.entities@[index])
                ==> selected@.contains(cache.entities@[index].entity_id),
)]
#[must_use]
pub fn select(intent: &Intent, area: &AreaId, cache: &StateCache) -> Vec<EntityId> {
    let mut selected = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            forall|position: int| 0 <= position < selected@.len()
                ==> picked(*intent, *cache, #[trigger] selected@[position])
                    && picked_local(*intent, *cache, *area, selected@[position]),
            forall|earlier: int| 0 <= earlier < index
                && plain_local(*intent, *cache, *area, #[trigger] cache.entities@[earlier])
                ==> selected@.contains(cache.entities@[earlier].entity_id),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_local(intent, area, cache, entity) {
            selected.push(EntityId(entity.entity_id.0.clone()));
        }
        index += 1;
    }
    selected
}

/// True when `entity` is in `area`, fits a filter of `intent`, is not excluded and is not skipped.
#[verus_spec(found =>
    ensures
        found ==> local_match(*intent, *cache, *area, *entity),
        plain_local(*intent, *cache, *area, *entity) ==> found,
)]
fn is_local(intent: &Intent, area: &AreaId, cache: &StateCache, entity: &Entity) -> bool {
    is_filtered(cache, &intent.select, entity)
        && !is_listed(&intent.exclude, entity.entity_id.0.as_str())
        && !is_skipped(cache, entity)
        && matches!(cache.entity_area(entity), Some(found) if found.0.as_str() == area.0.as_str())
}

/// True when `entity` has an entity category, is hidden or disabled, or is on a satellite device.
#[verus_spec(found =>
    ensures
        found == skipped(*cache, *entity),
)]
fn is_skipped(cache: &StateCache, entity: &Entity) -> bool {
    entity.entity_category.is_some()
        || entity.hidden_by.is_some()
        || entity.disabled_by.is_some()
        || matches!(&entity.device_id, Some(device) if is_satellite(cache, device.as_str()))
}

/// True when the device `device` has an `assist_satellite` entity.
#[verus_spec(found =>
    ensures
        found == satellite(*cache, device@),
)]
fn is_satellite(cache: &StateCache, device: &str) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> !satellite_of(#[trigger] cache.entities@[earlier], device@),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if StateCache::in_domain(entity.entity_id.0.as_str(), "assist_satellite")
            && matches!(&entity.device_id, Some(id) if id.as_str() == device)
        {
            return true;
        }
        index += 1;
    }
    false
}

/// True when `text` is in `items`.
#[verus_spec(found =>
    ensures
        found == listed(items@, text@),
)]
fn is_listed(items: &[String], text: &str) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= items@.len(),
            forall|earlier: int| 0 <= earlier < index ==> #[trigger] items@[earlier]@ != text@,
        decreases items@.len() - index,
    ))]
    while index < items.len() {
        if items[index].as_str() == text {
            return true;
        }
        index += 1;
    }
    false
}

/// True when `entity` fits a filter of `filters`.
#[verus_spec(found =>
    ensures
        found ==> filtered(*cache, filters@, *entity),
        plain(*cache, filters@, *entity) ==> found,
)]
fn is_filtered(cache: &StateCache, filters: &[Filter], entity: &Entity) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= filters@.len(),
            forall|earlier: int| 0 <= earlier < index ==> !(filters@[earlier].name is None
                && fits(*cache, #[trigger] filters@[earlier], *entity)),
        decreases filters@.len() - index,
    ))]
    while index < filters.len() {
        if fits_filter(cache, &filters[index], entity) {
            return true;
        }
        index += 1;
    }
    false
}

/// True when `entity` has the domain, the device class and the name words of `filter`.
#[verus_spec(found =>
    ensures
        found ==> fits(*cache, *filter, *entity),
        filter.name is None ==> found == fits(*cache, *filter, *entity),
)]
fn fits_filter(cache: &StateCache, filter: &Filter, entity: &Entity) -> bool {
    let id = entity.entity_id.0.as_str();
    StateCache::in_domain(id, filter.domain.as_str())
        && match &filter.device_class {
            Some(class) => cache.device_class(id) == Some(class.as_str()),
            None => true,
        }
        && match &filter.name {
            Some(name) => is_named(entity, name.as_str()),
            None => true,
        }
}

/// True when each word of `name` is a word of the normalized name or of a normalized alias of
/// `entity`.
#[verus_verify]
fn is_named(entity: &Entity, name: &str) -> bool {
    let wanted = words(&intent::normalize(name));
    if has_words(&words(&intent::normalize(entity.name.as_str())), &wanted) {
        return true;
    }
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= entity.aliases@.len(),
        decreases entity.aliases@.len() - index,
    ))]
    while index < entity.aliases.len() {
        let alias = intent::normalize(entity.aliases[index].as_str());
        if has_words(&words(&alias), &wanted) {
            return true;
        }
        index += 1;
    }
    false
}

/// True when each of `wanted` is in `words`.
#[verus_verify]
fn has_words(words: &[String], wanted: &[String]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= wanted@.len(),
        decreases wanted@.len() - index,
    ))]
    while index < wanted.len() {
        if !is_listed(words, wanted[index].as_str()) {
            return false;
        }
        index += 1;
    }
    true
}

/// The parts of `text` between spaces.
#[verus_verify]
fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= chars@.len(),
        decreases chars@.len() - index,
    ))]
    while index < chars.len() {
        if chars[index] == ' ' {
            words.push(word);
            word = String::new();
        } else {
            word.push(chars[index]);
        }
        index += 1;
    }
    words.push(word);
    words
}

verus! {

broadcast use vstd::seq_lib::lemma_seq_contains_after_push;

/// True when `entity` has the domain and the device class of `filter`.
pub open spec fn fits(cache: StateCache, filter: Filter, entity: Entity) -> bool {
    &&& crate::domain::state_cache::in_domain(entity.entity_id.0@, filter.domain@)
    &&& filter.device_class matches Some(class)
        ==> crate::domain::state_cache::device_class(cache, entity.entity_id.0@) == Some(class@)
}

/// True when `entity` fits a filter of `filters`.
pub open spec fn filtered(cache: StateCache, filters: Seq<Filter>, entity: Entity) -> bool {
    exists|index: int| 0 <= index < filters.len() && fits(cache, #[trigger] filters[index], entity)
}

/// True when `entity` fits a filter of `filters` that has no name.
pub open spec fn plain(cache: StateCache, filters: Seq<Filter>, entity: Entity) -> bool {
    exists|index: int| 0 <= index < filters.len() && filters[index].name is None
        && fits(cache, #[trigger] filters[index], entity)
}

/// True when `text` is in `items`.
pub open spec fn listed(items: Seq<String>, text: Seq<char>) -> bool {
    exists|index: int| 0 <= index < items.len() && #[trigger] items[index]@ == text
}

/// True when `entity` is an `assist_satellite` entity of the device `device`.
pub open spec fn satellite_of(entity: Entity, device: Seq<char>) -> bool {
    &&& crate::domain::state_cache::in_domain(entity.entity_id.0@, "assist_satellite"@)
    &&& entity.device_id matches Some(id) && id@ == device
}

/// True when the device `device` has an `assist_satellite` entity in `cache`.
pub open spec fn satellite(cache: StateCache, device: Seq<char>) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && satellite_of(#[trigger] cache.entities@[index], device)
}

/// True when `entity` has an entity category, is hidden or disabled, or is on a satellite device.
pub open spec fn skipped(cache: StateCache, entity: Entity) -> bool {
    ||| entity.entity_category is Some
    ||| entity.hidden_by is Some
    ||| entity.disabled_by is Some
    ||| entity.device_id matches Some(device) && satellite(cache, device@)
}

/// True when `entity` is not excluded by `intent`, is not skipped and is in `area`.
pub open spec fn kept(intent: Intent, cache: StateCache, area: AreaId, entity: Entity) -> bool {
    &&& !listed(intent.exclude@, entity.entity_id.0@)
    &&& !skipped(cache, entity)
    &&& crate::domain::state_cache::entity_area(cache, entity) matches Some(found) && found.0@ == area.0@
}

/// True when `entity` fits a filter of `intent`, is not excluded and is not skipped.
pub open spec fn candidate(intent: Intent, cache: StateCache, entity: Entity) -> bool {
    &&& filtered(cache, intent.select@, entity)
    &&& !listed(intent.exclude@, entity.entity_id.0@)
    &&& !skipped(cache, entity)
}

/// True when `entity` is a candidate of `intent` in `area`.
pub open spec fn local_match(intent: Intent, cache: StateCache, area: AreaId, entity: Entity) -> bool {
    filtered(cache, intent.select@, entity) && kept(intent, cache, area, entity)
}

/// True when `entity` fits a filter of `intent` that has no name, and is kept in `area`.
pub open spec fn plain_local(intent: Intent, cache: StateCache, area: AreaId, entity: Entity) -> bool {
    plain(cache, intent.select@, entity) && kept(intent, cache, area, entity)
}

/// True when an entity of `cache` is a candidate of `intent` in `area`.
pub open spec fn any_local(intent: Intent, cache: StateCache, area: AreaId) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && local_match(intent, cache, area, #[trigger] cache.entities@[index])
}

/// True when `id` is the ID of a candidate of `intent`.
pub open spec fn picked(intent: Intent, cache: StateCache, id: EntityId) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && #[trigger] cache.entities@[index].entity_id == id
        && candidate(intent, cache, cache.entities@[index])
}

/// True when `id` is the ID of a candidate of `intent` in `area`.
pub open spec fn picked_local(intent: Intent, cache: StateCache, area: AreaId, id: EntityId) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && #[trigger] cache.entities@[index].entity_id == id
        && local_match(intent, cache, area, cache.entities@[index])
}

} // verus!

/// Tests of entity selection.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::catalog::Catalog;
    use crate::domain::state::Snapshot;
    use crate::testing::fakes::house;

    /// The intent `name` of `intents.toml`.
    fn intent(name: &str) -> Intent {
        let catalog: Catalog =
            toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog");
        catalog
            .intents
            .into_iter()
            .find(|intent| intent.name == name)
            .unwrap_or_else(|| panic!("intents.toml has no intent {name}"))
    }

    /// The entity IDs that `intent` selects in `area` of `snapshot`.
    fn selected(intent: &Intent, area: &str, snapshot: Snapshot) -> Vec<String> {
        let cache = StateCache::load(snapshot);
        let ids: Vec<EntityId> = select(intent, &AreaId(area.to_owned()), &cache);
        ids.into_iter().map(|id| id.0).collect()
    }

    /// `snapshot` with a copy of the entity `template` that has the ID `entity_id` and `change`.
    fn with(
        mut snapshot: Snapshot,
        template: &str,
        entity_id: &str,
        change: impl FnOnce(&mut Entity),
    ) -> Snapshot {
        let mut entity = snapshot
            .entities
            .iter()
            .find(|entity| entity.entity_id.0 == template)
            .cloned()
            .unwrap_or_else(|| panic!("the house has no {template}"));
        entity.entity_id = EntityId(entity_id.to_owned());
        change(&mut entity);
        snapshot.entities.push(entity);
        snapshot
    }

    /// AC-4.4-1.
    #[test]
    fn ac_4_4_1_kitchen_lights() {
        assert_eq!(
            selected(&intent("lights_on"), "kitchen", house()),
            ["light.kitchen", "light.strawberry_lamp"]
        );
    }

    /// AC-4.4-7.
    #[test]
    fn ac_4_4_7_excluded() {
        let mut lights_on = intent("lights_on");
        lights_on.exclude.push("light.strawberry_lamp".to_owned());
        assert_eq!(selected(&lights_on, "kitchen", house()), ["light.kitchen"]);
    }

    /// AC-4.4b-1.
    #[test]
    fn ac_4_4b_1_garage_light_switch() {
        let ids = selected(&intent("lights_on"), "garage", house());
        assert!(ids.contains(&"switch.garage_light".to_owned()), "{ids:?}");
    }

    /// AC-4.4b-2.
    #[test]
    fn ac_4_4b_2_no_fan_switch_for_lights() {
        let ids = selected(&intent("lights_on"), "living_room", house());
        assert!(!ids.is_empty(), "no living room lights");
        assert!(
            !ids.contains(&"switch.living_room_fan".to_owned()),
            "{ids:?}"
        );
    }

    /// AC-4.4b-3.
    #[test]
    fn ac_4_4b_3_alias_word() {
        let house = with(
            house(),
            "media_player.apple_tv",
            "media_player.box",
            |entity| {
                entity.name = "Box".to_owned();
            },
        );
        assert_eq!(
            selected(&intent("tv_show"), "living_room", house),
            ["media_player.apple_tv", "media_player.box"]
        );
    }

    /// AC-4.4b-4.
    #[test]
    fn ac_4_4b_4_part_of_word() {
        let house = with(
            house(),
            "switch.living_room_fan",
            "switch.monitor",
            |entity| {
                entity.name = "Infant Monitor".to_owned();
            },
        );
        assert_eq!(
            selected(&intent("fan_on"), "living_room", house),
            ["switch.living_room_fan"]
        );
    }

    /// AC-4.4b-5.
    #[test]
    fn ac_4_4b_5_skipped() {
        let satellite = house()
            .entities
            .into_iter()
            .find(|entity| entity.entity_id.0 == "assist_satellite.carson_kitchen")
            .and_then(|entity| entity.device_id);
        let house = with(house(), "light.kitchen", "light.category", |entity| {
            entity.entity_category = Some("config".to_owned());
        });
        let house = with(house, "light.kitchen", "light.hidden", |entity| {
            entity.hidden_by = Some("user".to_owned());
        });
        let house = with(house, "light.kitchen", "light.disabled", |entity| {
            entity.disabled_by = Some("user".to_owned());
        });
        let house = with(house, "light.kitchen", "light.led", |entity| {
            entity.area_id = None;
            entity.device_id = satellite;
        });
        assert_eq!(
            selected(&intent("lights_on"), "kitchen", house),
            ["light.kitchen", "light.strawberry_lamp"]
        );
    }
}
