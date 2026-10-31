//! Softmax and BIO decoding of model outputs.

use vstd::prelude::*;

use crate::domain::intent::Span;

/// A slot value of a transcript.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// The slot name.
    pub name: String,
    /// The text of the value.
    pub value: String,
}

/// An error in the slot tags of a prediction.
#[verus_verify]
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub enum BioError {
    /// `offsets` and `tag_ids` have different lengths.
    Lengths {
        /// The number of offsets.
        offsets: usize,
        /// The number of tag IDs.
        tag_ids: usize,
    },
    /// A tag ID that is not an index of the tags.
    Tag(usize),
    /// A span that is not a character range of the text.
    Span {
        /// The byte offset of the span.
        start: usize,
        /// The byte offset after the span.
        end: usize,
    },
}

/// The probabilities of `logits`: `e^(x - max) / sum` for each logit `x`.
#[verus_spec(values =>
    ensures
        softmaxed(logits@, values@),
)]
#[must_use]
pub fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = maximum(logits);
    let (powers, sum) = exponentials(logits, max);
    let mut values = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= powers@.len(),
            powers@.len() == logits@.len(),
            divided(powers@.take(index as int), sum, values@),
        decreases powers@.len() - index,
    ))]
    while index < powers.len() {
        proof! {
            assert(powers@.take(index + 1).drop_last() =~= powers@.take(index as int));
        }
        values.push(powers[index] / sum);
        index += 1;
    }
    proof! {
        assert(powers@.take(index as int) =~= powers@);
        assert(softmax_of(logits@, max, powers@, sum, values@));
    }
    values
}

/// The largest of `logits`, or 0 for no logits.
#[verus_spec(max =>
    ensures
        peak(logits@, max),
)]
fn maximum(logits: &[f32]) -> f32 {
    let mut max = 0.0;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= logits@.len(),
            peak(logits@.take(index as int), max),
        decreases logits@.len() - index,
    ))]
    while index < logits.len() {
        let logit = logits[index];
        let greater = logit > max;
        proof! {
            assert(logits@.take(index + 1).drop_last() =~= logits@.take(index as int));
            assert(vstd::std_specs::cmp::gt_ensures::<f32>(logits@.take(index + 1).last(), max, greater));
        }
        if index == 0 || greater {
            max = logit;
        }
        index += 1;
    }
    proof! {
        assert(logits@.take(index as int) =~= logits@);
    }
    max
}

/// `e^(x - max)` for each logit `x`, and the sum of these powers.
#[verus_spec(result =>
    ensures
        exponents(logits@, max, result.0@),
        summed(result.0@, result.1),
)]
fn exponentials(logits: &[f32], max: f32) -> (Vec<f32>, f32) {
    let mut powers = Vec::new();
    let mut sum = 0.0;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= logits@.len(),
            exponents(logits@.take(index as int), max, powers@),
            summed(powers@, sum),
        decreases logits@.len() - index,
    ))]
    while index < logits.len() {
        let power = (logits[index] - max).exp();
        let next = sum + power;
        proof! {
            assert(powers@.push(power).drop_last() =~= powers@);
            assert(powers@.push(power).last() == power);
            assert(summed(powers@.push(power), next));
            assert(logits@.take(index + 1).drop_last() =~= logits@.take(index as int));
        }
        sum = next;
        powers.push(power);
        index += 1;
    }
    proof! {
        assert(logits@.take(index as int) =~= logits@);
    }
    (powers, sum)
}

/// The slots of `text` from the tag IDs `tag_ids` of the tokens at the byte offsets `offsets`.
///
/// # Errors
///
/// The lengths differ, a tag ID is not an index of `tags`, or a span is not a character range of
/// `text`.
#[verus_spec(result =>
    ensures
        result matches Ok(slots) ==> decoded(text@, offsets@, tag_ids@, tags@, slots@),
)]
pub fn decode_bio(
    text: &str,
    offsets: &[(usize, usize)],
    tag_ids: &[usize],
    tags: &[String],
) -> Result<Vec<Slot>, BioError> {
    if offsets.len() != tag_ids.len() {
        return Err(BioError::Lengths {
            offsets: offsets.len(),
            tag_ids: tag_ids.len(),
        });
    }
    values(text, &spans(offsets, tag_ids, tags)?)
}

