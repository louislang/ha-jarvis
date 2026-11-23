//! Timer durations.

use std::num::ParseIntError;

use text2num::Language;
use vstd::prelude::*;

use crate::domain::intent;
use crate::domain::select;

/// The longest timer in seconds: 24 hours.
#[verus_verify]
pub const LONGEST: u32 = 86_400;

/// The seconds of the spoken duration `text`, for example 600 for "ten minutes". `None` when a
/// word is not a number, a unit or "and", a number has no unit, or the sum is not 1 s to 24 h.
#[verus_spec(result =>
    ensures
        match result {
            Some(seconds) => 1 <= seconds <= LONGEST,
            None => true,
        },
)]
#[must_use]
pub fn duration(text: &str) -> Option<u32> {
    let words = select::words(&intent::normalize(text));
    let mut total: u32 = 0;
    let mut number = String::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= words@.len(),
        decreases words@.len() - index,
    ))]
    while index < words.len() {
        let word = words[index].as_str();
        if let Some(size) = unit(word) {
            total = total.checked_add(count(&number)?.checked_mul(size)?)?;
            number = String::new();
        } else {
            number = pending(number, word);
        }
        index += 1;
    }
    if number.is_empty() && total != 0 && total <= LONGEST {
        Some(total)
    } else {
        None
    }
}

/// `number` with `word` added after a space. An "and" with no number before it adds nothing.
#[verus_verify]
fn pending(mut number: String, word: &str) -> String {
    if word == "and" && number.is_empty() {
        return number;
    }
    if !number.is_empty() {
        number.push(' ');
    }
    number.push_str(word);
    number
}

/// The seconds of the unit `word`, or `None` when `word` is not a unit.
#[verus_verify]
fn unit(word: &str) -> Option<u32> {
    if word == "second" || word == "seconds" {
        Some(1)
    } else if word == "minute" || word == "minutes" {
        Some(60)
    } else if word == "hour" || word == "hours" {
        Some(3600)
    } else {
        None
    }
}

/// The value of the number words or digits `number`: "a" and "an" give 1.
#[verus_verify]
fn count(number: &str) -> Option<u32> {
    if number == "a" || number == "an" {
        return Some(1);
    }
    let parsed: Result<u32, ParseIntError> =
        match text2num::text2digits(number, &Language::english()) {
            Ok(digits) => digits.parse(),
            Err(_) => number.parse(),
        };
    parsed.ok()
}

/// `seconds` as spoken hours, minutes and seconds with digits, for example "4 minutes and 12
/// seconds". The seconds are left out at 1 hour or more.
#[verus_verify]
#[must_use]
pub fn spoken(seconds: u32) -> String {
    let hours = seconds / 3600;
    let minutes = seconds % 3600 / 60;
    if hours > 0 {
        joined(&part(hours, "hour"), minutes, "minute")
    } else if minutes > 0 {
        joined(&part(minutes, "minute"), seconds % 60, "second")
    } else {
        part(seconds, "second")
    }
}

/// `first`, then "and" with `count` and `unit` when `count` is not 0.
#[verus_verify]
fn joined(first: &str, count: u32, unit: &str) -> String {
    if count == 0 {
        first.to_owned()
    } else {
        let second = part(count, unit);
        format!("{first} and {second}")
    }
}

/// `count` and `unit`, with "s" when `count` is not 1.
#[verus_verify]
fn part(count: u32, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_19_2_1_durations_in_seconds() {
        let cases = [
            ("ten minutes", 600),
            ("10 minutes", 600),
            ("five minutes", 300),
            ("an hour", 3600),
            ("one hour and twenty minutes", 4800),
            ("thirty seconds", 30),
            ("1 minute and 30 seconds", 90),
        ];
        for (text, seconds) in cases {
            assert_eq!(duration(text), Some(seconds), "{text}");
        }
    }

    #[test]
    fn ac_19_2_2_unclear_durations() {
        let cases = [
            "zero minutes",
            "twenty five hours",
            "ten",
            "minutes",
            "ten bananas",
            "half an hour",
            "",
        ];
        for text in cases {
            assert_eq!(duration(text), None, "{text}");
        }
    }

    #[test]
    fn ac_19_2_3_spoken_times() {
        let cases = [
            (45, "45 seconds"),
            (61, "1 minute and 1 second"),
            (252, "4 minutes and 12 seconds"),
            (3600, "1 hour"),
            (3900, "1 hour and 5 minutes"),
            (3661, "1 hour and 1 minute"),
            (0, "0 seconds"),
        ];
        for (seconds, text) in cases {
            assert_eq!(spoken(seconds), text, "{seconds}");
        }
    }
}
