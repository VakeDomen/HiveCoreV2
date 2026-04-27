use std::io;
use std::sync::Mutex;

use rusqlite::{Connection, Error as SqlError, params};

use crate::shared::http::UsageEvent;
use crate::shared::log;

pub struct UsageTrackingDb {
    connection: Mutex<Connection>,
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
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("usage database mutex poisoned"))?;
        let transaction = connection.transaction().map_err(to_io_error)?;

        let mut statement = transaction
            .prepare(
                "INSERT INTO usage_tracking \
                 (key_name, model, prompt_tokens, completion_tokens, duration_ms, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .map_err(to_io_error)?;

        for event in events {
            statement
                .execute(params![
                    event.key_name,
                    event.model,
                    event.prompt_tokens,
                    event.completion_tokens,
                    event.duration_ms,
                    event.created_at,
                ])
                .map_err(to_io_error)?;
        }

        drop(statement);
        transaction.commit().map_err(to_io_error)?;
        log::info(format!(
            "persisted {} usage events to sqlite",
            events.len()
        ));
        Ok(())
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), SqlError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_tracking (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            key_name TEXT NOT NULL,
            model TEXT NOT NULL,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );
         CREATE INDEX IF NOT EXISTS idx_usage_key_model
             ON usage_tracking (key_name, model);
         CREATE INDEX IF NOT EXISTS idx_usage_created
             ON usage_tracking (created_at);",
    )
}

fn to_io_error(error: SqlError) -> io::Error {
    io::Error::other(error)
}
