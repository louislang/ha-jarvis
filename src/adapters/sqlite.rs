//! `InteractionLog` with SQLite.

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Params;
use rusqlite::params;
use rusqlite::types::FromSql;

use crate::domain::feedback::Interaction;
use crate::domain::feedback::Mark;
use crate::domain::speech::Audio;
use crate::ports::Change;
use crate::ports::Filter;
use crate::ports::InteractionLog;
use crate::ports::Page;

/// The migration steps of the schema. The step at index `n` moves the schema from version `n` to
/// version `n + 1`.
const STEPS: [&str; 2] = [
    "CREATE TABLE interactions (id INTEGER PRIMARY KEY, time INTEGER NOT NULL, record TEXT NOT NULL, audio BLOB)",
    "CREATE TABLE marks (interaction INTEGER NOT NULL, mark TEXT NOT NULL)",
];

/// The JSON record of an interaction with its marks, in the order that they were stored.
const RECORD: &str = "json_set(record, '$.marks', json((SELECT json_group_array(json(mark) ORDER BY rowid) FROM marks WHERE interaction = interactions.id)))";

/// The condition of the interactions with a bad mark.
const BAD: &str =
    "id IN (SELECT interaction FROM marks WHERE json_extract(mark, '$.value') = 'Bad')";

/// The sample rate of the stored audio.
const RATE: u32 = 16_000;

/// The interaction log in one SQLite database file. Each interaction is a JSON record.
pub struct SqliteLog {
    /// The open database.
    connection: Arc<Mutex<Connection>>,
}

