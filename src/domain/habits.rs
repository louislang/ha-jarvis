//! Request habits: the time bucket of a request, decayed day scores, usual times and the habit
//! file.

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use serde::Deserialize;
use serde::Serialize;
use vstd::prelude::*;

#[cfg(verus_only)]
use crate::domain::contracts::date_of;
#[cfg(verus_only)]
use crate::domain::contracts::date_until;
#[cfg(verus_only)]
use crate::domain::contracts::hour;
#[cfg(verus_only)]
use crate::domain::contracts::local;
#[cfg(verus_only)]
use crate::domain::contracts::power;
#[cfg(verus_only)]
use crate::domain::contracts::weekday;
#[cfg(verus_only)]
use crate::domain::contracts::whole_hours;

/// The day type of a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DayType {
    /// Monday to Friday.
    Weekdays,
    /// Saturday and Sunday.
    Weekends,
}

/// One time slot for habits. Weekdays come before weekends, then the hour.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bucket {
    /// The day type of the time.
    pub day: DayType,
    /// The hour of the time, 0 to 23.
    pub hour: i8,
}

/// The decayed score of one request key in one bucket.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strength {
    /// The score on the day `last`.
    pub score: f64,
    /// The local date of the last day that counted.
    pub last: Date,
}

/// The request habit model: the strength of each request key and bucket. `habits.json` keeps it
/// as a list of entries, because a JSON map key must be a string.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(from = "Vec<Entry>", into = "Vec<Entry>")]
pub struct RequestHabits {
    /// The strength of each request key and bucket.
    pub strengths: BTreeMap<(String, Bucket), Strength>,
}

/// One strength of `habits.json`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    /// The request key.
    pub key: String,
    /// The day type of the bucket.
    pub day: DayType,
    /// The hour of the bucket.
    pub hour: i8,
    /// The score on the day `last`.
    pub score: f64,
    /// The local date of the last day that counted.
    pub last: Date,
}

#[verus_verify]
impl From<Vec<Entry>> for RequestHabits {
    fn from(entries: Vec<Entry>) -> Self {
        let mut strengths = BTreeMap::new();
        for entry in entries {
            let bucket = Bucket {
                day: entry.day,
                hour: entry.hour,
            };
            let strength = Strength {
                score: entry.score,
                last: entry.last,
            };
            strengths.insert((entry.key, bucket), strength);
        }
        Self { strengths }
    }
}

#[verus_verify]
impl From<RequestHabits> for Vec<Entry> {
    fn from(habits: RequestHabits) -> Self {
        let mut entries = Vec::new();
        let strengths = habits.strengths.iter();
        for (entry, strength) in strengths {
            entries.push(Entry {
                key: entry.0.clone(),
                day: entry.1.day,
                hour: entry.1.hour,
                score: strength.score,
                last: strength.last,
            });
        }
        entries
    }
}

