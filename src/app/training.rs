//! Generated examples, training trigger, training run, accept and swap.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use xxhash_rust::xxh3;

use crate::TrainConfig;
use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::intent;
use crate::ports::Reasoner;
use crate::ports::Store;

/// The generated examples of `train.generated`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Generated {
    /// The examples of each intent, by name.
    pub intents: BTreeMap<String, Paraphrases>,
    /// The examples of `unknown`.
    pub unknown: Paraphrases,
}

/// The valid generated examples of one label, with the hash of the input that gave them.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Paraphrases {
    /// The hash of the seeds of the intent, or of the intent names for `unknown`.
    pub hash: u64,
    /// The examples in `[value](slot)` markup.
    pub examples: Vec<String>,
}

/// `generated` with new examples for each intent of `catalog` whose seeds changed, and for
/// `unknown` when the intent names changed, without the intents that `catalog` does not have.
/// Saves the result to `store`. A failed `paraphrase` keeps the old examples of its label.
///
/// # Errors
///
/// The save fails.
pub async fn refresh(
    reasoner: &dyn Reasoner,
    store: &dyn Store<Generated>,
    catalog: &Catalog,
    generated: Generated,
    train: &TrainConfig,
) -> anyhow::Result<Generated> {
    let checker = Checker {
        reasoner,
        seeds: seed_texts(catalog),
    };
    let intents = checker.intents(catalog, generated.intents, train).await;
    let unknown = checker.unknown(catalog, generated.unknown, train).await;
    let generated = Generated { intents, unknown };
    store.save(&generated)?;
    Ok(generated)
}

/// One `paraphrase` request for a label.
struct Request<'a> {
    /// The label.
    target: &'a str,
    /// The unheld seed examples of each intent in the prompt.
    seeds: &'a BTreeMap<String, Vec<String>>,
    /// The slots of the label.
    slots: &'a BTreeSet<String>,
    /// The number of examples.
    n: usize,
    /// The hash of the input of the label.
    hash: u64,
}

/// Asks System Two for examples and keeps the valid ones.
struct Checker<'a> {
    /// System Two, which writes the examples.
    reasoner: &'a dyn Reasoner,
    /// The normalized texts of all seeds, held-out ones included.
    seeds: BTreeSet<String>,
}

impl Checker<'_> {
    /// The examples of each intent of `catalog`, refreshed from `old`.
    async fn intents(
        &self,
        catalog: &Catalog,
        mut old: BTreeMap<String, Paraphrases>,
        train: &TrainConfig,
    ) -> BTreeMap<String, Paraphrases> {
        let mut intents = BTreeMap::new();
        for intent in &catalog.intents {
            let unheld = intent::unheld(&intent.examples, train.holdout);
            let mut slots = BTreeSet::new();
            catalog::insert_slots(intent, &mut slots);
            let request = Request {
                target: &intent.name,
                seeds: &BTreeMap::from([(intent.name.clone(), unheld)]),
                slots: &slots,
                n: train.paraphrases,
                hash: hash(&intent.examples),
            };
            let stored = old.remove(&intent.name).unwrap_or_default();
            intents.insert(intent.name.clone(), self.refreshed(stored, request).await);
        }
        intents
    }

    /// The examples of `unknown`, refreshed from `old`.
    async fn unknown(
        &self,
        catalog: &Catalog,
        old: Paraphrases,
        train: &TrainConfig,
    ) -> Paraphrases {
        let mut seeds = BTreeMap::new();
        let mut names = Vec::new();
        for intent in &catalog.intents {
            let unheld = intent::unheld(&intent.examples, train.holdout);
            seeds.insert(intent.name.clone(), unheld);
            names.push(intent.name.clone());
        }
        let request = Request {
            target: "unknown",
            seeds: &seeds,
            slots: &BTreeSet::new(),
            n: train.unknown_paraphrases,
            hash: hash(&names),
        };
        self.refreshed(old, request).await
    }

    /// `old` when its hash is the hash of `request`, else the valid examples of `request`, or
    /// `old` when the call fails.
    async fn refreshed(&self, old: Paraphrases, request: Request<'_>) -> Paraphrases {
        if old.hash == request.hash {
            return old;
        }
        let Request {
            target,
            seeds,
            slots,
            n,
            hash,
        } = request;
        match self.reasoner.paraphrase(target, seeds, slots, n).await {
            Ok(examples) => {
                let valid = |markup: &String| intent::validate_example(markup, slots, &self.seeds);
                let examples = examples.into_iter().filter(|markup| valid(markup).is_ok());
                Paraphrases {
                    hash,
                    examples: examples.collect(),
                }
            }
            Err(error) => {
                tracing::warn!("the paraphrase of {target} failed: {error:#}");
                old
            }
        }
    }
}

