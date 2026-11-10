//! Dialog state: open dialogs, turns, timeout and generations. Answer fill and cancel.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use jiff::Timestamp;
use vstd::prelude::*;

use crate::domain::intent;
use crate::domain::model::Slot;
use crate::domain::select;

/// The key of a dialog: the satellite ID, else the conversation ID.
#[verus_verify]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(pub String);

/// What an open dialog waits for.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    /// A slot value of a System One intent.
    Slot {
        /// The intent name.
        intent: String,
        /// The slots that the request has.
        slots: Vec<Slot>,
        /// The name of the missing slot.
        missing: String,
        /// The values that fill the missing slot. Empty when any answer fills it.
        choices: Vec<String>,
        /// The transcript of the request that asked.
        transcript: String,
    },
    /// The answer to a question of System Two.
    Llm {
        /// The ID of the response that asked.
        response_id: String,
        /// The ID of the tool call that asked.
        call_id: String,
    },
}

/// One open dialog.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Dialog {
    /// What the dialog waits for.
    pub pending: Pending,
    /// The number of questions so far.
    pub turns: u32,
    /// The time of the last question.
    pub asked: Timestamp,
}

/// The open dialogs and the generation of each key.
#[verus_verify]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dialogs {
    /// The open dialog of each key.
    pub dialogs: BTreeMap<Key, Dialog>,
    /// The generation of each key. A key that is not in the map has generation 0.
    pub generations: BTreeMap<Key, u64>,
}

#[verus_verify]
impl Dialogs {
    /// Opens a dialog on `key` with one question at `now`. It replaces the open dialog of `key`.
    #[verus_spec(
        ensures
            final(self).dialogs@ == old(self).dialogs@.insert(key, Dialog { pending, turns: 1, asked: now }),
            final(self).generations@ == old(self).generations@,
    )]
    pub fn open(&mut self, key: Key, pending: Pending, now: Timestamp) {
        let dialog = Dialog {
            pending,
            turns: 1,
            asked: now,
        };
        self.dialogs.insert(key, dialog);
    }

    /// Removes the dialog of `key` and gives it, or `None` when `timeout_s` passed since its last
    /// question.
    #[verus_spec(taken =>
        ensures
            final(self).dialogs@ == old(self).dialogs@.remove(*key),
            final(self).generations@ == old(self).generations@,
            taken == live(old(self).dialogs@, *key, now, timeout_s),
    )]
    pub fn take(&mut self, key: &Key, now: Timestamp, timeout_s: u64) -> Option<Dialog> {
        let dialog = self.dialogs.remove(key)?;
        let timeout = i128::from(timeout_s) * 1_000_000_000;
        match dialog.asked.as_nanosecond().checked_add(timeout) {
            Some(deadline) if now.as_nanosecond() >= deadline => None,
            _ => Some(dialog),
        }
    }

    /// Removes the dialog of `key` and increases the generation of `key` by 1. A generation of
    /// `u64::MAX` stays.
    #[verus_spec(
        ensures
            final(self).dialogs@ == old(self).dialogs@.remove(key),
            final(self).generations@ == old(self).generations@.insert(key, increased(generation_of(old(self).generations@, key))),
    )]
    pub fn close(&mut self, key: Key) {
        self.dialogs.remove(&key);
        let generation = self.generation(&key).saturating_add(1);
        self.generations.insert(key, generation);
    }

    /// The generation of `key`.
    #[verus_spec(current =>
        ensures
            current == generation_of(self.generations@, *key),
    )]
    #[must_use]
    pub fn generation(&self, key: &Key) -> u64 {
        *self.generations.get(key).unwrap_or(&0)
    }
}

/// `dialog` with one more question at `now`, or `None` when it has `max_turns` questions.
#[verus_spec(next =>
    requires
        dialog.turns <= max_turns,
    ensures
        match next {
            Some(next) => dialog.turns < max_turns && next == (Dialog { pending: dialog.pending, turns: (dialog.turns + 1) as u32, asked: now }),
            None => dialog.turns >= max_turns,
        },
)]
#[must_use]
pub fn next_turn(dialog: Dialog, max_turns: u32, now: Timestamp) -> Option<Dialog> {
    if dialog.turns >= max_turns {
        return None;
    }
    Some(Dialog {
        turns: dialog.turns + 1,
        asked: now,
        ..dialog
    })
}

