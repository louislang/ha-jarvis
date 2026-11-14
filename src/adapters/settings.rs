//! The fields of the settings form: read and edit of `jarvis.toml` that keep its comments and
//! order.

use std::collections::BTreeMap;

use anyhow::Context;
use serde_json::Value as Json;
use toml_edit::Array;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::Value;

use crate::ports::Setting;

/// The form fields in their groups, in the order of the form. No secret, path or address is a
/// field.
pub const FIELDS: [(&str, &[&str]); 8] = [
    (
        "Speech",
        &[
            "asr.vad_threshold",
            "asr.vad_min_silence_ms",
            "asr.early_commit_ms",
            "asr.endpoint_ms",
            "asr.max_utterance_ms",
        ],
    ),
    ("Voice", &["tts.voice", "tts.num_steps"]),
    ("System One", &["model.threshold", "model.commit_threshold"]),
    (
        "System Two",
        &[
            "openai.model",
            "openai.timeout_ms",
            "openai.max_output_tokens",
            "openai.max_tool_calls",
        ],
    ),
    (
        "Dialogs",
        &["dialog.max_turns", "dialog.timeout_s", "dialog.cancel"],
    ),
    ("Announce", &["announce.margin_ms", "announce.idle_wait_s"]),
    ("Habits", &["habits.half_life_days", "habits.min_score"]),
    ("Log", &["log.audio_days", "log.record_days"]),
];

/// Each field of [`FIELDS`] with its value in `text`, in the order of [`FIELDS`].
///
/// # Errors
///
/// `text` is not TOML, or it has no value for a field.
pub fn read(text: &str) -> anyhow::Result<Vec<Setting>> {
    let root: toml::Value = toml::from_str(text)?;
    let mut settings = Vec::new();
    for (group, names) in FIELDS {
        for &name in names {
            let value = name
                .split('.')
                .try_fold(&root, |value, key| value.get(key))
                .with_context(|| format!("{name} is missing"))?;
            settings.push(Setting {
                group,
                name,
                value: serde_json::to_value(value)?,
            });
        }
    }
    Ok(settings)
}

/// `text` with each field of `changes` set to its new value. All other text stays, also the
/// comments. A new value keeps the TOML type of the old value.
///
/// # Errors
///
/// `text` is not TOML, a name is not in [`FIELDS`] or not in `text`, or a value does not fit
/// the TOML type of the old value. The error names the field.
pub fn edit(text: &str, changes: BTreeMap<String, Json>) -> Result<String, String> {
    let mut document: DocumentMut = text.parse().map_err(|error| format!("{error}"))?;
    for (name, new) in changes {
        let old = field(&mut document, &name)?;
        let mut value = converted(old, &new)
            .ok_or_else(|| format!("{name} needs a value of type {}", old.type_name()))?;
        *value.decor_mut() = old.decor().clone();
        *old = value;
    }
    Ok(document.to_string())
}

/// The value of the field `name` in `document`.
///
/// # Errors
///
/// `name` is not in [`FIELDS`] or not in `document`. The error names the field.
fn field<'document>(
    document: &'document mut DocumentMut,
    name: &str,
) -> Result<&'document mut Value, String> {
    if !FIELDS.iter().any(|(_, names)| names.contains(&name)) {
        return Err(format!("{name} is not a settings field"));
    }
    name.split('.')
        .try_fold(document.as_item_mut(), |item, key| item.get_mut(key))
        .and_then(Item::as_value_mut)
        .ok_or_else(|| format!("{name} is missing"))
}

/// `new` as a TOML value of the type of `old`, or `None` when the kinds do not match. A JSON
/// number for a float becomes a float.
fn converted(old: &Value, new: &Json) -> Option<Value> {
    match (old, new) {
        (Value::Integer(_), Json::Number(number)) => number.as_i64().map(Value::from),
        (Value::Float(_), Json::Number(number)) => number.as_f64().map(Value::from),
        (Value::String(_), Json::String(string)) => Some(Value::from(string.as_str())),
        (Value::Array(array), Json::Array(items)) => {
            let strings: Option<Vec<&str>> = items.iter().map(Json::as_str).collect();
            strings.map(|strings| Value::Array(restyled(array, &strings)))
        }
        _ => None,
    }
}

