use std::io;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, Utc};
use rusqlite::{Connection, Error as SqlError, params};

use crate::shared::http::UsageEvent;
use crate::shared::log;

pub struct UsageTrackingDb {
    connection: Mutex<Connection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyUsageRow {
    pub key_id: Option<i64>,
    pub key_name: String,
    pub model: String,
    pub request_count: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub duration_ms: u64,
}

impl UsageTrackingDb {
    pub fn open(database_url: &str) -> io::Result<Self> {
        let connection = Connection::open(database_url).map_err(to_io_error)?;
        initialize_schema(&connection).map_err(to_io_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn insert_batch(&self, events: &[UsageEvent]) -> io::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("usage database mutex poisoned"))?;
        let transaction = connection.transaction().map_err(to_io_error)?;

        let mut statement = transaction
            .prepare(
                "INSERT INTO usage_tracking \
                 (usage_identity, key_id, key_name, model, usage_day, prompt_tokens, completion_tokens, duration_ms, request_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1) \
                 ON CONFLICT(usage_identity, model, usage_day) DO UPDATE SET \
                     key_id = excluded.key_id, \
                     key_name = excluded.key_name, \
                     prompt_tokens = prompt_tokens + excluded.prompt_tokens, \
                     completion_tokens = completion_tokens + excluded.completion_tokens, \
                     duration_ms = duration_ms + excluded.duration_ms, \
                     request_count = request_count + 1",
            )
            .map_err(to_io_error)?;

        for event in events {
            let usage_day = local_day_from_unix(event.created_at)
                .format("%Y-%m-%d")
                .to_string();
            let usage_identity = usage_identity(event);
            statement
                .execute(params![
                    usage_identity,
                    event.key_id,
                    event.key_name,
                    event.model,
                    usage_day,
                    event.prompt_tokens,
                    event.completion_tokens,
                    event.duration_ms,
                ])
                .map_err(to_io_error)?;
        }

        drop(statement);
        transaction.commit().map_err(to_io_error)?;
        log::info(format!("persisted {} usage events to sqlite", events.len()));
        Ok(())
    }

    pub fn usage_report_for_day(&self, day: NaiveDate) -> io::Result<Vec<DailyUsageRow>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("usage database mutex poisoned"))?;
        let mut statement = connection
            .prepare(
                "SELECT key_id, key_name, model, request_count, prompt_tokens, completion_tokens, duration_ms
                 FROM usage_tracking
                 WHERE usage_day = ?1
                 ORDER BY key_name ASC, model ASC",
            )
            .map_err(to_io_error)?;
        let rows = statement
            .query_map(params![day.format("%Y-%m-%d").to_string()], |row| {
                Ok(DailyUsageRow {
                    key_id: row.get(0)?,
                    key_name: row.get(1)?,
                    model: row.get(2)?,
                    request_count: row.get(3)?,
                    prompt_tokens: row.get(4)?,
                    completion_tokens: row.get(5)?,
                    duration_ms: row.get(6)?,
                })
            })
            .map_err(to_io_error)?;
        let mut report = Vec::new();
        for row in rows {
            report.push(row.map_err(to_io_error)?);
        }
        Ok(report)
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), SqlError> {
    if !table_exists(connection, "usage_tracking")? {
        create_usage_tracking_schema(connection)?;
        return Ok(());
    }

    let columns = table_columns(connection, "usage_tracking")?;
    if columns.iter().any(|column| column == "usage_identity")
        && columns.iter().any(|column| column == "key_id")
    {
        create_usage_tracking_indexes(connection)?;
        return Ok(());
    }

    migrate_usage_tracking_to_identity_schema(connection, &columns)
}

fn create_usage_tracking_schema(connection: &Connection) -> Result<(), SqlError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_tracking (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            usage_identity TEXT NOT NULL,
            key_id INTEGER,
            key_name TEXT NOT NULL,
            model TEXT NOT NULL,
            usage_day TEXT NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            UNIQUE(usage_identity, model, usage_day)
        );
         CREATE INDEX IF NOT EXISTS idx_usage_key_model_day
             ON usage_tracking (usage_identity, model, usage_day);
         CREATE INDEX IF NOT EXISTS idx_usage_day
             ON usage_tracking (usage_day);",
    )
}

