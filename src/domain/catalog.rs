//! Intent catalog types and validation.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use minijinja::Environment;
use serde::Deserialize;
use vstd::prelude::*;

use crate::domain::intent;
use crate::domain::intent::MarkupError;

/// The intent catalog of `intents.toml`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    /// The least number of seed examples of each intent.
    pub min_examples: usize,
    /// The seed examples of `unknown`.
    pub unknown_examples: Vec<String>,
    /// The intents in file order.
    #[serde(rename = "intent")]
    pub intents: Vec<Intent>,
}

/// One action that Jarvis can take.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    /// The label, for example `lights_on`.
    pub name: String,
    /// How Jarvis replies and acts.
    pub kind: Kind,
    /// The seed examples in `[value](slot)` markup.
    pub examples: Vec<String>,
    /// True when the intent acts for the speaker.
    #[serde(default)]
    pub needs_speaker: bool,
    /// The slots that can be missing.
    #[serde(default)]
    pub optional: Vec<String>,
    /// The HA service, for example `homeassistant.turn_on`.
    pub service: Option<String>,
    /// The entities that the service gets.
    #[serde(default)]
    pub target: Target,
    /// The entity template, used instead of `select`.
    pub entity: Option<String>,
    /// The entity filters.
    #[serde(default)]
    pub select: Vec<Filter>,
    /// The entity IDs that `select` never matches.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// The service data templates.
    #[serde(default)]
    pub data: BTreeMap<String, String>,
    /// The habit action of each slot that has a habit default.
    #[serde(default)]
    pub habit_defaults: BTreeMap<String, String>,
}

/// How Jarvis replies to an intent and acts on it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Reply from the pool and call the service in the background.
    Command,
    /// Reply with data from the hot cache.
    Query,
    /// Reply with a filler, call the service and announce the answer.
    Fetch,
    /// Reply with the prefetched answer when it is fresh.
    Prefetch,
    /// Reply from the pool and start a training run.
    Train,
}

/// The entities that the service of an intent gets.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// One entity.
    #[default]
    Entity,
    /// All matching entities in the area.
    Entities,
}

/// An entity filter of `select`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// The entity domain, for example `light`.
    pub domain: String,
    /// A whole word of the entity name or an alias.
    pub name: Option<String>,
    /// The device class, for example `temperature`.
    pub device_class: Option<String>,
}

/// An error in the intent catalog.
#[verus_verify]
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub enum CatalogError {
    /// The intent with this name has fewer than `min_examples` examples.
    TooFewExamples(String),
    /// An example of an intent has a markup error.
    Markup {
        /// The intent name.
        intent: String,
        /// The markup error.
        error: MarkupError,
    },
    /// Two intents have this name.
    Duplicate(String),
    /// An intent is named `unknown`.
    Unknown,
    /// A template of the intent with this name does not parse.
    Template(String),
    /// A template variable that is not a built-in, a habit default or a tagged slot.
    Untagged(String),
    /// An example whose text is not normalized.
    NotNormal(String),
}

/// Checks the example counts, the example markup, the intent names, the template variables and
/// the example texts of `catalog`.
///
/// # Errors
///
/// The first intent that fails a check.
#[verus_spec(result =>
    ensures
        result is Ok ==> valid_catalog(*catalog),
)]
pub fn validate_catalog(catalog: &Catalog) -> Result<(), CatalogError> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= catalog.intents@.len(),
            checked(*catalog, index as int),
        decreases catalog.intents@.len() - index,
    ))]
    while index < catalog.intents.len() {
        check_intent(catalog, index)?;
        check_slots(&catalog.intents[index])?;
        index += 1;
    }
    Ok(())
}

/// Checks the intent at `index` of `catalog`.
#[verus_spec(result =>
    requires
        index < catalog.intents@.len(),
    ensures
        result is Ok <==> valid_intent(*catalog, index as int),
)]
fn check_intent(catalog: &Catalog, index: usize) -> Result<(), CatalogError> {
    let intent = &catalog.intents[index];
    if intent.examples.len() < catalog.min_examples {
        return Err(CatalogError::TooFewExamples(intent.name.clone()));
    }
    check_examples(intent)?;
    check_name(catalog, index)
}

