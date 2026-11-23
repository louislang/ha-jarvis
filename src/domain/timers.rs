//! Timer durations and the time left on a timer.

use std::num::ParseIntError;

use jiff::Timestamp;
use text2num::Language;
use vstd::prelude::*;

use crate::domain::intent;
use crate::domain::select;
use crate::domain::state::State;

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

/// The seconds left on the HA timer helper `state` at `now`: to `finishes_at`, rounded up, when it
/// is `active`, and its `remaining` time when it is `paused`. `None` for another state, or for a
/// missing or unclear attribute.
#[verus_spec(result =>
    ensures
        !(state.state@ == "active"@ || state.state@ == "paused"@) ==> result is None,
        state.state@ == "paused"@ ==> match result {
            Some(total) => paused_left(state.attributes, total),
            None => true,
        },
)]
#[must_use]
pub fn left(state: &State, now: Timestamp) -> Option<u32> {
    if state.state.as_str() == "active" {
        let end: Timestamp = state
            .attributes
            .get("finishes_at")?
            .as_str()?
            .parse()
            .ok()?;
        until(end.as_nanosecond().checked_sub(now.as_nanosecond())?)
    } else if state.state.as_str() == "paused" {
        clock(state.attributes.get("remaining")?.as_str()?)
    } else {
        None
    }
}

/// The whole seconds of `nanoseconds`, rounded up: 0 when it is not positive, `None` above
/// `u32::MAX`.
#[verus_verify]
fn until(nanoseconds: i128) -> Option<u32> {
    if nanoseconds <= 0 {
        return Some(0);
    }
    u32::try_from((nanoseconds - 1) / 1_000_000_000 + 1).ok()
}

/// The seconds of the time `text`, `H:MM:SS`, or `None` when it does not parse.
#[verus_spec(result =>
    ensures
        match result {
            Some(total) => is_clock(text@, total),
            None => true,
        },
)]
fn clock(text: &str) -> Option<u32> {
    use std::str::FromStr;

    let (hours, rest) = text.split_once(':')?;
    let (minutes, seconds) = rest.split_once(':')?;
    let hour = u32::from_str(hours).ok()?;
    let minute = u32::from_str(minutes).ok()?;
    let second = u32::from_str(seconds).ok()?;
    if minute >= 60 || second >= 60 {
        return None;
    }
    let total = hour.checked_mul(3600)?.checked_add(minute * 60 + second)?;
    proof! {
        assert(text@ =~= hours@ + seq![':'] + minutes@ + seq![':'] + seconds@);
        assert(clock_of(text@, hours@, minutes@, seconds@, total));
    }
    Some(total)
}

verus! {

broadcast use crate::domain::contracts::axiom_char_pattern;

/// True when `text` is `hours`, a colon, `minutes`, a colon and `seconds`, with the minutes and the
/// seconds less than 60, and `total` is the seconds of that time.
pub open spec fn clock_of(text: Seq<char>, hours: Seq<char>, minutes: Seq<char>, seconds: Seq<char>, total: u32) -> bool {
    &&& text == hours + seq![':'] + minutes + seq![':'] + seconds
    &&& crate::domain::contracts::natural(hours) matches Some(h)
        && crate::domain::contracts::natural(minutes) matches Some(m)
        && crate::domain::contracts::natural(seconds) matches Some(s)
        && m < 60 && s < 60 && total == h * 3600 + m * 60 + s
}

/// True when `text` is a time `H:MM:SS` of `total` seconds.
pub open spec fn is_clock(text: Seq<char>, total: u32) -> bool {
    exists|hours: Seq<char>, minutes: Seq<char>, seconds: Seq<char>| #[trigger] clock_of(text, hours, minutes, seconds, total)
}

/// True when the `remaining` attribute of `attributes` is a time `H:MM:SS` of `total` seconds.
pub open spec fn paused_left(attributes: serde_json::Map<String, serde_json::Value>, total: u32) -> bool {
    crate::domain::contracts::attribute(attributes, "remaining") matches Some(value)
        && crate::domain::contracts::text(value) matches Some(remaining)
        && is_clock(remaining, total)
}

} // verus!

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use serde_json::json;

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

    /// The helper `timer.jarvis_1` with the HA state `state` and the attributes `attributes`.
    fn helper(state: &str, attributes: &Value) -> State {
        let value: Value = json!({
            "entity_id": "timer.jarvis_1",
            "state": state,
            "attributes": attributes,
            "last_updated": "2026-11-22T12:00:00Z",
            "context": { "id": "c1", "user_id": null, "parent_id": null },
        });
        serde_json::from_value(value).expect("a helper state")
    }

    /// `now` of the tests.
    fn now() -> Timestamp {
        "2026-11-22T12:00:00Z".parse().expect("a timestamp")
    }

    #[test]
    fn ac_19_3_1_active_until_finishes_at() {
        let cases = [
            ("2026-11-22T12:04:12+00:00", Some(252)),
            ("2026-11-22T12:04:11.2+00:00", Some(252)),
            ("2026-11-22T11:59:00+00:00", Some(0)),
        ];
        for (finishes_at, seconds) in cases {
            let state = helper(
                "active",
                &json!({ "finishes_at": finishes_at, "remaining": "0:10:00" }),
            );
            assert_eq!(left(&state, now()), seconds, "{finishes_at}");
        }
    }

    #[test]
    fn ac_19_3_2_paused_remaining() {
        let cases = [("0:04:12", Some(252)), ("1:00:00", Some(3600))];
        for (remaining, seconds) in cases {
            let state = helper("paused", &json!({ "remaining": remaining }));
            assert_eq!(left(&state, now()), seconds, "{remaining}");
        }
    }

    #[test]
    fn ac_19_3_3_no_time_left() {
        let cases = [
            helper("idle", &json!({ "remaining": "0:10:00" })),
            helper("active", &json!({ "remaining": "0:10:00" })),
            helper("paused", &json!({ "remaining": "0:61:00" })),
        ];
        for state in cases {
            assert_eq!(left(&state, now()), None, "{}", state.state);
        }
    }
}