impl SqliteLog {
    /// Opens the database at `path`, which it creates when it does not exist, and migrates its
    /// schema to the current version in one transaction.
    ///
    /// # Errors
    ///
    /// The database cannot open, a migration step fails, or the schema version of the database is
    /// newer than the code.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut connection = Connection::open(path)?;
        let stored: usize =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let current = STEPS.len();
        anyhow::ensure!(
            stored <= current,
            "the schema version {stored} of the database is newer than the version {current} of Jarvis"
        );
        let transaction = connection.transaction()?;
        for step in &STEPS[stored..] {
            transaction.execute_batch(step)?;
        }
        transaction.pragma_update(None, "user_version", current)?;
        transaction.commit()?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// The result of `work` on the database, in a blocking thread.
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("an earlier database call panicked"))?;
            Ok(work(&connection)?)
        })
        .await?
    }

    /// The interactions with their marks that `clauses` select with `values`.
    async fn records(
        &self,
        clauses: String,
        values: impl Params + Send + 'static,
    ) -> anyhow::Result<Vec<Interaction>> {
        let query = format!("SELECT {RECORD} FROM interactions {clauses}");
        let records = self
            .run(move |connection| {
                let mut statement = connection.prepare(&query)?;
                let rows = statement.query_map(values, |row| row.get(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .await?;
        let interactions = records.iter().map(|record| serde_json::from_str(record));
        Ok(interactions.collect::<Result<_, _>>()?)
    }

    /// The value of `column` of the interaction `id`, or `None` when the interaction does not
    /// exist.
    async fn column<T: FromSql + Send + 'static>(
        &self,
        column: &str,
        id: u64,
    ) -> anyhow::Result<Option<T>> {
        let query = format!("SELECT {column} FROM interactions WHERE id = ?1");
        self.run(move |connection| {
            connection
                .query_row(&query, [id], |row| row.get(0))
                .optional()
        })
        .await
    }
}

#[async_trait]
impl InteractionLog for SqliteLog {
    async fn append(&self, interaction: Interaction, audio: Option<Audio>) -> anyhow::Result<()> {
        let record = serde_json::to_string(&interaction)?;
        let bytes: Option<Vec<u8>> = audio.map(|audio| {
            audio
                .samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect()
        });
        let (id, time) = (interaction.id, interaction.time.as_millisecond());
        self.run(move |connection| {
            let insert =
                "INSERT INTO interactions (id, time, record, audio) VALUES (?1, ?2, ?3, ?4)";
            connection.execute(insert, params![id, time, record, bytes])
        })
        .await?;
        Ok(())
    }

    async fn update(&self, id: u64, change: Change) -> anyhow::Result<()> {
        let Change::Response(spoken) = change;
        let spoken = serde_json::to_string(&spoken)?;
        self.run(move |connection| {
            let add = "UPDATE interactions SET record = json_insert(record, '$.responses[#]', json(?2)) WHERE id = ?1";
            connection.execute(add, params![id, spoken])
        })
        .await?;
        Ok(())
    }

    async fn list(&self, filter: Filter, page: Page) -> anyhow::Result<Vec<Interaction>> {
        let kept = match filter {
            Filter::All => String::new(),
            Filter::Bad => format!("WHERE {BAD}"),
        };
        let offset = page.number.saturating_mul(page.size);
        let clauses = format!("{kept} ORDER BY time DESC, id DESC LIMIT ?1 OFFSET ?2");
        self.records(clauses, (page.size, offset)).await
    }

    async fn get(&self, id: u64) -> anyhow::Result<Option<Interaction>> {
        let mut found = self.records("WHERE id = ?1".to_owned(), (id,)).await?;
        Ok(found.pop())
    }

    async fn audio(&self, id: u64) -> anyhow::Result<Option<Audio>> {
        let bytes: Option<Option<Vec<u8>>> = self.column("audio", id).await?;
        Ok(bytes.flatten().map(|bytes| {
            let (pairs, _) = bytes.as_chunks::<2>();
            let samples = pairs.iter().map(|pair| i16::from_le_bytes(*pair));
            Audio {
                samples: samples.collect(),
                rate: RATE,
            }
        }))
    }

    async fn mark(&self, mark: Mark) -> anyhow::Result<()> {
        let (id, kept) = (mark.interaction, mark.value.is_some());
        let record = serde_json::to_string(&mark)?;
        self.run(move |connection| {
            let remove = "DELETE FROM marks WHERE interaction = ?1 AND json_extract(mark, '$.target') = json_extract(?2, '$.target')";
            connection.execute(remove, params![id, record])?;
            if kept {
                let add = "INSERT INTO marks (interaction, mark) VALUES (?1, ?2)";
                connection.execute(add, params![id, record])?;
            }
            Ok(())
        })
        .await
    }

    async fn marked(&self) -> anyhow::Result<Vec<Interaction>> {
        let clauses = "WHERE id IN (SELECT interaction FROM marks) ORDER BY time, id";
        self.records(clauses.to_owned(), ()).await
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;
    use crate::domain::feedback::Action;
    use crate::domain::feedback::Mark;
    use crate::domain::feedback::Route;
    use crate::domain::feedback::Spoken;
    use crate::domain::feedback::Tagged;
    use crate::domain::feedback::Target;
    use crate::domain::feedback::Value;
    use crate::domain::state::ServiceCall;

    /// The first page of 50 interactions.
    const PAGE: Page = Page {
        number: 0,
        size: 50,
    };

    /// A log in a new temporary folder, and the folder.
    fn log() -> (SqliteLog, TempDir) {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let log = SqliteLog::open(&folder.path().join("jarvis.db")).expect("the log opens");
        (log, folder)
    }

    /// The mark `value` on `target` of the interaction `id`, with the correction `tv_show`.
    fn mark(id: u64, target: Target, value: Option<Value>) -> Mark {
        Mark {
            interaction: id,
            target,
            value,
            correction: Some("tv_show".to_owned()),
            time: Timestamp::from_second(1).expect("a valid second"),
        }
    }

    /// The IDs of `interactions`.
    fn ids(interactions: &[Interaction]) -> Vec<u64> {
        interactions
            .iter()
            .map(|interaction| interaction.id)
            .collect()
    }

    /// The response `text` from the pool `pool`.
    fn spoken(pool: &str, text: &str) -> Spoken {
        let (pool, text) = (pool.to_owned(), text.to_owned());
        Spoken {
            template: text.clone(),
            text,
            pool,
        }
    }

    /// The interaction `id` at second `id`, with a value in every field.
    fn interaction(id: u64) -> Interaction {
        let office = "media_player.office".to_owned();
        let call = ServiceCall {
            domain: "media_player".to_owned(),
            service: "turn_on".to_owned(),
            target: vec![office.clone()],
            data: Map::from_iter([("volume".to_owned(), 0.5.into())]),
            return_response: false,
        };
        let slot = Tagged {
            name: "area".to_owned(),
            value: "office".to_owned(),
            start: 16,
            end: 22,
        };
        Interaction {
            id,
            time: Timestamp::from_second(id.cast_signed()).expect("a valid second"),
            satellite: Some("assist_satellite.office".to_owned()),
            transcript: "Turn on the TV in the office.".to_owned(),
            model_context: "area: office".to_owned(),
            model: "fixture".to_owned(),
            top_labels: vec![("tv_on".to_owned(), 0.75), ("unknown".to_owned(), 0.25)],
            slots: vec![slot],
            action: Action {
                intent: "tv_on".to_owned(),
                targets: vec![office],
            },
            decision: Route::Reply,
            service_calls: vec![call],
            responses: vec![spoken("tv_on", "Turning on the TV.")],
            latency_ms: Some(120),
            link: Some(1),
            marks: Vec::new(),
        }
    }

    /// AC-12.4-1.
    #[tokio::test]
    async fn ac_12_4_1_list_and_get_return_every_field() -> anyhow::Result<()> {
        let (log, _folder) = log();
        log.append(interaction(7), None).await?;
        assert_eq!(log.list(Filter::All, PAGE).await?, [interaction(7)]);
        assert_eq!(log.get(7).await?, Some(interaction(7)));
        Ok(())
    }

    /// AC-12.4-2.
    #[tokio::test]
    async fn ac_12_4_2_list_gives_one_page_newest_first() -> anyhow::Result<()> {
        let (log, _folder) = log();
        for id in 1..=60 {
            log.append(interaction(id), None).await?;
        }
        let listed = log.list(Filter::All, PAGE).await?;
        assert_eq!(ids(&listed), (11..=60).rev().collect::<Vec<u64>>());
        Ok(())
    }

    /// AC-12.4-3.
    #[test]
    fn ac_12_4_3_open_creates_the_database() -> anyhow::Result<()> {
        let (_log, folder) = log();
        let connection = Connection::open(folder.path().join("jarvis.db"))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 2);
        let count: u32 =
            connection.query_row("SELECT count(*) FROM interactions", [], |row| row.get(0))?;
        assert_eq!(count, 0);
        Ok(())
    }

    /// AC-12.4-4.
    #[test]
    fn ac_12_4_4_newer_schema_is_an_error() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("jarvis.db");
        Connection::open(&path)?.pragma_update(None, "user_version", 3)?;
        let Err(error) = SqliteLog::open(&path) else {
            panic!("a newer schema opens");
        };
        let expected = "the schema version 3 of the database is newer than the version 2 of Jarvis";
        assert_eq!(error.to_string(), expected);
        Ok(())
    }

    /// AC-12.4b-1.
    #[tokio::test]
    async fn ac_12_4b_1_update_adds_the_announce() -> anyhow::Result<()> {
        let (log, _folder) = log();
        log.append(interaction(7), None).await?;
        let announce = spoken("announce", "The show is on.");
        log.update(7, Change::Response(announce.clone())).await?;
        let mut expected = interaction(7);
        expected.responses.push(announce);
        assert_eq!(log.list(Filter::All, PAGE).await?, [expected]);
        Ok(())
    }

    /// AC-12.4b-2.
    #[tokio::test]
    async fn ac_12_4b_2_audio_gives_the_stored_samples() -> anyhow::Result<()> {
        let (log, _folder) = log();
        let audio = Audio {
            samples: vec![0, 1, -1, i16::MAX, i16::MIN, 300],
            rate: 16_000,
        };
        log.append(interaction(7), Some(audio.clone())).await?;
        log.append(interaction(8), None).await?;
        assert_eq!(log.audio(7).await?, Some(audio));
        assert_eq!(log.audio(8).await?, None);
        Ok(())
    }

    /// AC-12.5-1.
    #[tokio::test]
    async fn ac_12_5_1_marked_gives_the_correction() -> anyhow::Result<()> {
        let (log, _folder) = log();
        log.append(interaction(7), None).await?;
        log.append(interaction(8), None).await?;
        let bad = mark(7, Target::Action, Some(Value::Bad));
        log.mark(bad.clone()).await?;
        let mut expected = interaction(7);
        expected.marks.push(bad);
        assert_eq!(log.marked().await?, [expected]);
        Ok(())
    }

    /// AC-12.5-2.
    #[tokio::test]
    async fn ac_12_5_2_bad_filter_keeps_bad_marks() -> anyhow::Result<()> {
        let (log, _folder) = log();
        for id in 1..=5 {
            log.append(interaction(id), None).await?;
        }
        let marks = [
            mark(1, Target::Action, Some(Value::Good)),
            mark(2, Target::Response(0), Some(Value::Bad)),
            mark(4, Target::Action, Some(Value::Bad)),
            mark(4, Target::Action, None),
            mark(5, Target::Action, Some(Value::Bad)),
        ];
        for marked in marks {
            log.mark(marked).await?;
        }
        assert_eq!(ids(&log.list(Filter::Bad, PAGE).await?), [5, 2]);
        Ok(())
    }

    /// Writes a database of schema version 1 at `path` with the record of `interaction` without
    /// its marks.
    fn version_one(path: &Path, interaction: &Interaction) -> anyhow::Result<()> {
        let connection = Connection::open(path)?;
        connection.execute_batch(STEPS[0])?;
        connection.pragma_update(None, "user_version", 1)?;
        let mut record = serde_json::to_value(interaction)?;
        if let Some(fields) = record.as_object_mut() {
            fields.remove("marks");
        }
        let (id, time) = (interaction.id, interaction.time.as_millisecond());
        let insert = "INSERT INTO interactions (id, time, record) VALUES (?1, ?2, ?3)";
        connection.execute(insert, params![id, time, record.to_string()])?;
        Ok(())
    }

    /// AC-12.5-3.
    #[tokio::test]
    async fn ac_12_5_3_migration_keeps_every_record() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("jarvis.db");
        version_one(&path, &interaction(7))?;
        let log = SqliteLog::open(&path)?;
        assert_eq!(log.get(7).await?, Some(interaction(7)));
        let connection = Connection::open(&path)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 2);
        Ok(())
    }
}
