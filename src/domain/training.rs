//! Training set, context samples, held-out split, accuracy measures and change count.

use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use vstd::prelude::*;
use xxhash_rust::xxh3;

use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::catalog::Intent;
use crate::domain::feedback::Interaction;
use crate::domain::feedback::Route;
use crate::domain::feedback::Tagged;
use crate::domain::feedback::Value;
use crate::domain::intent;
use crate::domain::intent::Span;
use crate::domain::learn::LearnedIntent;
use crate::domain::model::Prediction;
use crate::domain::model::Slot;

/// The training set file: the contract between Jarvis and `train/train.py`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingSet {
    /// The model to fine-tune.
    pub base_model: String,
    /// The number of passes over the examples.
    pub epochs: usize,
    /// The number of examples in one step.
    pub batch_size: usize,
    /// The intent labels in output order.
    pub labels: Vec<String>,
    /// The slot tags in output order.
    pub tags: Vec<String>,
    /// The labeled examples.
    pub examples: Vec<Example>,
}

/// The training settings that a training set carries.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The model to fine-tune.
    pub base_model: String,
    /// The number of passes over the examples.
    pub epochs: usize,
    /// The number of examples in one step.
    pub batch_size: usize,
}

/// One labeled transcript with its model context.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(from = "ExampleFile", into = "ExampleFile")]
pub struct Example {
    /// Segment A of the model input.
    pub context: String,
    /// The normalized transcript.
    pub text: String,
    /// The intent of the example, or the intent that it is not.
    pub label: Label,
    /// The slot values in `text`, or `None` when the slots are not known.
    pub spans: Option<Vec<Span>>,
}

/// An example in the training set file, with its spans in character offsets.
#[derive(Deserialize, Serialize)]
pub struct ExampleFile {
    /// Segment A of the model input.
    pub context: String,
    /// The normalized transcript.
    pub text: String,
    /// The label, as the key `intent` or `not`.
    #[serde(flatten)]
    pub label: Label,
    /// The slot values in `text`, or `None` when the slots are not known.
    pub spans: Option<Vec<SpanFile>>,
}

/// One slot value in the text of an example in the training set file.
#[derive(Deserialize, Serialize)]
pub struct SpanFile {
    /// The slot name.
    pub slot: String,
    /// The character offset of the value.
    pub start: usize,
    /// The character offset after the value.
    pub end: usize,
}

/// The label of an example.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Label {
    /// The example has this label.
    Intent(String),
    /// The example has any label except this one.
    Not(String),
}

#[verus_verify]
impl From<Example> for ExampleFile {
    /// The example with its spans in character offsets.
    fn from(example: Example) -> Self {
        let spans = example
            .spans
            .as_ref()
            .map(|spans| to_chars(&example.text, spans));
        Self {
            context: example.context,
            text: example.text,
            label: example.label,
            spans,
        }
    }
}

#[verus_verify]
impl From<ExampleFile> for Example {
    /// The example with its spans in byte offsets.
    fn from(file: ExampleFile) -> Self {
        let spans = file.spans.as_ref().map(|spans| to_bytes(&file.text, spans));
        Self {
            context: file.context,
            text: file.text,
            label: file.label,
            spans,
        }
    }
}

/// The byte spans `spans` of `text` in character offsets.
#[verus_spec(files =>
    ensures
        files@.len() == spans@.len(),
        forall|index: int| #![trigger files@[index]] 0 <= index < spans@.len()
            ==> in_chars(text@, spans@[index], files@[index]),
)]
fn to_chars(text: &str, spans: &[Span]) -> Vec<SpanFile> {
    let mut files = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            prefix_in_chars(text@, spans@, files@, index as int),
        decreases spans@.len() - index,
    ))]
    while index < spans.len() {
        let span = &spans[index];
        files.push(SpanFile {
            slot: span.slot.clone(),
            start: char_offset(text, span.start),
            end: char_offset(text, span.end),
        });
        index += 1;
    }
    files
}

/// The character spans `files` of `text` in byte offsets.
#[verus_spec(spans =>
    ensures
        spans@.len() == files@.len(),
        forall|index: int| #![trigger spans@[index]] 0 <= index < files@.len()
            ==> in_bytes(text@, files@[index], spans@[index]),
)]
fn to_bytes(text: &str, files: &[SpanFile]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            prefix_in_bytes(text@, files@, spans@, index as int),
        decreases files@.len() - index,
    ))]
    while index < files.len() {
        let file = &files[index];
        spans.push(Span {
            slot: file.slot.clone(),
            start: byte_offset(text, file.start),
            end: byte_offset(text, file.end),
        });
        index += 1;
    }
    spans
}

/// The number of characters of `text` before the byte offset `byte`.
#[verus_spec(count =>
    ensures
        counts(text@, byte as int, count as int),
)]
fn char_offset(text: &str, byte: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut offset = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            walked(text@, chars@, bytes@, count as int, offset as int),
            count > 0 ==> bytes_before(text@, count - 1) < byte,
        decreases chars@.len() - count,
    ))]
    while offset < byte && offset < bytes.len() {
        proof! {
            lemma_next(text@, count as int);
        }
        offset += chars[count].len_utf8();
        count += 1;
    }
    proof! {
        lemma_next(text@, count as int);
        lemma_counted(text@, byte as int, count as int);
    }
    count
}

/// The number of bytes of the first `count` characters of `text`, or of all of `text`.
#[verus_spec(byte =>
    ensures
        count <= text@.len() ==> byte == bytes_before(text@, count as int),
)]
fn byte_offset(text: &str, count: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut byte = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            walked(text@, chars@, bytes@, index as int, byte as int),
            index <= count,
        decreases chars@.len() - index,
    ))]
    while index < count && byte < bytes.len() {
        proof! {
            lemma_next(text@, index as int);
        }
        byte += chars[index].len_utf8();
        index += 1;
    }
    proof! {
        lemma_next(text@, index as int);
    }
    byte
}

/// Segment A of the model input: the area, or `none` when it is absent.
#[verus_spec(context =>
    ensures
        context@ == context_spec(shown(area)),
)]
#[must_use]
pub fn context_text(area: Option<&str>) -> String {
    let mut context = "area: ".to_owned();
    match area {
        Some(area) => context.push_str(area),
        None => context.push_str("none"),
    }
    context
}