/// The spans of the tokens with the tag IDs `tag_ids` at `offsets`.
#[verus_spec(result =>
    requires
        offsets@.len() == tag_ids@.len(),
    ensures
        result is Ok ==> indexes(tag_ids@, tags@),
        result matches Ok(spans) ==> seen(spans@) == all_spans(token_tags(tag_ids@, tags@), offsets@),
)]
fn spans(
    offsets: &[(usize, usize)],
    tag_ids: &[usize],
    tags: &[String],
) -> Result<Vec<Span>, BioError> {
    let mut spans = Vec::new();
    let mut open = None;
    let mut index = 0;
    proof! {
        assert(seen(spans@) =~= seq![]);
    }
    #[cfg_attr(verus_only, verus_spec(
        invariant
            tagging(offsets@, tag_ids@, tags@, index as int, spans@, open),
        decreases tag_ids@.len() - index,
    ))]
    while index < tag_ids.len() {
        let id = tag_ids[index];
        if id >= tags.len() {
            return Err(BioError::Tag(id));
        }
        open = step(open, &tags[id], offsets[index], &mut spans);
        index += 1;
    }
    if let Some(span) = open {
        spans.push(span);
    }
    Ok(spans)
}

/// The open span after a token with `tag` at `offset`, with the span that the token closes pushed
/// to `spans`.
#[verus_spec(next =>
    ensures
        seen(final(spans)@) == seen(old(spans)@) + closed(seen_open(open), tag@),
        seen_open(next) == opened(seen_open(open), tag@, offset),
)]
fn step(
    open: Option<Span>,
    tag: &str,
    offset: (usize, usize),
    spans: &mut Vec<Span>,
) -> Option<Span> {
    let parsed = split(tag);
    if let Some(span) = open {
        match parsed {
            Some((prefix, slot)) if prefix == "I-" && span.slot.as_str() == slot => {
                return Some(Span {
                    slot: span.slot,
                    start: span.start,
                    end: offset.1,
                });
            }
            _ => spans.push(span),
        }
    }
    let (_, slot) = parsed?;
    Some(Span {
        slot: slot.to_owned(),
        start: offset.0,
        end: offset.1,
    })
}

/// The prefix `B-` or `I-` of `tag` and the slot name after it.
#[verus_spec(parts =>
    ensures
        match parts {
            Some((prefix, slot)) => parse(tag@) == Some((prefix@, slot@)),
            None => parse(tag@) is None,
        },
)]
fn split(tag: &str) -> Option<(&str, &str)> {
    match (tag.get(..2), tag.get(2..)) {
        (Some(prefix), Some(slot)) if prefix == "B-" || prefix == "I-" => Some((prefix, slot)),
        _ => None,
    }
}

/// The slot and the text of each of `spans` in `text`.
#[verus_spec(result =>
    ensures
        result matches Ok(slots) ==> valued(text@, seen(spans@), slots@),
)]
fn values(text: &str, spans: &[Span]) -> Result<Vec<Slot>, BioError> {
    let mut slots = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= spans@.len(),
            valued(text@, seen(spans@).take(index as int), slots@),
        decreases spans@.len() - index,
    ))]
    while index < spans.len() {
        let span = &spans[index];
        let Some(value) = text.get(span.start..span.end) else {
            return Err(BioError::Span {
                start: span.start,
                end: span.end,
            });
        };
        proof! {
            assert(seen(spans@).take(index + 1) =~= seen(spans@).take(index as int).push(seen(spans@)[index as int]));
        }
        slots.push(Slot {
            name: span.slot.clone(),
            value: value.to_owned(),
        });
        index += 1;
    }
    proof! {
        assert(seen(spans@).take(index as int) =~= seen(spans@));
    }
    Ok(slots)
}

