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
    /// The feedback with `mark` on `rated` instead of the earlier mark of its interaction and
    /// target. A mark on a dialog answer counts for the text of the first interaction.
    #[verus_spec(applied =>
        ensures
            mark.value is None ==> applied.marks@ == self.marks@.filter(other(mark.interaction, mark.target)),
            mark.value is Some ==> {
                &&& applied.marks@.drop_last() == self.marks@.filter(other(mark.interaction, mark.target))
                &&& applied.marks@.last().mark == mark && applied.marks@.last().rated == rated
                &&& applied.marks@.last().text@ == intent::normalized(asked(rated))
            },
    )]
    #[must_use]
    pub fn apply(self, rated: Rated, mark: Mark) -> Self {
        let mut marks = without(self.marks, mark.interaction, mark.target);
        if mark.value.is_some() {
            let text = match &rated.link {
                Some(first) => intent::normalize(first),
                None => intent::normalize(&rated.transcript),
            };
            marks.push(Marked { mark, text, rated });
        }
        Self { marks }
    }

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

/// The text that the marks of `rated` count for: the transcript of the first interaction of a
/// dialog, else the transcript.
pub open spec fn asked(rated: Rated) -> Seq<char> {
    match rated.link {
        Some(first) => first@,
        None => rated.transcript@,
    }
}

/// The predicate of the marks that are not on `target` of `interaction`.
pub open spec fn other(interaction: u64, target: Target) -> spec_fn(Marked) -> bool {
    |marked: Marked| marked.mark.interaction != interaction || marked.mark.target != target
}

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
    use std::collections::BTreeMap;
    use std::mem;

    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;

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
            mark: mark(interaction, Some(value)),
            text: "turn on the tv".to_owned(),
            rated: rated(TV, office()),
        });
        Feedback {
            marks: marks.collect(),
        }
    }

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

    /// AC-12.3b-1.
    #[test]
    fn ac_12_3b_1_removed_mark_restores_blocked() {
        let feedback = marked(&[Value::Bad, Value::Good]);
        let removed = feedback.apply(rated(TV, office()), mark(2, None));
        assert!(blocked(&removed, TV), "not blocked without the good mark");
        let removed = removed.apply(rated(TV, office()), mark(1, None));
        assert!(!blocked(&removed, TV), "blocked without marks");
    }

    /// AC-12.3b-3.
    #[test]
    fn ac_12_3b_3_dialog_answer_counts_for_the_first_text() {
        let link = Some(TV.to_owned());
        let answer = Rated {
            link,
            ..rated("the office", office())
        };
        let feedback = Feedback::default().apply(answer, mark(2, Some(Value::Bad)));
        assert!(blocked(&feedback, TV), "the first text is not blocked");
    }

    /// The targets of the model decision `d`: the Office or the Living Room media player.
    fn targets(d: &str) -> Vec<String> {
        let room = if d == "d1" { "office" } else { "living_room" };
        vec![format!("media_player.{room}")]
    }

    /// A mark value of `feedback.qnt`.
    #[derive(Deserialize)]
    struct Pick {
        /// `Good` or `Bad`.
        tag: String,
    }

    /// The good and bad marks of `feedback.qnt` for each text and decision.
    #[derive(Deserialize)]
    struct Model {
        /// The good marks.
        good: BTreeMap<(String, String), i64>,
        /// The bad marks.
        bad: BTreeMap<(String, String), i64>,
    }

    /// `blocked` for each text and decision.
    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(from = "Model")]
    struct Blocked(BTreeMap<(String, String), bool>);

    impl From<Model> for Blocked {
        /// Bad marks more than good marks.
        fn from(model: Model) -> Self {
            let blocked = model.bad.into_iter().map(|(key, bad)| {
                let good = model.good[&key];
                (key, bad > good)
            });
            Self(blocked.collect())
        }
    }

    impl ModelState<FeedbackDriver> for Blocked {
        /// `blocked` for "turn on the tv" with each decision.
        fn from_driver(driver: &FeedbackDriver) -> anyhow::Result<Self> {
            let blocked = ["d1", "d2"].map(|d| {
                let found = driver.feedback.blocked(TV, "tv_on", &targets(d));
                (("t".to_owned(), d.to_owned()), found)
            });
            Ok(Self(blocked.into()))
        }
    }

    /// Replays `feedback.qnt` against `Feedback`. The text `t` is "turn on the tv", and each mark
    /// is on a new interaction.
    #[derive(Default)]
    struct FeedbackDriver {
        /// The feedback.
        feedback: Feedback,
        /// The interaction, decision and value of each mark that is not removed.
        marks: Vec<(u64, String, Value)>,
        /// The number of marks so far, which is the ID of the last marked interaction.
        count: u64,
    }

    impl FeedbackDriver {
        /// Applies `mark` on the action of decision `d`.
        fn apply(&mut self, d: &str, mark: Mark) {
            let rated = rated(TV, targets(d));
            self.feedback = mem::take(&mut self.feedback).apply(rated, mark);
        }

        /// Marks decision `d` with `value` on a new interaction.
        fn mark(&mut self, d: String, value: Value) {
            self.count += 1;
            self.apply(&d, mark(self.count, Some(value)));
            self.marks.push((self.count, d, value));
        }

        /// Removes a mark with `value` on decision `d`.
        fn unmark(&mut self, d: &str, value: Value) {
            let found = self.marks.iter().position(|m| m.1 == d && m.2 == value);
            let (interaction, ..) = self.marks.remove(found.expect("a mark to remove"));
            self.apply(d, mark(interaction, None));
        }
    }

    impl Driver for FeedbackDriver {
        type State = Blocked;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::default(),
                markGood(d: String) => self.mark(d, Value::Good),
                markBad(d: String) => self.mark(d, Value::Bad),
                unmark(d: String, value: Pick) => {
                    self.unmark(&d, if value.tag == "Bad" { Value::Bad } else { Value::Good });
                },
                decide => {},
            })
        }
    }

    /// AC-12.3b-2. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/feedback.qnt", max_steps = 30, seed = "1")]
    fn ac_12_3b_2_conformance() -> impl Driver {
        FeedbackDriver::default()
    }
}