/// The context of the area of `areas` that the hash of the example text `text` picks.
#[verus_spec(context =>
    ensures
        context@ == sampled(areas@, crate::domain::contracts::xxh3(vstd::utf8::encode_utf8(text@))),
)]
#[must_use]
pub fn sample_context(areas: &[String], text: &str) -> String {
    let random = xxh3::xxh3_64(text.as_bytes());
    if areas.is_empty() {
        return context_text(None);
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the remainder is less than the number of areas"
    )]
    let index = (random % areas.len() as u64) as usize;
    context_text(Some(areas[index].as_str()))
}

/// The training set of the seed examples of `catalog`, each with a context from `areas` that its
/// text picks.
///
/// # Panics
///
/// An example of `catalog` has a markup error, which `validate_catalog` rejects.
#[verus_spec(set =>
    requires
        catalog::valid_catalog(*catalog),
    ensures
        set.examples@.len() == seeds(catalog.intents@) + catalog.unknown_examples@.len(),
        catalog::views(set.labels@) == seq!["unknown"@] + catalog::names(catalog.intents@),
)]
#[must_use]
pub fn seed_set(catalog: &Catalog, areas: &[String], settings: Settings) -> TrainingSet {
    let mut examples = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= catalog.intents@.len(),
            catalog::valid_catalog(*catalog),
            examples@.len() == seeds(catalog.intents@.take(index as int)),
        decreases catalog.intents@.len() - index,
    ))]
    while index < catalog.intents.len() {
        proof! {
            assert(catalog::valid_intent(*catalog, index as int));
            assert(catalog.intents@.take(index + 1).drop_last() =~= catalog.intents@.take(index as int));
        }
        push_intent(&catalog.intents[index], areas, &mut examples);
        index += 1;
    }
    proof! {
        assert(catalog.intents@.take(index as int) =~= catalog.intents@);
    }
    push_unknown(&catalog.unknown_examples, areas, &mut examples);
    TrainingSet {
        base_model: settings.base_model,
        epochs: settings.epochs,
        batch_size: settings.batch_size,
        labels: catalog::labels(catalog),
        tags: catalog::tags(catalog),
        examples,
    }
}

/// Pushes one example of `intent` for each of its seed examples to `examples`.
#[verus_spec(
    requires
        catalog::parsed(intent.examples@, intent.examples@.len() as int),
    ensures
        final(examples)@.len() == old(examples)@.len() + intent.examples@.len(),
)]
fn push_intent(intent: &Intent, areas: &[String], examples: &mut Vec<Example>) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= intent.examples@.len(),
            catalog::parsed(intent.examples@, intent.examples@.len() as int),
            examples@.len() == old(examples)@.len() + index,
        decreases intent.examples@.len() - index,
    ))]
    while index < intent.examples.len() {
        let example =
            intent::parse_example(intent.examples[index].as_str()).expect("the catalog is valid");
        examples.push(Example {
            context: sample_context(areas, &example.text),
            text: example.text,
            label: Label::Intent(intent.name.clone()),
            spans: Some(example.spans),
        });
        index += 1;
    }
}

/// Pushes one `unknown` example with an empty span list for each of `texts` to `examples`.
#[verus_spec(
    ensures
        final(examples)@.len() == old(examples)@.len() + texts@.len(),
)]
fn push_unknown(texts: &[String], areas: &[String], examples: &mut Vec<Example>) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= texts@.len(),
            examples@.len() == old(examples)@.len() + index,
        decreases texts@.len() - index,
    ))]
    while index < texts.len() {
        examples.push(Example {
            context: sample_context(areas, &texts[index]),
            text: texts[index].clone(),
            label: Label::Intent("unknown".to_owned()),
            spans: Some(Vec::new()),
        });
        index += 1;
    }
}

/// An example with its precedence in the training set.
#[verus_verify]
struct Ranked {
    /// The example.
    example: Example,
    /// 0 for a generated example, 1 for a seed and 2 for a mark.
    rank: u8,
    /// The mark time in nanoseconds, or 0 for another example.
    time: i128,
}

/// The labeled examples of the action marks of `marked`, which also holds the first interaction
/// of each marked dialog answer.
#[verus_spec(examples =>
    ensures
        forall|index: int| 0 <= index < examples@.len()
            ==> #[trigger] from_mark(examples@[index], marked@),
)]
#[must_use]
pub fn mark_examples(marked: &[Interaction]) -> Vec<Example> {
    let mut examples = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= marked@.len(),
            forall|done: int| 0 <= done < examples@.len()
                ==> #[trigger] from_mark(examples@[done], marked@),
        decreases marked@.len() - index,
    ))]
    while index < marked.len() {
        if let Some(example) = mark_example(&marked[index], marked) {
            examples.push(example);
        }
        index += 1;
    }
    examples
}

/// The example of the action mark of `interaction`, or `None` without a mark or without the
/// first interaction of its dialog in `marked`.
#[verus_spec(example =>
    ensures
        interaction.marks@.len() == 0 ==> example is None,
        example matches Some(example) ==> interaction.marks@.len() > 0
            && labeled(example.label, *interaction),
        example matches Some(example) ==> (interaction.link matches Some(link)
            ==> example.spans is None && started(example, marked@, link)),
)]
fn mark_example(interaction: &Interaction, marked: &[Interaction]) -> Option<Example> {
    let mark = interaction.marks.first()?;
    let (request, spans) = match interaction.link {
        Some(link) => (&marked[position(marked, link)?], None),
        None => (interaction, Some(spans(&interaction.slots))),
    };
    let selected = interaction.action.intent.clone();
    let (label, spans) = match (mark.value?, &mark.correction, interaction.decision) {
        (Value::Good, _, Route::Llm) => (Label::Intent("unknown".to_owned()), None),
        (Value::Good, _, _) => (Label::Intent(selected), spans),
        (Value::Bad, Some(correction), _) => (Label::Intent(correction.clone()), None),
        (Value::Bad, None, _) => (Label::Not(selected), None),
    };
    Some(Example {
        context: request.model_context.clone(),
        text: intent::normalize(&request.transcript),
        label,
        spans,
    })
}