verus! {

broadcast use {
    vstd::utf8::encode_utf8_valid_utf8,
    vstd::utf8::encode_utf8_decode_utf8,
    vstd::utf8::is_char_boundary_start_end_of_seq,
};

/// The slot name, the start byte and the end byte of a span.
pub type SpanView = (Seq<char>, usize, usize);

/// The slot name, the start byte and the end byte of each of `spans`.
pub open spec fn seen(spans: Seq<Span>) -> Seq<SpanView> {
    spans.map_values(|span: Span| (span.slot@, span.start, span.end))
}

/// The view of the open span `open`.
pub open spec fn seen_open(open: Option<Span>) -> Option<SpanView> {
    match open {
        Some(span) => Some((span.slot@, span.start, span.end)),
        None => None,
    }
}

/// The prefix and the slot name of `tag`: its first two bytes and the rest, when the first two
/// bytes are a character range with the text `B-` or `I-`.
pub open spec fn parse(tag: Seq<char>) -> Option<(Seq<char>, Seq<char>)> {
    let bytes = vstd::utf8::encode_utf8(tag);
    let prefix = vstd::utf8::decode_utf8(bytes.take(2));
    if 2 <= bytes.len() && vstd::utf8::is_char_boundary(bytes, 2) && (prefix == "B-"@ || prefix == "I-"@) {
        Some((prefix, vstd::utf8::decode_utf8(bytes.skip(2))))
    } else {
        None
    }
}

/// True when a token with `tag` continues the open span `open`: the tag is `I-x` and `open` is a
/// span of `x`.
pub open spec fn continues(open: Option<SpanView>, tag: Seq<char>) -> bool {
    &&& open is Some
    &&& parse(tag) is Some
    &&& parse(tag).unwrap().0 == "I-"@
    &&& parse(tag).unwrap().1 == open.unwrap().0
}

/// The spans that a token with `tag` closes: the open span, unless the token continues it.
pub open spec fn closed(open: Option<SpanView>, tag: Seq<char>) -> Seq<SpanView> {
    if open is Some && !continues(open, tag) {
        seq![open.unwrap()]
    } else {
        seq![]
    }
}

/// The open span after a token with `tag` at `offset`: `open` up to the token when the token
/// continues it, a new span for another `B-x` or `I-x`, and none for any other tag.
pub open spec fn opened(open: Option<SpanView>, tag: Seq<char>, offset: (usize, usize)) -> Option<SpanView> {
    if continues(open, tag) {
        Some((open.unwrap().0, open.unwrap().1, offset.1))
    } else if parse(tag) is Some {
        Some((parse(tag).unwrap().1, offset.0, offset.1))
    } else {
        None
    }
}

/// The closed spans and the open span after the first `count` tokens with `tags` at `offsets`.
pub open spec fn run(tags: Seq<Seq<char>>, offsets: Seq<(usize, usize)>, count: int) -> (Seq<SpanView>, Option<SpanView>)
    decreases count,
{
    if count <= 0 {
        (seq![], None)
    } else {
        let (spans, open) = run(tags, offsets, count - 1);
        (spans + closed(open, tags[count - 1]), opened(open, tags[count - 1], offsets[count - 1]))
    }
}

/// The spans after all tokens with `tags` at `offsets`: the closed spans, then the open span.
pub open spec fn all_spans(tags: Seq<Seq<char>>, offsets: Seq<(usize, usize)>) -> Seq<SpanView> {
    let (spans, open) = run(tags, offsets, tags.len() as int);
    match open {
        Some(span) => spans.push(span),
        None => spans,
    }
}

/// True when each of `tag_ids` is an index of `tags`.
pub open spec fn indexes(tag_ids: Seq<usize>, tags: Seq<String>) -> bool {
    forall|index: int| 0 <= index < tag_ids.len() ==> #[trigger] tag_ids[index] < tags.len()
}

/// The tag of each token: the tag at its tag ID.
pub open spec fn token_tags(tag_ids: Seq<usize>, tags: Seq<String>) -> Seq<Seq<char>> {
    tag_ids.map_values(|id: usize| tags[id as int]@)
}

/// The loop invariant of `spans`: the first `index` tag IDs are indexes of `tags`, and
/// `spans` and `open` are the state after their tokens.
spec fn tagging(offsets: Seq<(usize, usize)>, tag_ids: Seq<usize>, tags: Seq<String>, index: int, spans: Seq<Span>, open: Option<Span>) -> bool {
    &&& offsets.len() == tag_ids.len()
    &&& 0 <= index <= tag_ids.len()
    &&& forall|done: int| 0 <= done < index ==> #[trigger] tag_ids[done] < tags.len()
    &&& run(token_tags(tag_ids, tags), offsets, index) == (seen(spans), seen_open(open))
}

/// True when `start..end` is a character range of `text`.
pub open spec fn char_range(text: Seq<char>, start: usize, end: usize) -> bool {
    let bytes = vstd::utf8::encode_utf8(text);
    &&& start <= end <= bytes.len()
    &&& vstd::utf8::is_char_boundary(bytes, start as int)
    &&& vstd::utf8::is_char_boundary(bytes, end as int)
}

/// True when each of `slots` has the slot name of its span of `spans`, and the span is a
/// character range of `text` with the bytes of the value.
pub open spec fn valued(text: Seq<char>, spans: Seq<SpanView>, slots: Seq<Slot>) -> bool {
    &&& slots.len() == spans.len()
    &&& forall|index: int| #![trigger slots[index]] 0 <= index < spans.len() ==> {
        &&& slots[index].name@ == spans[index].0
        &&& char_range(text, spans[index].1, spans[index].2)
        &&& vstd::utf8::encode_utf8(slots[index].value@)
            == vstd::utf8::encode_utf8(text).subrange(spans[index].1 as int, spans[index].2 as int)
    }
}

