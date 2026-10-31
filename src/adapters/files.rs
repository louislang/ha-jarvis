//! `Store<T>` with TOML or JSON files and atomic write.

use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::io;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tempfile::NamedTempFile;

use crate::ports::Store;

/// The file format, from the file extension.
#[derive(Clone, Copy, Debug)]
enum Format {
    /// `.toml`.
    Toml,
    /// `.json`.
    Json,
}

/// A value in one TOML or JSON file.
#[derive(Clone, Debug)]
pub struct FileStore {
    /// The file.
    path: PathBuf,
    /// The format of the file.
    format: Format,
}

impl FileStore {
    /// The store of the file at `path`, in the format of its extension.
    ///
    /// # Errors
    ///
    /// The extension is not `.toml` or `.json`. The error names the path.
    pub fn try_new(path: PathBuf) -> anyhow::Result<Self> {
        let format = match path.extension().and_then(OsStr::to_str) {
            Some("toml") => Format::Toml,
            Some("json") => Format::Json,
            _ => anyhow::bail!("{} is not a .toml or .json file", path.display()),
        };
        Ok(Self { path, format })
    }

    /// Writes `value` to `file` in the format of the store.
    ///
    /// # Errors
    ///
    /// `value` cannot be serialized, or the write fails.
    fn write<T: Serialize>(&self, file: &mut File, value: &T) -> anyhow::Result<()> {
        let mut writer = BufWriter::new(file);
        match self.format {
            Format::Toml => writer.write_all(toml::to_string_pretty(value)?.as_bytes())?,
            Format::Json => serde_json::to_writer_pretty(&mut writer, value)?,
        }
        writer.flush()?;
        Ok(())
    }
}

impl<T: Serialize + DeserializeOwned> Store<T> for FileStore {
    fn load(&self) -> anyhow::Result<Option<T>> {
        let context = || format!("cannot load {}", self.path.display());
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(context),
        };
        let value = match self.format {
            Format::Toml => toml::from_str(&text).with_context(context)?,
            Format::Json => serde_json::from_str(&text).with_context(context)?,
        };
        Ok(Some(value))
    }

    fn save(&self, value: &T) -> anyhow::Result<()> {
        // A rename is atomic only inside one file system, so the temporary file is in the
        // folder of the target.
        let folder = self.path.parent().unwrap_or(Path::new(""));
        let mut file = NamedTempFile::new_in(folder)?;
        self.write(file.as_file_mut(), value)?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        Ok(())
    }
}

/// Tests of `FileStore`.
#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    /// The saved data: use counts by template.
    type Counts = BTreeMap<String, u32>;

    /// Two use counts.
    fn counts() -> Counts {
        BTreeMap::from([("Done.".to_owned(), 3), ("Okay.".to_owned(), 1)])
    }

    /// Saves `counts()` to the file `name` in a temporary folder and loads it again.
    fn round_trip(name: &str) -> anyhow::Result<Option<Counts>> {
        let folder = tempfile::tempdir()?;
        let store = FileStore::try_new(folder.path().join(name))?;
        store.save(&counts())?;
        store.load()
    }

    /// AC-4.3-1.
    #[test]
    fn ac_4_3_1_toml_round_trip_is_equal() -> anyhow::Result<()> {
        assert_eq!(round_trip("pools.toml")?, Some(counts()));
        Ok(())
    }

    /// AC-4.3-2.
    #[test]
    fn ac_4_3_2_json_round_trip_is_equal() -> anyhow::Result<()> {
        assert_eq!(round_trip("pools.json")?, Some(counts()));
        Ok(())
    }

    /// AC-4.3-3.
    #[test]
    fn ac_4_3_3_stopped_save_keeps_last_data() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let store = FileStore::try_new(folder.path().join("pools.json"))?;
        store.save(&counts())?;
        // A JSON key must be a string, so the save stops after it writes `{`.
        let invalid: BTreeMap<Vec<u8>, u32> = BTreeMap::from([(vec![1], 1)]);
        store.save(&invalid).expect_err("a list is not a JSON key");
        let loaded: Option<Counts> = store.load()?;
        assert_eq!(loaded, Some(counts()));
        Ok(())
    }

    /// AC-4.3-4.
    #[test]
    fn ac_4_3_4_missing_file_loads_none() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let store = FileStore::try_new(folder.path().join("pools.json"))?;
        let loaded: Option<Counts> = store.load()?;
        assert_eq!(loaded, None);
        Ok(())
    }

    /// AC-4.3-5.
    #[test]
    fn ac_4_3_5_other_extension_names_path() {
        let error = FileStore::try_new(PathBuf::from("state/pools.yaml"))
            .expect_err("yaml is not a store format")
            .to_string();
        assert!(
            error.contains("state/pools.yaml"),
            "error has no path: {error}"
        );
    }

    /// AC-4.3-6.
    #[test]
    fn ac_4_3_6_parse_error_names_path() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("pools.toml");
        fs::write(&path, "count = [")?;
        let error = Store::<Counts>::load(&FileStore::try_new(path.clone())?)
            .expect_err("the file is not TOML")
            .to_string();
        assert!(
            error.contains(&path.display().to_string()),
            "error has no path: {error}"
        );
        Ok(())
    }
}