/// The position of the interaction `id` in `marked`.
#[verus_spec(found =>
    ensures
        found matches Some(index) ==> index < marked@.len() && marked@[index as int].id == id,
)]
fn position(marked: &[Interaction], id: u64) -> Option<usize> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= marked@.len(),
        decreases marked@.len() - index,
    ))]
    while index < marked.len() {
        if marked[index].id == id {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The logged slot values `slots` as spans.
#[verus_verify]
fn spans(slots: &[Tagged]) -> Vec<Span> {
    let spans = slots.iter().map(|slot| Span {
        slot: slot.name.clone(),
        start: slot.start,
        end: slot.end,
    });
    spans.collect()
}

/// The full training set: the seeds of `catalog`, `generated` and the mark examples of `marked`.
/// Of the examples with the same text and different labels, only the newest mark, else a seed,
/// else a generated example stays.
///
/// # Panics
///
/// An example of `catalog` has a markup error, which `validate_catalog` rejects.
#[verus_spec(
    requires
        catalog::valid_catalog(*catalog),
)]
#[must_use]
pub fn training_set(
    catalog: &Catalog,
    generated: Vec<Example>,
    marked: &[Interaction],
    areas: &[String],
    settings: Settings,
) -> TrainingSet {
    let seeds = seed_set(catalog, areas, settings);
    let mut all = Vec::new();
    push_ranked(generated, 0, &mut all);
    push_ranked(seeds.examples, 1, &mut all);
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= marked@.len(),
        decreases marked@.len() - index,
    ))]
    while index < marked.len() {
        let interaction = &marked[index];
        if let (Some(example), Some(mark)) =
            (mark_example(interaction, marked), interaction.marks.first())
        {
            let time = mark.time.as_nanosecond();
            all.push(Ranked {
                example,
                rank: 2,
                time,
            });
        }
        index += 1;
    }
    TrainingSet {
        examples: kept(all),
        ..seeds
    }
}

/// Pushes each of `examples` with the rank `rank` and no mark time to `ranked`.
#[verus_verify]
fn push_ranked(examples: Vec<Example>, rank: u8, ranked: &mut Vec<Ranked>) {
    for example in examples {
        ranked.push(Ranked {
            example,
            rank,
            time: 0,
        });
    }
}

/// The examples of `ranked` that no example with the same text and another label outranks.
#[verus_spec(examples =>
    ensures
        examples@ == chosen(ranked@, ranked@.len() as int),
)]
#[expect(
    clippy::explicit_counter_loop,
    reason = "Verus has no specification of `enumerate`"
)]
fn kept(ranked: Vec<Ranked>) -> Vec<Example> {
    let winners = winners(&ranked);
    let mut examples = Vec::new();
    let mut index = 0;
    let all = ranked.into_iter();
    proof_decl! {
        let ghost seen = vstd::std_specs::iter::IteratorSpec::remaining(&all);
    }
    #[cfg_attr(verus_only, verus_spec(ranked_iter =>
        invariant
            ranked_iter.seq() == seen,
            seen == ranked@,
            index == ranked_iter.index(),
            index <= seen.len() == winners@.len() <= usize::MAX,
            forall|done: int| 0 <= done < seen.len() ==> #[trigger] winners@[done] == wins(seen, done),
            examples@ == chosen(seen, index as int),
    ))]
    for candidate in all {
        if winners[index] {
            examples.push(candidate.example);
        }
        index += 1;
    }
    examples
}

/// For each example of `ranked`, true when no example outranks it.
#[verus_spec(winners =>
    ensures
        winners@.len() == ranked@.len() <= usize::MAX,
        forall|index: int| 0 <= index < ranked@.len()
            ==> #[trigger] winners@[index] == wins(ranked@, index),
)]
fn winners(ranked: &[Ranked]) -> Vec<bool> {
    let mut winners = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= ranked@.len(),
            winners@.len() == index,
            forall|done: int| 0 <= done < index ==> #[trigger] winners@[done] == wins(ranked@, done),
        decreases ranked@.len() - index,
    ))]
    while index < ranked.len() {
        winners.push(wins_at(ranked, index));
        index += 1;
    }
    winners
}

/// True when no example of `ranked` outranks `ranked[index]`.
#[verus_spec(won =>
    requires
        index < ranked@.len(),
    ensures
        won == wins(ranked@, index as int),
)]
fn wins_at(ranked: &[Ranked], index: usize) -> bool {
    let mut other = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            other <= ranked@.len(),
            index < ranked@.len(),
            forall|done: int| 0 <= done < other ==> !#[trigger] outranks(ranked@, done, index as int),
        decreases ranked@.len() - other,
    ))]
    while other < ranked.len() {
        if outranks_at(ranked, other, index) {
            return false;
        }
        other += 1;
    }
    true
}

/// True when `ranked[other]` outranks `ranked[index]`.
#[verus_spec(result =>
    requires
        other < ranked@.len(),
        index < ranked@.len(),
    ensures
        result == outranks(ranked@, other as int, index as int),
)]
fn outranks_at(ranked: &[Ranked], other: usize, index: usize) -> bool {
    let (high, low) = (&ranked[other], &ranked[index]);
    let higher = high.rank > low.rank
        || (high.rank == low.rank
            && (high.time > low.time || (high.time == low.time && other > index)));
    high.example.text.as_str() == low.example.text.as_str()
        && !same_label(&high.example.label, &low.example.label)
        && higher
}

/// True when `a` and `b` are the same label.
#[verus_spec(same =>
    ensures
        same == (label_view(*a) == label_view(*b)),
)]
fn same_label(a: &Label, b: &Label) -> bool {
    match (a, b) {
        (Label::Intent(a), Label::Intent(b)) | (Label::Not(a), Label::Not(b)) => {
            a.as_str() == b.as_str()
        }
        _ => false,
    }
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

/// The accuracy of a model on the held-out examples.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// The share of examples with a correct top label.
    pub intent: f64,
    /// The share of examples with spans whose decoded slots equal the spans.
    pub slots: f64,
}

/// The accuracy of `predictions` on `examples`, the prediction at each position for the example
/// at the same position. An example without a prediction is wrong.
#[verus_verify]
#[must_use]
pub fn metrics(predictions: &[Prediction], examples: &[Example], threshold: f32) -> Metrics {
    let (mut intent, mut slots, mut spanned) = (0, 0, 0);
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            intent <= index,
            slots <= spanned <= index,
            index <= examples@.len(),
        decreases examples@.len() - index,
    ))]
    while index < examples.len() {
        let (found, example) = (index < predictions.len(), &examples[index]);
        if found && correct_label(&predictions[index], &example.label, threshold) {
            intent += 1;
        }
        if let Some(spans) = &example.spans {
            spanned += 1;
            if found && same_slots(&predictions[index].slots, &example.text, spans) {
                slots += 1;
            }
        }
        index += 1;
    }
    Metrics {
        intent: share(intent, examples.len()),
        slots: share(slots, spanned),
    }
}