/// True when the normalized `text` is a phrase of `cancel`.
#[verus_spec(cancelled =>
    ensures
        cancelled == select::listed(cancel@, intent::normalized(text@)),
)]
#[must_use]
pub fn is_cancel(text: &str, cancel: &[String]) -> bool {
    select::is_listed(cancel, intent::normalize(text).as_str())
}

/// The value that `answer` gives: the normalized answer when `choices` is empty, else the longest
/// choice whose words are all words of the answer, else the choice most similar to the answer
/// without a leading "the" when its Jaro-Winkler similarity is at least `similarity`.
#[verus_spec(filled =>
    ensures
        choices@.len() == 0 ==> (filled matches Some(value) && value@ == intent::normalized(answer@)),
        !none_named(heard(answer@), choices@, choices@.len() as int)
            ==> (filled matches Some(value) && longest_named(heard(answer@), choices@, value@)),
        choices@.len() > 0 && none_named(heard(answer@), choices@, choices@.len() as int) ==> match filled {
            Some(value) => similar(trimmed(intent::normalized(answer@)), choices@, value@, similarity),
            None => dissimilar(trimmed(intent::normalized(answer@)), choices@, similarity),
        },
)]
#[must_use]
pub fn fill(answer: &str, choices: &[String], similarity: f64) -> Option<String> {
    let normal = intent::normalize(answer);
    if choices.is_empty() {
        return Some(normal);
    }
    if let Some(best) = longest(&select::words(normal.as_str()), choices) {
        return Some(choices[best].clone());
    }
    let text = match normal.as_str().strip_prefix("the ") {
        Some(rest) => {
            proof! {
                assert(normal@.take("the "@.len() as int) =~= "the "@);
                assert(normal@.skip("the "@.len() as int) =~= rest@);
            }
            rest
        }
        None => normal.as_str(),
    };
    let (best, score) = closest(text, choices);
    proof! {
        assert(most_similar(text@, choices@, choices@.len() as int, best as int));
    }
    if score >= similarity {
        Some(choices[best].clone())
    } else {
        None
    }
}

/// The longest choice whose words are all in `heard`.
#[verus_spec(found =>
    ensures
        match found {
            Some(best) => longest_of(select::views(heard@), choices@, choices@.len() as int, best as int),
            None => none_named(select::views(heard@), choices@, choices@.len() as int),
        },
)]
fn longest(heard: &[String], choices: &[String]) -> Option<usize> {
    let mut found: Option<usize> = None;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= choices@.len(),
            match found {
                Some(best) => longest_of(select::views(heard@), choices@, index as int, best as int),
                None => none_named(select::views(heard@), choices@, index as int),
            },
        decreases choices@.len() - index,
    ))]
    while index < choices.len() {
        let longer = match found {
            Some(best) => choices[index].len() > choices[best].len(),
            None => true,
        };
        let wanted = select::words(intent::normalize(choices[index].as_str()).as_str());
        if longer && select::has_words(heard, &wanted) {
            found = Some(index);
        }
        index += 1;
    }
    found
}

/// The index of the choice most similar to `text`, and its Jaro-Winkler similarity.
#[verus_spec(found =>
    requires
        choices@.len() > 0,
    ensures
        most_similar(text@, choices@, choices@.len() as int, found.0 as int),
        found.1 == similarity_to(text@, choices@[found.0 as int]@),
)]
fn closest(text: &str, choices: &[String]) -> (usize, f64) {
    let mut best = (0, 0.0);
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= choices@.len(),
            best.0 < choices@.len(),
            index > 0 ==> best.1 == similarity_to(text@, choices@[best.0 as int]@),
            most_similar(text@, choices@, index as int, best.0 as int),
        decreases choices@.len() - index,
    ))]
    while index < choices.len() {
        let score = strsim::jaro_winkler(text, intent::normalize(choices[index].as_str()).as_str());
        proof! {
            crate::domain::contracts::axiom_total_order_f64_is_total();
            assert((crate::domain::contracts::total_order_f64())(score, score) != Ordering::Greater);
        }
        if index == 0 || matches!(score.total_cmp(&best.1), Ordering::Greater) {
            best = (index, score);
        }
        index += 1;
    }
    best
}

