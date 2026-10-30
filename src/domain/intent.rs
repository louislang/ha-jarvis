//! Text normalization and example markup.

use std::mem;

use vstd::prelude::*;

/// A seed example without its slot markup.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Example {
    /// The markup with each `[value](slot)` replaced by `value`.
    pub text: String,
    /// The slot values in `text`, in text order.
    pub spans: Vec<Span>,
}

/// One slot value in the text of an example.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// The slot name.
    pub slot: String,
    /// The byte offset of the value.
    pub start: usize,
    /// The byte offset after the value.
    pub end: usize,
}

/// An error in example markup.
#[verus_verify]
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub enum MarkupError {
    /// A `[` has no closing `](slot)`.
    Unclosed {
        /// The byte offset of the `[` in the markup.
        position: usize,
    },
    /// A slot name is empty.
    EmptySlot,
}

/// The text with ASCII letters in lowercase, and only letters, digits, apostrophes, dots between
/// digits and single inner spaces.
#[verus_spec(normal =>
    ensures
        is_normal(normal@),
        is_normal(text@) ==> normal@ == text@,
)]
#[must_use]
pub fn normalize(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut normal = String::new();
    let mut space = true;
    let mut index = 0;
    proof! {
        assert(text@.take(0) =~= normal@);
    }
    #[cfg_attr(verus_only, verus_spec(
        invariant
            chars@ == text@,
            building(text@, normal@, index as int, space),
        decreases chars@.len() - index,
    ))]
    while index < chars.len() {
        let kept = kept(&chars, index);
        proof! {
            lemma_pushed(text@, normal@, index as int, space, kept);
        }
        if kept != ' ' || !space {
            normal.push(kept);
        }
        space = kept == ' ';
        index += 1;
    }
    proof! {
        lemma_trimmed(text@, normal@, space);
    }
    if space {
        normal.pop();
    }
    normal
}

/// The character at `index` of `chars` in normalized text, before spaces collapse.
#[verus_spec(kept =>
    requires
        index < chars@.len(),
    ensures
        keeps(chars@, index as int, kept),
)]
fn kept(chars: &[char], index: usize) -> char {
    let c = chars[index];
    if c.is_alphabetic()
        || c.is_ascii_digit()
        || c == '\''
        || (c == '.' && between_digits(chars, index))
    {
        c.to_ascii_lowercase()
    } else {
        ' '
    }
}

/// True when the characters before and after `index` of `chars` are ASCII digits.
#[verus_spec(between =>
    requires
        index < chars@.len(),
    ensures
        between == (0 < index < chars@.len() - 1 && digit(chars@[index - 1])
            && digit(chars@[index + 1])),
)]
fn between_digits(chars: &[char], index: usize) -> bool {
    index > 0
        && chars.len() - index > 1
        && chars[index - 1].is_ascii_digit()
        && chars[index + 1].is_ascii_digit()
}

/// The text and the slot spans of `markup`.
///
/// # Errors
///
/// A `[` has no closing `](slot)`, or a slot name is empty.
#[verus_spec(result =>
    ensures
        result is Ok <==> accepted(markup@),
        result matches Ok(example)
            ==> in_order(example.spans@, vstd::utf8::encode_utf8(example.text@).len() as int),
)]
pub fn parse_example(markup: &str) -> Result<Example, MarkupError> {
    let chars: Vec<char> = markup.chars().collect();
    let bytes = markup.as_bytes();
    let mut parser = Parser {
        text: String::new(),
        spans: Vec::new(),
        slot: String::new(),
        mode: Mode::Text,
        open: 0,
        start: 0,
        end: 0,
    };
    let mut byte = 0;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            parsing(markup@, chars@, bytes@, index as int, byte as int, parser),
        decreases bytes@.len() - byte,
    ))]
    while byte < bytes.len() {
        proof! {
            lemma_prefix(chars@, index as int);
        }
        let c = chars[index];
        parser.read(c, byte)?;
        byte += c.len_utf8();
        index += 1;
    }
    proof! {
        lemma_prefix(chars@, index as int);
    }
    parser.finish()
}

