//! The plan of a mute request and the rule of its end.

use jiff::Timestamp;
use vstd::prelude::*;

use crate::domain::state::EntityId;
use crate::domain::state_cache;
use crate::domain::state_cache::StateCache;
use crate::domain::timers;

/// A mute request of System One.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct MuteRequest {
    /// The intent name.
    pub intent: String,
    /// The `duration` slot.
    pub duration: Option<String>,
    /// The transcript of the request.
    pub transcript: String,
}

/// The plan of a mute request.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// Turn on `switch`, for `seconds` if given.
    Mute {
        /// The mute switch of the satellite.
        switch: EntityId,
        /// The length of a timed mute.
        seconds: Option<u32>,
    },
    /// The request has no satellite, or the satellite has no mute switch.
    Missing,
    /// The duration does not read.
    Unclear,
}

/// A timed voice mute.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Mute {
    /// The satellite of the request.
    pub satellite: String,
    /// The mute switch of the satellite.
    pub switch: EntityId,
    /// The end of the mute.
    pub end: Timestamp,
    /// The `last_updated` of the `on` state of the mute call.
    pub updated: Timestamp,
}

/// The plan of `request` from `satellite`.
#[verus_spec(planned =>
    ensures
        match planned {
            Plan::Mute { switch, seconds } => satellite matches Some(name)
                && state_cache::satellite_switch(*cache, name@, switch)
                && (seconds matches Some(total) ==> request.duration is Some
                    && 1 <= total <= timers::LONGEST),
            _ => true,
        },
)]
#[must_use]
pub fn plan(request: &MuteRequest, cache: &StateCache, satellite: Option<&str>) -> Plan {
    let switch = match satellite {
        Some(satellite) => state_cache::mute_switch(cache, satellite),
        None => None,
    };
    let Some(switch) = switch else {
        return Plan::Missing;
    };
    match &request.duration {
        Some(text) => match timers::duration(text) {
            Some(seconds) => Plan::Mute {
                switch,
                seconds: Some(seconds),
            },
            None => Plan::Unclear,
        },
        None => Plan::Mute {
            switch,
            seconds: None,
        },
    }
}

/// True when the hot cache shows the switch of `mute` `on` with the `last_updated` of the mute.
#[verus_spec(found =>
    ensures
        found == still_muted(*cache, *mute),
)]
#[must_use]
pub fn unmutes(mute: &Mute, cache: &StateCache) -> bool {
    matches!(cache.states.get(mute.switch.0.as_str()), Some(state)
        if state.state.as_str() == "on"
            && state.last_updated.as_nanosecond() == mute.updated.as_nanosecond())
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
};

/// True when `cache` has the switch of `mute` `on` with the `last_updated` of the mute.
pub open spec fn still_muted(cache: StateCache, mute: Mute) -> bool {
    let key = crate::domain::contracts::str_key(mute.switch.0@);
    &&& cache.states@.contains_key(key)
    &&& cache.states@[key].state@ == "on"@
    &&& crate::domain::contracts::nanosecond(cache.states@[key].last_updated)
        == crate::domain::contracts::nanosecond(mute.updated)
}

} // verus!

/// Tests of the mute plan.
#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;
    use crate::testing::fakes::house;

    /// The Kitchen satellite.
    const KITCHEN: &str = "assist_satellite.carson_kitchen";

    /// The mute switch of the Kitchen satellite.
    const KITCHEN_MUTE: &str = "switch.carson_kitchen_mute_microphones";

    /// A mute request with the duration slot `duration`.
    fn request(duration: Option<&str>) -> MuteRequest {
        MuteRequest {
            intent: "mute".to_owned(),
            duration: duration.map(str::to_owned),
            transcript: "mute".to_owned(),
        }
    }

    /// AC-20.5-2.
    #[test]
    fn ac_20_5_2_plan_of_mute_request() {
        let cache = StateCache::load(house());
        let switch = EntityId(KITCHEN_MUTE.to_owned());
        let kitchen = Some(KITCHEN);
        assert_eq!(
            plan(&request(Some("ten minutes")), &cache, kitchen),
            Plan::Mute {
                switch: switch.clone(),
                seconds: Some(600),
            }
        );
        assert_eq!(
            plan(&request(None), &cache, kitchen),
            Plan::Mute {
                switch,
                seconds: None,
            }
        );
        assert_eq!(
            plan(&request(Some("half an hour")), &cache, kitchen),
            Plan::Unclear
        );
        assert_eq!(plan(&request(None), &cache, None), Plan::Missing);
    }

    /// The reference house with the Kitchen mute switch `value` since `updated`.
    fn kitchen_switch(value: &str, updated: Timestamp) -> StateCache {
        let mut house = house();
        let state = house
            .states
            .iter_mut()
            .find(|state| state.entity_id == KITCHEN_MUTE)
            .expect("the house has the Kitchen mute switch");
        state.state = value.to_owned();
        state.last_updated = updated;
        StateCache::load(house)
    }

    /// AC-20.5-3.
    #[test]
    fn ac_20_5_3_unmutes_only_the_state_of_the_mute() {
        let updated: Timestamp = "2026-11-28T10:00:00Z"
            .parse()
            .expect("the time is RFC 3339");
        let mute = Mute {
            satellite: KITCHEN.to_owned(),
            switch: EntityId(KITCHEN_MUTE.to_owned()),
            end: updated + SignedDuration::from_secs(600),
            updated,
        };
        let later = updated + SignedDuration::from_secs(1);
        assert!(unmutes(&mute, &kitchen_switch("on", updated)));
        assert!(!unmutes(&mute, &kitchen_switch("off", updated)));
        assert!(!unmutes(&mute, &kitchen_switch("on", later)));
    }
}
