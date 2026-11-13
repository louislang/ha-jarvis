//! `InteractionLog` with SQLite.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use jiff::Timestamp;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Params;
use rusqlite::params;
use rusqlite::types::FromSql;

use crate::LogConfig;
use crate::domain::feedback::Deferred;
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

/// Adds the mark `?2` to the interaction `?1`.
const ADD: &str = "INSERT INTO marks (interaction, mark) VALUES (?1, ?2)";

/// The condition of the interactions with a bad mark.
const BAD: &str =
    "id IN (SELECT interaction FROM marks WHERE json_extract(mark, '$.value') = 'Bad')";

/// The System Two requests after `?1` without a bad action mark, oldest first.
const DEFERRED: &str = "WHERE time > ?1 AND json_extract(record, '$.decision') = 'Llm' AND json_extract(record, '$.link') IS NULL \
    AND id NOT IN (SELECT interaction FROM marks WHERE json_extract(mark, '$.target') = 'Action' AND json_extract(mark, '$.value') = 'Bad') \
    ORDER BY time, id";

/// Deletes the unmarked records older than `?2` days before `?1` that no marked record links to.
const PRUNE: &str = "DELETE FROM interactions WHERE time < ?1 - ?2 * 86400000 AND id NOT IN (SELECT interaction FROM marks) \
    AND id NOT IN (SELECT json_extract(record, '$.link') FROM interactions \
    WHERE json_extract(record, '$.link') IS NOT NULL AND id IN (SELECT interaction FROM marks))";

/// The sample rate of the stored audio.
const RATE: u32 = 16_000;

/// The interaction log in one SQLite database file. Each interaction is a JSON record.
pub struct SqliteLog {
    /// The open database.
    connection: Arc<Mutex<Connection>>,
    /// The age in days after which `prune` deletes the audio.
    audio_days: u64,
    /// The age in days after which `prune` deletes an unmarked record.
    record_days: u64,
}