/// The state of the markup parser.
#[verus_verify]
enum Mode {
    /// Outside a slot.
    Text,
    /// In the value of a slot.
    Value,
    /// After the `]` of a value.
    Close,
    /// In the name of a slot.
    Slot,
}

/// The markup parser between two characters.
#[verus_verify]
struct Parser {
    /// The text so far.
    text: String,
    /// The closed spans so far.
    spans: Vec<Span>,
    /// The name of the open slot so far.
    slot: String,
    /// The parser state.
    mode: Mode,
    /// The byte offset of the open `[` in the markup.
    open: usize,
    /// The byte offset of the open value in `text`.
    start: usize,
    /// The byte offset after the open value in `text`.
    end: usize,
}

#[verus_verify]
impl Parser {
    /// Reads the character `c` at byte `byte` of the markup.
    #[verus_spec(result =>
        requires
            valid(*old(self)),
        ensures
            reads(*old(self), *final(self), c, result is Ok),
    )]
    fn read(&mut self, c: char, byte: usize) -> Result<(), MarkupError> {
        match self.mode {
            Mode::Text | Mode::Value => {
                self.read_value(c, byte);
                Ok(())
            }
            Mode::Close | Mode::Slot => self.read_slot(c),
        }
    }

    /// Reads the character `c` at byte `byte` of the markup outside a slot or in a value.
    #[verus_spec(
        requires
            valid(*old(self)),
            old(self).mode is Text || old(self).mode is Value,
        ensures
            reads(*old(self), *final(self), c, true),
    )]
    fn read_value(&mut self, c: char, byte: usize) {
        if matches!(self.mode, Mode::Text) && c == '[' {
            self.mode = Mode::Value;
            self.open = byte;
            self.start = self.text.len();
        } else if matches!(self.mode, Mode::Value) && c == ']' {
            self.mode = Mode::Close;
            self.end = self.text.len();
        } else {
            self.text.push(c);
        }
    }

    /// Reads the character `c` of the markup after a value or in a slot name.
    #[verus_spec(result =>
        requires
            valid(*old(self)),
            old(self).mode is Close || old(self).mode is Slot,
        ensures
            reads(*old(self), *final(self), c, result is Ok),
    )]
    fn read_slot(&mut self, c: char) -> Result<(), MarkupError> {
        match self.mode {
            Mode::Close if c == '(' => self.mode = Mode::Slot,
            Mode::Slot if c != ')' => self.slot.push(c),
            Mode::Slot if !self.slot.is_empty() => {
                let mut slot = String::new();
                mem::swap(&mut slot, &mut self.slot);
                self.spans.push(Span {
                    slot,
                    start: self.start,
                    end: self.end,
                });
                self.mode = Mode::Text;
            }
            Mode::Slot => return Err(MarkupError::EmptySlot),
            Mode::Text | Mode::Value | Mode::Close => {
                return Err(MarkupError::Unclosed {
                    position: self.open,
                });
            }
        }
        Ok(())
    }

    /// The example after the last character.
    #[verus_spec(result =>
        requires
            valid(self),
        ensures
            result is Ok <==> self.mode is Text,
            result matches Ok(example)
                ==> in_order(example.spans@, vstd::utf8::encode_utf8(example.text@).len() as int),
    )]
    fn finish(self) -> Result<Example, MarkupError> {
        match self.mode {
            Mode::Text => Ok(Example {
                text: self.text,
                spans: self.spans,
            }),
            Mode::Value | Mode::Close | Mode::Slot => Err(MarkupError::Unclosed {
                position: self.open,
            }),
        }
    }
}