/// Checks that each example of `intent` parses.
#[verus_spec(result =>
    ensures
        result is Ok <==> parsed(intent.examples@, intent.examples@.len() as int),
)]
fn check_examples(intent: &Intent) -> Result<(), CatalogError> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= intent.examples@.len(),
            parsed(intent.examples@, index as int),
        decreases intent.examples@.len() - index,
    ))]
    while index < intent.examples.len() {
        if let Err(error) = intent::parse_example(intent.examples[index].as_str()) {
            return Err(CatalogError::Markup {
                intent: intent.name.clone(),
                error,
            });
        }
        index += 1;
    }
    Ok(())
}

/// Checks that the intent at `index` of `catalog` is not `unknown` and has a new name.
#[verus_spec(result =>
    requires
        index < catalog.intents@.len(),
    ensures
        result is Ok <==> named_once(catalog.intents@, index as int),
)]
fn check_name(catalog: &Catalog, index: usize) -> Result<(), CatalogError> {
    let name = catalog.intents[index].name.as_str();
    if name == "unknown" {
        return Err(CatalogError::Unknown);
    }
    let mut earlier = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            earlier <= index < catalog.intents@.len(),
            name@ == catalog.intents@[index as int].name@,
            distinct(catalog.intents@, index as int, earlier as int),
        decreases index - earlier,
    ))]
    while earlier < index {
        if catalog.intents[earlier].name.as_str() == name {
            return Err(CatalogError::Duplicate(name.to_owned()));
        }
        earlier += 1;
    }
    Ok(())
}

/// Checks that each example text of `intent` is normalized and each template variable is a
/// built-in, a habit default or a tagged slot.
#[verus_verify]
fn check_slots(intent: &Intent) -> Result<(), CatalogError> {
    check_normal(intent)?;
    let mut allowed = BTreeSet::new();
    insert_slots(intent, &mut allowed);
    for name in &["area", "entity", "person", "response"] {
        allowed.insert((*name).to_owned());
    }
    for key in intent.habit_defaults.keys() {
        allowed.insert(key.clone());
    }
    if let Some(entity) = &intent.entity {
        check_template(intent, entity.as_str(), &allowed)?;
    }
    for template in intent.data.values() {
        check_template(intent, template.as_str(), &allowed)?;
    }
    Ok(())
}

/// Checks that the text of each example of `intent` is normalized.
#[verus_verify]
fn check_normal(intent: &Intent) -> Result<(), CatalogError> {
    for markup in &intent.examples {
        // `check_examples` rejects markup errors first.
        match intent::parse_example(markup.as_str()) {
            Ok(example)
                if intent::normalize(example.text.as_str()).as_str() != example.text.as_str() =>
            {
                return Err(CatalogError::NotNormal(markup.clone()));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Adds the slots that the examples of `intent` tag to `slots`.
#[verus_verify]
fn insert_slots(intent: &Intent, slots: &mut BTreeSet<String>) {
    for markup in &intent.examples {
        if let Ok(example) = intent::parse_example(markup.as_str()) {
            for span in example.spans {
                slots.insert(span.slot);
            }
        }
    }
}

/// Checks that `template` of `intent` parses and has only `allowed` variables.
#[verus_verify]
fn check_template(
    intent: &Intent,
    template: &str,
    allowed: &BTreeSet<String>,
) -> Result<(), CatalogError> {
    let environment = Environment::new();
    let Ok(parsed) = environment.template_from_str(template) else {
        return Err(CatalogError::Template(intent.name.clone()));
    };
    let variables = BTreeSet::from_iter(parsed.undeclared_variables(false));
    match variables
        .iter()
        .find(|variable| !allowed.contains(*variable))
    {
        Some(variable) => Err(CatalogError::Untagged(variable.clone())),
        None => Ok(()),
    }
}

/// The System One labels of `catalog`: `unknown`, then the intent names in file order.
#[verus_spec(result =>
    ensures
        views(result@) == seq!["unknown"@] + names(catalog.intents@),
)]
#[must_use]
pub fn labels(catalog: &Catalog) -> Vec<String> {
    let mut labels = Vec::new();
    labels.push("unknown".to_owned());
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= catalog.intents@.len(),
            views(labels@) == seq!["unknown"@] + names(catalog.intents@.take(index as int)),
        decreases catalog.intents@.len() - index,
    ))]
    while index < catalog.intents.len() {
        proof! {
            lemma_named(labels@, catalog.intents@, index as int);
        }
        labels.push(catalog.intents[index].name.clone());
        index += 1;
    }
    proof! {
        assert(catalog.intents@.take(index as int) =~= catalog.intents@);
    }
    labels
}

