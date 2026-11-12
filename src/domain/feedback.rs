//! Feedback: marks on interactions and the decisions that they block.

use jiff::Timestamp;
use vstd::prelude::*;

use crate::domain::intent;
use crate::domain::select;

/// The part of an interaction that a mark rates.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The action: the intent and its targets.
    Action,
    /// The response at this position in the interaction: filler, reply, announce or question.
    Response(usize),
}

/// A rating.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    /// The rated part was right.
    Good,
    /// The rated part was wrong.
    Bad,
}

/// A rating of one part of one interaction.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
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
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    /// The intent name.
    pub intent: String,
    /// The target entity IDs, sorted.
    pub targets: Vec<String>,
}

/// The parts of one interaction that its marks rate.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Rated {
    /// The transcript.
    pub transcript: String,
    /// The transcript of the first interaction of the dialog, for a dialog answer.
    pub link: Option<String>,
    /// The action.
    pub action: Action,
}

/// One mark with the interaction that it rates.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Marked {
    /// The mark. Its value is not `None`.
    pub mark: Mark,
    /// The normalized text that an action mark counts for.
    pub text: String,
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
    /// True when the bad action marks for the normalized `text`, `intent` and the set `targets`
    /// are more than the good ones.
    #[verus_spec(found =>
        ensures
            found == (counted(self.marks@, text@, intent@, targets@, Value::Bad)
                > counted(self.marks@, text@, intent@, targets@, Value::Good)),
    )]
    #[must_use]
    pub fn blocked(&self, text: &str, intent: &str, targets: &[String]) -> bool {
        let normal = intent::normalize(text);
        let bad = count(&self.marks, &normal, intent, targets, Value::Bad);
        bad > count(&self.marks, &normal, intent, targets, Value::Good)
    }
}

/// The number of action marks of `marks` with `value` for the normalized text `normal`, `intent`
/// and the set `targets`.
#[verus_spec(found =>
    ensures
        found == marks@.filter(counts((normal@, intent@, select::views(targets@)), value)).len(),
)]
fn count(marks: &[Marked], normal: &str, intent: &str, targets: &[String], value: Value) -> usize {
    let (mut found, mut index): (usize, usize) = (0, 0);
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= marks@.len() && found <= index,
            found == marks@.take(index as int).filter(counts((normal@, intent@, select::views(targets@)), value)).len(),
        decreases marks@.len() - index,
    ))]
    while index < marks.len() {
        proof! {
            lemma_filter_step(marks@, index as int, counts((normal@, intent@, select::views(targets@)), value));
        }
        let valued = matches!(
            (marks[index].mark.value, value),
            (Some(Value::Good), Value::Good) | (Some(Value::Bad), Value::Bad)
        );
        if valued && is_for(&marks[index], normal, intent, targets) {
            found += 1;
        }
        index += 1;
    }
    proof! {
        assert(marks@.take(marks@.len() as int) =~= marks@);
    }
    found
}

/// True when `marked` is an action mark for the normalized text `normal`, `intent` and the set
/// `targets`.
#[verus_spec(found =>
    ensures
        found == action_of(marked.mark, marked.text@, marked.rated.action, (normal@, intent@, select::views(targets@))),
)]
fn is_for(marked: &Marked, normal: &str, intent: &str, targets: &[String]) -> bool {
    let action = &marked.rated.action;
    matches!(marked.mark.target, Target::Action)
        && marked.text.as_str() == normal
        && action.intent.as_str() == intent
        && select::has_words(&action.targets, targets)
        && select::has_words(targets, &action.targets)
}

verus! {

/// True when `mark` is an action mark on `action` for the key: a normalized text, an intent and a
/// set of targets.
pub open spec fn action_of(mark: Mark, text: Seq<char>, action: Action, key: (Seq<char>, Seq<char>, Seq<Seq<char>>)) -> bool {
    let targets = select::views(action.targets@);
    &&& mark.target == Target::Action && text == key.0 && action.intent@ == key.1
    &&& select::has_all(targets, key.2) && select::has_all(key.2, targets)
}

/// The predicate of the action marks with `value` for `key`.
pub open spec fn counts(key: (Seq<char>, Seq<char>, Seq<Seq<char>>), value: Value) -> spec_fn(Marked) -> bool {
    |marked: Marked| action_of(marked.mark, marked.text@, marked.rated.action, key) && marked.mark.value == Some(value)
}

/// The number of action marks of `marks` with `value` for the normalized `text`, `intent` and the
/// set `targets`.
pub open spec fn counted(marks: Seq<Marked>, text: Seq<char>, intent: Seq<char>, targets: Seq<String>, value: Value) -> nat {
    marks.filter(counts((intent::normalized(text), intent, select::views(targets)), value)).len()
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

    /// The request of the tests.
    const TV: &str = "Turn on the TV.";

    /// The entity IDs of the Office media player.
    fn office() -> Vec<String> {
        vec!["media_player.office".to_owned()]
    }

    /// The interaction of `transcript` with the action `tv_on` on `targets`.
    fn rated(transcript: &str, targets: Vec<String>) -> Rated {
        let intent = "tv_on".to_owned();
        Rated {
            transcript: transcript.to_owned(),
            link: None,
            action: Action { intent, targets },
        }
    }

    /// The feedback with one action mark for each of `values` on "turn on the tv" with the Office
    /// media player.
    fn marked(values: &[Value]) -> Feedback {
        let marks = values.iter().zip(1..).map(|(&value, interaction)| Marked {
            mark: Mark {
                interaction,
                target: Target::Action,
                value: Some(value),
                correction: None,
                time: Timestamp::UNIX_EPOCH,
            },
            text: "turn on the tv".to_owned(),
            rated: rated(TV, office()),
        });
        Feedback {
            marks: marks.collect(),
        }
    }

    /// True when `feedback` blocks `tv_on` with the Office media player for `text`.
    fn blocked(feedback: &Feedback, text: &str) -> bool {
        feedback.blocked(text, "tv_on", &office())
    }

    /// AC-12.3-1.
    #[test]
    fn ac_12_3_1_bad_action_is_blocked() {
        let feedback = marked(&[Value::Bad]);
        assert!(blocked(&feedback, "turn on the tv"), "not blocked");
    }

    /// AC-12.3-2.
    #[test]
    fn ac_12_3_2_one_bad_and_two_good_marks_do_not_block() {
        let feedback = marked(&[Value::Bad, Value::Good, Value::Good]);
        assert!(!blocked(&feedback, TV), "blocked after two good marks");
    }

    /// AC-12.3-3.
    #[test]
    fn ac_12_3_3_other_text_is_not_blocked() {
        let feedback = marked(&[Value::Bad]);
        assert!(
            !blocked(&feedback, "turn on the lights"),
            "the lights are blocked"
        );
    }
}
