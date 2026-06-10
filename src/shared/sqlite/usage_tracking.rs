use std::io;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, Utc};
use rusqlite::{Connection, Error as SqlError, params};
use serde::Serialize;

use crate::shared::http::UsageEvent;
use crate::shared::log;

pub struct UsageTrackingDb {
    connection: Mutex<Connection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DailyUsageRow {
    pub usage_day: String,
    pub key_id: Option<i64>,
    pub key_name: String,
    pub model: String,
    pub request_count: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub queue_ms: u64,
    pub worker_ms: u64,
    pub total_ms: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkerUsageRow {
    pub usage_day: String,
    pub worker_name: String,
    pub backend: String,
    pub model: String,
    pub request_count: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub queue_ms: u64,
    pub worker_ms: u64,
    pub total_ms: u64,
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
                 (usage_identity, key_id, key_name, model, usage_day, request_count, success_count, error_count, prompt_tokens, completion_tokens, total_tokens, queue_ms, worker_ms, total_ms, duration_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
                 ON CONFLICT(usage_identity, model, usage_day) DO UPDATE SET \
                     key_id = excluded.key_id, \
                     key_name = excluded.key_name, \
                     success_count = success_count + excluded.success_count, \
                     error_count = error_count + excluded.error_count, \
                     prompt_tokens = prompt_tokens + excluded.prompt_tokens, \
                     completion_tokens = completion_tokens + excluded.completion_tokens, \
                     total_tokens = total_tokens + excluded.total_tokens, \
                     queue_ms = queue_ms + excluded.queue_ms, \
                     worker_ms = worker_ms + excluded.worker_ms, \
                     total_ms = total_ms + excluded.total_ms, \
                     duration_ms = duration_ms + excluded.duration_ms, \
                     request_count = request_count + 1",
            )
            .map_err(to_io_error)?;

        let mut worker_statement = transaction
            .prepare(
                "INSERT INTO worker_usage_tracking \
                 (usage_day, worker_name, backend, model, request_count, success_count, error_count, prompt_tokens, completion_tokens, total_tokens, queue_ms, worker_ms, total_ms, duration_ms) \
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                 ON CONFLICT(usage_day, worker_name, backend, model) DO UPDATE SET \
                     success_count = success_count + excluded.success_count, \
                     error_count = error_count + excluded.error_count, \
                     prompt_tokens = prompt_tokens + excluded.prompt_tokens, \
                     completion_tokens = completion_tokens + excluded.completion_tokens, \
                     total_tokens = total_tokens + excluded.total_tokens, \
                     queue_ms = queue_ms + excluded.queue_ms, \
                     worker_ms = worker_ms + excluded.worker_ms, \
                     total_ms = total_ms + excluded.total_ms, \
                     duration_ms = duration_ms + excluded.duration_ms, \
                     request_count = request_count + 1",
            )
            .map_err(to_io_error)?;

        for event in events {
            let usage_day = local_day_from_unix(event.created_at)
                .format("%Y-%m-%d")
                .to_string();
            let usage_identity = usage_identity(event);
            let success_count = u64::from(event.status_code < 400);
            let error_count = u64::from(event.status_code >= 400);
            statement
                .execute(params![
                    &usage_identity,
                    event.key_id,
                    &event.key_name,
                    &event.model,
                    &usage_day,
                    success_count,
                    error_count,
                    event.prompt_tokens,
                    event.completion_tokens,
                    event.total_tokens,
                    event.queue_ms,
                    event.worker_ms,
                    event.total_ms,
                    event.duration_ms,
                ])
                .map_err(to_io_error)?;
            worker_statement
                .execute(params![
                    &usage_day,
                    &event.worker_name,
                    &event.backend,
                    &event.model,
                    success_count,
                    error_count,
                    event.prompt_tokens,
                    event.completion_tokens,
                    event.total_tokens,
                    event.queue_ms,
                    event.worker_ms,
                    event.total_ms,
                    event.duration_ms,
                ])
                .map_err(to_io_error)?;
        }

        drop(statement);
        drop(worker_statement);
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
                "SELECT usage_day, key_id, key_name, model, request_count, success_count, error_count, prompt_tokens, completion_tokens, total_tokens, queue_ms, worker_ms, total_ms, duration_ms
                 FROM usage_tracking
                 WHERE usage_day = ?1
                 ORDER BY key_name ASC, model ASC",
            )
            .map_err(to_io_error)?;
        let rows = statement
            .query_map(params![day.format("%Y-%m-%d").to_string()], |row| {
                Ok(DailyUsageRow {
                    usage_day: row.get(0)?,
                    key_id: row.get(1)?,
                    key_name: row.get(2)?,
                    model: row.get(3)?,
                    request_count: row.get(4)?,
                    success_count: row.get(5)?,
                    error_count: row.get(6)?,
                    prompt_tokens: row.get(7)?,
                    completion_tokens: row.get(8)?,
                    total_tokens: row.get(9)?,
                    queue_ms: row.get(10)?,
                    worker_ms: row.get(11)?,
                    total_ms: row.get(12)?,
                    duration_ms: row.get(13)?,
                })
            })
            .map_err(to_io_error)?;
        let mut report = Vec::new();
        for row in rows {
            report.push(row.map_err(to_io_error)?);
        }
        Ok(report)
    }