fn create_usage_tracking_indexes(connection: &Connection) -> Result<(), SqlError> {
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_usage_key_model_day
             ON usage_tracking (usage_identity, model, usage_day);
         CREATE INDEX IF NOT EXISTS idx_usage_day
             ON usage_tracking (usage_day);",
    )
}

fn migrate_usage_tracking_to_identity_schema(
    connection: &Connection,
    columns: &[String],
) -> Result<(), SqlError> {
    connection.execute_batch(
        "DROP TABLE IF EXISTS usage_tracking_v2;
         CREATE TABLE usage_tracking_v2 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            usage_identity TEXT NOT NULL,
            key_id INTEGER,
            key_name TEXT NOT NULL,
            model TEXT NOT NULL,
            usage_day TEXT NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            UNIQUE(usage_identity, model, usage_day)
        );",
    )?;

    let usage_identity_expr = if columns.iter().any(|column| column == "usage_identity") {
        "COALESCE(usage_identity, 'name:' || key_name)"
    } else {
        "'name:' || key_name"
    };
    let key_id_expr = if columns.iter().any(|column| column == "key_id") {
        "key_id"
    } else {
        "NULL"
    };
    let copy_sql = format!(
        "INSERT INTO usage_tracking_v2 (
            id, usage_identity, key_id, key_name, model, usage_day,
            request_count, prompt_tokens, completion_tokens, duration_ms
         )
         SELECT
            id,
            {usage_identity_expr},
            {key_id_expr},
            key_name,
            model,
            usage_day,
            request_count,
            prompt_tokens,
            completion_tokens,
            duration_ms
         FROM usage_tracking
         WHERE true
         ON CONFLICT(usage_identity, model, usage_day) DO UPDATE SET
            request_count = request_count + excluded.request_count,
            prompt_tokens = prompt_tokens + excluded.prompt_tokens,
            completion_tokens = completion_tokens + excluded.completion_tokens,
            duration_ms = duration_ms + excluded.duration_ms"
    );
    connection.execute(&copy_sql, [])?;
    connection.execute_batch(
        "DROP TABLE usage_tracking;
         ALTER TABLE usage_tracking_v2 RENAME TO usage_tracking;",
    )?;
    create_usage_tracking_indexes(connection)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, SqlError> {
    connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
        )",
        params![table],
        |row| row.get::<_, bool>(0),
    )
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, SqlError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}

fn usage_identity(event: &UsageEvent) -> String {
    match event.key_id {
        Some(key_id) => format!("key:{key_id}"),
        None => format!("name:{}", event.key_name),
    }
}

pub fn run_usage_tracking_worker(database_url: String, rx: Receiver<UsageEvent>) -> io::Result<()> {
    let db = UsageTrackingDb::open(&database_url)?;
    let mut pending = Vec::new();

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                pending.push(event);
                if pending.len() >= 100 {
                    db.insert_batch(&pending)?;
                    pending.clear();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !pending.is_empty() {
                    db.insert_batch(&pending)?;
                    pending.clear();
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                if !pending.is_empty() {
                    db.insert_batch(&pending)?;
                }
                return Ok(());
            }
        }
    }
}