#[verus_verify]
impl RequestHabits {
    /// The model with one request of `key` at `time` in `zone`. A day counts once for each key
    /// and bucket, and an older request changes nothing.
    #[verus_spec(observed =>
        ensures
            observed.strengths@.remove((key, bucket_at(time, *zone)))
                =~= self.strengths@.remove((key, bucket_at(time, *zone))),
            self.strengths@.contains_key((key, bucket_at(time, *zone)))
                && hours_between(self.strengths@[(key, bucket_at(time, *zone))].last, date_of(local(*zone, time))) <= 0
                ==> observed == self,
    )]
    #[must_use]
    pub fn observe(
        mut self,
        key: String,
        time: Timestamp,
        zone: &TimeZone,
        half_life_days: f64,
    ) -> Self {
        let today = zone.to_datetime(time).date();
        let entry = (key, bucket(time, zone));
        let strength = match self.strengths.get(&entry) {
            None => Strength {
                score: 1.0,
                last: today,
            },
            Some(old) if old.last.duration_until(today).as_hours() <= 0 => return self,
            Some(old) => Strength {
                score: decayed(old.score, old.last, today, half_life_days) + 1.0,
                last: today,
            },
        };
        self.strengths.insert(entry, strength);
        self
    }

    /// The buckets of `key` whose score decayed to `today` is `min_score` or more, in bucket order.
    #[verus_spec(times =>
        ensures
            all_usual(self.strengths@, key@, times@, today, half_life_days, min_score),
            forall|entry: (String, Bucket)| #[trigger] self.strengths@.contains_key(entry) && entry.0@ == key@ && !times@.contains(entry.1)
                ==> scored(self.strengths@[entry], today, half_life_days, min_score, false),
    )]
    #[must_use]
    pub fn usual_times(
        &self,
        key: &str,
        today: Date,
        half_life_days: f64,
        min_score: f64,
    ) -> Vec<Bucket> {
        let mut times = Vec::new();
        let entries = self.strengths.iter();
        proof_decl! {
            let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
        }
        #[cfg_attr(verus_only, verus_spec(entry_iter =>
            invariant
                entry_iter.seq() == all,
                lists(self.strengths@, all),
                all_usual(self.strengths@, key@, times@, today, half_life_days, min_score),
                usual_checked(all, entry_iter.index(), key@, times@, today, half_life_days, min_score),
        ))]
        for (entry, strength) in entries {
            let usual = decayed(strength.score, strength.last, today, half_life_days) >= min_score;
            proof! {
                assert(self.strengths@.contains_key(*all[entry_iter.index() as int].0));
                assert(scored(*strength, today, half_life_days, min_score, usual));
            }
            if entry.0.as_str() == key && usual {
                proof! {
                    lemma_pushed(times@, entry.1);
                }
                times.push(entry.1);
            }
        }
        times
    }

    /// The model without each strength whose score decayed to `today` is less than 0.01.
    #[verus_spec(pruned =>
        ensures
            kept_from(self.strengths@, pruned.strengths@, today, half_life_days),
            forall|entry: (String, Bucket)| #[trigger] self.strengths@.contains_key(entry) && !pruned.strengths@.contains_key(entry)
                ==> scored(self.strengths@[entry], today, half_life_days, 0.01f64, false),
    )]
    #[must_use]
    pub fn prune(self, today: Date, half_life_days: f64) -> Self {
        let mut strengths = BTreeMap::new();
        let entries = self.strengths.iter();
        proof_decl! {
            let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
        }
        #[cfg_attr(verus_only, verus_spec(entry_iter =>
            invariant
                entry_iter.seq() == all,
                lists(self.strengths@, all),
                kept_from(self.strengths@, strengths@, today, half_life_days),
                prune_checked(all, entry_iter.index(), strengths@, today, half_life_days),
        ))]
        for (entry, strength) in entries {
            let kept = decayed(strength.score, strength.last, today, half_life_days) >= 0.01;
            proof! {
                assert(self.strengths@.contains_key(*all[entry_iter.index() as int].0));
                assert(scored(*strength, today, half_life_days, 0.01f64, kept));
            }
            if kept {
                strengths.insert((entry.0.clone(), entry.1), *strength);
            }
        }
        Self { strengths }
    }
}

/// The bucket of `time` in `zone`.
#[verus_spec(found =>
    ensures
        found == (Bucket {
            day: day_of(weekday(local(*zone, time))),
            hour: hour(local(*zone, time)),
        }),
)]
#[must_use]
pub fn bucket(time: Timestamp, zone: &TimeZone) -> Bucket {
    let local = zone.to_datetime(time);
    let day = match local.weekday() {
        Weekday::Saturday | Weekday::Sunday => DayType::Weekends,
        _ => DayType::Weekdays,
    };
    Bucket {
        day,
        hour: local.hour(),
    }
}

/// `score` decayed from the day `last` to the day `today`: it halves each `half_life_days`.
#[verus_spec(found =>
    ensures
        decays(score, last, today, half_life_days, found),
)]
#[must_use]
pub fn decayed(score: f64, last: Date, today: Date, half_life_days: f64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an f64 holds each hour count below 2^53 exactly"
    )]
    let hours = last.duration_until(today).as_hours() as f64;
    let days = hours / 24.0;
    let exponent = days / half_life_days;
    let found = score * 0.5_f64.powf(exponent);
    proof! {
        assert(decay_steps(score, last, today, half_life_days, hours, days, exponent, found));
    }
    found
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_string_obeys_cmp,
    crate::domain::contracts::axiom_bucket_obeys_cmp,
    vstd::laws_cmp::group_laws_cmp,
};