/// The slot tags of `catalog`: `O`, then `B-<slot>` and `I-<slot>` for each slot name in sorted
/// order.
#[verus_verify]
#[must_use]
pub fn tags(catalog: &Catalog) -> Vec<String> {
    let mut slots = BTreeSet::new();
    for intent in &catalog.intents {
        insert_slots(intent, &mut slots);
    }
    bio_tags(&slots)
}

/// `O`, then `B-<slot>` and `I-<slot>` for each element of `slots` in sorted order.
#[verus_spec(result =>
    ensures
        tagged(slots@, views(result@)),
)]
fn bio_tags(slots: &BTreeSet<String>) -> Vec<String> {
    let mut tags = Vec::new();
    tags.push("O".to_owned());
    let sorted = slots.iter();
    proof! {
        lemma_sorted(slots@, vstd::std_specs::iter::IteratorSpec::remaining(&sorted));
    }
    #[cfg_attr(verus_only, verus_spec(slot_iter =>
        invariant
            sorts(slot_iter.seq().unref(), slots@),
            views(tags@) == bio(slot_iter.seq().unref(), slot_iter.index()),
        ensures
            tagged(slots@, views(tags@)),
    ))]
    for slot in sorted {
        proof! {
            lemma_tagged(tags@, slot_iter.seq().unref(), slot_iter.index());
        }
        tags.push(tag("B-", slot));
        tags.push(tag("I-", slot));
    }
    tags
}

/// `prefix` followed by `slot`.
#[verus_spec(tag =>
    ensures
        tag@ == prefix@ + slot@,
)]
fn tag(prefix: &str, slot: &str) -> String {
    let mut tag = prefix.to_owned();
    tag.push_str(slot);
    tag
}