verus! {

broadcast use {crate::domain::contracts::axiom_key_obeys_cmp, crate::domain::contracts::axiom_str_pattern};

/// The generation of `key` in `generations`: 0 when it has none.
pub open spec fn generation_of(generations: Map<Key, u64>, key: Key) -> u64 {
    if generations.contains_key(key) { generations[key] } else { 0 }
}

/// `generation` plus 1, or `u64::MAX` when it is `u64::MAX`.
pub open spec fn increased(generation: u64) -> u64 {
    if generation < u64::MAX { (generation + 1) as u64 } else { generation }
}

/// The dialog of `key` in `dialogs` when `timeout_s` did not pass at `now` since its last question.
pub open spec fn live(dialogs: Map<Key, Dialog>, key: Key, now: Timestamp, timeout_s: u64) -> Option<Dialog> {
    let elapsed = crate::domain::contracts::nanosecond(now) - crate::domain::contracts::nanosecond(dialogs[key].asked);
    if dialogs.contains_key(key) && elapsed < timeout_s * 1_000_000_000 { Some(dialogs[key]) } else { None }
}

/// The words of the normalized `answer`.
pub open spec fn heard(answer: Seq<char>) -> Seq<Seq<char>> {
    select::split(intent::normalized(answer))
}

/// True when each word of the normalized `choice` is in `heard`.
pub open spec fn named(heard: Seq<Seq<char>>, choice: Seq<char>) -> bool {
    select::has_all(heard, select::split(intent::normalized(choice)))
}

/// The length of `text` in UTF-8 bytes.
pub open spec fn size(text: Seq<char>) -> nat {
    vstd::utf8::encode_utf8(text).len()
}

/// True when no choice of the first `count` of `choices` is named in `heard`.
pub open spec fn none_named(heard: Seq<Seq<char>>, choices: Seq<String>, count: int) -> bool {
    forall|other: int| 0 <= other < count ==> !named(heard, #[trigger] choices[other]@)
}

/// True when the choice at `best` is named in `heard`, and no named choice of the first `count`
/// of `choices` is longer.
pub open spec fn longest_of(heard: Seq<Seq<char>>, choices: Seq<String>, count: int, best: int) -> bool {
    &&& 0 <= best < count
    &&& named(heard, choices[best]@)
    &&& forall|other: int| 0 <= other < count && named(heard, #[trigger] choices[other]@)
        ==> size(choices[other]@) <= size(choices[best]@)
}

/// True when `value` is a longest choice of `choices` that is named in `heard`.
pub open spec fn longest_named(heard: Seq<Seq<char>>, choices: Seq<String>, value: Seq<char>) -> bool {
    exists|best: int| #[trigger] longest_of(heard, choices, choices.len() as int, best) && choices[best]@ == value
}

/// `answer` without a leading "the ".
pub open spec fn trimmed(answer: Seq<char>) -> Seq<char> {
    if "the "@.is_prefix_of(answer) { answer.skip("the "@.len() as int) } else { answer }
}

/// The Jaro-Winkler similarity of `text` to the normalized `choice`.
pub open spec fn similarity_to(text: Seq<char>, choice: Seq<char>) -> f64 {
    crate::domain::contracts::jaro_winkler(text, intent::normalized(choice))
}

/// True when no choice of the first `count` of `choices` is more similar to `text` than the choice
/// at `best`.
pub open spec fn most_similar(text: Seq<char>, choices: Seq<String>, count: int, best: int) -> bool {
    &&& 0 <= best < choices.len()
    &&& forall|other: int| 0 <= other < count ==> (crate::domain::contracts::total_order_f64())(
        similarity_to(text, #[trigger] choices[other]@), similarity_to(text, choices[best]@)) != Ordering::Greater
}

/// True when `value` is a choice most similar to `text`, with a similarity of at least `threshold`.
pub open spec fn similar(text: Seq<char>, choices: Seq<String>, value: Seq<char>, threshold: f64) -> bool {
    exists|best: int| #[trigger] most_similar(text, choices, choices.len() as int, best) && choices[best]@ == value
        && vstd::std_specs::cmp::ge_ensures::<f64>(similarity_to(text, choices[best]@), threshold, true)
}

/// True when a choice most similar to `text` has a similarity below `threshold`.
pub open spec fn dissimilar(text: Seq<char>, choices: Seq<String>, threshold: f64) -> bool {
    exists|best: int| #[trigger] most_similar(text, choices, choices.len() as int, best)
        && vstd::std_specs::cmp::ge_ensures::<f64>(similarity_to(text, choices[best]@), threshold, false)
}

} // verus!

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;

    use super::*;

    /// The `dialog.max_turns` of `jarvis.toml` and the `MAX_TURNS` of `dialog.qnt`.
    const MAX_TURNS: u32 = 3;

    /// The `dialog.timeout_s` of `jarvis.toml`.
    const TIMEOUT_S: u64 = 15;

    /// The time `second` seconds after the Unix epoch.
    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("a valid time")
    }

    /// The key of `satellite`.
    fn key(satellite: &str) -> Key {
        Key(satellite.to_owned())
    }

    /// A System Two question of `request`: its response ID is the request.
    fn pending(request: i64) -> Pending {
        Pending::Llm {
            response_id: request.to_string(),
            call_id: "call".to_owned(),
        }
    }

    /// A dialog of `request` with `turns` questions, the last at `asked` seconds.
    fn dialog(request: i64, turns: u32, asked: i64) -> Dialog {
        let (pending, asked) = (pending(request), at(asked));
        Dialog {
            pending,
            turns,
            asked,
        }
    }

    /// AC-10.2-1.
    #[test]
    fn ac_10_2_1_take_returns_and_removes_the_open_dialog() {
        let mut dialogs = Dialogs::default();
        dialogs.open(key("kitchen"), pending(1), at(0));
        let taken = dialogs.take(&key("kitchen"), at(1), TIMEOUT_S);
        assert_eq!(taken, Some(dialog(1, 1, 0)));
        assert_eq!(dialogs.take(&key("kitchen"), at(1), TIMEOUT_S), None);
    }

    /// AC-10.2-2.
    #[test]
    fn ac_10_2_2_take_times_out_after_the_last_question() {
        let mut dialogs = Dialogs::default();
        dialogs.open(key("kitchen"), pending(1), at(0));
        let late = dialogs.clone().take(&key("kitchen"), at(15), TIMEOUT_S);
        assert_eq!(late, None);
        let dialog = dialogs.take(&key("kitchen"), at(5), TIMEOUT_S);
        let next = next_turn(dialog.expect("open"), MAX_TURNS, at(5)).expect("a turn");
        dialogs.dialogs.insert(key("kitchen"), next.clone());
        assert_eq!(dialogs.take(&key("kitchen"), at(15), TIMEOUT_S), Some(next));
    }

    /// AC-10.2-3.
    #[test]
    fn ac_10_2_3_take_leaves_the_other_satellite() {
        let mut dialogs = Dialogs::default();
        dialogs.open(key("kitchen"), pending(1), at(0));
        dialogs.open(key("office"), pending(2), at(0));
        dialogs.take(&key("kitchen"), at(1), TIMEOUT_S);
        let office = BTreeMap::from([(key("office"), dialog(2, 1, 0))]);
        assert_eq!(dialogs.dialogs, office);
    }

    /// AC-10.2-4.
    #[test]
    fn ac_10_2_4_next_turn_at_max_turns_returns_none() {
        assert_eq!(next_turn(dialog(1, MAX_TURNS, 0), MAX_TURNS, at(1)), None);
    }

    /// AC-10.2-6.
    #[test]
    fn ac_10_2_6_close_increases_only_the_generation_of_the_key() {
        let mut dialogs = Dialogs::default();
        dialogs.close(key("office"));
        dialogs.open(key("kitchen"), pending(1), at(0));
        dialogs.close(key("kitchen"));
        let generations = [key("kitchen"), key("office")].map(|key| dialogs.generation(&key));
        assert_eq!(generations, [1, 1]);
        assert!(dialogs.dialogs.is_empty(), "the dialog is open");
    }

    /// The `dialog.choice_similarity` of `jarvis.toml`.
    const CHOICE_SIMILARITY: f64 = 0.85;

    /// The choices "kitchen" and "living room".
    fn rooms() -> Vec<String> {
        ["kitchen", "living room"].map(str::to_owned).to_vec()
    }

    /// AC-10.3-1.
    #[test]
    fn ac_10_3_1_cancel_phrase() {
        let cancel = ["never mind", "cancel", "forget it", "stop"].map(str::to_owned);
        assert!(
            is_cancel("Never mind.", &cancel),
            "\"never mind\" does not cancel"
        );
        assert!(!is_cancel("kitchen", &cancel), "\"kitchen\" cancels");
    }

    /// AC-10.3-2.
    #[test]
    fn ac_10_3_2_the_kitchen_fills_kitchen() {
        let filled = fill("The kitchen.", &rooms(), CHOICE_SIMILARITY);
        assert_eq!(filled.as_deref(), Some("kitchen"));
    }

    /// AC-10.3-3.
    #[test]
    fn ac_10_3_3_similar_answer_fills_kitchen() {
        let score = strsim::jaro_winkler("kitchn", "kitchen");
        assert!(
            score >= CHOICE_SIMILARITY,
            "similarity {score} is below the threshold"
        );
        let filled =
            ["kitchn", "the kitchn"].map(|answer| fill(answer, &rooms(), CHOICE_SIMILARITY));
        assert_eq!(
            filled,
            [Some("kitchen".to_owned()), Some("kitchen".to_owned())]
        );
    }

    /// AC-10.3-4.
    #[test]
    fn ac_10_3_4_dissimilar_answer_fills_nothing() {
        assert_eq!(fill("garage", &rooms(), CHOICE_SIMILARITY), None);
    }

    /// AC-10.3-5.
    #[test]
    fn ac_10_3_5_no_choices_fill_the_normalized_answer() {
        let filled = fill("Twenty Minutes!", &[], CHOICE_SIMILARITY);
        assert_eq!(filled.as_deref(), Some("twenty minutes"));
    }

    /// AC-10.3-6.
    #[test]
    fn ac_10_3_6_choice_inside_the_answer_fills_it() {
        let filled = fill("in the kitchen please", &rooms(), CHOICE_SIMILARITY);
        assert_eq!(filled.as_deref(), Some("kitchen"));
    }

    /// The `TIMEOUT` of `dialog.qnt`, with one model time unit as one second.
    const MODEL_TIMEOUT_S: u64 = 3;

    /// The fields of `dialog.qnt` that the conformance test reads.
    #[derive(Deserialize)]
    struct Model {
        /// The questions of each key. 0 is no open dialog.
        turns: BTreeMap<String, u32>,
        /// The time of the last question of each key.
        asked: BTreeMap<String, i64>,
        /// The request of each key.
        request: BTreeMap<String, i64>,
    }

    /// The open dialog of each key.
    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(from = "Model")]
    struct Open(BTreeMap<String, Dialog>);

    impl From<Model> for Open {
        /// The keys with `turns > 0`.
        fn from(model: Model) -> Self {
            let open = model.turns.into_iter().filter(|&(_, turns)| turns > 0);
            let open = open.map(|(key, turns)| {
                let dialog = dialog(model.request[&key], turns, model.asked[&key]);
                (key, dialog)
            });
            Self(open.collect())
        }
    }

    impl ModelState<DialogDriver> for Open {
        /// The dialogs that `take` gives at the model time.
        fn from_driver(driver: &DialogDriver) -> anyhow::Result<Self> {
            let (mut dialogs, now) = (driver.dialogs.clone(), at(driver.now));
            let open = driver.dialogs.dialogs.keys().filter_map(|key| {
                let dialog = dialogs.take(key, now, MODEL_TIMEOUT_S)?;
                Some((key.0.clone(), dialog))
            });
            Ok(Self(open.collect()))
        }
    }

    /// Replays `dialog.qnt` against `Dialogs`. The response ID of each dialog is its request.
    #[derive(Default)]
    struct DialogDriver {
        /// The dialogs.
        dialogs: Dialogs,
        /// The model time in seconds.
        now: i64,
        /// The number of requests so far, which is the ID of the last request.
        requests: i64,
        /// The key of the deferred job of each request.
        jobs: BTreeMap<i64, Key>,
    }

    impl Driver for DialogDriver {
        type State = Open;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            let now = at(self.now);
            switch!(step {
                init => *self = Self::default(),
                open(k: String) => {
                    self.requests += 1;
                    self.dialogs.open(Key(k), pending(self.requests), now);
                },
                defer(k: String) => {
                    self.requests += 1;
                    self.jobs.insert(self.requests, Key(k.clone()));
                    self.dialogs.close(Key(k));
                },
                ask(r: i64) => self.dialogs.open(self.jobs[&r].clone(), pending(r), now),
                jobAct => {},
                askAgain(k: String) => {
                    let dialog = self.dialogs.take(&Key(k.clone()), now, MODEL_TIMEOUT_S);
                    if let Some(next) = next_turn(dialog.expect("open"), MAX_TURNS, now) {
                        self.dialogs.dialogs.insert(Key(k), next);
                    }
                },
                fill(k: String) => self.dialogs.close(Key(k)),
                cancel(k: String) => self.dialogs.close(Key(k)),
                tick => self.now += 1,
            })
        }
    }

    /// AC-10.2-5. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/dialog.qnt", max_steps = 30, seed = "1")]
    fn ac_10_2_5_conformance() -> impl Driver {
        DialogDriver::default()
    }
}