/// The normalized texts of the seed examples of `catalog`.
fn seed_texts(catalog: &Catalog) -> BTreeSet<String> {
    let intents = catalog.intents.iter().flat_map(|intent| &intent.examples);
    let markups = intents.chain(&catalog.unknown_examples);
    let examples = markups.filter_map(|markup| intent::parse_example(markup).ok());
    examples.map(|example| example.text).collect()
}

/// The hash of `texts`, stable across builds.
fn hash(texts: &[String]) -> u64 {
    xxh3::xxh3_64(texts.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::slice;

    use crate::Config;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStore;

    /// The catalog of `intents.toml`.
    fn catalog() -> Catalog {
        toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog")
    }

    /// The result of `refresh` of `catalog` and `generated` with `reasoner`.
    async fn refreshed(
        reasoner: &FakeReasoner,
        catalog: &Catalog,
        generated: Generated,
    ) -> Generated {
        let config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
        let store = FakeStore::new(None);
        let refreshed = refresh(reasoner, &store, catalog, generated, &config.train).await;
        let refreshed = refreshed.expect("the fake store saves");
        assert_eq!(
            store.saves(),
            slice::from_ref(&refreshed),
            "one save of the result"
        );
        refreshed
    }

    /// The examples of `catalog` that a first `refresh` generates.
    async fn first(catalog: &Catalog) -> Generated {
        let reasoner = FakeReasoner::default();
        let lights = Ok(vec!["turn the [den](area) lights on".to_owned()]);
        reasoner.script_paraphrase("lights_on", lights);
        let generated = refreshed(&reasoner, catalog, Generated::default()).await;
        let targets = reasoner.paraphrased();
        assert!(
            targets.contains(&"lights_on".to_owned()),
            "a first refresh generates lights_on: {targets:?}"
        );
        generated
    }

    /// The `paraphrase` targets of a `refresh` of `changed` after a first `refresh` of `intents.toml`.
    async fn targets(changed: &Catalog) -> Vec<String> {
        let generated = first(&catalog()).await;
        let reasoner = FakeReasoner::default();
        refreshed(&reasoner, changed, generated).await;
        reasoner.paraphrased()
    }

    /// `intents.toml` with a new seed example of `lights_on`.
    fn new_lights_seed() -> Catalog {
        let mut catalog = catalog();
        let lights = catalog
            .intents
            .iter_mut()
            .find(|intent| intent.name == "lights_on");
        let lights = lights.expect("intents.toml has lights_on");
        lights.examples.push("lights on please".to_owned());
        catalog
    }

    #[tokio::test]
    async fn ac_13_6_1_unchanged_seeds_give_no_call() {
        let targets = targets(&catalog()).await;

        assert!(
            !targets.contains(&"lights_on".to_owned()),
            "no paraphrase call for lights_on: {targets:?}"
        );
    }

    #[tokio::test]
    async fn ac_13_6_2_changed_seeds_give_one_call() {
        let targets = targets(&new_lights_seed()).await;

        assert_eq!(targets, ["lights_on"]);
    }

    #[tokio::test]
    async fn ac_13_6_3_changed_intent_list_regenerates_unknown() {
        let mut changed = catalog();
        changed.intents.pop();

        let targets = targets(&changed).await;

        assert_eq!(targets, ["unknown"]);
    }

    #[tokio::test]
    async fn ac_13_6_4_failed_paraphrase_keeps_examples() {
        let generated = first(&catalog()).await;
        let before = generated.intents["lights_on"].clone();
        let reasoner = FakeReasoner::default();
        reasoner.script_paraphrase("lights_on", Err("the request failed"));

        let refreshed = refreshed(&reasoner, &new_lights_seed(), generated).await;

        assert_eq!(before.examples, ["turn the [den](area) lights on"]);
        assert_eq!(refreshed.intents["lights_on"], before);
    }

    #[tokio::test]
    async fn ac_13_6_5_removed_intent_loses_examples() {
        let generated = first(&catalog()).await;
        let mut changed = catalog();
        changed.intents.retain(|intent| intent.name != "lights_on");

        let refreshed = refreshed(&FakeReasoner::default(), &changed, generated).await;

        assert!(
            !refreshed.intents.contains_key("lights_on"),
            "lights_on is gone: {:?}",
            refreshed.intents.keys()
        );
    }
}
