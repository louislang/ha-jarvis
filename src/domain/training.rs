//! Training set, context samples and held-out split.

use rand::Rng;
use rand::rngs::StdRng;
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

/// The context of an area that `rng` picks from `areas`.
#[verus_spec(context =>
    ensures
        context@ == sampled(areas@, crate::domain::contracts::next(&*old(rng))),
)]
#[must_use]
pub fn sample_context(areas: &[String], rng: &mut StdRng) -> String {
    let random = rng.next_u32() as usize;
    if areas.is_empty() {
        return context_text(None);
    }
    let area = &areas[random % areas.len()];
    context_text(Some(area.as_str()))
}

/// The training set of the seed examples of `catalog`, each with a context that `rng` samples
/// from `areas`.
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
pub fn seed_set(
    catalog: &Catalog,
    areas: &[String],
    rng: &mut StdRng,
    settings: Settings,
) -> TrainingSet {
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
        push_intent(&catalog.intents[index], areas, rng, &mut examples);
        index += 1;
    }
    proof! {
        assert(catalog.intents@.take(index as int) =~= catalog.intents@);
    }
    push_unknown(&catalog.unknown_examples, areas, rng, &mut examples);
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
fn push_intent(intent: &Intent, areas: &[String], rng: &mut StdRng, examples: &mut Vec<Example>) {
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
            context: sample_context(areas, rng),
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
fn push_unknown(texts: &[String], areas: &[String], rng: &mut StdRng, examples: &mut Vec<Example>) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= texts@.len(),
            examples@.len() == old(examples)@.len() + index,
        decreases texts@.len() - index,
    ))]
    while index < texts.len() {
        examples.push(Example {
            context: sample_context(areas, rng),
            text: texts[index].clone(),
            label: Label::Intent("unknown".to_owned()),
            spans: Some(Vec::new()),
        });
        index += 1;
    }
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
pub open spec fn sampled(areas: Seq<String>, random: u32) -> Seq<char> {
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

    use jiff::Timestamp;
    use rand::SeedableRng;

    use crate::domain::feedback::Action;
    use crate::domain::feedback::Mark;

    /// The catalog of `intents.toml` and its seed training set.
    fn training_set() -> (Catalog, TrainingSet) {
        let catalog: Catalog =
            toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog");
        let settings = Settings {
            base_model: "microsoft/MiniLM-L12-H384-uncased".to_owned(),
            epochs: 8,
            batch_size: 8,
        };
        let areas = ["kitchen".to_owned(), "office".to_owned()];
        let set = seed_set(&catalog, &areas, &mut StdRng::seed_from_u64(1), settings);
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

    /// AC-11.5-1.
    #[test]
    fn ac_11_5_1_context_text_is_the_area_or_none() {
        assert_eq!(context_text(Some("kitchen")), "area: kitchen");
        assert_eq!(context_text(None), "area: none");
    }

    /// AC-3.3-3.
    #[test]
    fn ac_3_3_3_same_seed_gives_same_context() {
        let areas = ["kitchen".to_owned(), "office".to_owned()];
        let sample = |seed| sample_context(&areas, &mut StdRng::seed_from_u64(seed));
        assert_eq!(sample(7), sample(7));
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