verus! {

broadcast use {crate::domain::contracts::axiom_ascii_alphabetic, vstd::utf8::encode_utf8_push};

/// True when `c` is a letter that is not an ASCII capital, a digit, an apostrophe, a dot or a
/// space.
pub open spec fn allowed(c: char) -> bool {
    (crate::domain::contracts::alphabetic(c) && !('A' <= c && c <= 'Z')) || digit(c) || c == '\''
        || c == '.' || c == ' '
}

/// True when `c` is an ASCII digit.
pub open spec fn digit(c: char) -> bool {
    '0' <= c && c <= '9'
}

/// True when the character at `index` of `text` can be in normalized text.
pub open spec fn fits(text: Seq<char>, index: int) -> bool {
    &&& allowed(text[index])
    &&& text[index] == ' ' ==> 0 < index < text.len() - 1 && text[index + 1] != ' '
    &&& text[index] == '.' ==> 0 < index < text.len() - 1 && digit(text[index - 1])
        && digit(text[index + 1])
}

/// True when `text` is the output of `normalize`.
pub open spec fn is_normal(text: Seq<char>) -> bool {
    forall|index: int| 0 <= index < text.len() ==> #[trigger] fits(text, index)
}

/// True when the character at `index` of `text` can start normalized text that is not complete.
spec fn begun(text: Seq<char>, index: int) -> bool {
    &&& allowed(text[index])
    &&& text[index] == ' ' ==> 0 < index && (index + 1 < text.len() ==> text[index + 1] != ' ')
    &&& text[index] == '.' ==> 0 < index && digit(text[index - 1])
        && (index + 1 < text.len() ==> digit(text[index + 1]))
}

/// True when `kept` is the character at `index` of `text` in normalized text.
spec fn keeps(text: Seq<char>, index: int, kept: char) -> bool {
    &&& allowed(kept)
    &&& kept == '.' ==> 0 < index < text.len() - 1 && digit(text[index - 1]) && digit(text[index + 1])
    &&& digit(text[index]) ==> kept == text[index]
    &&& fits(text, index) ==> kept == text[index]
}

/// The loop invariant of `normalize`: `normal` is the output for the first `index` characters
/// of `text`, and `space` is true when it is empty or ends with a space.
spec fn building(text: Seq<char>, normal: Seq<char>, index: int, space: bool) -> bool {
    &&& 0 <= index <= text.len()
    &&& space == (normal.len() == 0 || normal.last() == ' ')
    &&& forall|position: int| 0 <= position < normal.len() ==> #[trigger] begun(normal, position)
    &&& normal.len() > 0 && normal.last() == '.' ==> index < text.len() && digit(text[index])
    &&& index > 0 && digit(text[index - 1]) ==> normal.len() > 0 && normal.last() == text[index - 1]
    &&& is_normal(text) ==> normal == text.take(index)
}

/// The loop invariant of `normalize` holds after `kept`, the character at `index`, is pushed or
/// skipped.
proof fn lemma_pushed(text: Seq<char>, normal: Seq<char>, index: int, space: bool, kept: char)
    requires
        index < text.len(),
        building(text, normal, index, space),
        keeps(text, index, kept),
    ensures
        building(
            text,
            if kept != ' ' || !space { normal.push(kept) } else { normal },
            index + 1,
            kept == ' ',
        ),
{
    if kept != ' ' || !space {
        assert forall|position: int| 0 <= position <= normal.len()
            implies #[trigger] begun(normal.push(kept), position) by {
            if position < normal.len() {
                assert(begun(normal, position));
            }
        }
    }
    if is_normal(text) {
        assert(fits(text, index));
        if index > 0 {
            assert(fits(text, index - 1));
        }
        assert(text.take(index).push(text[index]) =~= text.take(index + 1));
    }
}

/// The output of `normalize` after the loop, with the last space removed when `space`, is
/// normalized text, and normalized text gives itself.
proof fn lemma_trimmed(text: Seq<char>, normal: Seq<char>, space: bool)
    requires
        building(text, normal, text.len() as int, space),
    ensures
        space ==> is_normal(normal.drop_last()) || normal.len() == 0,
        !space ==> is_normal(normal),
        is_normal(text) ==> normal == text && (!space || normal.len() == 0),
{
    assert(text.take(text.len() as int) =~= text);
    if space && normal.len() > 0 {
        assert forall|position: int| 0 <= position < normal.len() - 1
            implies #[trigger] fits(normal.drop_last(), position) by {
            assert(begun(normal, position));
            assert(begun(normal, position + 1));
        }
    }
    if !space {
        assert forall|position: int| 0 <= position < normal.len()
            implies #[trigger] fits(normal, position) by {
            assert(begun(normal, position));
            if position + 1 < normal.len() {
                assert(begun(normal, position + 1));
            }
        }
    }
    if is_normal(text) && text.len() > 0 {
        assert(fits(text, text.len() - 1));
    }
}