/// `part` divided by `whole`, or 0 when `whole` is 0.
#[verus_verify]
fn share(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "an f64 holds each example count below 2^53 exactly"
    )]
    let (part, whole) = (part as f64, whole as f64);
    part / whole
}

/// True when the top label of `prediction` is correct for `label` at `threshold`.
#[verus_spec(correct =>
    ensures
        judged(*prediction, *label, threshold, correct),
)]
fn correct_label(prediction: &Prediction, label: &Label, threshold: f32) -> bool {
    let Some((top, probability)) = prediction.ranked.first() else {
        return false;
    };
    let confident = *probability >= threshold;
    match label {
        Label::Intent(name) => {
            (confident && top.as_str() == name.as_str())
                || (!confident && name.as_str() == "unknown")
        }
        Label::Not(name) => !confident || top.as_str() != name.as_str(),
    }
}

/// True when `slots` and the values of `spans` in `text` are the same set.
#[verus_spec(same =>
    ensures
        same == same_set(slots@, text@, spans@),
)]
fn same_slots(slots: &[Slot], text: &str, spans: &[Span]) -> bool {
    all_in_spans(slots, text, spans) && all_in_slots(slots, text, spans)
}

/// True when each of `slots` is the value of a span of `spans` in `text`.
#[verus_spec(all =>
    ensures
        all == forall|index: int| 0 <= index < slots@.len() ==> in_spans(text@, spans@, #[trigger] slots@[index]),
)]
fn all_in_spans(slots: &[Slot], text: &str, spans: &[Span]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= slots@.len(),
            forall|done: int| 0 <= done < index ==> in_spans(text@, spans@, #[trigger] slots@[done]),
        decreases slots@.len() - index,
    ))]
    while index < slots.len() {
        if !has_span(&slots[index], text, spans) {
            return false;
        }
        index += 1;
    }
    true
}

/// True when the value of each of `spans` in `text` is a slot of `slots`.
#[verus_spec(all =>
    ensures
        all == forall|index: int| 0 <= index < spans@.len() ==> in_slots(text@, slots@, #[trigger] spans@[index]),
)]
fn all_in_slots(slots: &[Slot], text: &str, spans: &[Span]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= spans@.len(),
            forall|done: int| 0 <= done < index ==> in_slots(text@, slots@, #[trigger] spans@[done]),
        decreases spans@.len() - index,
    ))]
    while index < spans.len() {
        if !has_slot(slots, text, &spans[index]) {
            return false;
        }
        index += 1;
    }
    true
}

/// True when `slot` is the value of a span of `spans` in `text`.
#[verus_spec(found =>
    ensures
        found == in_spans(text@, spans@, *slot),
)]
fn has_span(slot: &Slot, text: &str, spans: &[Span]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= spans@.len(),
            forall|done: int| 0 <= done < index ==> !#[trigger] slot_of(text@, spans@[done], *slot),
        decreases spans@.len() - index,
    ))]
    while index < spans.len() {
        if is_value(slot, text, &spans[index]) {
            return true;
        }
        index += 1;
    }
    false
}

/// True when the value of `span` in `text` is a slot of `slots`.
#[verus_spec(found =>
    ensures
        found == in_slots(text@, slots@, *span),
)]
fn has_slot(slots: &[Slot], text: &str, span: &Span) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= slots@.len(),
            forall|done: int| 0 <= done < index ==> !#[trigger] slot_of(text@, *span, slots@[done]),
        decreases slots@.len() - index,
    ))]
    while index < slots.len() {
        if is_value(&slots[index], text, span) {
            return true;
        }
        index += 1;
    }
    false
}

/// True when `slot` has the slot name of `span` and the text of `span` in `text`.
#[verus_spec(same =>
    ensures
        same == slot_of(text@, *span, *slot),
)]
fn is_value(slot: &Slot, text: &str, span: &Span) -> bool {
    if slot.name.as_str() != span.slot.as_str() {
        return false;
    }
    match text.get(span.start..span.end) {
        Some(value) => {
            proof! {
                vstd::utf8::encode_utf8_decode_utf8(value@);
                vstd::utf8::encode_utf8_decode_utf8(slot.value@);
            }
            value == slot.value.as_str()
        }
        None => false,
    }
}

/// True when the new model is never worse than the old one: both measures of `new` are equal or
/// higher.
#[verus_spec(accepted =>
    ensures
        exists|intent: bool, slots: bool| #[trigger] vstd::std_specs::cmp::ge_ensures::<f64>(new.intent, old.intent, intent)
            && #[trigger] vstd::std_specs::cmp::ge_ensures::<f64>(new.slots, old.slots, slots)
            && accepted == (intent && slots),
)]
#[must_use]
pub fn accept(new: Metrics, old: Metrics) -> bool {
    let intent = new.intent >= old.intent;
    let slots = new.slots >= old.slots;
    intent && slots
}

/// The state of the training runs.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TrainingState {
    /// The run of the active model, or `None` for the fixture model.
    pub current: Option<String>,
    /// The start time of the last run.
    pub last_time: Timestamp,
}

/// The number of action marks of `marked` with a mark time after the last run of `state`, plus
/// the number of entries of `learned` activated after it.
#[verus_spec(count =>
    requires
        marked@.len() + learned@.len() <= usize::MAX,
    ensures
        count == changed(marked@, crate::domain::contracts::nanosecond(state.last_time), marked@.len() as int)
            + activated(learned@, crate::domain::contracts::nanosecond(state.last_time), learned@.len() as int),
)]
#[must_use]
pub fn changes(state: &TrainingState, marked: &[Interaction], learned: &[LearnedIntent]) -> usize {
    let after = state.last_time.as_nanosecond();
    let (mut count, mut index) = (0, 0);
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= marked@.len(),
            count <= index,
            count == changed(marked@, after, index as int),
        decreases marked@.len() - index,
    ))]
    while index < marked.len() {
        if matches!(marked[index].marks.first(), Some(mark) if mark.time.as_nanosecond() > after) {
            count += 1;
        }
        index += 1;
    }
    count + activations(learned, after)
}

