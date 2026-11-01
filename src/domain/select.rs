//! Entity selection for an intent and an area.

use vstd::prelude::*;

use crate::domain::catalog::Filter;
use crate::domain::catalog::Intent;
use crate::domain::catalog::Target;
use crate::domain::intent;
use crate::domain::state::AreaId;
use crate::domain::state::Entity;
use crate::domain::state::EntityId;
use crate::domain::state_cache::StateCache;

/// The entities that `intent` acts on in `area`: the `entity` template of the intent, the
/// matches in `area`, the match in `area` with the lowest entity ID, or the only match of the
/// house.
#[verus_spec(selected =>
    ensures
        !fixed(*intent) ==> forall|position: int| 0 <= position < selected@.len()
            ==> #[trigger] picked(*intent, *cache, selected@[position]),
        intent.target is Entities && any_local(*intent, *cache, *area) ==> (forall|position: int|
            0 <= position < selected@.len()
                ==> #[trigger] picked_local(*intent, *cache, *area, selected@[position]))
            && forall|index: int| 0 <= index < cache.entities@.len()
                && local_match(*intent, *cache, *area, #[trigger] cache.entities@[index])
                ==> selected@.contains(cache.entities@[index].entity_id),
        !fixed(*intent) && !any_local(*intent, *cache, *area) ==> selected@.len() <= 1
            && (selected@.len() == 1 <==> count(*intent, *cache, cache.entities@.len() as int) == 1),
        intent.target is Entity && intent.entity is None && any_local(*intent, *cache, *area)
            ==> selected@.len() == 1 && picked_local(*intent, *cache, *area, selected@[0])
                && forall|index: int| 0 <= index < cache.entities@.len()
                    && local_match(*intent, *cache, *area, #[trigger] cache.entities@[index])
                    ==> !lower(cache.entities@[index].entity_id.0, selected@[0].0),
)]
#[must_use]
pub fn select(intent: &Intent, area: &AreaId, cache: &StateCache) -> Vec<EntityId> {
    let mut selected = Vec::new();
    if let (Target::Entity, Some(entity)) = (intent.target, &intent.entity) {
        selected.push(EntityId(entity.clone()));
        return selected;
    }
    let local = local_matches(intent, area, cache);
    if local.is_empty() {
        let house = house_matches(intent, cache);
        return if house.len() == 1 { house } else { selected };
    }
    if let Target::Entities = intent.target {
        return local;
    }
    selected.push(lowest(&local));
    selected
}

/// The IDs of the entities in `area` that fit a filter of `intent`, are not excluded and are not
/// skipped.
#[verus_spec(local =>
    ensures
        forall|position: int| 0 <= position < local@.len()
            ==> picked(*intent, *cache, #[trigger] local@[position])
                && picked_local(*intent, *cache, *area, local@[position]),
        forall|index: int| 0 <= index < cache.entities@.len()
            && local_match(*intent, *cache, *area, #[trigger] cache.entities@[index])
            ==> local@.contains(cache.entities@[index].entity_id),
        local@.len() > 0 ==> any_local(*intent, *cache, *area),
)]
fn local_matches(intent: &Intent, area: &AreaId, cache: &StateCache) -> Vec<EntityId> {
    let mut local = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            forall|position: int| 0 <= position < local@.len()
                ==> picked(*intent, *cache, #[trigger] local@[position])
                    && picked_local(*intent, *cache, *area, local@[position]),
            forall|earlier: int| 0 <= earlier < index
                && local_match(*intent, *cache, *area, #[trigger] cache.entities@[earlier])
                ==> local@.contains(cache.entities@[earlier].entity_id),
            local@.len() > 0 ==> any_local(*intent, *cache, *area),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_local(intent, area, cache, entity) {
            local.push(EntityId(entity.entity_id.0.clone()));
        }
        index += 1;
    }
    local
}

/// The IDs of the entities of the house that fit a filter of `intent`, are not excluded and are
/// not skipped.
#[verus_spec(house =>
    ensures
        house@.len() == count(*intent, *cache, cache.entities@.len() as int),
        forall|position: int| 0 <= position < house@.len()
            ==> #[trigger] picked(*intent, *cache, house@[position]),
)]
fn house_matches(intent: &Intent, cache: &StateCache) -> Vec<EntityId> {
    let mut house = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            house@.len() == count(*intent, *cache, index as int),
            forall|position: int| 0 <= position < house@.len()
                ==> #[trigger] picked(*intent, *cache, house@[position]),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_candidate(intent, cache, entity) {
            house.push(EntityId(entity.entity_id.0.clone()));
        }
        index += 1;
    }
    house
}

/// True when `entity` is in `area`, fits a filter of `intent`, is not excluded and is not skipped.
#[verus_spec(found =>
    ensures
        found == local_match(*intent, *cache, *area, *entity),
)]
fn is_local(intent: &Intent, area: &AreaId, cache: &StateCache, entity: &Entity) -> bool {
    is_candidate(intent, cache, entity)
        && matches!(cache.entity_area(entity), Some(found) if found.0.as_str() == area.0.as_str())
}

