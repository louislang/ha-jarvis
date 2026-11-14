//! Feedback: marks on interactions and the decisions that they block.

use std::collections::BTreeSet;

use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use vstd::prelude::*;

use crate::domain::state::ServiceCall;

/// The part of an interaction that a mark rates.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum Target {
    /// The action: the intent and its targets.
    Action,
    /// The response at this position in the interaction: filler, reply, announce or question.
    Response(usize),
}

/// A rating.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum Value {
    /// The rated part was right.
    Good,
    /// The rated part was wrong.
    Bad,
}

/// A rating of one part of one interaction.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Mark {
    /// The ID of the interaction.
    pub interaction: u64,
    /// The rated part.
    pub target: Target,
    /// The rating. `None` removes the earlier mark of the interaction and target.
    pub value: Option<Value>,
    /// The intent that Jarvis should have selected, or `unknown` for System Two.
    pub correction: Option<String>,
    /// The time of the mark.
    pub time: Timestamp,
}

/// The intent of an action and the sorted entity IDs of its targets.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Action {
    /// The intent name.
    pub intent: String,
    /// The target entity IDs, sorted.
    pub targets: Vec<String>,
}

/// One response of an interaction.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Spoken {
    /// The spoken text.
    pub text: String,
    /// The pool name.
    pub pool: String,
    /// The response template.
    pub template: String,
}

/// The parts of one interaction that its marks rate.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Rated {
    /// The responses, in the order of the interaction.
    pub responses: Vec<Spoken>,
}

/// A slot value with its byte offsets in the normalized transcript.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Tagged {
    /// The slot name.
    pub name: String,
    /// The text of the value.
    pub value: String,
    /// The byte offset of the value.
    pub start: usize,
    /// The byte offset after the value.
    pub end: usize,
}

/// The kind of a decision.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum Route {
    /// A reply, with or without a service call.
    Reply,
    /// `Defer` with `Job::Fetch`.
    Fetch,
    /// `Defer` with `Job::Llm`: System Two.
    Llm,
    /// A question.
    Ask,
}

/// One logged request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Interaction {
    /// The ID.
    pub id: u64,
    /// The time of the request.
    pub time: Timestamp,
    /// The satellite entity, or `None` for a typed request.
    pub satellite: Option<String>,
    /// The transcript.
    pub transcript: String,
    /// Segment A of the model input.
    pub model_context: String,
    /// The run name of the classifier.
    pub model: String,
    /// The three top labels with their probabilities, highest first.
    pub top_labels: Vec<(String, f32)>,
    /// The slot values.
    pub slots: Vec<Tagged>,
    /// The intent of the decision and its targets.
    pub action: Action,
    /// The kind of the decision.
    pub decision: Route,
    /// The service calls.
    pub service_calls: Vec<ServiceCall>,
    /// The responses, in order.
    pub responses: Vec<Spoken>,
    /// The time from the voice end of the utterance to the end of `handle`, or `None` without
    /// audio.
    pub latency_ms: Option<u64>,
    /// The ID of the first interaction of the dialog, for a dialog answer.
    pub link: Option<u64>,
    /// The marks, at most one for each target.
    pub marks: Vec<Mark>,
}

/// A request that System Two answered.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Deferred {
    /// The ID of the interaction.
    pub id: u64,
    /// The transcript.
    pub text: String,
    /// The time of the request.
    pub time: Timestamp,
    /// True when the request came from a satellite.
    pub spoken: bool,
}

/// One mark with the interaction that it rates.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Marked {
    /// The mark. Its value is not `None`.
    pub mark: Mark,
    /// The rated interaction.
    pub rated: Rated,
}

/// The current marks: at most one for each interaction and target.
#[verus_verify]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feedback {
    /// The marks, in the order of `apply`.
    pub marks: Vec<Marked>,
}

