//! Habit model: source, buckets, decay, scores, summary.

use serde_json::Map;
use serde_json::Value;
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::string::to_string_from_display_ensures;

#[cfg(verus_only)]
use crate::domain::contracts::attribute;
#[cfg(verus_only)]
use crate::domain::contracts::number;
#[cfg(verus_only)]
use crate::domain::contracts::text;
use crate::domain::state::Context;
use crate::domain::state::State;

/// The cause of a state change.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// A person, in HA or on the device.
    Manual,
    /// An automation or a script.
    Automation,
    /// Jarvis.
    Voice,
    /// Not known, for example a change from the HA history.
    Unknown,
}

/// What a state change did.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// The state became `on`.
    On,
    /// The state became `off`.
    Off,
    /// A media player started to play a show or a title.
    Play,
    /// A thermostat got a new target temperature.
    SetTemperature,
}

/// The source of a change with `context`. `jarvis_user_id` is the HA user of Jarvis.
#[verus_spec(found =>
    ensures
        found == source_of(*context, jarvis_user_id@),
)]
#[must_use]
pub fn source(context: &Context, jarvis_user_id: &str) -> Source {
    match (&context.user_id, &context.parent_id) {
        (Some(user), _) if user.as_str() == jarvis_user_id => Source::Voice,
        (_, Some(_)) => Source::Automation,
        _ => Source::Manual,
    }
}

/// The habit action of the change of an entity of `domain` from `old` to `new`, with its value:
/// `on` or `off` from the state, `play` with the show or the title, `set_temperature` with the new
/// temperature. `None` when no tracked field changes.
#[verus_spec(found =>
    ensures
        acted(domain@, *old, *new, found),
)]
#[must_use]
pub fn action(domain: &str, old: &State, new: &State) -> Option<(Action, Option<String>)> {
    if domain == "media_player" && new.state.as_str() == "playing" {
        let shown = show(&new.attributes);
        if old.state.as_str() != "playing" || show(&old.attributes) != shown {
            return match shown {
                Some(show) => Some((Action::Play, Some(show.to_owned()))),
                None => Some((Action::Play, None)),
            };
        }
    }
    if domain == "climate" {
        let value = new_temperature(&old.attributes, &new.attributes);
        if value.is_some() {
            return Some((Action::SetTemperature, value));
        }
    }
    switched(&old.state, &new.state)
}

/// `on` or `off` when the state changes from `old` to that state.
#[verus_spec(found =>
    ensures
        old@ == new@ ==> found is None,
        match found {
            Some((Action::On, None)) => old@ != new@ && new@ == "on"@,
            Some((Action::Off, None)) => old@ != new@ && new@ == "off"@,
            Some(_) => false,
            None => true,
        },
)]
fn switched(old: &str, new: &str) -> Option<(Action, Option<String>)> {
    if old == new {
        None
    } else if new == "on" {
        Some((Action::On, None))
    } else if new == "off" {
        Some((Action::Off, None))
    } else {
        None
    }
}

/// The text of the series title in `attributes`, else of the title.
#[verus_spec(found =>
    ensures
        texts(found) == show_of(*attributes),
)]
fn show(attributes: &Map<String, Value>) -> Option<&str> {
    match text_at_key(attributes, "media_series_title") {
        None => text_at_key(attributes, "media_title"),
        series => series,
    }
}

/// The text of the string at `key` in `attributes`.
#[verus_spec(found =>
    ensures
        texts(found) == text_at(*attributes, key),
)]
fn text_at_key<'map>(attributes: &'map Map<String, Value>, key: &str) -> Option<&'map str> {
    match attributes.get(key) {
        Some(value) => value.as_str(),
        None => None,
    }
}

/// The new `temperature` of `new` as text, or `None` when it is not a number or equals the one of
/// `old`.
#[verus_spec(found =>
    ensures
        found matches Some(value) ==> shown(*new, value),
        attribute(*old, "temperature") == attribute(*new, "temperature") ==> found is None,
)]
fn new_temperature(old: &Map<String, Value>, new: &Map<String, Value>) -> Option<String> {
    let temperature = new.get("temperature")?;
    match old.get("temperature") {
        Some(previous) if previous.eq(temperature) => return None,
        _ => {}
    }
    let degrees = temperature.as_f64()?;
    Some(degrees.to_string())
}