/// True when `slots` is the BIO decode of `text`: the lengths are equal, each tag ID is an index
/// of `tags`, and `slots` holds the spans of the tokens.
pub open spec fn decoded(text: Seq<char>, offsets: Seq<(usize, usize)>, tag_ids: Seq<usize>, tags: Seq<String>, slots: Seq<Slot>) -> bool {
    &&& offsets.len() == tag_ids.len()
    &&& indexes(tag_ids, tags)
    &&& valued(text, all_spans(token_tags(tag_ids, tags), offsets), slots)
}

/// True when `max` is the result of the comparisons of `maximum` over `logits`.
pub open spec fn peak(logits: Seq<f32>, max: f32) -> bool
    decreases logits.len(),
{
    if logits.len() <= 1 {
        max == if logits.len() == 0 { 0.0f32 } else { logits[0] }
    } else {
        exists|before: f32, greater: bool| #[trigger] vstd::std_specs::cmp::gt_ensures::<f32>(logits.last(), before, greater)
            && peak(logits.drop_last(), before)
            && max == if greater { logits.last() } else { before }
    }
}

/// True when `power` is `e^(logit - max)`.
pub open spec fn exponent(logit: f32, max: f32, power: f32) -> bool {
    exists|difference: f32| #[trigger] vstd::std_specs::ops::sub_ensures::<f32>(logit, max, difference)
        && crate::domain::contracts::exp_ensures(difference, power)
}