#[verus_verify]
impl Feedback {
    /// The feedback with `mark` on `rated` instead of the earlier mark of its interaction and
    /// target.
    #[verus_spec(applied =>
        ensures
            mark.value is None ==> applied.marks@ == self.marks@.filter(other(mark.interaction, mark.target)),
            mark.value is Some ==> {
                &&& applied.marks@.drop_last() == self.marks@.filter(other(mark.interaction, mark.target))
                &&& applied.marks@.last().mark == mark && applied.marks@.last().rated == rated
            },
    )]
    #[must_use]
    pub fn apply(self, rated: Rated, mark: Mark) -> Self {
        let mut marks = without(self.marks, mark.interaction, mark.target);
        if mark.value.is_some() {
            marks.push(Marked { mark, rated });
        }
        Self { marks }
    }

    /// The pool and the template of each response with a bad mark.
    #[verus_spec(found =>
        ensures
            forall|pair: (String, String)| found@.contains(pair) <==> bad_listed(self.marks@, self.marks@.len() as int, pair),
    )]
    #[must_use]
    pub fn bad_responses(&self) -> BTreeSet<(String, String)> {
        let mut found = BTreeSet::new();
        let mut index = 0;
        #[cfg_attr(verus_only, verus_spec(
            invariant
                index <= self.marks@.len(),
                forall|pair: (String, String)| found@.contains(pair) <==> bad_listed(self.marks@, index as int, pair),
            decreases self.marks@.len() - index,
        ))]
        while index < self.marks.len() {
            proof! {
                lemma_listed_step(self.marks@, index as int);
            }
            if let Some(pair) = bad_pair(&self.marks[index]) {
                found.insert(pair);
            }
            index += 1;
        }
        found
    }

    /// The texts of up to `n` good and up to `n` bad marked responses of `pool`.
    #[must_use]
    pub fn examples(&self, pool: &str, n: usize) -> (Vec<String>, Vec<String>) {
        let (mut good, mut bad, mut index) = (Vec::new(), Vec::new(), 0);
        #[cfg_attr(verus_only, verus_spec(
            invariant
                index <= self.marks@.len(),
            decreases self.marks@.len() - index,
        ))]
        while index < self.marks.len() {
            let marked = &self.marks[index];
            if let Some(text) = text_in(marked, pool) {
                match marked.mark.value {
                    Some(Value::Good) if good.len() < n => good.push(text),
                    Some(Value::Bad) if bad.len() < n => bad.push(text),
                    _ => {}
                }
            }
            index += 1;
        }
        (good, bad)
    }
}

/// The text of the response that `marked` marks, when the response is from `pool`.
#[verus_verify]
fn text_in(marked: &Marked, pool: &str) -> Option<String> {
    let responses = &marked.rated.responses;
    match marked.mark.target {
        Target::Response(index)
            if index < responses.len() && responses[index].pool.as_str() == pool =>
        {
            Some(responses[index].text.clone())
        }
        _ => None,
    }
}

/// The pool and the template of the response that `marked` marks bad, if any.
#[verus_spec(pair =>
    ensures
        pair == bad_of(*marked),
)]
fn bad_pair(marked: &Marked) -> Option<(String, String)> {
    let responses = &marked.rated.responses;
    match marked.mark.target {
        Target::Response(index) if index < responses.len() => match marked.mark.value {
            Some(Value::Bad) => Some((
                responses[index].pool.clone(),
                responses[index].template.clone(),
            )),
            _ => None,
        },
        _ => None,
    }
}

/// `marks` without the mark on `target` of `interaction`.
#[verus_spec(kept =>
    ensures
        kept@ == marks@.filter(other(interaction, target)),
)]
fn without(marks: Vec<Marked>, interaction: u64, target: Target) -> Vec<Marked> {
    let mut kept = Vec::new();
    let all = marks.into_iter();
    proof_decl! {
        let ghost seen = vstd::std_specs::iter::IteratorSpec::remaining(&all);
    }
    #[cfg_attr(verus_only, verus_spec(marked_iter =>
        invariant
            marked_iter.seq() == seen,
            seen == marks@,
            kept@ == seen.take(marked_iter.index() as int).filter(other(interaction, target)),
    ))]
    for marked in all {
        proof! {
            lemma_filter_step(seen, marked_iter.index() as int, other(interaction, target));
        }
        let same = match (marked.mark.target, target) {
            (Target::Action, Target::Action) => true,
            (Target::Response(left), Target::Response(right)) => left == right,
            _ => false,
        };
        if marked.mark.interaction != interaction || !same {
            kept.push(marked);
        }
    }
    proof! {
        assert(seen.take(seen.len() as int) =~= seen);
    }
    kept
}