pub fn local_day_from_unix(created_at: u64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(created_at as i64, 0)
        .map(|value| value.with_timezone(&Local).date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

pub fn current_local_day() -> NaiveDate {
    Local::now().date_naive()
}

fn to_io_error(error: SqlError) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::PathBuf;

    use rusqlite::Connection;
    use rusqlite::params;
    use uuid::Uuid;

    use crate::shared::http::UsageEvent;

    use super::UsageTrackingDb;

    fn temp_db_path(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hive_core_v2_usage_{test_name}_{}.db",
            Uuid::new_v4()
        ))
    }

    fn cleanup(path: &PathBuf) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn insert_batch_aggregates_by_user_model_and_day() -> io::Result<()> {
        let path = temp_db_path("aggregate_day");
        let db = UsageTrackingDb::open(path.to_str().expect("utf8 path"))?;

        db.insert_batch(&[
            UsageEvent {
                key_id: None,
                key_name: "alice".to_string(),
                model: "bge-m3".to_string(),
                prompt_tokens: 10,
                completion_tokens: 2,
                duration_ms: 20,
                created_at: 1_745_020_800,
            },
            UsageEvent {
                key_id: None,
                key_name: "alice".to_string(),
                model: "bge-m3".to_string(),
                prompt_tokens: 7,
                completion_tokens: 3,
                duration_ms: 15,
                created_at: 1_745_021_100,
            },
        ])?;

        let connection = Connection::open(&path).map_err(io::Error::other)?;
        let row = connection
            .query_row(
                "SELECT usage_day, request_count, prompt_tokens, completion_tokens, duration_ms \
                 FROM usage_tracking WHERE key_name = ?1 AND model = ?2",
                params!["alice", "bge-m3"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(io::Error::other)?;

        assert_eq!(row.0, "2025-04-19");
        assert_eq!(row.1, 2);
        assert_eq!(row.2, 17);
        assert_eq!(row.3, 5);
        assert_eq!(row.4, 35);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn insert_batch_aggregates_by_key_id_after_rename() -> io::Result<()> {
        let path = temp_db_path("aggregate_rename");
        let db = UsageTrackingDb::open(path.to_str().expect("utf8 path"))?;

        db.insert_batch(&[
            UsageEvent {
                key_id: Some(42),
                key_name: "CrowleyAgent".to_string(),
                model: "DeepSeek-V4-Flash".to_string(),
                prompt_tokens: 18_100,
                completion_tokens: 8,
                duration_ms: 1_616,
                created_at: 1_749_427_200,
            },
            UsageEvent {
                key_id: Some(42),
                key_name: "client-Foras".to_string(),
                model: "DeepSeek-V4-Flash".to_string(),
                prompt_tokens: 36_000,
                completion_tokens: 25,
                duration_ms: 3_473,
                created_at: 1_749_427_400,
            },
        ])?;

        let rows = db.usage_report_for_day(
            chrono::NaiveDate::from_ymd_opt(2025, 6, 9).expect("valid date"),
        )?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key_id, Some(42));
        assert_eq!(rows[0].key_name, "client-Foras");
        assert_eq!(rows[0].request_count, 2);
        assert_eq!(rows[0].prompt_tokens, 54_100);
        assert_eq!(rows[0].completion_tokens, 33);
        assert_eq!(rows[0].duration_ms, 5_089);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn open_migrates_legacy_name_based_usage_table() -> io::Result<()> {
        let path = temp_db_path("legacy_migration");
        {
            let connection = Connection::open(&path).map_err(io::Error::other)?;
            connection
                .execute_batch(
                    "CREATE TABLE usage_tracking (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        key_name TEXT NOT NULL,
                        model TEXT NOT NULL,
                        usage_day TEXT NOT NULL,
                        request_count INTEGER NOT NULL DEFAULT 0,
                        prompt_tokens INTEGER NOT NULL DEFAULT 0,
                        completion_tokens INTEGER NOT NULL DEFAULT 0,
                        duration_ms INTEGER NOT NULL DEFAULT 0,
                        UNIQUE(key_name, model, usage_day)
                    );",
                )
                .map_err(io::Error::other)?;
            connection
                .execute(
                    "INSERT INTO usage_tracking (
                        key_name, model, usage_day, request_count,
                        prompt_tokens, completion_tokens, duration_ms
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params!["old-name", "bge-m3", "2025-04-19", 2, 17, 5, 35],
                )
                .map_err(io::Error::other)?;
        }

        let db = UsageTrackingDb::open(path.to_str().expect("utf8 path"))?;
        let rows = db.usage_report_for_day(
            chrono::NaiveDate::from_ymd_opt(2025, 4, 19).expect("valid date"),
        )?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key_id, None);
        assert_eq!(rows[0].key_name, "old-name");
        assert_eq!(rows[0].request_count, 2);
        assert_eq!(rows[0].prompt_tokens, 17);

        db.insert_batch(&[UsageEvent {
            key_id: Some(7),
            key_name: "new-name".to_string(),
            model: "bge-m3".to_string(),
            prompt_tokens: 11,
            completion_tokens: 1,
            duration_ms: 9,
            created_at: 1_745_020_800,
        }])?;
        let rows = db.usage_report_for_day(
            chrono::NaiveDate::from_ymd_opt(2025, 4, 19).expect("valid date"),
        )?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.key_id == Some(7)));

        cleanup(&path);
        Ok(())
    }
}