    pub fn usage_report_for_range(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> io::Result<(Vec<DailyUsageRow>, Vec<WorkerUsageRow>)> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("usage database mutex poisoned"))?;
        let from = from.format("%Y-%m-%d").to_string();
        let to = to.format("%Y-%m-%d").to_string();

        let mut usage_statement = connection
            .prepare(
                "SELECT usage_day, key_id, key_name, model, request_count, success_count, error_count, prompt_tokens, completion_tokens, total_tokens, queue_ms, worker_ms, total_ms, duration_ms
                 FROM usage_tracking
                 WHERE usage_day >= ?1 AND usage_day <= ?2
                 ORDER BY usage_day ASC, key_name ASC, model ASC",
            )
            .map_err(to_io_error)?;
        let usage_rows = usage_statement
            .query_map(params![&from, &to], daily_usage_row)
            .map_err(to_io_error)?;
        let mut usage = Vec::new();
        for row in usage_rows {
            usage.push(row.map_err(to_io_error)?);
        }

        let mut worker_statement = connection
            .prepare(
                "SELECT usage_day, worker_name, backend, model, request_count, success_count, error_count, prompt_tokens, completion_tokens, total_tokens, queue_ms, worker_ms, total_ms, duration_ms
                 FROM worker_usage_tracking
                 WHERE usage_day >= ?1 AND usage_day <= ?2
                 ORDER BY usage_day ASC, worker_name ASC, backend ASC, model ASC",
            )
            .map_err(to_io_error)?;
        let worker_rows = worker_statement
            .query_map(params![&from, &to], worker_usage_row)
            .map_err(to_io_error)?;
        let mut workers = Vec::new();
        for row in worker_rows {
            workers.push(row.map_err(to_io_error)?);
        }

        Ok((usage, workers))
    }
}

fn daily_usage_row(row: &rusqlite::Row<'_>) -> Result<DailyUsageRow, SqlError> {
    Ok(DailyUsageRow {
        usage_day: row.get(0)?,
        key_id: row.get(1)?,
        key_name: row.get(2)?,
        model: row.get(3)?,
        request_count: row.get(4)?,
        success_count: row.get(5)?,
        error_count: row.get(6)?,
        prompt_tokens: row.get(7)?,
        completion_tokens: row.get(8)?,
        total_tokens: row.get(9)?,
        queue_ms: row.get(10)?,
        worker_ms: row.get(11)?,
        total_ms: row.get(12)?,
        duration_ms: row.get(13)?,
    })
}

fn worker_usage_row(row: &rusqlite::Row<'_>) -> Result<WorkerUsageRow, SqlError> {
    Ok(WorkerUsageRow {
        usage_day: row.get(0)?,
        worker_name: row.get(1)?,
        backend: row.get(2)?,
        model: row.get(3)?,
        request_count: row.get(4)?,
        success_count: row.get(5)?,
        error_count: row.get(6)?,
        prompt_tokens: row.get(7)?,
        completion_tokens: row.get(8)?,
        total_tokens: row.get(9)?,
        queue_ms: row.get(10)?,
        worker_ms: row.get(11)?,
        total_ms: row.get(12)?,
        duration_ms: row.get(13)?,
    })
}