verus! {

broadcast use {crate::domain::contracts::axiom_string_obeys_cmp, vstd::laws_cmp::group_laws_cmp};

/// The predicate of the marks that are not on `target` of `interaction`.
pub open spec fn other(interaction: u64, target: Target) -> spec_fn(Marked) -> bool {
    |marked: Marked| marked.mark.interaction != interaction || marked.mark.target != target
}

/// The pool and the template of the response that `marked` marks bad, if any.
pub open spec fn bad_of(marked: Marked) -> Option<(String, String)> {
    let responses = marked.rated.responses@;
    match marked.mark.target {
        Target::Response(index) => if index < responses.len() && marked.mark.value == Some(Value::Bad) {
            Some((responses[index as int].pool, responses[index as int].template))
        } else {
            None
        },
        Target::Action => None,
    }
}

/// True when one of the first `count` marks of `marks` marks bad the response with `pair`.
pub open spec fn bad_listed(marks: Seq<Marked>, count: int, pair: (String, String)) -> bool {
    exists|index: int| 0 <= index < count && #[trigger] bad_of(marks[index]) == Some(pair)
}

/// A pair is in the first `index + 1` marks when it is in the first `index`, or the mark at
/// `index` marks it bad.
proof fn lemma_listed_step(marks: Seq<Marked>, index: int)
    requires
        0 <= index < marks.len(),
    ensures
        forall|pair: (String, String)| bad_listed(marks, index + 1, pair)
            <==> bad_listed(marks, index, pair) || bad_of(marks[index]) == Some(pair),
{
    assert forall|pair: (String, String)| bad_listed(marks, index + 1, pair)
        implies bad_listed(marks, index, pair) || bad_of(marks[index]) == Some(pair) by {
        let found = choose|found: int| 0 <= found < index + 1 && #[trigger] bad_of(marks[found]) == Some(pair);
        if found < index {
            assert(bad_listed(marks, index, pair));
        }
    }
}

/// The filter of the first `index + 1` items is the filter of the first `index`, with the item at
/// `index` when `keep` holds for it.
proof fn lemma_filter_step<T>(items: Seq<T>, index: int, keep: spec_fn(T) -> bool)
    requires
        0 <= index < items.len(),
    ensures
        items.take(index + 1).filter(keep) == if keep(items[index]) {
            items.take(index).filter(keep).push(items[index])
        } else {
            items.take(index).filter(keep)
        },
{
    reveal_with_fuel(Seq::filter, 1);
    assert(items.take(index + 1).drop_last() =~= items.take(index));
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    /// The mark `value` on the action of `interaction`.
    fn mark(interaction: u64, value: Option<Value>) -> Mark {
        Mark {
            interaction,
            target: Target::Action,
            value,
            correction: None,
            time: Timestamp::UNIX_EPOCH,
        }
    }

    /// AC-12.3c-1.
    #[test]
    fn ac_12_3c_1_bad_responses_has_only_the_bad_response() {
        let spoken = |pool: &str| Spoken {
            text: format!("The {pool} text."),
            pool: pool.to_owned(),
            template: format!("The {pool} template."),
        };
        let responses = ["filler", "tv_on", "failure"].map(spoken).to_vec();
        let rated = Rated { responses };
        let response = Mark {
            target: Target::Response(1),
            ..mark(1, Some(Value::Bad))
        };
        let feedback = Feedback::default().apply(rated, response);
        let pair = ("tv_on".to_owned(), "The tv_on template.".to_owned());
        assert_eq!(feedback.bad_responses(), BTreeSet::from([pair]));
    }
}
