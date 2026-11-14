//! The fields of the settings form: read and edit of `jarvis.toml` that keep its comments and
//! order, and the backup, trial start marker and rollback of a save.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use serde_json::Value as Json;
use toml_edit::Array;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::Value;

use crate::ports::Setting;

/// The exit code after which systemd starts Jarvis again.
pub const RESTART: u8 = 75;

/// The rollback reason when the trial start left no error text.
const CRASHED: &str = "start after save crashed";

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

/// The file `<config><suffix>`, for example the backup `<config>.bak`.
#[must_use]
pub fn beside(config: &Path, suffix: &str) -> PathBuf {
    let mut path = config.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

/// Removes the file at `path` when it exists.
///
/// # Errors
///
/// The file exists and cannot be removed.
fn removed(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Removes `<config>.rollback`, so that the status has no rollback message after the next start.
///
/// # Errors
///
/// The file exists and cannot be removed.
pub fn clear_rollback(config: &Path) -> anyhow::Result<()> {
    Ok(removed(&beside(config, ".rollback"))?)
}

/// Starts with `config`. When `<config>.trying` exists, the trial start after a save failed or
/// crashed: puts `<config>.bak` back and keeps the reason in `<config>.rollback`. Else, when
/// `<config>.bak` exists, makes an empty `<config>.trying`: this start is the trial start.
///
/// # Errors
///
/// A file cannot be read, written, removed or renamed.
pub fn begin(config: &Path) -> anyhow::Result<()> {
    let (backup, trying) = (beside(config, ".bak"), beside(config, ".trying"));
    if trying.exists() {
        let error = fs::read_to_string(&trying)?;
        let reason = if error.is_empty() { CRASHED } else { &error };
        fs::write(beside(config, ".rollback"), reason)?;
        // Before the rename: a crash between them gives one more trial start, not a loop.
        fs::remove_file(&trying)?;
        fs::rename(&backup, config)?;
        tracing::error!("settings rolled back: {reason}");
    } else if backup.exists() {
        fs::write(&trying, "")?;
    }
    Ok(())
}

/// Ends the trial start of `config`: removes `<config>.trying`, then `<config>.bak`.
///
/// # Errors
///
/// A file exists and cannot be removed.
pub fn confirm(config: &Path) -> io::Result<()> {
    removed(&beside(config, ".trying"))?;
    removed(&beside(config, ".bak"))
}

/// Writes `error` to `<config>.trying` when it exists. Gives whether it exists, that is whether
/// the trial start failed.
#[must_use]
pub fn failed_trial(config: &Path, error: &str) -> bool {
    let trying = beside(config, ".trying");
    let exists = trying.exists();
    if exists {
        // An empty marker rolls back too, with the reason of a crash.
        let _ = fs::write(&trying, error);
    }
    exists
}

/// The exit code of a panic: [`RESTART`] during the trial start of `config`, else 101.
#[must_use]
pub fn panic_code(config: &Path) -> i32 {
    if beside(config, ".trying").exists() {
        i32::from(RESTART)
    } else {
        101
    }
}

/// The status message of the last rollback of `config`, when `<config>.rollback` exists.
#[must_use]
pub fn rolled_back(config: &Path) -> Option<String> {
    let reason = fs::read_to_string(beside(config, ".rollback")).ok()?;
    Some(format!("settings rolled back: {reason}"))
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

    /// AC-12.9i-3.
    #[test]
    fn ac_12_9i_3_begin_after_a_failed_trial_puts_the_backup_back() -> anyhow::Result<()> {
        for (error, reason) in [("bind failed", "bind failed"), ("", CRASHED)] {
            let folder = tempfile::tempdir()?;
            let config = folder.path().join("jarvis.toml");
            fs::write(&config, "new")?;
            fs::write(beside(&config, ".bak"), "old")?;
            fs::write(beside(&config, ".trying"), error)?;
            begin(&config)?;
            assert_eq!(fs::read_to_string(&config)?, "old");
            assert_eq!(fs::read_to_string(beside(&config, ".rollback"))?, reason);
            let left = [".bak", ".trying"].map(|suffix| beside(&config, suffix).exists());
            assert_eq!(left, [false, false], "the backup and the marker are gone");
        }
        Ok(())
    }
}
