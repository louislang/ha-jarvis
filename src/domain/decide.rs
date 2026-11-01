//! `decide`: a prediction and a request to a decision.

use rand::rngs::StdRng;
use vstd::prelude::*;

use crate::domain::catalog::Catalog;
use crate::domain::catalog::Kind;
use crate::domain::model::Prediction;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::state::AreaId;
use crate::domain::state::ServiceCall;

/// What Jarvis does for a request.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Speak the reply and make the call.
    Reply {
        /// The reply.
        reply: Chosen,
        /// The service call, if any.
        call: Option<ServiceCall>,
    },
    /// Speak the filler and run the job.
    Defer {
        /// The filler.
        filler: Chosen,
        /// The deferred work.
        job: Job,
    },
    /// Speak the reply from the pool `fallback`: Jarvis knows the intent but cannot act.
    Fallback {
        /// The reply.
        reply: Chosen,
    },
}

/// A selected response.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Chosen {
    /// The text to speak.
    pub text: String,
    /// The pool name.
    pub pool: String,
    /// The index of the response in the pool.
    pub index: usize,
}

/// Work that System One defers.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// Ask System Two this question.
    Llm(String),
}

/// The transcript and the satellite area of a request.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// The transcript.
    pub transcript: String,
    /// The area of the satellite.
    pub area: Option<AreaId>,
}

/// The decision for `prediction` of `request`: a System Two job for an `unknown`, uncertain,
/// removed or non-command label, else a reply from the pool of the command.
#[verus_spec(decision =>
    requires
        responses::valid_pools(pools@, *catalog),
    ensures
        decision is Reply ==> prediction.ranked@.len() > 0
            && prediction.ranked@[0].0@ != "unknown"@
            && vstd::std_specs::cmp::ge_ensures::<f32>(prediction.ranked@[0].1, threshold, true),
        prediction.ranked@.len() > 0 && !cataloged(*catalog, prediction.ranked@[0].0@)
            ==> deferred(decision, request.transcript),
)]
#[must_use]
pub fn decide(
    prediction: &Prediction,
    request: &Request,
    catalog: &Catalog,
    pools: &Pools,
    threshold: f32,
    rng: &mut StdRng,
) -> Decision {
    let Some((label, probability)) = prediction.ranked.as_slice().first() else {
        return defer(request, pools, rng);
    };
    let confident = *probability >= threshold;
    match position(catalog, label.as_str()) {
        Some(index)
            if confident
                && label.as_str() != "unknown"
                && catalog.intents[index].kind == Kind::Command =>
        {
            Decision::Reply {
                reply: choose(pools, catalog.intents[index].name.as_str(), rng),
                call: None,
            }
        }
        _ => defer(request, pools, rng),
    }
}

/// A filler and a System Two job with the transcript of `request`.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key("filler"@)),
        responses::filled(pools@),
    ensures
        deferred(decision, request.transcript),
)]
fn defer(request: &Request, pools: &Pools, rng: &mut StdRng) -> Decision {
    Decision::Defer {
        filler: choose(pools, "filler", rng),
        job: Job::Llm(request.transcript.clone()),
    }
}

/// A random response of the pool `name`, with its template as the text.
#[verus_spec(
    requires
        pools@.contains_key(crate::domain::contracts::string_key(name@)),
        responses::filled(pools@),
)]
fn choose(pools: &Pools, name: &str, rng: &mut StdRng) -> Chosen {
    let pool = pools.get(name).expect("the pool exists");
    let index = responses::select(pool, rng);
    Chosen {
        text: pool.responses[index].text.clone(),
        pool: name.to_owned(),
        index,
    }
}

/// The index of the intent of `catalog` with the name `name`.
#[verus_spec(found =>
    ensures
        match found {
            Some(index) => index < catalog.intents@.len() && catalog.intents@[index as int].name@ == name@,
            None => !cataloged(*catalog, name@),
        },
)]
fn position(catalog: &Catalog, name: &str) -> Option<usize> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= catalog.intents@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> #[trigger] catalog.intents@[earlier].name@ != name@,
        decreases catalog.intents@.len() - index,
    ))]
    while index < catalog.intents.len() {
        if catalog.intents[index].name.as_str() == name {
            return Some(index);
        }
        index += 1;
    }
    None
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_string_obeys_cmp,
    crate::domain::contracts::axiom_string_borrow,
    crate::domain::contracts::axiom_string_contains_borrowed_key,
    crate::domain::contracts::axiom_string_maps_borrowed_key,
};

/// True when `catalog` has an intent with the name `name`.
pub open spec fn cataloged(catalog: Catalog, name: Seq<char>) -> bool {
    exists|index: int| 0 <= index < catalog.intents@.len() && #[trigger] catalog.intents@[index].name@ == name
}

/// True when `decision` asks System Two `question`.
pub open spec fn deferred(decision: Decision, question: String) -> bool {
    decision matches Decision::Defer { job: Job::Llm(asked), .. } && asked@ == question@
}

} // verus!

/// Tests of `decide`.
#[cfg(test)]
mod tests {
    use super::*;

    use rand::SeedableRng;

    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;

    /// The value of `model.threshold` in `jarvis.toml`.
    const THRESHOLD: f32 = 0.6;

    /// The decision for a top label `label` with `probability` and the transcript `transcript`.
    fn decided(label: &str, probability: f32, transcript: &str) -> Decision {
        let catalog: Catalog =
            toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog");
        let names = catalog.intents.iter().map(|intent| intent.name.as_str());
        let pools: Pools = names
            .chain(["filler", "failure", "fallback"])
            .map(|name| {
                let response = Response {
                    text: format!("The {name} response."),
                    uses: 0,
                };
                let pool = Pool {
                    responses: vec![response],
                    last: None,
                };
                (name.to_owned(), pool)
            })
            .collect();
        let prediction = Prediction {
            ranked: vec![(label.to_owned(), probability), ("fan_on".to_owned(), 0.01)],
            slots: Vec::new(),
        };
        let request = Request {
            transcript: transcript.to_owned(),
            area: None,
        };
        let mut rng = StdRng::seed_from_u64(1);
        decide(&prediction, &request, &catalog, &pools, THRESHOLD, &mut rng)
    }

    /// The decision that defers `transcript` to System Two with the filler.
    fn deferral(transcript: &str) -> Decision {
        Decision::Defer {
            filler: Chosen {
                text: "The filler response.".to_owned(),
                pool: "filler".to_owned(),
                index: 0,
            },
            job: Job::Llm(transcript.to_owned()),
        }
    }

    /// AC-4.5-2.
    #[test]
    fn ac_4_5_2_unknown_defers_with_transcript() {
        let transcript = "what is the weather like";
        assert_eq!(decided("unknown", 0.76, transcript), deferral(transcript));
    }

    /// AC-4.5-3.
    #[test]
    fn ac_4_5_3_low_probability_defers() {
        let reply = Decision::Reply {
            reply: Chosen {
                text: "The lights_on response.".to_owned(),
                pool: "lights_on".to_owned(),
                index: 0,
            },
            call: None,
        };
        assert_eq!(
            decided("lights_on", 0.59, "turn on the lights"),
            deferral("turn on the lights")
        );
        assert_eq!(decided("lights_on", THRESHOLD, "turn on the lights"), reply);
    }

    /// AC-4.5-12.
    #[test]
    fn ac_4_5_12_label_not_in_catalog_defers() {
        assert_eq!(
            decided("open_garage", 0.95, "open the garage"),
            deferral("open the garage")
        );
    }
}