/// True when `entity` fits a filter of `intent`, is not excluded and is not skipped.
#[verus_spec(found =>
    ensures
        found == candidate(*intent, *cache, *entity),
)]
fn is_candidate(intent: &Intent, cache: &StateCache, entity: &Entity) -> bool {
    is_filtered(cache, &intent.select, entity)
        && !is_listed(&intent.exclude, entity.entity_id.0.as_str())
        && !is_skipped(cache, entity)
}

/// The lowest entity ID of `ids`.
#[verus_spec(lowest =>
    requires
        ids@.len() > 0,
    ensures
        ids@.contains(lowest),
        forall|id: EntityId| #[trigger] ids@.contains(id) ==> !lower(id.0, lowest.0),
)]
fn lowest(ids: &[EntityId]) -> EntityId {
    let mut best = 0;
    let mut position = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            best < ids@.len(),
            position <= ids@.len(),
            forall|earlier: int| 0 <= earlier < position
                ==> !lower(#[trigger] ids@[earlier].0, ids@[best as int].0),
        decreases ids@.len() - position,
    ))]
    while position < ids.len() {
        proof! {
            lemma_string_order();
        }
        if ids[position].0 < ids[best].0 {
            proof! {
                assert(lower(ids@[position as int].0, ids@[best as int].0));
            }
            best = position;
        }
        position += 1;
    }
    EntityId(ids[best].0.clone())
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

/// True when `entity` has the domain, the device class and the name words of `filter`.
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
        && match &filter.name {
            Some(name) => is_named(entity, name.as_str()),
            None => true,
        }
}

/// True when each word of `name` is a word of the normalized name or of a normalized alias of
/// `entity`.
#[verus_spec(found =>
    ensures
        found == named(*entity, name@),
)]
fn is_named(entity: &Entity, name: &str) -> bool {
    let wanted = words(&intent::normalize(name));
    if has_words(&words(&intent::normalize(entity.name.as_str())), &wanted) {
        return true;
    }
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= entity.aliases@.len(),
            views(wanted@) == split(intent::normalized(name@)),
            !names(entity.name@, name@),
            forall|earlier: int| 0 <= earlier < index
                ==> !names(#[trigger] entity.aliases@[earlier]@, name@),
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
#[verus_spec(found =>
    ensures
        found == has_all(views(words@), views(wanted@)),
)]
fn has_words(words: &[String], wanted: &[String]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= wanted@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> views(words@).contains(#[trigger] views(wanted@)[earlier]),
        decreases wanted@.len() - index,
    ))]
    while index < wanted.len() {
        proof! {
            lemma_listed(words@, wanted@[index as int]@);
            assert(views(wanted@)[index as int] == wanted@[index as int]@);
        }
        if !is_listed(words, wanted[index].as_str()) {
            return false;
        }
        index += 1;
    }
    true
}

/// The parts of `text` between spaces.
#[verus_spec(words =>
    ensures
        views(words@) == split(text@),
)]
fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= chars@.len(),
            chars@ == text@,
            views(words@).push(word@) == split(text@.take(index as int)),
        decreases chars@.len() - index,
    ))]
    while index < chars.len() {
        proof! {
            lemma_words(text@, index as int, words@, word);
        }
        if chars[index] == ' ' {
            words.push(word);
            word = String::new();
        } else {
            word.push(chars[index]);
        }
        index += 1;
    }
    proof! {
        lemma_words(text@, index as int, words@, word);
    }
    words.push(word);
    words
}