/// True when the markup `chars` has no error.
pub closed spec fn accepted(chars: Seq<char>) -> bool {
    run(chars) == Some((Mode::Text, false))
}

/// The parser state and whether the slot name is not empty after `chars`, or `None` after an
/// error.
spec fn run(chars: Seq<char>) -> Option<(Mode, bool)>
    decreases chars.len(),
{
    if chars.len() == 0 {
        Some((Mode::Text, false))
    } else {
        match run(chars.drop_last()) {
            Some((mode, named)) => step(mode, named, chars.last()),
            None => None,
        }
    }
}

/// The parser state after `c` in `mode`, or `None` for an error.
spec fn step(mode: Mode, named: bool, c: char) -> Option<(Mode, bool)> {
    match mode {
        Mode::Text => Some((if c == '[' { Mode::Value } else { Mode::Text }, false)),
        Mode::Value => Some((if c == ']' { Mode::Close } else { Mode::Value }, false)),
        Mode::Close => if c == '(' { Some((Mode::Slot, false)) } else { None },
        Mode::Slot => if c != ')' {
            Some((Mode::Slot, true))
        } else if named {
            Some((Mode::Text, false))
        } else {
            None
        },
    }
}

/// True when each span of `spans` is in a text of `len` bytes, and the spans are in order and
/// do not overlap.
pub open spec fn in_order(spans: Seq<Span>, len: int) -> bool {
    &&& forall|index: int| #![trigger spans[index]] 0 <= index < spans.len()
        ==> spans[index].start <= spans[index].end <= len
    &&& forall|index: int| #![trigger spans[index]] 0 < index < spans.len()
        ==> spans[index - 1].end <= spans[index].start
}

/// True when `after` is `before` after it reads `c`, and `ok` is false only for an error.
spec fn reads(before: Parser, after: Parser, c: char, ok: bool) -> bool {
    match step(before.mode, before.slot@.len() > 0, c) {
        Some(state) => ok && valid(after) && state == (after.mode, after.slot@.len() > 0),
        None => !ok,
    }
}

/// True when the spans, the offsets and the slot name of `parser` agree with its state.
spec fn valid(parser: Parser) -> bool {
    let len = vstd::utf8::encode_utf8(parser.text@).len() as int;
    &&& in_order(parser.spans@, len)
    &&& !(parser.mode is Text) ==> parser.start <= len
    &&& !(parser.mode is Text) && parser.spans@.len() > 0 ==> parser.spans@.last().end <= parser.start
    &&& (parser.mode is Close || parser.mode is Slot) ==> parser.start <= parser.end <= len
    &&& !(parser.mode is Slot) ==> parser.slot@.len() == 0
}

/// The loop invariant of `parse_example`: `parser` has read the first `index` characters of
/// `markup`, which end at byte `byte`.
spec fn parsing(markup: Seq<char>, chars: Seq<char>, bytes: Seq<u8>, index: int, byte: int, parser: Parser) -> bool {
    &&& chars == markup
    &&& bytes == vstd::utf8::encode_utf8(markup)
    &&& 0 <= index <= chars.len()
    &&& byte == vstd::utf8::encode_utf8(chars.take(index)).len()
    &&& valid(parser)
    &&& run(chars.take(index)) == Some((parser.mode, parser.slot@.len() > 0))
}

