//! Text normalization.

use vstd::prelude::*;

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

verus! {

broadcast use crate::domain::contracts::axiom_ascii_alphabetic;

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

} // verus!

/// Tests of normalize.
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

    /// AC-3.1-7.
    #[test]
    fn ac_3_1_7_normalize_keeps_decimal_point_between_digits() {
        assert_eq!(
            normalize("Set it to 72.5, twenty-two"),
            "set it to 72.5 twenty two"
        );
    }
}
