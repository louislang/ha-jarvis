//! Request habits: the time bucket of a request and decayed day scores.

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
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
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

/// The request habit model: the strength of each request key and bucket.
#[verus_verify]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RequestHabits {
    /// The strength of each request key and bucket.
    pub strengths: BTreeMap<(String, Bucket), Strength>,
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

} // verus!

/// Tests of buckets and request habit scores.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::fakes::house;

    /// The default half-life in days.
    const HALF_LIFE_DAYS: f64 = 14.0;

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
}