/// The facts about the prefix of `chars` with `index` characters that the loop of
/// `parse_example` needs.
proof fn lemma_prefix(chars: Seq<char>, index: int)
    requires
        0 <= index <= chars.len(),
    ensures
        index == chars.len() ==> chars.take(index) == chars,
        index < chars.len() ==> {
            &&& chars.take(index + 1).drop_last() == chars.take(index)
            &&& chars.take(index).push(chars[index]) == chars.take(index + 1)
            &&& vstd::utf8::encode_utf8(chars.take(index)).len()
                < vstd::utf8::encode_utf8(chars).len()
            &&& vstd::utf8::encode_utf8(chars.take(index + 1)).len()
                <= vstd::utf8::encode_utf8(chars).len()
            &&& run(chars.take(index + 1)) is None ==> run(chars) is None
        },
{
    assert(chars.take(chars.len() as int) =~= chars);
    if index < chars.len() {
        assert(chars.take(index + 1).drop_last() =~= chars.take(index));
        assert(chars.take(index).push(chars[index]) =~= chars.take(index + 1));
        vstd::utf8::lemma_encode_utf8_len_strictly_monotonic(chars, index, chars.len() as int);
        if index + 1 < chars.len() {
            vstd::utf8::lemma_encode_utf8_len_strictly_monotonic(chars, index + 1, chars.len() as int);
        }
        if run(chars.take(index + 1)) is None {
            lemma_run_none(chars, index + 1);
        }
    }
}

/// An error in a prefix of `chars` is an error of `chars`.
proof fn lemma_run_none(chars: Seq<char>, index: int)
    requires
        0 <= index <= chars.len(),
        run(chars.take(index)) is None,
    ensures
        run(chars) is None,
    decreases chars.len() - index,
{
    if index == chars.len() {
        assert(chars.take(index) =~= chars);
    } else {
        assert(chars.take(index + 1).drop_last() =~= chars.take(index));
        lemma_run_none(chars, index + 1);
    }
}

} // verus!

/// Tests of normalize and example markup.
#[cfg(test)]
mod tests {
    use super::*;

    /// AC-3.1-1.
    #[test]
    fn ac_3_1_1_normalize_lowercases_and_removes_punctuation() {
        assert_eq!(normalize("Turn ON the  Lights!"), "turn on the lights");
    }

    /// AC-3.1-2.
    #[test]
    fn ac_3_1_2_normalize_of_normalized_text_does_not_change() {
        for text in [
            "Turn ON the  Lights!",
            "  It's 72.5, Twenty-Two... ",
            "1.2.3 .5 5. a.b",
            "Café ÉCOLE straße 東京 €5",
            "",
        ] {
            let once = normalize(text);
            assert_eq!(
                normalize(&once),
                once,
                "normalize changed its output for {text:?}"
            );
        }
    }

    /// AC-3.1b-1.
    #[test]
    fn ac_3_1b_1_parse_example_gives_text_and_span() {
        let example = parse_example("turn on the [kitchen](area) lights").expect("valid markup");
        assert_eq!(example.text, "turn on the kitchen lights");
        assert_eq!(
            example.spans,
            [Span {
                slot: "area".to_owned(),
                start: 12,
                end: 19,
            }]
        );
    }

    /// AC-3.1b-2.
    #[test]
    fn ac_3_1b_2_two_slots_give_spans_in_text_order() {
        let example =
            parse_example("set the [bedroom](area) to [72](temperature)").expect("valid markup");
        let slots: Vec<(&str, &str)> = example
            .spans
            .iter()
            .map(|span| (span.slot.as_str(), &example.text[span.start..span.end]))
            .collect();
        assert_eq!(slots, [("area", "bedroom"), ("temperature", "72")]);
    }

    /// AC-3.1b-3.
    #[test]
    fn ac_3_1b_3_unclosed_bracket_gives_its_byte_offset() {
        for markup in ["on [the hall", "on [the] hall", "café [a](area"] {
            assert_eq!(
                parse_example(markup),
                Err(MarkupError::Unclosed {
                    position: markup.find('[').expect("markup has a ["),
                }),
                "markup {markup:?}"
            );
        }
    }

    /// AC-3.1b-4.
    #[test]
    fn ac_3_1b_4_empty_slot_name_is_an_error() {
        assert_eq!(
            parse_example("turn on the [kitchen]() lights"),
            Err(MarkupError::EmptySlot)
        );
    }

    /// AC-3.1-7.
    #[test]
    fn ac_3_1_7_normalize_keeps_decimal_point_between_digits() {
        assert_eq!(
            normalize("Set it to 72.5, twenty-two"),
            "set it to 72.5 twenty two"
        );
    }
}
