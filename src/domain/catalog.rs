//! Intent catalog types and validation.

use std::collections::BTreeMap;

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
}

/// Checks the example counts, the example markup and the intent names of `catalog`.
///
/// # Errors
///
/// The first intent that fails a check.
#[verus_spec(result =>
    ensures
        result is Ok <==> valid_catalog(*catalog),
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

verus! {

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
}
