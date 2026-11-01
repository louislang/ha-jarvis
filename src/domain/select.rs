//! Entity selection for an intent and an area.

use vstd::prelude::*;

use crate::domain::catalog::Filter;
use crate::domain::catalog::Intent;
use crate::domain::state::AreaId;
use crate::domain::state::Entity;
use crate::domain::state::EntityId;
use crate::domain::state_cache::StateCache;

/// The IDs of the entities in `area` that match a filter of `intent` and are not excluded.
#[verus_spec(selected =>
    ensures
        forall|position: int| 0 <= position < selected@.len()
            ==> #[trigger] picked(*intent, *cache, selected@[position]),
        intent.target is Entities && any_local(*intent, *cache, *area) ==> (forall|position: int|
            0 <= position < selected@.len()
                ==> #[trigger] picked_local(*intent, *cache, *area, selected@[position]))
            && forall|index: int| 0 <= index < cache.entities@.len()
                && local_match(*intent, *cache, *area, #[trigger] cache.entities@[index])
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
                && local_match(*intent, *cache, *area, #[trigger] cache.entities@[earlier])
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

/// True when `entity` is in `area`, fits a filter of `intent` and is not excluded.
#[verus_spec(found =>
    ensures
        found == local_match(*intent, *cache, *area, *entity),
)]
fn is_local(intent: &Intent, area: &AreaId, cache: &StateCache, entity: &Entity) -> bool {
    is_filtered(cache, &intent.select, entity)
        && !is_listed(&intent.exclude, entity.entity_id.0.as_str())
        && matches!(cache.entity_area(entity), Some(found) if found.0.as_str() == area.0.as_str())
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
        found == filtered(*cache, filters@, *entity),
)]
fn is_filtered(cache: &StateCache, filters: &[Filter], entity: &Entity) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= filters@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> !fits(*cache, #[trigger] filters@[earlier], *entity),
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

/// True when `entity` has the domain and the device class of `filter`.
#[verus_spec(found =>
    ensures
        found == fits(*cache, *filter, *entity),
)]
fn fits_filter(cache: &StateCache, filter: &Filter, entity: &Entity) -> bool {
    let id = entity.entity_id.0.as_str();
    StateCache::in_domain(id, filter.domain.as_str())
        && match &filter.device_class {
            Some(class) => cache.device_class(id) == Some(class.as_str()),
            None => true,
        }
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

/// True when `text` is in `items`.
pub open spec fn listed(items: Seq<String>, text: Seq<char>) -> bool {
    exists|index: int| 0 <= index < items.len() && #[trigger] items[index]@ == text
}

/// True when `entity` fits a filter of `intent` and is not excluded.
pub open spec fn candidate(intent: Intent, cache: StateCache, entity: Entity) -> bool {
    filtered(cache, intent.select@, entity) && !listed(intent.exclude@, entity.entity_id.0@)
}

/// True when `entity` is a candidate of `intent` in `area`.
pub open spec fn local_match(intent: Intent, cache: StateCache, area: AreaId, entity: Entity) -> bool {
    &&& candidate(intent, cache, entity)
    &&& crate::domain::state_cache::entity_area(cache, entity) matches Some(found) && found.0@ == area.0@
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
    use crate::testing::fakes::house;

    /// The intent `lights_on` of `intents.toml`.
    fn lights_on() -> Intent {
        let catalog: Catalog =
            toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog");
        catalog
            .intents
            .into_iter()
            .find(|intent| intent.name == "lights_on")
            .expect("intents.toml has lights_on")
    }

    /// The entity IDs that `intent` selects in the kitchen of the house.
    fn kitchen(intent: &Intent) -> Vec<String> {
        let cache = StateCache::load(house());
        let ids: Vec<EntityId> = select(intent, &AreaId("kitchen".to_owned()), &cache);
        ids.into_iter().map(|id| id.0).collect()
    }

    /// AC-4.4-1.
    #[test]
    fn ac_4_4_1_kitchen_lights() {
        assert_eq!(
            kitchen(&lights_on()),
            ["light.kitchen", "light.strawberry_lamp"]
        );
    }

    /// AC-4.4-7.
    #[test]
    fn ac_4_4_7_excluded() {
        let mut intent = lights_on();
        intent.exclude.push("light.strawberry_lamp".to_owned());
        assert_eq!(kitchen(&intent), ["light.kitchen"]);
    }
}