/// The number of entries of `learned` activated after the nanosecond `after`.
#[verus_spec(count =>
    ensures
        count <= learned@.len(),
        count == activated(learned@, after, learned@.len() as int),
)]
fn activations(learned: &[LearnedIntent], after: i128) -> usize {
    let (mut count, mut index) = (0, 0);
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= learned@.len(),
            count <= index,
            count == activated(learned@, after, index as int),
        decreases learned@.len() - index,
    ))]
    while index < learned.len() {
        let time = learned[index].activated;
        if matches!(time, Some(time) if time.as_nanosecond() > after) {
            count += 1;
        }
        index += 1;
    }
    count
}

/// True when `count` changes are enough to start a training run.
#[verus_spec(train =>
    ensures
        train == (count >= min_changes),
)]
#[must_use]
pub fn should_train(count: usize, min_changes: usize) -> bool {
    count >= min_changes
}

verus! {

broadcast use vstd::utf8::encode_utf8_push;

/// The number of bytes of the first `count` characters of `text`.
pub open spec fn bytes_before(text: Seq<char>, count: int) -> int {
    vstd::utf8::encode_utf8(text.take(count)).len() as int
}

/// True when `chars` is a character offset of `text`, and it is the number of characters before
/// `byte` when a character ends at `byte`.
pub open spec fn counts(text: Seq<char>, byte: int, chars: int) -> bool {
    &&& chars <= text.len()
    &&& forall|count: int| 0 <= count <= text.len() && #[trigger] bytes_before(text, count) == byte
        ==> chars == count
}

/// True when `file` is the byte span `span` of `text` in character offsets.
pub open spec fn in_chars(text: Seq<char>, span: Span, file: SpanFile) -> bool {
    &&& file.slot == span.slot
    &&& counts(text, span.start as int, file.start as int)
    &&& counts(text, span.end as int, file.end as int)
}

/// True when `span` is the character span `file` of `text` in byte offsets.
pub open spec fn in_bytes(text: Seq<char>, file: SpanFile, span: Span) -> bool {
    &&& span.slot == file.slot
    &&& file.start <= text.len() ==> span.start == bytes_before(text, file.start as int)
    &&& file.end <= text.len() ==> span.end == bytes_before(text, file.end as int)
}

/// The loop invariant of `char_offset` and `byte_offset`: `offset` is the number of bytes of the
/// first `count` characters of `text`, and `chars` and `bytes` are the characters and the bytes
/// of `text`.
spec fn walked(text: Seq<char>, chars: Seq<char>, bytes: Seq<u8>, count: int, offset: int) -> bool {
    &&& chars == text
    &&& bytes == vstd::utf8::encode_utf8(text)
    &&& 0 <= count <= text.len()
    &&& offset == bytes_before(text, count)
}

/// Before the end of `text`, the character at `count` adds its UTF-8 length to `bytes_before`,
/// and the sum is at most the length of `text`.
proof fn lemma_next(text: Seq<char>, count: int)
    requires
        0 <= count <= text.len(),
    ensures
        count < text.len() <==> bytes_before(text, count) < vstd::utf8::encode_utf8(text).len(),
        count < text.len() ==> bytes_before(text, count + 1) == bytes_before(text, count)
            + vstd::utf8::encode_scalar(text[count] as u32).len(),
        count < text.len() ==> bytes_before(text, count + 1) <= vstd::utf8::encode_utf8(text).len(),
{
    assert(text.take(text.len() as int) =~= text);
    if count < text.len() {
        assert(text.take(count + 1) =~= text.take(count).push(text[count]));
        vstd::utf8::lemma_encode_utf8_len_strictly_monotonic(text, count, text.len() as int);
        if count + 1 < text.len() {
            vstd::utf8::lemma_encode_utf8_len_strictly_monotonic(text, count + 1, text.len() as int);
        }
    }
}

/// When the loop of `char_offset` stops at `count`, `count` is the number of characters before
/// `byte` if a character ends there.
proof fn lemma_counted(text: Seq<char>, byte: int, count: int)
    requires
        0 <= count <= text.len(),
        count > 0 ==> bytes_before(text, count - 1) < byte,
        bytes_before(text, count) >= byte || count == text.len(),
    ensures
        counts(text, byte, count),
{
    assert forall|other: int| 0 <= other <= text.len() && #[trigger] bytes_before(text, other) == byte
        implies count == other by {
        if other < count - 1 {
            vstd::utf8::lemma_encode_utf8_len_strictly_monotonic(text, other, count - 1);
        }
        if count < other {
            vstd::utf8::lemma_encode_utf8_len_strictly_monotonic(text, count, other);
        }
    }
}

/// The loop invariant of `to_chars`: `files` is the first `index` spans of `spans`.
spec fn prefix_in_chars(text: Seq<char>, spans: Seq<Span>, files: Seq<SpanFile>, index: int) -> bool {
    &&& 0 <= index <= spans.len()
    &&& files.len() == index
    &&& forall|done: int| #![trigger files[done]] 0 <= done < index
        ==> in_chars(text, spans[done], files[done])
}

/// The loop invariant of `to_bytes`: `spans` is the first `index` spans of `files`.
spec fn prefix_in_bytes(text: Seq<char>, files: Seq<SpanFile>, spans: Seq<Span>, index: int) -> bool {
    &&& 0 <= index <= files.len()
    &&& spans.len() == index
    &&& forall|done: int| #![trigger spans[done]] 0 <= done < index
        ==> in_bytes(text, files[done], spans[done])
}

/// The text of `area`, or `none`.
pub open spec fn shown(area: Option<&str>) -> Seq<char> {
    match area {
        Some(area) => area@,
        None => "none"@,
    }
}

/// `context_text` of the shown `area`.
pub open spec fn context_spec(area: Seq<char>) -> Seq<char> {
    "area: "@ + area
}

/// The context of the area of `areas` at `random` modulo its length, or of no area.
pub open spec fn sampled(areas: Seq<String>, random: u64) -> Seq<char> {
    let area = if areas.len() == 0 { "none"@ } else { areas[random as int % areas.len() as int]@ };
    context_spec(area)
}

/// The number of seed examples of `intents`.
pub open spec fn seeds(intents: Seq<Intent>) -> int
    decreases intents.len(),
{
    if intents.len() == 0 { 0 } else { seeds(intents.drop_last()) + intents.last().examples@.len() }
}

/// The kind and the intent name of `label`: `true` for `Intent`.
pub open spec fn label_view(label: Label) -> (bool, Seq<char>) {
    match label {
        Label::Intent(name) => (true, name@),
        Label::Not(name) => (false, name@),
    }
}

/// True when `label` is the label of the action mark of `interaction`: `unknown` for a good mark
/// on System Two, the selected intent for another good mark, the correction for a bad mark with
/// one, and `Not` the selected intent for a bad mark without one.
pub open spec fn labeled(label: Label, interaction: Interaction) -> bool {
    let mark = interaction.marks@[0];
    let selected = interaction.action.intent@;
    match (mark.value, mark.correction, interaction.decision) {
        (Some(Value::Good), _, Route::Llm) => label_view(label) == (true, "unknown"@),
        (Some(Value::Good), _, _) => label_view(label) == (true, selected),
        (Some(Value::Bad), Some(correction), _) => label_view(label) == (true, correction@),
        (Some(Value::Bad), None, _) => label_view(label) == (false, selected),
        (None, _, _) => false,
    }
}

/// True when `example` comes from the action mark of an interaction of `marked`.
pub open spec fn from_mark(example: Example, marked: Seq<Interaction>) -> bool {
    exists|index: int| 0 <= index < marked.len() && marked[index].marks@.len() > 0
        && #[trigger] labeled(example.label, marked[index])
}

/// True when `example` has the normalized transcript and the context of the interaction `link`
/// of `marked`.
pub open spec fn started(example: Example, marked: Seq<Interaction>, link: u64) -> bool {
    exists|index: int| 0 <= index < marked.len() && #[trigger] marked[index].id == link
        && example.context@ == marked[index].model_context@
        && example.text@ == intent::normalized(marked[index].transcript@)
}

/// True when `ranked[other]` has the text of `ranked[index]`, another label, and a
/// higher precedence: a higher rank, a later mark time, or a later position.
spec fn outranks(ranked: Seq<Ranked>, other: int, index: int) -> bool {
    let (high, low) = (ranked[other], ranked[index]);
    &&& high.example.text@ == low.example.text@
    &&& label_view(high.example.label) != label_view(low.example.label)
    &&& high.rank > low.rank || (high.rank == low.rank && (high.time > low.time
        || (high.time == low.time && other > index)))
}

/// True when no example of `ranked` outranks `ranked[index]`.
spec fn wins(ranked: Seq<Ranked>, index: int) -> bool {
    forall|other: int| 0 <= other < ranked.len() ==> !#[trigger] outranks(ranked, other, index)
}

/// The examples of the first `count` of `ranked` that win, in order.
spec fn chosen(ranked: Seq<Ranked>, count: int) -> Seq<Example>
    decreases count,
{
    if count <= 0 {
        Seq::empty()
    } else if wins(ranked, count - 1) {
        chosen(ranked, count - 1).push(ranked[count - 1].example)
    } else {
        chosen(ranked, count - 1)
    }
}

/// True when `correct` is the verdict on the top label of `prediction` for `label`, with
/// `confident` the result of `p(top) >= threshold`. A prediction without labels is wrong.
pub open spec fn judged(prediction: Prediction, label: Label, threshold: f32, correct: bool) -> bool {
    if prediction.ranked@.len() == 0 {
        !correct
    } else {
        exists|confident: bool| #[trigger] vstd::std_specs::cmp::ge_ensures::<f32>(prediction.ranked@[0].1, threshold, confident)
            && correct == verdict(prediction.ranked@[0].0@, label, confident)
    }
}

/// True when the top label `top` is correct for `label`: `Intent(k)` needs `k` with `confident`,
/// or `unknown` without it. `Not(k)` needs another label or no `confident`.
pub open spec fn verdict(top: Seq<char>, label: Label, confident: bool) -> bool {
    match label {
        Label::Intent(name) => (confident && top == name@) || (!confident && name@ == "unknown"@),
        Label::Not(name) => !confident || top != name@,
    }
}

/// True when `slot` has the slot name of `span`, and the span is a character range of `text`
/// with the bytes of the value.
pub open spec fn slot_of(text: Seq<char>, span: Span, slot: Slot) -> bool {
    &&& slot.name@ == span.slot@
    &&& crate::domain::model::char_range(text, span.start, span.end)
    &&& vstd::utf8::encode_utf8(slot.value@)
        == vstd::utf8::encode_utf8(text).subrange(span.start as int, span.end as int)
}

/// True when `slot` is the value of a span of `spans` in `text`.
pub open spec fn in_spans(text: Seq<char>, spans: Seq<Span>, slot: Slot) -> bool {
    exists|index: int| 0 <= index < spans.len() && #[trigger] slot_of(text, spans[index], slot)
}

/// True when the value of `span` in `text` is a slot of `slots`.
pub open spec fn in_slots(text: Seq<char>, slots: Seq<Slot>, span: Span) -> bool {
    exists|index: int| 0 <= index < slots.len() && #[trigger] slot_of(text, span, slots[index])
}

/// True when `slots` and the values of `spans` in `text` are the same set.
pub open spec fn same_set(slots: Seq<Slot>, text: Seq<char>, spans: Seq<Span>) -> bool {
    &&& forall|index: int| 0 <= index < slots.len() ==> in_spans(text, spans, #[trigger] slots[index])
    &&& forall|index: int| 0 <= index < spans.len() ==> in_slots(text, slots, #[trigger] spans[index])
}

/// The number of the first `count` interactions of `marked` with an action mark after the
/// nanosecond `after`.
pub open spec fn changed(marked: Seq<Interaction>, after: i128, count: int) -> int
    decreases count,
{
    if count <= 0 {
        0
    } else {
        let marks = marked[count - 1].marks@;
        let new = if marks.len() > 0 && crate::domain::contracts::nanosecond(marks[0].time) > after { 1int } else { 0int };
        changed(marked, after, count - 1) + new
    }
}

/// The number of the first `count` entries of `learned` activated after the nanosecond `after`.
pub open spec fn activated(learned: Seq<LearnedIntent>, after: i128, count: int) -> int
    decreases count,
{
    if count <= 0 {
        0
    } else {
        let new = if learned[count - 1].activated matches Some(time)
            && crate::domain::contracts::nanosecond(time) > after { 1int } else { 0int };
        activated(learned, after, count - 1) + new
    }
}

/// The conversion from `Example` has no spec function. Only Verus has this trait.
#[cfg(verus_only)]
impl vstd::std_specs::convert::FromSpecImpl<Example> for ExampleFile {
    open spec fn obeys_from_spec() -> bool {
        false
    }

    open spec fn from_spec(example: Example) -> Self {
        arbitrary()
    }
}

/// The conversion from `ExampleFile` has no spec function. Only Verus has this trait.
#[cfg(verus_only)]
impl vstd::std_specs::convert::FromSpecImpl<ExampleFile> for Example {
    open spec fn obeys_from_spec() -> bool {
        false
    }

    open spec fn from_spec(file: ExampleFile) -> Self {
        arbitrary()
    }
}

} // verus!

/// Tests of the training set, the context samples and the held-out split.
#[cfg(test)]
mod tests {
    use super::*;

    use crate::domain::feedback::Action;
    use crate::domain::feedback::Mark;

    /// The training settings of the tests.
    fn settings() -> Settings {
        Settings {
            base_model: "microsoft/MiniLM-L12-H384-uncased".to_owned(),
            epochs: 8,
            batch_size: 8,
        }
    }

    /// The catalog of `intents.toml` and its seed training set.
    fn training_set() -> (Catalog, TrainingSet) {
        let catalog: Catalog =
            toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog");
        let areas = ["kitchen".to_owned(), "office".to_owned()];
        let set = seed_set(&catalog, &areas, settings());
        (catalog, set)
    }

    /// The `lights_on` reply to "Turn on the Kitchen lights." in the office, with the action mark
    /// `value` and `correction`, or no mark.
    fn marked(id: u64, value: Option<Value>, correction: Option<&str>) -> Interaction {
        let time: Timestamp = "2026-11-15T08:00:00Z".parse().expect("a timestamp");
        let mark = value.map(|value| Mark {
            interaction: id,
            value: Some(value),
            correction: correction.map(str::to_owned),
            time,
        });
        Interaction {
            id,
            time,
            satellite: None,
            transcript: "Turn on the Kitchen lights.".to_owned(),
            model_context: context_text(Some("office")),
            model: "fixture".to_owned(),
            top_labels: Vec::new(),
            slots: vec![Tagged {
                name: "area".to_owned(),
                value: "kitchen".to_owned(),
                start: 12,
                end: 19,
            }],
            action: Action {
                intent: "lights_on".to_owned(),
                targets: Vec::new(),
            },
            decision: Route::Reply,
            service_calls: Vec::new(),
            responses: Vec::new(),
            latency_ms: None,
            link: None,
            marks: mark.into_iter().collect(),
            speaker: None,
            source: None,
            embedded: false,
        }
    }

    /// The labels and the spans of the mark examples of `marked`.
    fn labels_and_spans(marked: &[Interaction]) -> Vec<(Label, Option<Vec<Span>>)> {
        let examples = mark_examples(marked).into_iter();
        examples
            .map(|example| (example.label, example.spans))
            .collect()
    }

    /// The label `Intent(name)`.
    fn intent(name: &str) -> Label {
        Label::Intent(name.to_owned())
    }

    /// AC-13.1-1.
    #[test]
    fn ac_13_1_1_good_mark_gives_logged_spans_and_context() {
        let span = Span {
            slot: "area".to_owned(),
            start: 12,
            end: 19,
        };
        let example = Example {
            context: context_text(Some("office")),
            text: "turn on the kitchen lights".to_owned(),
            label: intent("lights_on"),
            spans: Some(vec![span]),
        };
        assert_eq!(
            mark_examples(&[marked(1, Some(Value::Good), None)]),
            [example]
        );
    }

    /// AC-13.1-2.
    #[test]
    fn ac_13_1_2_correction_gives_example_without_spans() {
        let examples = labels_and_spans(&[marked(1, Some(Value::Bad), Some("tv_show"))]);
        assert_eq!(examples, [(intent("tv_show"), None)]);
    }

    /// AC-13.1-3.
    #[test]
    fn ac_13_1_3_bad_mark_gives_not_example() {
        let examples = labels_and_spans(&[marked(1, Some(Value::Bad), None)]);
        assert_eq!(examples, [(Label::Not("lights_on".to_owned()), None)]);
    }

    /// AC-13.1-4.
    #[test]
    fn ac_13_1_4_no_mark_gives_no_example() {
        assert_eq!(mark_examples(&[marked(1, None, None)]), []);
    }

    /// AC-13.1-6.
    #[test]
    fn ac_13_1_6_unknown_correction_and_good_system_two_give_unknown() {
        let mut llm = marked(2, Some(Value::Good), None);
        llm.decision = Route::Llm;
        let examples = labels_and_spans(&[marked(1, Some(Value::Bad), Some("unknown")), llm]);
        assert_eq!(
            examples,
            [(intent("unknown"), None), (intent("unknown"), None)]
        );
    }

    /// AC-13.1-7.
    #[test]
    fn ac_13_1_7_dialog_answer_gives_first_text_and_context() {
        let mut first = marked(1, None, None);
        first.transcript = "Turn on the lights".to_owned();
        let mut answer = marked(2, Some(Value::Good), None);
        answer.transcript = "The kitchen.".to_owned();
        answer.model_context = context_text(Some("kitchen"));
        answer.link = Some(1);
        let example = Example {
            context: context_text(Some("office")),
            text: "turn on the lights".to_owned(),
            label: intent("lights_on"),
            spans: None,
        };
        assert_eq!(mark_examples(&[first, answer]), [example]);
    }

    /// AC-13.1b-1.
    #[test]
    fn ac_13_1b_1_mark_wins_over_seed() {
        let mut mark = marked(1, Some(Value::Bad), Some("fan_on"));
        mark.transcript = "Turn on the lights!".to_owned();
        let (catalog, _) = training_set();
        let set = super::training_set(&catalog, Vec::new(), &[mark], &[], settings());
        let examples = set.examples.into_iter();
        let same = examples.filter(|example| example.text == "turn on the lights");
        let labels: Vec<Label> = same.map(|example| example.label).collect();
        assert_eq!(labels, [intent("fan_on")]);
    }

    /// The example "turn on the kitchen lights" with `label` and the `area` span of "kitchen".
    fn held(label: Label) -> Example {
        Example {
            context: context_text(None),
            text: "turn on the kitchen lights".to_owned(),
            label,
            spans: Some(vec![Span {
                slot: "area".to_owned(),
                start: 12,
                end: 19,
            }]),
        }
    }

    /// The prediction of `label` with `probability` and the `area` slot `area`.
    fn predicted(label: &str, probability: f32, area: &str) -> Prediction {
        Prediction {
            ranked: vec![(label.to_owned(), probability)],
            slots: vec![Slot {
                name: "area".to_owned(),
                value: area.to_owned(),
            }],
        }
    }

    /// AC-13.2-1.
    #[test]
    fn ac_13_2_1_top_label_below_threshold_is_wrong() {
        let predictions = [
            predicted("lights_on", 0.6, "kitchen"),
            predicted("lights_on", 0.8, "kitchen"),
        ];
        let examples = [held(intent("lights_on")), held(intent("lights_on"))];
        assert_eq!(metrics(&predictions, &examples, 0.7).intent, 0.5);
    }

    /// AC-13.2-2.
    #[test]
    fn ac_13_2_2_other_slots_are_wrong() {
        let predictions = [
            predicted("lights_on", 0.8, "office"),
            predicted("lights_on", 0.8, "kitchen"),
        ];
        let examples = [held(intent("lights_on")), held(intent("lights_on"))];
        assert_eq!(metrics(&predictions, &examples, 0.7).slots, 0.5);
    }

    /// AC-13.2-3.
    #[test]
    fn ac_13_2_3_not_example_with_other_top_label_is_correct() {
        let predictions = [
            predicted("fan_on", 0.9, "kitchen"),
            predicted("lights_on", 0.9, "kitchen"),
        ];
        let not = Label::Not("lights_on".to_owned());
        let examples = [held(not.clone()), held(not)];
        assert_eq!(metrics(&predictions, &examples, 0.7).intent, 0.5);
    }

    /// An example without a prediction is wrong in both measures.
    #[test]
    fn missing_prediction_is_wrong() {
        let predictions = [predicted("lights_on", 0.8, "kitchen")];
        let examples = [held(intent("lights_on")), held(intent("lights_on"))];
        let expected = Metrics {
            intent: 0.5,
            slots: 0.5,
        };
        assert_eq!(metrics(&predictions, &examples, 0.7), expected);
    }

    /// AC-13.2-4.
    #[test]
    fn ac_13_2_4_accept_needs_both_measures() {
        let old = Metrics {
            intent: 0.8,
            slots: 0.9,
        };
        let new = Metrics {
            intent: 0.9,
            slots: 0.8,
        };
        assert!(accept(old, old), "equal measures are accepted");
        assert!(!accept(new, old), "a lower slot accuracy is rejected");
    }

    /// AC-13.3-1.
    #[test]
    fn ac_13_3_1_changes_counts_marks_after_last_run() {
        let state = TrainingState {
            current: Some("run-1".to_owned()),
            last_time: "2026-11-15T07:00:00Z".parse().expect("a timestamp"),
        };
        let mut marks: Vec<Interaction> = (0..23)
            .map(|id| marked(id, Some(Value::Good), None))
            .collect();
        for interaction in &mut marks[..3] {
            interaction.marks[0].time = "2026-11-15T06:00:00Z".parse().expect("a timestamp");
        }
        assert_eq!(changes(&state, &marks, &[]), 20);
    }

    /// AC-13.3-2.
    #[test]
    fn ac_13_3_2_should_train_at_min_changes() {
        assert!(!should_train(19, 20), "one change short starts no run");
        assert!(should_train(20, 20), "min_changes starts a run");
    }

    /// AC-11.5-1.
    #[test]
    fn ac_11_5_1_context_text_is_the_area_or_none() {
        assert_eq!(context_text(Some("kitchen")), "area: kitchen");
        assert_eq!(context_text(None), "area: none");
    }

    /// AC-13.12-1.
    #[test]
    fn ac_13_12_1_same_text_gives_same_context() {
        let areas = ["kitchen".to_owned(), "office".to_owned()];
        let first = sample_context(&areas, "turn on the lights");
        assert_eq!(first, sample_context(&areas, "turn on the lights"));
    }

    /// AC-13.12-2.
    #[test]
    fn ac_13_12_2_texts_spread_over_the_areas() {
        let areas = ["kitchen", "office", "den", "hall"].map(str::to_owned);
        let contexts: Vec<String> = (0..1000)
            .map(|index| sample_context(&areas, &format!("turn on light {index}")))
            .collect();
        for area in &areas {
            let context = context_text(Some(area));
            let count = contexts.iter().filter(|other| **other == context).count();
            assert!((200..=300).contains(&count), "{area} occurs {count} times");
        }
    }

    /// AC-3.3b-1.
    #[test]
    fn ac_3_3b_1_one_example_for_each_seed() {
        let (catalog, set) = training_set();
        let seeds: usize = catalog.intents.iter().map(|i| i.examples.len()).sum();
        assert_eq!(set.examples.len(), seeds + catalog.unknown_examples.len());
    }

    /// AC-3.3b-2.
    #[test]
    fn ac_3_3b_2_labels_and_tags_come_from_catalog() {
        let (catalog, set) = training_set();
        assert_eq!(set.labels, catalog::labels(&catalog));
        assert_eq!(set.tags, catalog::tags(&catalog));
    }

    /// AC-3.3b-3.
    #[test]
    fn ac_3_3b_3_json_round_trip_is_equal() {
        let (_, mut set) = training_set();
        set.examples.push(Example {
            context: context_text(Some("office")),
            text: "turn on the tv".to_owned(),
            label: Label::Not("tv_on".to_owned()),
            spans: None,
        });
        let json = serde_json::to_string(&set).expect("a training set is JSON");
        let read: TrainingSet = serde_json::from_str(&json).expect("the JSON is a training set");
        assert_eq!(read, set);
    }

    /// AC-3.3c-1.
    #[test]
    fn ac_3_3c_1_json_spans_are_character_offsets() {
        let markup = intent::parse_example("set the [café](area) lights").expect("valid markup");
        let example = Example {
            context: context_text(None),
            text: markup.text,
            label: Label::Intent("lights_on".to_owned()),
            spans: Some(markup.spans),
        };
        let json = serde_json::to_value(&example).expect("an example is JSON");
        let span = serde_json::json!([{ "slot": "area", "start": 8, "end": 12 }]);
        assert_eq!(json["spans"], span);
        let read: Example = serde_json::from_value(json).expect("the JSON is an example");
        let spans = read.spans.expect("the example has spans");
        assert_eq!((spans[0].start, spans[0].end), (8, 13));
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