impl SqliteLog {
    /// Opens the database of `config`, which it creates when it does not exist, and migrates its
    /// schema to the current version in one transaction.
    ///
    /// # Errors
    ///
    /// The database cannot open, a migration step fails, or the schema version of the database is
    /// newer than the code.
    pub fn open(config: &LogConfig) -> anyhow::Result<Self> {
        let mut connection = Connection::open(&config.database)?;
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
            audio_days: config.audio_days,
            record_days: config.record_days,
        })
    }

    /// The largest stored interaction ID, or `None` without interactions.
    ///
    /// # Errors
    ///
    /// The query fails.
    pub fn last_id(&self) -> anyhow::Result<Option<u64>> {
        let connection = self.connection.lock();
        let connection = connection.map_err(|_| anyhow::anyhow!("a database call panicked"))?;
        let query = "SELECT MAX(id) FROM interactions";
        Ok(connection.query_row(query, [], |row| row.get(0))?)
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
        let marks: Vec<String> = interaction
            .marks
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        let (id, time) = (interaction.id, interaction.time.as_millisecond());
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let insert =
                "INSERT INTO interactions (id, time, record, audio) VALUES (?1, ?2, ?3, ?4)";
            transaction.execute(insert, params![id, time, record, bytes])?;
            for mark in &marks {
                transaction.execute(ADD, params![id, mark])?;
            }
            transaction.commit()
        })
        .await
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
            let transaction = connection.unchecked_transaction()?;
            let remove = "DELETE FROM marks WHERE interaction = ?1 AND json_extract(mark, '$.target') = json_extract(?2, '$.target')";
            transaction.execute(remove, params![id, record])?;
            if kept {
                transaction.execute(ADD, params![id, record])?;
            }
            transaction.commit()
        })
        .await
    }

    async fn marked(&self) -> anyhow::Result<Vec<Interaction>> {
        let clauses = "WHERE id IN (SELECT interaction FROM marks) ORDER BY time, id";
        self.records(clauses.to_owned(), ()).await
    }

    async fn deferred(&self, since: Timestamp) -> anyhow::Result<Vec<Deferred>> {
        let found = self
            .records(DEFERRED.to_owned(), (since.as_millisecond(),))
            .await?;
        let deferred = found.into_iter().map(|interaction| Deferred {
            id: interaction.id,
            text: interaction.transcript,
            time: interaction.time,
            spoken: interaction.satellite.is_some(),
        });
        Ok(deferred.collect())
    }

    async fn prune(&self, now: Timestamp) -> anyhow::Result<()> {
        let (now, audio_days, record_days) =
            (now.as_millisecond(), self.audio_days, self.record_days);
        self.run(move |connection| {
            let audio = "UPDATE interactions SET audio = NULL WHERE time < ?1 - ?2 * 86400000";
            connection.execute(audio, params![now, audio_days])?;
            connection.execute(PRUNE, params![now, record_days])
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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

    /// The log configuration of `jarvis.toml` with the database in `folder`.
    fn config(folder: &TempDir) -> LogConfig {
        LogConfig {
            database: folder.path().join("jarvis.db"),
            audio_days: 30,
            record_days: 90,
            correlate_s: 10,
        }
    }

    /// A log in a new temporary folder, and the folder.
    fn log() -> (SqliteLog, TempDir) {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let log = SqliteLog::open(&config(&folder)).expect("the log opens");
        (log, folder)
    }

    /// The start of day `day`.
    fn day(day: i64) -> Timestamp {
        Timestamp::from_second(day * 86_400).expect("a valid day")
    }

    /// The mark `value` on `target` of the interaction `id`, with the correction `tv_show`.
    fn mark(id: u64, target: Target, value: Option<Value>) -> Mark {
        Mark {
            interaction: id,
            target,
            value,
            correction: Some("tv_show".to_owned()),
            time: day(1),
        }
    }

    /// The interaction `id` with the decision `decision` and no link.
    fn request(id: u64, decision: Route) -> Interaction {
        Interaction {
            decision,
            link: None,
            ..interaction(id)
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
        let config = config(&folder);
        Connection::open(&config.database)?.pragma_update(None, "user_version", 3)?;
        let Err(error) = SqliteLog::open(&config) else {
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

    /// `append` stores the marks of the interaction.
    #[tokio::test]
    async fn append_keeps_the_marks() -> anyhow::Result<()> {
        let (log, _folder) = log();
        let mut marked = interaction(7);
        marked
            .marks
            .push(mark(7, Target::Action, Some(Value::Good)));
        log.append(marked.clone(), None).await?;
        assert_eq!(log.get(7).await?, Some(marked));
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
        let config = config(&folder);
        version_one(&config.database, &interaction(7))?;
        let log = SqliteLog::open(&config)?;
        assert_eq!(log.get(7).await?, Some(interaction(7)));
        let connection = Connection::open(&config.database)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 2);
        Ok(())
    }

    /// Interactions 1 and 6 to 11: System Two requests at second 1, 6 (a known intent with a low
    /// probability), 7 (typed, a known intent that System One cannot act on), 10 and 11, a fetch
    /// at 8 and a dialog answer at 9.
    fn requests() -> [Interaction; 7] {
        let low = Interaction {
            top_labels: vec![("tv_on".to_owned(), 0.4), ("unknown".to_owned(), 0.3)],
            ..request(6, Route::Llm)
        };
        let typed = Interaction {
            satellite: None,
            top_labels: vec![("weather".to_owned(), 0.9)],
            ..request(7, Route::Llm)
        };
        let answer = Interaction {
            link: Some(6),
            ..request(9, Route::Llm)
        };
        [
            request(1, Route::Llm),
            low,
            typed,
            request(8, Route::Fetch),
            answer,
            request(10, Route::Llm),
            request(11, Route::Llm),
        ]
    }

    /// AC-12.5b-1.
    #[tokio::test]
    async fn ac_12_5b_1_deferred_gives_system_two_requests() -> anyhow::Result<()> {
        let (log, _folder) = log();
        for each in requests() {
            log.append(each, None).await?;
        }
        log.mark(mark(10, Target::Action, Some(Value::Bad))).await?;
        log.mark(mark(11, Target::Response(0), Some(Value::Bad)))
            .await?;
        let deferred = log.deferred(Timestamp::from_second(5)?).await?;
        let found: Vec<(u64, bool)> = deferred.iter().map(|each| (each.id, each.spoken)).collect();
        assert_eq!(found, [(6, true), (7, false), (11, true)]);
        Ok(())
    }

    /// The log with the interaction `id` at day `at` with audio, for each pair of `days`, and the
    /// folder.
    async fn aged(days: &[(u64, i64)]) -> anyhow::Result<(SqliteLog, TempDir)> {
        let (log, folder) = log();
        let audio = Audio {
            samples: vec![1, 2, 3],
            rate: RATE,
        };
        for &(id, at) in days {
            let old = Interaction {
                time: day(at),
                link: None,
                ..interaction(id)
            };
            log.append(old, Some(audio.clone())).await?;
        }
        Ok((log, folder))
    }

    /// AC-12.5b-2.
    #[tokio::test]
    async fn ac_12_5b_2_prune_deletes_old_audio_and_records() -> anyhow::Result<()> {
        let (log, _folder) = aged(&[(1, 5), (2, 50), (3, 80)]).await?;
        log.prune(day(100)).await?;
        assert_eq!(log.get(1).await?, None);
        assert!(log.get(2).await?.is_some(), "the record of day 50 is kept");
        assert_eq!(log.audio(2).await?, None);
        assert!(log.audio(3).await?.is_some(), "the audio of day 80 is kept");
        Ok(())
    }

    /// AC-12.5b-3.
    #[tokio::test]
    async fn ac_12_5b_3_prune_keeps_marked_records() -> anyhow::Result<()> {
        let (log, _folder) = aged(&[(1, 1), (2, 2)]).await?;
        log.mark(mark(1, Target::Response(0), Some(Value::Good)))
            .await?;
        log.prune(day(1000)).await?;
        assert_eq!(ids(&log.list(Filter::All, PAGE).await?), [1]);
        Ok(())
    }

    /// AC-12.5b-4.
    #[tokio::test]
    async fn ac_12_5b_4_prune_keeps_the_linked_first_interaction() -> anyhow::Result<()> {
        let (log, _folder) = aged(&[(1, 1), (3, 3)]).await?;
        let answer = Interaction {
            time: day(2),
            link: Some(1),
            ..interaction(2)
        };
        log.append(answer, None).await?;
        log.mark(mark(2, Target::Action, Some(Value::Bad))).await?;
        log.prune(day(1000)).await?;
        assert_eq!(ids(&log.list(Filter::All, PAGE).await?), [2, 1]);
        Ok(())
    }
}