/// The day type of `day`.
pub open spec fn day_of(day: Weekday) -> DayType {
    match day {
        Weekday::Saturday | Weekday::Sunday => DayType::Weekends,
        _ => DayType::Weekdays,
    }
}

/// The bucket of `time` in `zone`.
pub open spec fn bucket_at(time: Timestamp, zone: TimeZone) -> Bucket {
    Bucket { day: day_of(weekday(local(zone, time))), hour: hour(local(zone, time)) }
}

/// The whole hours from `last` to `today`.
pub open spec fn hours_between(last: Date, today: Date) -> i64 {
    whole_hours(date_until(last, today))
}

/// True when `hours`, `days`, `exponent` and `found` are the steps of
/// `score * 0.5^(days / half_life_days)`, with the calendar days from `last` to `today`.
pub open spec fn decay_steps(score: f64, last: Date, today: Date, half_life_days: f64, hours: f64, days: f64, exponent: f64, found: f64) -> bool {
    &&& vstd::float::float_cast_spec::<i64, f64>(hours_between(last, today), hours)
    &&& vstd::std_specs::ops::div_ensures::<f64>(hours, 24.0f64, days)
    &&& vstd::std_specs::ops::div_ensures::<f64>(days, half_life_days, exponent)
    &&& vstd::std_specs::ops::mul_ensures::<f64>(score, power(0.5f64, exponent), found)
}

/// True when `found` is `score * 0.5^(days / half_life_days)`, with the calendar days from `last`
/// to `today`.
pub open spec fn decays(score: f64, last: Date, today: Date, half_life_days: f64, found: f64) -> bool {
    exists|hours: f64, days: f64, exponent: f64| #[trigger] decay_steps(score, last, today, half_life_days, hours, days, exponent, found)
}

/// True when the score of `strength` decayed to `today` compares to `min_score` with the result
/// `kept` of `>=`.
pub open spec fn scored(strength: Strength, today: Date, half_life_days: f64, min_score: f64, kept: bool) -> bool {
    exists|found: f64| #[trigger] decays(strength.score, strength.last, today, half_life_days, found)
        && vstd::std_specs::cmp::ge_ensures::<f64>(found, min_score, kept)
}

/// True when `strengths` has a strength of the key with the text `key` in `bucket` whose score
/// decayed to `today` is `min_score` or more.
pub open spec fn usual(strengths: Map<(String, Bucket), Strength>, key: Seq<char>, bucket: Bucket, today: Date, half_life_days: f64, min_score: f64) -> bool {
    exists|entry: (String, Bucket)| #[trigger] strengths.contains_key(entry) && entry.0@ == key && entry.1 == bucket
        && scored(strengths[entry], today, half_life_days, min_score, true)
}

/// True when `all` lists each entry of `strengths` with its strength.
pub open spec fn lists(strengths: Map<(String, Bucket), Strength>, all: Seq<(&(String, Bucket), &Strength)>) -> bool {
    &&& forall|index: int| 0 <= index < all.len()
        ==> #[trigger] strengths.contains_key(*all[index].0) && strengths[*all[index].0] == *all[index].1
    &&& forall|entry: (String, Bucket)| #[trigger] strengths.contains_key(entry) ==> all.contains((&entry, &strengths[entry]))
}

/// True when each bucket of `times` is a usual time of `key`.
pub open spec fn all_usual(strengths: Map<(String, Bucket), Strength>, key: Seq<char>, times: Seq<Bucket>, today: Date, half_life_days: f64, min_score: f64) -> bool {
    forall|index: int| 0 <= index < times.len() ==> #[trigger] usual(strengths, key, times[index], today, half_life_days, min_score)
}

/// True when each of the first `count` entries of `all` with the key `key` has its bucket in
/// `times`, or a decayed score less than `min_score`.
pub open spec fn usual_checked(all: Seq<(&(String, Bucket), &Strength)>, count: int, key: Seq<char>, times: Seq<Bucket>, today: Date, half_life_days: f64, min_score: f64) -> bool {
    forall|seen: int| 0 <= seen < count && (*all[seen].0).0@ == key
        ==> times.contains((*all[seen].0).1) || scored(*all[seen].1, today, half_life_days, min_score, false)
}