verus! {

broadcast use vstd::seq_lib::lemma_seq_contains_after_push;

/// True when `entity` has the domain, the device class and the name words of `filter`.
pub open spec fn fits(cache: StateCache, filter: Filter, entity: Entity) -> bool {
    &&& crate::domain::state_cache::in_domain(entity.entity_id.0@, filter.domain@)
    &&& filter.device_class matches Some(class)
        ==> crate::domain::state_cache::device_class(cache, entity.entity_id.0@) == Some(class@)
    &&& filter.name matches Some(name) ==> named(entity, name@)
}

/// The texts of `items`.
pub open spec fn views(items: Seq<String>) -> Seq<Seq<char>> {
    items.map_values(|item: String| item@)
}

/// The parts of `text` between spaces.
pub open spec fn split(text: Seq<char>) -> Seq<Seq<char>>
    decreases text.len(),
{
    if text.len() == 0 {
        seq![Seq::empty()]
    } else {
        let before = split(text.drop_last());
        if text.last() == ' ' {
            before.push(Seq::empty())
        } else {
            before.update(before.len() - 1, before.last().push(text.last()))
        }
    }
}

/// True when each of `wanted` is in `words`.
pub open spec fn has_all(words: Seq<Seq<char>>, wanted: Seq<Seq<char>>) -> bool {
    forall|index: int| 0 <= index < wanted.len() ==> words.contains(#[trigger] wanted[index])
}

/// True when each word of the normalized `name` is a word of the normalized `text`.
pub open spec fn names(text: Seq<char>, name: Seq<char>) -> bool {
    has_all(split(intent::normalized(text)), split(intent::normalized(name)))
}

/// True when each word of the normalized `name` is a word of one normalized alias of `entity`.
pub open spec fn aliased(entity: Entity, name: Seq<char>) -> bool {
    exists|index: int| 0 <= index < entity.aliases@.len() && names(#[trigger] entity.aliases@[index]@, name)
}

/// True when each word of the normalized `name` is a word of the normalized name or of one
/// normalized alias of `entity`.
pub open spec fn named(entity: Entity, name: Seq<char>) -> bool {
    names(entity.name@, name) || aliased(entity, name)
}

/// The facts about `text` and `words` that the loop of `words` needs at `index`.
proof fn lemma_words(text: Seq<char>, index: int, words: Seq<String>, word: String)
    requires
        0 <= index <= text.len(),
    ensures
        views(words.push(word)) == views(words).push(word@),
        index < text.len() ==> text.take(index + 1).drop_last() == text.take(index),
        index == text.len() ==> text.take(index) == text,
{
    assert(views(words.push(word)) =~= views(words).push(word@));
    assert(text.take(text.len() as int) =~= text);
    if index < text.len() {
        assert(text.take(index + 1).drop_last() =~= text.take(index));
    }
}

/// `listed` is `contains` on the texts of `items`.
proof fn lemma_listed(items: Seq<String>, text: Seq<char>)
    ensures
        listed(items, text) == views(items).contains(text),
{
    if listed(items, text) {
        let index = choose|index: int| 0 <= index < items.len() && #[trigger] items[index]@ == text;
        assert(views(items)[index] == text);
    }
}

/// True when `entity` fits a filter of `filters`.
pub open spec fn filtered(cache: StateCache, filters: Seq<Filter>, entity: Entity) -> bool {
    exists|index: int| 0 <= index < filters.len() && fits(cache, #[trigger] filters[index], entity)
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

/// True when `intent` names its one entity with a template instead of filters.
pub open spec fn fixed(intent: Intent) -> bool {
    intent.target is Entity && intent.entity is Some
}

/// The number of candidates of `intent` among the first `prefix` entities of `cache`.
pub open spec fn count(intent: Intent, cache: StateCache, prefix: int) -> int
    decreases prefix,
{
    if prefix <= 0 {
        0
    } else {
        count(intent, cache, prefix - 1)
            + if candidate(intent, cache, cache.entities@[prefix - 1]) { 1int } else { 0int }
    }
}

/// True when `left` is before `right` in the order of `String`.
pub open spec fn lower(left: String, right: String) -> bool {
    vstd::std_specs::cmp::PartialOrdSpec::partial_cmp_spec(&left, &right)
        == Some(std::cmp::Ordering::Less)
}

/// `String` comparisons give its order, which is irreflexive and transitive.
proof fn lemma_string_order()
    ensures
        <String as vstd::std_specs::cmp::PartialOrdSpec>::obeys_partial_cmp_spec(),
        forall|text: String| !#[trigger] lower(text, text),
        forall|first: String, second: String, third: String|
            #[trigger] lower(first, second) && #[trigger] lower(second, third) ==> lower(first, third),
{
    broadcast use crate::domain::contracts::axiom_string_obeys_cmp;
    assert(vstd::laws_cmp::obeys_cmp::<String>());
    reveal(vstd::laws_cmp::obeys_cmp_partial_ord);
    reveal(vstd::laws_cmp::obeys_partial_cmp_spec_properties);
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
        let mut tv_show = intent("tv_show");
        tv_show.target = Target::Entities;
        let house = with(
            house(),
            "media_player.apple_tv",
            "media_player.box",
            |entity| {
                entity.name = "Box".to_owned();
            },
        );
        assert_eq!(
            selected(&tv_show, "living_room", house),
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

    /// AC-4.4c-1.
    #[test]
    fn ac_4_4c_1_fan_switch() {
        assert_eq!(
            selected(&intent("fan_on"), "living_room", house()),
            ["switch.living_room_fan"]
        );
    }

    /// AC-4.4c-2.
    #[test]
    fn ac_4_4c_2_only_climate_of_house() {
        assert_eq!(
            selected(&intent("climate_down"), "bedroom", house()),
            ["climate.thermostat"]
        );
    }

    /// AC-4.4c-3.
    #[test]
    fn ac_4_4c_3_two_matches_in_house() {
        let ids = selected(&intent("fan_on"), "bedroom", house());
        assert!(ids.is_empty(), "{ids:?}");
    }

    /// AC-4.4c-4.
    #[test]
    fn ac_4_4c_4_lowest_entity_id() {
        let house = with(house(), "fan.office_fan", "fan.desk", |_| {});
        assert_eq!(selected(&intent("fan_on"), "office", house), ["fan.desk"]);
    }
}