verus! {

broadcast use crate::domain::contracts::axiom_string_obeys_cmp;

/// True when every intent of `catalog` passes the checks.
pub open spec fn valid_catalog(catalog: Catalog) -> bool {
    checked(catalog, catalog.intents@.len() as int)
}

/// True when the first `count` intents of `catalog` pass the checks.
pub open spec fn checked(catalog: Catalog, count: int) -> bool {
    forall|index: int| 0 <= index < count ==> #[trigger] valid_intent(catalog, index)
}

/// True when the intent at `index` of `catalog` passes checks 1 to 3.
pub open spec fn valid_intent(catalog: Catalog, index: int) -> bool {
    let examples = catalog.intents@[index].examples@;
    &&& examples.len() >= catalog.min_examples
    &&& parsed(examples, examples.len() as int)
    &&& named_once(catalog.intents@, index)
}

/// True when the first `count` markup strings of `examples` parse.
pub open spec fn parsed(examples: Seq<String>, count: int) -> bool {
    forall|index: int| 0 <= index < count ==> crate::domain::intent::accepted(#[trigger] examples[index]@)
}

/// True when the intent at `index` of `intents` is not `unknown` and has a new name.
pub open spec fn named_once(intents: Seq<Intent>, index: int) -> bool {
    intents[index].name@ != "unknown"@ && distinct(intents, index, index)
}

/// True when none of the first `count` intents has the name of the intent at `index`.
pub open spec fn distinct(intents: Seq<Intent>, index: int, count: int) -> bool {
    forall|earlier: int| 0 <= earlier < count ==> #[trigger] intents[earlier].name@ != intents[index].name@
}


/// The texts of `strings`.
pub open spec fn views(strings: Seq<String>) -> Seq<Seq<char>> {
    strings.map_values(|string: String| string@)
}

/// The names of `intents`.
pub open spec fn names(intents: Seq<Intent>) -> Seq<Seq<char>> {
    intents.map_values(|intent: Intent| intent.name@)
}

/// True when `tags` is `bio` of the elements of `slots` in sorted order.
pub open spec fn tagged(slots: Set<String>, tags: Seq<Seq<char>>) -> bool {
    exists|sorted: Seq<String>| #[trigger] sorts(sorted, slots) && tags == bio(sorted, sorted.len() as int)
}

/// True when `sorted` has the elements of `slots` in increasing order.
pub open spec fn sorts(sorted: Seq<String>, slots: Set<String>) -> bool {
    &&& sorted.to_set() == slots
    &&& forall|first: int, second: int| 0 <= first < second < sorted.len()
        ==> vstd::std_specs::cmp::OrdSpec::cmp_spec(&#[trigger] sorted[first], &#[trigger] sorted[second]) is Less
}

/// Pushing the name of the intent at `index` extends the labels of the first `index` intents.
proof fn lemma_named(labels: Seq<String>, intents: Seq<Intent>, index: int)
    requires
        0 <= index < intents.len(),
        views(labels) == seq!["unknown"@] + names(intents.take(index)),
    ensures
        views(labels.push(intents[index].name)) == seq!["unknown"@] + names(intents.take(index + 1)),
{
    let name = intents[index].name;
    assert(names(intents.take(index + 1)) =~= names(intents.take(index)).push(name@));
    assert(views(labels.push(name)) =~= views(labels).push(name@));
    assert(seq!["unknown"@] + names(intents.take(index + 1))
        =~= (seq!["unknown"@] + names(intents.take(index))).push(name@));
}

/// The keys of a `BTreeSet` iterator sort the elements of its set.
proof fn lemma_sorted(slots: Set<String>, keys: Seq<&String>)
    requires
        keys.unref().to_set() == slots,
        vstd::std_specs::btree::increasing_seq(keys),
    ensures
        sorts(keys.unref(), slots),
{
    assert(vstd::laws_cmp::obeys_cmp::<&String>());
    assert forall|first: int, second: int| 0 <= first < second < keys.len()
        implies vstd::std_specs::cmp::OrdSpec::cmp_spec(&#[trigger] keys.unref()[first], &#[trigger] keys.unref()[second]) is Less by {
        assert(vstd::std_specs::cmp::OrdSpec::cmp_spec(&keys[first], &keys[second]) is Less);
    }
}

/// Pushing the tags of the slot at `count` extends the tags of the first `count` slots.
proof fn lemma_tagged(tags: Seq<String>, sorted: Seq<String>, count: int)
    requires
        0 <= count < sorted.len(),
        views(tags) == bio(sorted, count),
    ensures
        forall|begin: String, inside: String|
            begin@ == "B-"@ + sorted[count]@ && inside@ == "I-"@ + sorted[count]@
            ==> views(#[trigger] tags.push(begin).push(inside)) == bio(sorted, count + 1),
{
    assert forall|begin: String, inside: String|
        begin@ == "B-"@ + sorted[count]@ && inside@ == "I-"@ + sorted[count]@
        implies views(#[trigger] tags.push(begin).push(inside)) == bio(sorted, count + 1) by {
        assert(views(tags.push(begin).push(inside)) =~= views(tags) + seq![begin@, inside@]);
    }
}

/// `O`, then `B-<slot>` and `I-<slot>` for each of the first `count` slots of `sorted`.
pub open spec fn bio(sorted: Seq<String>, count: int) -> Seq<Seq<char>>
    decreases count,
{
    if count <= 0 {
        seq!["O"@]
    } else {
        let slot = sorted[count - 1]@;
        bio(sorted, count - 1) + seq!["B-"@ + slot, "I-"@ + slot]
    }
}

} // verus!

/// Tests of the catalog.
#[cfg(test)]
mod tests {
    use super::*;

    /// The intent catalog of the repository.
    const INTENTS: &str = include_str!("../../intents.toml");

    /// Returns the catalog of `intents.toml`.
    fn catalog() -> Catalog {
        toml::from_str(INTENTS).expect("intents.toml is a catalog")
    }

    /// Returns `validate_catalog` of `intents.toml` after `change` modifies its intents.
    fn validate(change: impl FnOnce(&mut Vec<Intent>)) -> Result<(), CatalogError> {
        let mut catalog = catalog();
        change(&mut catalog.intents);
        validate_catalog(&catalog)
    }

    /// AC-3.2-1.
    #[test]
    fn ac_3_2_1_intents_toml_parses_and_is_valid() {
        let catalog = catalog();
        let names: Vec<&str> = catalog.intents.iter().map(|i| i.name.as_str()).collect();
        let expected = "lights_on fan_on climate_down tv_show temperature_query weather";
        assert_eq!(names.join(" "), expected);
        assert_eq!(validate_catalog(&catalog), Ok(()));
    }

    /// AC-3.2-2.
    #[test]
    fn ac_3_2_2_too_few_examples_names_intent() {
        let result = validate(|intents| intents[1].examples.truncate(2));
        assert_eq!(
            result,
            Err(CatalogError::TooFewExamples("fan_on".to_owned()))
        );
    }

    /// AC-3.2-3.
    #[test]
    fn ac_3_2_3_markup_error_names_intent() {
        let result = validate(|intents| intents[2].examples[0] = "turn [the air".to_owned());
        assert!(
            matches!(&result, Err(CatalogError::Markup { intent, .. }) if intent == "climate_down"),
            "expected a markup error of climate_down: {result:?}"
        );
    }

    /// AC-3.2-5.
    #[test]
    fn ac_3_2_5_duplicate_name_is_an_error() {
        let result = validate(|intents| intents[3].name = "fan_on".to_owned());
        assert_eq!(result, Err(CatalogError::Duplicate("fan_on".to_owned())));
    }

    /// AC-3.2-6.
    #[test]
    fn ac_3_2_6_unknown_name_is_an_error() {
        let result = validate(|intents| intents[0].name = "unknown".to_owned());
        assert_eq!(result, Err(CatalogError::Unknown));
    }

    /// AC-3.2b-1.
    #[test]
    fn ac_3_2b_1_untagged_template_slot_names_slot() {
        let result = validate(|intents| {
            let season = "{{ season }}".to_owned();
            intents[3].data.insert("season".to_owned(), season);
        });
        assert_eq!(result, Err(CatalogError::Untagged("season".to_owned())));
        let result = validate(|intents| {
            let speed = "{{ speed }} {{ area }} {{ person }} {{ response }}".to_owned();
            intents[1].data.insert("percentage".to_owned(), speed);
            let action = "fan.set_percentage".to_owned();
            intents[1].habit_defaults.insert("speed".to_owned(), action);
            intents[1].entity = Some("{{ entity }}".to_owned());
        });
        assert_eq!(result, Ok(()));
    }

    /// AC-3.2b-2.
    #[test]
    fn ac_3_2b_2_example_not_normalized_names_example() {
        for example in ["turn on the [Kitchen](area) lights", "lights on, please"] {
            let result = validate(|intents| intents[0].examples[0] = example.to_owned());
            assert_eq!(result, Err(CatalogError::NotNormal(example.to_owned())));
        }
    }

    /// AC-3.2b-3.
    #[test]
    fn ac_3_2b_3_template_parse_error_names_intent() {
        let result = validate(|intents| intents[5].entity = Some("{{ weather".to_owned()));
        assert_eq!(result, Err(CatalogError::Template("weather".to_owned())));
    }

    /// AC-3.2c-1.
    #[test]
    fn ac_3_2c_1_tags_are_o_then_bio_of_sorted_slots() {
        let expected = ["O", "B-area", "I-area", "B-show", "I-show"];
        assert_eq!(tags(&catalog()), expected);
    }

    /// AC-3.2c-2.
    #[test]
    fn ac_3_2c_2_labels_are_unknown_then_intent_names() {
        let expected = "unknown lights_on fan_on climate_down tv_show temperature_query weather";
        assert_eq!(labels(&catalog()).join(" "), expected);
    }
}