/// True when each strength of `kept` is the strength of `strengths` with a decayed score of 0.01 or
/// more.
pub open spec fn kept_from(strengths: Map<(String, Bucket), Strength>, kept: Map<(String, Bucket), Strength>, today: Date, half_life_days: f64) -> bool {
    forall|entry: (String, Bucket)| #[trigger] kept.contains_key(entry) ==> {
        &&& strengths.contains_key(entry)
        &&& kept[entry] == strengths[entry]
        &&& scored(strengths[entry], today, half_life_days, 0.01f64, true)
    }
}

/// True when each of the first `count` entries of `all` is in `kept`, or has a decayed score less
/// than 0.01.
pub open spec fn prune_checked(all: Seq<(&(String, Bucket), &Strength)>, count: int, kept: Map<(String, Bucket), Strength>, today: Date, half_life_days: f64) -> bool {
    forall|seen: int| 0 <= seen < count
        ==> kept.contains_key(*all[seen].0) || scored(*all[seen].1, today, half_life_days, 0.01f64, false)
}

/// `times` with `bucket` pushed contains `bucket` and each bucket of `times`.
pub proof fn lemma_pushed(times: Seq<Bucket>, bucket: Bucket)
    ensures
        times.push(bucket).contains(bucket),
        forall|other: Bucket| times.contains(other) ==> #[trigger] times.push(bucket).contains(other),
{
    assert(times.push(bucket)[times.len() as int] == bucket);
    assert forall|other: Bucket| times.contains(other) implies #[trigger] times.push(bucket).contains(other) by {
        let index = choose|index: int| 0 <= index < times.len() && times[index] == other;
        assert(times.push(bucket)[index] == other);
    }
}

/// The conversion from a list of entries has no spec function. Only Verus has this trait.
#[cfg(verus_only)]
impl vstd::std_specs::convert::FromSpecImpl<Vec<Entry>> for RequestHabits {
    open spec fn obeys_from_spec() -> bool {
        false
    }

    open spec fn from_spec(entries: Vec<Entry>) -> Self {
        arbitrary()
    }
}

/// The conversion to a list of entries has no spec function. Only Verus has this trait.
#[cfg(verus_only)]
impl vstd::std_specs::convert::FromSpecImpl<RequestHabits> for Vec<Entry> {
    open spec fn obeys_from_spec() -> bool {
        false
    }

    open spec fn from_spec(habits: RequestHabits) -> Self {
        arbitrary()
    }
}

} // verus!