/// True when each of `powers` is `e^(logit - max)` of its logit.
pub open spec fn exponents(logits: Seq<f32>, max: f32, powers: Seq<f32>) -> bool {
    &&& powers.len() == logits.len()
    &&& forall|index: int| 0 <= index < logits.len() ==> exponent(logits[index], max, #[trigger] powers[index])
}

/// True when `sum` is the sum of `powers` from the left.
pub open spec fn summed(powers: Seq<f32>, sum: f32) -> bool
    decreases powers.len(),
{
    if powers.len() == 0 {
        sum == 0.0f32
    } else {
        exists|before: f32| #[trigger] vstd::std_specs::ops::add_ensures::<f32>(before, powers.last(), sum)
            && summed(powers.drop_last(), before)
    }
}

/// True when each of `values` is its power divided by `sum`.
pub open spec fn divided(powers: Seq<f32>, sum: f32, values: Seq<f32>) -> bool {
    &&& values.len() == powers.len()
    &&& forall|index: int| 0 <= index < powers.len() ==> vstd::std_specs::ops::div_ensures::<f32>(powers[index], sum, #[trigger] values[index])
}

/// True when `values` is the softmax of `logits` with the maximum `max`, the powers `powers` and
/// their sum `sum`.
pub open spec fn softmax_of(logits: Seq<f32>, max: f32, powers: Seq<f32>, sum: f32, values: Seq<f32>) -> bool {
    &&& peak(logits, max)
    &&& exponents(logits, max, powers)
    &&& summed(powers, sum)
    &&& divided(powers, sum, values)
}

/// True when `values` is the softmax of `logits`.
pub open spec fn softmaxed(logits: Seq<f32>, values: Seq<f32>) -> bool {
    exists|max: f32, powers: Seq<f32>, sum: f32| #[trigger] softmax_of(logits, max, powers, sum, values)
}

} // verus!

/// Tests of softmax.
#[cfg(test)]
mod tests {
    use super::*;

    /// AC-3.11-1.
    #[test]
    fn ac_3_11_1_softmax_values_are_probabilities() {
        let values = softmax(&[2.0, -1.0, 0.5, 0.5]);
        let sum: f32 = values.iter().sum();
        assert!(
            values.iter().all(|value| (0.0..=1.0).contains(value)),
            "{values:?}"
        );
        assert!((sum - 1.0).abs() <= 0.0001, "sum {sum}");
    }

    /// AC-3.11-2.
    #[test]
    fn ac_3_11_2_softmax_of_large_logits_is_finite() {
        let values = softmax(&[1000.0, 0.0]);
        assert!(
            values.len() == 2 && values.iter().all(|value| value.is_finite()),
            "{values:?}"
        );
    }

    /// The tags of the slot `area`.
    fn tags() -> Vec<String> {
        vec!["O".to_owned(), "B-area".to_owned(), "I-area".to_owned()]
    }

    /// A slot of `area` with `value`.
    fn area(value: &str) -> Slot {
        Slot {
            name: "area".to_owned(),
            value: value.to_owned(),
        }
    }

    /// AC-3.11b-1.
    #[test]
    fn ac_3_11b_1_decode_bio_joins_a_span() {
        let slots = decode_bio(
            "living room lights",
            &[(0, 6), (7, 11), (12, 18)],
            &[1, 2, 0],
            &tags(),
        );
        assert_eq!(slots, Ok(vec![area("living room")]));
    }

    /// AC-3.11b-2.
    #[test]
    fn ac_3_11b_2_inside_after_outside_starts_a_span() {
        let offsets = [(0, 7), (8, 11), (12, 18), (19, 23)];
        let slots = decode_bio("kitchen and living room", &offsets, &[2, 0, 2, 2], &tags());
        assert_eq!(slots, Ok(vec![area("kitchen"), area("living room")]));
    }

    /// AC-3.11b-3.
    #[test]
    fn ac_3_11b_3_decode_bio_rejects_bad_input() {
        let tags = tags();
        assert_eq!(
            decode_bio("kitchen", &[(0, 7)], &[1, 2], &tags),
            Err(BioError::Lengths {
                offsets: 1,
                tag_ids: 2
            })
        );
        assert_eq!(
            decode_bio("kitchen", &[(0, 7)], &[3], &tags),
            Err(BioError::Tag(3))
        );
        assert_eq!(
            decode_bio("café", &[(0, 4)], &[1], &tags),
            Err(BioError::Span { start: 0, end: 4 })
        );
    }
}