verus! {

/// The source of a change with `context`: the Jarvis user, then a parent, then any other case.
pub open spec fn source_of(context: Context, jarvis_user_id: Seq<char>) -> Source {
    if context.user_id matches Some(user) && user@ == jarvis_user_id {
        Source::Voice
    } else if context.parent_id is Some {
        Source::Automation
    } else {
        Source::Manual
    }
}

/// The texts of an optional string.
pub open spec fn texts(found: Option<&str>) -> Option<Seq<char>> {
    match found {
        Some(found) => Some(found@),
        None => None,
    }
}

/// The texts of an optional owned string.
pub open spec fn strings(found: Option<String>) -> Option<Seq<char>> {
    match found {
        Some(found) => Some(found@),
        None => None,
    }
}

/// The text of the string at `key` in `attributes`.
pub open spec fn text_at(attributes: Map<String, Value>, key: &str) -> Option<Seq<char>> {
    match attribute(attributes, key) {
        Some(value) => text(value),
        None => None,
    }
}

/// The text of the series title in `attributes`, else of the title.
pub open spec fn show_of(attributes: Map<String, Value>) -> Option<Seq<char>> {
    match text_at(attributes, "media_series_title") {
        None => text_at(attributes, "media_title"),
        series => series,
    }
}

/// True when a media player of `domain` starts to play or plays a new show.
pub open spec fn played(domain: Seq<char>, old: State, new: State) -> bool {
    &&& domain == "media_player"@
    &&& new.state@ == "playing"@
    &&& old.state@ != "playing"@ || show_of(old.attributes) != show_of(new.attributes)
}

/// True when `value` is the text of the number at `temperature` in `attributes`.
pub open spec fn shown(attributes: Map<String, Value>, value: String) -> bool {
    &&& attribute(attributes, "temperature") matches Some(temperature)
    &&& number(temperature) matches Some(degrees)
    &&& to_string_from_display_ensures::<f64>(&degrees, value)
}

/// True when no tracked field changes from `old` to `new`.
pub open spec fn unchanged(old: State, new: State) -> bool {
    &&& old.state@ == new.state@
    &&& show_of(old.attributes) == show_of(new.attributes)
    &&& attribute(old.attributes, "temperature") == attribute(new.attributes, "temperature")
}

/// True when `found` is a habit action of the change of an entity of `domain` from `old` to `new`.
pub open spec fn acted(domain: Seq<char>, old: State, new: State, found: Option<(Action, Option<String>)>) -> bool {
    &&& played(domain, old, new) ==> (found matches Some((Action::Play, _)))
    &&& unchanged(old, new) ==> found is None
    &&& match found {
        Some((Action::Play, value)) => played(domain, old, new) && strings(value) == show_of(new.attributes),
        Some((Action::SetTemperature, Some(value))) => domain == "climate"@ && shown(new.attributes, value),
        Some((Action::On, None)) => old.state@ != new.state@ && new.state@ == "on"@,
        Some((Action::Off, None)) => old.state@ != new.state@ && new.state@ == "off"@,
        Some(_) => false,
        None => true,
    }
}

} // verus!

/// Tests of source and action.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use jiff::Timestamp;

    use super::*;

    /// The HA user of Jarvis.
    const JARVIS: &str = "jarvis-user";

    /// A context with `user_id` and `parent_id`.
    fn context(user_id: Option<&str>, parent_id: Option<&str>) -> Context {
        Context {
            id: "change".to_owned(),
            user_id: user_id.map(str::to_owned),
            parent_id: parent_id.map(str::to_owned),
        }
    }

    /// A state `state` with the JSON object `attributes`.
    fn state(state: &str, attributes: &Value) -> State {
        State {
            entity_id: "entity.test".to_owned(),
            state: state.to_owned(),
            attributes: attributes.as_object().cloned().unwrap_or_default(),
            last_updated: Timestamp::UNIX_EPOCH,
            context: context(None, None),
        }
    }

    /// AC-11.1-1.
    #[test]
    fn ac_11_1_1_person_user_is_manual() {
        assert_eq!(source(&context(Some("alex"), None), JARVIS), Source::Manual);
    }

    /// AC-11.1-2.
    #[test]
    fn ac_11_1_2_parent_is_automation() {
        let parent = Some("automation-run");
        assert_eq!(source(&context(None, parent), JARVIS), Source::Automation);
        let source = source(&context(Some("alex"), parent), JARVIS);
        assert_eq!(source, Source::Automation);
    }

    /// AC-11.1-3.
    #[test]
    fn ac_11_1_3_no_user_and_no_parent_is_manual() {
        assert_eq!(source(&context(None, None), JARVIS), Source::Manual);
    }

    /// AC-11.1-4.
    #[test]
    fn ac_11_1_4_jarvis_user_is_voice() {
        assert_eq!(source(&context(Some(JARVIS), None), JARVIS), Source::Voice);
    }

    /// AC-11.1-5.
    #[test]
    fn ac_11_1_5_light_turned_on_is_on() {
        let found = action("light", &state("off", &json!({})), &state("on", &json!({})));
        assert_eq!(found, Some((Action::On, None)));
    }

    /// AC-11.1-6.
    #[test]
    fn ac_11_1_6_playing_media_is_play_with_the_show() {
        let idle = state("idle", &json!({}));
        let bluey = Some((Action::Play, Some("Bluey".to_owned())));
        let title = state("playing", &json!({ "media_title": "Bluey" }));
        assert_eq!(action("media_player", &idle, &title), bluey);
        let episode = json!({ "media_series_title": "Bluey", "media_title": "Sleepytime" });
        let episode = state("playing", &episode);
        assert_eq!(action("media_player", &idle, &episode), bluey);
    }

    /// AC-11.1-7.
    #[test]
    fn ac_11_1_7_new_temperature_is_set_temperature() {
        let old = state("cool", &json!({ "temperature": 72 }));
        let new = state("cool", &json!({ "temperature": 70 }));
        let found = action("climate", &old, &new);
        assert_eq!(found, Some((Action::SetTemperature, Some("70".to_owned()))));
    }

    /// AC-11.1-8.
    #[test]
    fn ac_11_1_8_untracked_attribute_is_none() {
        let old = state("on", &json!({ "brightness": 100, "temperature": 70 }));
        let new = state("on", &json!({ "brightness": 200, "temperature": 70 }));
        assert_eq!(action("climate", &old, &new), None);
        assert_eq!(action("light", &old, &new), None);
        let old = state(
            "playing",
            &json!({ "media_title": "Bluey", "volume_level": 0.2 }),
        );
        let new = state(
            "playing",
            &json!({ "media_title": "Bluey", "volume_level": 0.5 }),
        );
        assert_eq!(action("media_player", &old, &new), None);
    }
}
