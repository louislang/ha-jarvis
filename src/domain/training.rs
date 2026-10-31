//! Context samples and held-out split of the training set.

use rand::Rng;
use rand::rngs::StdRng;
use vstd::prelude::*;
use xxhash_rust::xxh3;

use crate::domain::intent;

/// Segment A of the model input: the area and the habit lines, or `none` for each that is absent.
#[verus_spec(context =>
    ensures
        context@ == context_spec(shown(area), habit_lines@),
)]
#[must_use]
pub fn context_text(area: Option<&str>, habit_lines: &[String]) -> String {
    let mut context = "area: ".to_owned();
    match area {
        Some(area) => context.push_str(area),
        None => context.push_str("none"),
    }
    context.push_str(" | habits: ");
    if habit_lines.is_empty() {
        context.push_str("none");
    } else {
        context.push_str(join(habit_lines).as_str());
    }
    context
}

/// The texts of `lines` joined by `; `.
#[verus_spec(text =>
    ensures
        text@ == joined(lines@),
)]
fn join(lines: &[String]) -> String {
    let mut text = String::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= lines.len(),
            text@ == joined(lines@.take(index as int)),
        decreases lines.len() - index,
    ))]
    while index < lines.len() {
        proof! {
            assert(lines@.take(index + 1).drop_last() =~= lines@.take(index as int));
        }
        if index > 0 {
            text.push_str("; ");
        }
        text.push_str(lines[index].as_str());
        index += 1;
    }
    proof! {
        assert(lines@.take(index as int) =~= lines@);
    }
    text
}

/// The context of an area that `rng` picks from `areas`, with no habit lines.
#[verus_spec(context =>
    ensures
        context@ == sampled(areas@, crate::domain::contracts::next(&*old(rng))),
)]
#[must_use]
pub fn sample_context(areas: &[String], rng: &mut StdRng) -> String {
    let random = rng.next_u32() as usize;
    if areas.is_empty() {
        return context_text(None, &[]);
    }
    let area = &areas[random % areas.len()];
    context_text(Some(area.as_str()), &[])
}

/// True when the normalized `text` is in the held-out split of size `holdout`.
#[verus_verify]
#[must_use]
pub fn held_out(text: &str, holdout: f64) -> bool {
    let normal = intent::normalize(text);
    let bucket = xxh3::xxh3_64(normal.as_bytes()) % 10000;
    #[expect(
        clippy::cast_precision_loss,
        reason = "an f64 holds each value below 10000 exactly"
    )]
    let bucket = bucket as f64;
    bucket < holdout * 10000.0
}

verus! {

/// The text of `area`, or `none`.
pub open spec fn shown(area: Option<&str>) -> Seq<char> {
    match area {
        Some(area) => area@,
        None => "none"@,
    }
}

/// `context_text` of the shown `area` and `habit_lines`.
pub open spec fn context_spec(area: Seq<char>, habit_lines: Seq<String>) -> Seq<char> {
    let habits = if habit_lines.len() == 0 { "none"@ } else { joined(habit_lines) };
    "area: "@ + area + " | habits: "@ + habits
}

/// The texts of `lines` joined by `; `.
pub open spec fn joined(lines: Seq<String>) -> Seq<char>
    decreases lines.len(),
{
    if lines.len() <= 1 {
        if lines.len() == 0 { Seq::empty() } else { lines[0]@ }
    } else {
        joined(lines.drop_last()) + "; "@ + lines.last()@
    }
}

/// The context of the area of `areas` at `random` modulo its length, or of no area.
pub open spec fn sampled(areas: Seq<String>, random: u32) -> Seq<char> {
    let area = if areas.len() == 0 { "none"@ } else { areas[random as int % areas.len() as int]@ };
    context_spec(area, Seq::empty())
}

} // verus!

/// Tests of the context samples and the held-out split.
#[cfg(test)]
mod tests {
    use super::*;

    use rand::SeedableRng;

    /// AC-3.3-1.
    #[test]
    fn ac_3_3_1_context_text_of_area_without_habits() {
        assert_eq!(
            context_text(Some("kitchen"), &[]),
            "area: kitchen | habits: none"
        );
    }

    /// AC-3.3-2.
    #[test]
    fn ac_3_3_2_context_text_of_habits_without_area() {
        let lines = ["a".to_owned(), "b".to_owned()];
        assert_eq!(context_text(None, &lines), "area: none | habits: a; b");
    }

    /// AC-3.3-3.
    #[test]
    fn ac_3_3_3_same_seed_gives_same_context() {
        let areas = ["kitchen".to_owned(), "office".to_owned()];
        let sample = |seed| sample_context(&areas, &mut StdRng::seed_from_u64(seed));
        assert_eq!(sample(7), sample(7));
    }

    /// AC-3.3-8.
    #[test]
    fn ac_3_3_8_held_out_is_stable() {
        let text = "Turn on the kitchen lights";
        assert_eq!(held_out(text, 0.1), held_out(text, 0.1));
    }

    /// AC-3.3-9.
    #[test]
    fn ac_3_3_9_holdout_share_is_near_holdout() {
        let held = (0..1000)
            .filter(|index| held_out(&format!("turn on light {index}"), 0.1))
            .count();
        assert!((70..=130).contains(&held), "{held} of 1000 texts held out");
    }
}
