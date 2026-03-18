use std::io;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, Error as SqlError, OptionalExtension, params};
use uuid::Uuid;

use crate::auth::{KeyRecord, Role};
use crate::log;

pub struct SqliteKeyStore {
    // The service opens SQLite once during startup and reuses this connection
    // for all subsequent key reads and writes.
    connection: Mutex<Connection>,
}

impl SqliteKeyStore {
    pub fn open(database_url: &str) -> io::Result<Self> {
        let database_exists = Path::new(database_url).exists();
        let connection = Connection::open(database_url).map_err(to_io_error)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS keys (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                value TEXT NOT NULL UNIQUE,
                role TEXT NOT NULL,
                whitelist_models TEXT NOT NULL DEFAULT '',
                blacklist_models TEXT NOT NULL DEFAULT ''
            );",
        )
        .map_err(to_io_error)?;

        if !database_exists {
            seed_bootstrap_admin(&connection).map_err(to_io_error)?;
        }

        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn insert_key(
        &self,
        token: String,
        role: Role,
        name: String,
        whitelist_models: Vec<String>,
        blacklist_models: Vec<String>,
    ) -> io::Result<KeyRecord> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("key database mutex poisoned"))?;
        connection
            .execute(
                "INSERT INTO keys (name, value, role, whitelist_models, blacklist_models)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    name,
                    token,
                    role.as_str(),
                    encode_models(&whitelist_models),
                    encode_models(&blacklist_models)
                ],
            )
            .map_err(to_io_error)?;

        Ok(KeyRecord {
            id: connection.last_insert_rowid(),
            token,
            role,
            name,
            whitelist_models,
            blacklist_models,
        })
    }

    pub fn list_keys(&self) -> io::Result<Vec<KeyRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("key database mutex poisoned"))?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, value, role, whitelist_models, blacklist_models
                 FROM keys
                 ORDER BY id ASC",
            )
            .map_err(to_io_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(KeyRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    token: row.get(2)?,
                    role: parse_role(&row.get::<_, String>(3)?).ok_or(SqlError::InvalidQuery)?,
                    whitelist_models: decode_models(&row.get::<_, String>(4)?),
                    blacklist_models: decode_models(&row.get::<_, String>(5)?),
                })
            })
            .map_err(to_io_error)?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(to_io_error)?);
        }
        Ok(records)
    }

    pub fn fetch_key_by_token(&self, token: &str) -> io::Result<Option<KeyRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("key database mutex poisoned"))?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, value, role, whitelist_models, blacklist_models
                 FROM keys
                 WHERE value = ?1",
            )
            .map_err(to_io_error)?;
        statement
            .query_row(params![token], |row| {
                let role: String = row.get(3)?;
                Ok(KeyRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    token: row.get(2)?,
                    role: parse_role(&role).ok_or(SqlError::InvalidQuery)?,
                    whitelist_models: decode_models(&row.get::<_, String>(4)?),
                    blacklist_models: decode_models(&row.get::<_, String>(5)?),
                })
            })
            .optional()
            .map_err(to_io_error)
    }
}

fn seed_bootstrap_admin(connection: &Connection) -> Result<(), SqlError> {
    let token = Uuid::new_v4().to_string();
    connection.execute(
        "INSERT OR IGNORE INTO keys (name, value, role, whitelist_models, blacklist_models)
         VALUES (?1, ?2, ?3, '', '')",
        params!["admin", token, Role::Admin.as_str()],
    )?;
    log::warn("created initial admin key in sqlite database");
    log::warn(format!("initial admin name=admin token={token}"));
    Ok(())
}

fn parse_role(value: &str) -> Option<Role> {
    match value {
        "Admin" => Some(Role::Admin),
        "Client" => Some(Role::Client),
        "Worker" => Some(Role::Worker),
        _ => None,
    }
}

fn encode_models(values: &[String]) -> String {
    values.join("\n")
}

fn decode_models(value: &str) -> Vec<String> {
    value
        .split('\n')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn to_io_error(error: SqlError) -> io::Error {
    if let SqlError::SqliteFailure(err, _) = &error {
        if err.code == rusqlite::ErrorCode::ConstraintViolation {
            return io::Error::new(io::ErrorKind::AlreadyExists, error);
        }
    }
    io::Error::other(error)
}