/// Tests of buckets and request habit scores.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::fakes::house;

    /// The default half-life in days.
    const HALF_LIFE_DAYS: f64 = 14.0;
    /// The default lowest score of a usual time.
    const MIN_SCORE: f64 = 2.0;
    /// Weekdays from 08:00 to 09:00.
    const WEEKDAY_8: Bucket = Bucket {
        day: DayType::Weekdays,
        hour: 8,
    };

    /// The time zone of the reference house.
    fn zone() -> TimeZone {
        TimeZone::get(&house().config.time_zone).expect("the house has a time zone")
    }

    /// The instant of the local `date` and `time` in the house time zone.
    fn at(date: &str, time: &str) -> Timestamp {
        let local: jiff::civil::DateTime = format!("{date}T{time}").parse().expect("a local time");
        local.to_zoned(zone()).expect("a zoned time").timestamp()
    }

    /// The local date `date`.
    fn day(date: &str) -> Date {
        date.parse().expect("a date")
    }

    /// The model after one request of `key` on each of `dates` at `time`.
    fn observed(key: &str, dates: &[&str], time: &str) -> RequestHabits {
        let zone = zone();
        let mut habits = RequestHabits::default();
        for date in dates {
            habits = habits.observe(key.to_owned(), at(date, time), &zone, HALF_LIFE_DAYS);
        }
        habits
    }

    /// The usual times of `get_traffic` in `habits` on `date`.
    fn traffic_times(habits: &RequestHabits, date: &str) -> Vec<Bucket> {
        habits.usual_times("get_traffic", day(date), HALF_LIFE_DAYS, MIN_SCORE)
    }

    /// AC-11.1b-1.
    #[test]
    fn ac_11_1b_1_saturday_evening_is_weekends_20() {
        let zone = TimeZone::get(&house().config.time_zone).expect("the house has a time zone");
        // Saturday 20:15 in Chicago.
        let time: Timestamp = "2026-11-08T02:15:00Z".parse().expect("a timestamp");
        let expected = Bucket {
            day: DayType::Weekends,
            hour: 20,
        };
        assert_eq!(bucket(time, &zone), expected);
    }

    /// AC-11.3-1.
    #[test]
    fn ac_11_3_1_score_halves_after_one_half_life() {
        let found = decayed(4.0, day("2026-11-01"), day("2026-11-15"), HALF_LIFE_DAYS);
        assert!((found - 2.0).abs() < 1e-12, "decayed score {found}");
    }

    /// AC-11.3-2.
    #[test]
    fn ac_11_3_2_same_or_older_day_changes_nothing() {
        let habits = observed("get_traffic", &["2026-11-11"], "08:10");
        for (date, time) in [("2026-11-11", "08:50"), ("2026-11-10", "08:10")] {
            let again = habits.clone().observe(
                "get_traffic".to_owned(),
                at(date, time),
                &zone(),
                HALF_LIFE_DAYS,
            );
            assert_eq!(again, habits, "a request on {date} at {time}");
        }
    }

    /// AC-11.3-3.
    #[test]
    fn ac_11_3_3_saturday_request_counts_in_weekends() {
        let habits = observed("get_weather", &["2026-11-07"], "09:30");
        let strengths = &habits.strengths;
        let key = "get_weather".to_owned();
        let weekend = Bucket {
            day: DayType::Weekends,
            hour: 9,
        };
        let weekday = Bucket {
            day: DayType::Weekdays,
            hour: 9,
        };
        assert!(
            strengths.contains_key(&(key.clone(), weekend)),
            "{strengths:?}"
        );
        assert!(!strengths.contains_key(&(key, weekday)), "{strengths:?}");
    }

    /// AC-11.3b-1.
    #[test]
    fn ac_11_3b_1_three_weekday_mornings_are_a_usual_time() {
        let dates = ["2026-11-09", "2026-11-10", "2026-11-11"];
        let habits = observed("get_traffic", &dates, "08:10");
        assert_eq!(traffic_times(&habits, "2026-11-11"), [WEEKDAY_8]);
    }

    /// AC-11.3b-2.
    #[test]
    fn ac_11_3b_2_two_days_of_requests_are_no_usual_time() {
        let dates = ["2026-11-09"; 5]
            .into_iter()
            .chain(["2026-11-10"; 5])
            .collect::<Vec<&str>>();
        let habits = observed("get_traffic", &dates, "08:10");
        assert_eq!(traffic_times(&habits, "2026-11-10"), []);
    }

    /// AC-11.3b-3.
    #[test]
    fn ac_11_3b_3_three_days_within_one_half_life_are_a_usual_time() {
        let dates = ["2026-10-28", "2026-11-04", "2026-11-11"];
        let habits = observed("get_traffic", &dates, "08:10");
        assert_eq!(traffic_times(&habits, "2026-11-11"), [WEEKDAY_8]);
    }

    /// AC-11.3b-4.
    #[test]
    fn ac_11_3b_4_usual_time_fades_after_eight_days() {
        let dates = ["2026-11-09", "2026-11-10", "2026-11-11"];
        let habits = observed("get_traffic", &dates, "08:10");
        assert_eq!(traffic_times(&habits, "2026-11-18"), [WEEKDAY_8]);
        assert_eq!(traffic_times(&habits, "2026-11-19"), []);
    }

    /// AC-11.3b-5.
    #[test]
    fn ac_11_3b_5_prune_removes_only_faded_strengths() {
        let old = observed("get_weather", &["2026-08-01"], "09:30");
        let new = observed("get_traffic", &["2026-11-09"], "08:10");
        let mut both = old;
        both.strengths.extend(new.strengths.clone());
        assert_eq!(both.prune(day("2026-11-09"), HALF_LIFE_DAYS), new);
    }
}
