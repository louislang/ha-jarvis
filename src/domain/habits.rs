//! Request habits: the time bucket of a request.

use jiff::Timestamp;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use vstd::prelude::*;

#[cfg(verus_only)]
use crate::domain::contracts::hour;
#[cfg(verus_only)]
use crate::domain::contracts::local;
#[cfg(verus_only)]
use crate::domain::contracts::weekday;

/// The day type of a bucket.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DayType {
    /// Monday to Friday.
    Weekdays,
    /// Saturday and Sunday.
    Weekends,
}

/// One time slot for habits.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bucket {
    /// The day type of the time.
    pub day: DayType,
    /// The hour of the time, 0 to 23.
    pub hour: i8,
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

verus! {

/// The day type of `day`.
pub open spec fn day_of(day: Weekday) -> DayType {
    match day {
        Weekday::Saturday | Weekday::Sunday => DayType::Weekends,
        _ => DayType::Weekdays,
    }
}

} // verus!

/// Tests of bucket.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::fakes::house;

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
}