fn initialize_schema(connection: &Connection) -> Result<(), SqlError> {
    if !table_exists(connection, "usage_tracking")? {
        create_usage_tracking_schema(connection)?;
        create_worker_usage_tracking_schema(connection)?;
        return Ok(());
    }

    let columns = table_columns(connection, "usage_tracking")?;
    if columns.iter().any(|column| column == "usage_identity")
        && columns.iter().any(|column| column == "key_id")
    {
        ensure_usage_tracking_columns(connection, &columns)?;
        create_usage_tracking_indexes(connection)?;
        create_worker_usage_tracking_schema(connection)?;
        return Ok(());
    }

    migrate_usage_tracking_to_identity_schema(connection, &columns)?;
    create_worker_usage_tracking_schema(connection)
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
            success_count INTEGER NOT NULL DEFAULT 0,
            error_count INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            queue_ms INTEGER NOT NULL DEFAULT 0,
            worker_ms INTEGER NOT NULL DEFAULT 0,
            total_ms INTEGER NOT NULL DEFAULT 0,
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

fn ensure_usage_tracking_columns(
    connection: &Connection,
    columns: &[String],
) -> Result<(), SqlError> {
    for (column, definition) in [
        ("success_count", "INTEGER NOT NULL DEFAULT 0"),
        ("error_count", "INTEGER NOT NULL DEFAULT 0"),
        ("total_tokens", "INTEGER NOT NULL DEFAULT 0"),
        ("queue_ms", "INTEGER NOT NULL DEFAULT 0"),
        ("worker_ms", "INTEGER NOT NULL DEFAULT 0"),
        ("total_ms", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !columns.iter().any(|existing| existing == column) {
            connection.execute(
                &format!("ALTER TABLE usage_tracking ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    connection.execute(
        "UPDATE usage_tracking
         SET success_count = CASE WHEN success_count = 0 AND error_count = 0 THEN request_count ELSE success_count END,
             total_tokens = CASE WHEN total_tokens = 0 THEN prompt_tokens + completion_tokens ELSE total_tokens END,
             worker_ms = CASE WHEN worker_ms = 0 THEN duration_ms ELSE worker_ms END,
             total_ms = CASE WHEN total_ms = 0 THEN duration_ms ELSE total_ms END",
        [],
    )?;
    Ok(())
}

fn create_worker_usage_tracking_schema(connection: &Connection) -> Result<(), SqlError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS worker_usage_tracking (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            usage_day TEXT NOT NULL,
            worker_name TEXT NOT NULL,
            backend TEXT NOT NULL,
            model TEXT NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            success_count INTEGER NOT NULL DEFAULT 0,
            error_count INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            queue_ms INTEGER NOT NULL DEFAULT 0,
            worker_ms INTEGER NOT NULL DEFAULT 0,
            total_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            UNIQUE(usage_day, worker_name, backend, model)
        );
         CREATE INDEX IF NOT EXISTS idx_worker_usage_day
             ON worker_usage_tracking (usage_day);
         CREATE INDEX IF NOT EXISTS idx_worker_usage_worker_model_day
             ON worker_usage_tracking (worker_name, model, usage_day);",
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
            success_count INTEGER NOT NULL DEFAULT 0,
            error_count INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            queue_ms INTEGER NOT NULL DEFAULT 0,
            worker_ms INTEGER NOT NULL DEFAULT 0,
            total_ms INTEGER NOT NULL DEFAULT 0,
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
            request_count, success_count, error_count, prompt_tokens, completion_tokens,
            total_tokens, queue_ms, worker_ms, total_ms, duration_ms
         )
         SELECT
            id,
            {usage_identity_expr},
            {key_id_expr},
            key_name,
            model,
            usage_day,
            request_count,
            request_count,
            0,
            prompt_tokens,
            completion_tokens,
            prompt_tokens + completion_tokens,
            0,
            duration_ms,
            duration_ms,
            duration_ms
         FROM usage_tracking
         WHERE true
         ON CONFLICT(usage_identity, model, usage_day) DO UPDATE SET
            request_count = request_count + excluded.request_count,
            success_count = success_count + excluded.success_count,
            error_count = error_count + excluded.error_count,
            prompt_tokens = prompt_tokens + excluded.prompt_tokens,
            completion_tokens = completion_tokens + excluded.completion_tokens,
            total_tokens = total_tokens + excluded.total_tokens,
            queue_ms = queue_ms + excluded.queue_ms,
            worker_ms = worker_ms + excluded.worker_ms,
            total_ms = total_ms + excluded.total_ms,
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

    fn usage_event(
        key_id: Option<i64>,
        key_name: &str,
        worker_name: &str,
        backend: &str,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        duration_ms: u64,
        created_at: u64,
    ) -> UsageEvent {
        UsageEvent {
            key_id,
            key_name: key_name.to_string(),
            worker_name: worker_name.to_string(),
            backend: backend.to_string(),
            model: model.to_string(),
            status_code: 200,
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            queue_ms: 0,
            worker_ms: duration_ms,
            total_ms: duration_ms,
            duration_ms,
            created_at,
        }
    }

    #[test]
    fn insert_batch_aggregates_by_user_model_and_day() -> io::Result<()> {
        let path = temp_db_path("aggregate_day");
        let db = UsageTrackingDb::open(path.to_str().expect("utf8 path"))?;

        db.insert_batch(&[
            usage_event(
                None,
                "alice",
                "worker-a",
                "ollama",
                "bge-m3",
                10,
                2,
                20,
                1_745_020_800,
            ),
            usage_event(
                None,
                "alice",
                "worker-a",
                "ollama",
                "bge-m3",
                7,
                3,
                15,
                1_745_021_100,
            ),
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
            usage_event(
                Some(42),
                "CrowleyAgent",
                "worker-a",
                "vllm",
                "DeepSeek-V4-Flash",
                18_100,
                8,
                1_616,
                1_749_427_200,
            ),
            usage_event(
                Some(42),
                "client-Foras",
                "worker-a",
                "vllm",
                "DeepSeek-V4-Flash",
                36_000,
                25,
                3_473,
                1_749_427_400,
            ),
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

        db.insert_batch(&[usage_event(
            Some(7),
            "new-name",
            "worker-a",
            "ollama",
            "bge-m3",
            11,
            1,
            9,
            1_745_020_800,
        )])?;
        let rows = db.usage_report_for_day(
            chrono::NaiveDate::from_ymd_opt(2025, 4, 19).expect("valid date"),
        )?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.key_id == Some(7)));

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn insert_batch_aggregates_worker_usage_by_day_worker_backend_and_model() -> io::Result<()> {
        let path = temp_db_path("worker_aggregate");
        let db = UsageTrackingDb::open(path.to_str().expect("utf8 path"))?;

        db.insert_batch(&[
            usage_event(
                Some(1),
                "client-a",
                "worker-a",
                "vllm",
                "DeepSeek-V4-Flash",
                10,
                20,
                30,
                1_749_427_200,
            ),
            usage_event(
                Some(2),
                "client-b",
                "worker-a",
                "vllm",
                "DeepSeek-V4-Flash",
                3,
                4,
                5,
                1_749_427_400,
            ),
            usage_event(
                Some(3),
                "client-c",
                "worker-b",
                "ollama",
                "qwen3:0.6b",
                7,
                8,
                9,
                1_749_427_500,
            ),
        ])?;

        let (_daily_rows, worker_rows) = db.usage_report_for_range(
            chrono::NaiveDate::from_ymd_opt(2025, 6, 9).expect("valid date"),
            chrono::NaiveDate::from_ymd_opt(2025, 6, 9).expect("valid date"),
        )?;

        assert_eq!(worker_rows.len(), 2);
        let worker_a = worker_rows
            .iter()
            .find(|row| row.worker_name == "worker-a")
            .expect("worker-a row");
        assert_eq!(worker_a.backend, "vllm");
        assert_eq!(worker_a.model, "DeepSeek-V4-Flash");
        assert_eq!(worker_a.request_count, 2);
        assert_eq!(worker_a.prompt_tokens, 13);
        assert_eq!(worker_a.completion_tokens, 24);
        assert_eq!(worker_a.duration_ms, 35);

        cleanup(&path);
        Ok(())
    }
}