/// `strings` in the layout of `old`: its comments before `]`, its trailing comma, and the
/// comments of the item at each position that stays.
fn restyled(old: &Array, strings: &[&str]) -> Array {
    let mut array = old.clone();
    array.clear();
    for string in strings {
        array.push(*string);
    }
    for (index, item) in array.iter_mut().enumerate() {
        if let Some(before) = old.get(index) {
            *item.decor_mut() = before.decor().clone();
        }
    }
    array
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// The `jarvis.toml` of the repository.
    const REPOSITORY: &str = include_str!("../../jarvis.toml");

    /// The changes that set `name` to `value`.
    fn change(name: &str, value: Json) -> BTreeMap<String, Json> {
        BTreeMap::from([(name.to_owned(), value)])
    }

    #[test]
    fn ac_12_9d_1_read_gives_the_fields_in_order_with_the_file_values() -> anyhow::Result<()> {
        let settings = read(REPOSITORY)?;
        let names: Vec<&str> = settings.iter().map(|setting| setting.name).collect();
        let expected: Vec<&str> = FIELDS
            .iter()
            .flat_map(|(_, names)| *names)
            .copied()
            .collect();
        assert_eq!(names, expected, "names in the order of FIELDS");
        assert!(
            names.iter().all(|name| !name.ends_with("_env")),
            "no secret in {names:?}"
        );
        let values: Json = settings.into_iter().map(|setting| setting.value).collect();
        let cancel = ["never mind", "cancel", "forget it", "stop"];
        let file = json!([
            0.65,
            60,
            100,
            600,
            8000,
            6,
            4,
            0.6,
            0.9,
            "gpt-5.4-mini",
            8000,
            150,
            5,
            3,
            15,
            cancel,
            300,
            30,
            14,
            2.0,
            30,
            90
        ]);
        assert_eq!(values, file, "values of the file");
        Ok(())
    }

    #[test]
    fn ac_12_9d_2_edit_changes_only_that_line_and_keeps_its_comment() {
        let input = REPOSITORY.replace("endpoint_ms = 600", "endpoint_ms = 600 # end of speech");
        let changed = edit(&input, change("asr.endpoint_ms", json!(700))).expect("valid change");
        let expected = input.replace("600 # end of speech", "700 # end of speech");
        assert_eq!(changed, expected, "only the endpoint_ms line differs");
    }

    #[test]
    fn ac_12_9d_2_edit_keeps_the_comments_inside_an_array() {
        let text = "[dialog]\ncancel = [\n    \"never mind\", # the usual\n    \"cancel\",\n    # more later\n]\n";
        let edited = edit(text, change("dialog.cancel", json!(["stop", "cancel"])));
        let expected = "[dialog]\ncancel = [\n    \"stop\", # the usual\n    \"cancel\",\n    # more later\n]\n";
        assert_eq!(edited.as_deref(), Ok(expected));
    }

    #[test]
    fn ac_12_9d_3_edit_refuses_fields_outside_the_form() {
        for name in ["ha.url", "ha.token_env"] {
            let error = edit(REPOSITORY, change(name, json!("x"))).expect_err("not a field");
            assert!(error.contains(name), "{error:?} names {name}");
        }
    }

    #[test]
    fn ac_12_9d_4_edit_keeps_the_toml_type() {
        let error = edit(REPOSITORY, change("asr.endpoint_ms", json!("700")))
            .expect_err("a string for an integer");
        assert!(
            error.contains("asr.endpoint_ms"),
            "{error:?} names the field"
        );
        let changed = edit(REPOSITORY, change("habits.min_score", json!(3))).expect("a number");
        assert!(changed.contains("min_score = 3.0"), "{changed}");
    }
}
