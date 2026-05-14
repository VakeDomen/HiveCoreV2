use std::io;
use std::sync::Mutex;

use rusqlite::{Connection, Error as SqlError, OptionalExtension, params};
use uuid::Uuid;

use crate::auth::{KeyRecord, Role};
use crate::shared::log;

pub struct SqliteKeyStore {
    // The service opens SQLite once during startup and reuses this connection
    // for all subsequent key reads and writes.
    connection: Mutex<Connection>,
}

impl SqliteKeyStore {
    pub fn open(database_url: &str) -> io::Result<Self> {
        let connection = Connection::open(database_url).map_err(to_io_error)?;
        initialize_schema(&connection).map_err(to_io_error)?;

        if count_keys(&connection).map_err(to_io_error)? == 0 {
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
        capture: bool,
        whitelist_models: Vec<String>,
        blacklist_models: Vec<String>,
    ) -> io::Result<KeyRecord> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("key database mutex poisoned"))?;
        let transaction = connection.transaction().map_err(to_io_error)?;
        transaction
            .execute(
                "INSERT INTO keys (name, value, role, capture)
                 VALUES (?1, ?2, ?3, ?4)",
                params![name, token, role.as_str(), capture],
            )
            .map_err(to_io_error)?;
        let key_id = transaction.last_insert_rowid();
        insert_model_rules(
            &transaction,
            key_id,
            ModelListType::Whitelist,
            &whitelist_models,
        )
        .map_err(to_io_error)?;
        insert_model_rules(
            &transaction,
            key_id,
            ModelListType::Blacklist,
            &blacklist_models,
        )
        .map_err(to_io_error)?;
        transaction.commit().map_err(to_io_error)?;

        Ok(KeyRecord {
            id: key_id,
            token,
            role,
            name,
            whitelist_models,
            blacklist_models,
            capture,
        })
    }

    pub fn list_keys(&self) -> io::Result<Vec<KeyRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("key database mutex poisoned"))?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, value, role, capture
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
                    whitelist_models: Vec::new(),
                    blacklist_models: Vec::new(),
                    capture: row.get(4)?,
                })
            })
            .map_err(to_io_error)?;

        let mut records = Vec::new();
        for row in rows {
            let mut record = row.map_err(to_io_error)?;
            record.whitelist_models =
                list_model_rules(&connection, record.id, ModelListType::Whitelist)
                    .map_err(to_io_error)?;
            record.blacklist_models =
                list_model_rules(&connection, record.id, ModelListType::Blacklist)
                    .map_err(to_io_error)?;
            records.push(record);
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
                "SELECT id, name, value, role, capture
                 FROM keys
                 WHERE value = ?1",
            )
            .map_err(to_io_error)?;
        let record = statement
            .query_row(params![token], |row| {
                let role: String = row.get(3)?;
                Ok(KeyRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    token: row.get(2)?,
                    role: parse_role(&role).ok_or(SqlError::InvalidQuery)?,
                    whitelist_models: Vec::new(),
                    blacklist_models: Vec::new(),
                    capture: row.get(4)?,
                })
            })
            .optional()
            .map_err(to_io_error)?;

        let Some(mut record) = record else {
            return Ok(None);
        };

        record.whitelist_models =
            list_model_rules(&connection, record.id, ModelListType::Whitelist)
                .map_err(to_io_error)?;
        record.blacklist_models =
            list_model_rules(&connection, record.id, ModelListType::Blacklist)
                .map_err(to_io_error)?;
        Ok(Some(record))
    }

    pub fn update_key_capture(&self, id: i64, capture: bool) -> io::Result<Option<KeyRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("key database mutex poisoned"))?;
        let changed = connection
            .execute(
                "UPDATE keys SET capture = ?1 WHERE id = ?2",
                params![capture, id],
            )
            .map_err(to_io_error)?;
        if changed == 0 {
            return Ok(None);
        }
        fetch_key_by_id(&connection, id).map_err(to_io_error)
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), SqlError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS keys (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             name TEXT NOT NULL UNIQUE,
             value TEXT NOT NULL UNIQUE,
             role TEXT NOT NULL,
             capture INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS key_model_rules (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             key_id INTEGER NOT NULL,
             list_type TEXT NOT NULL,
             model TEXT NOT NULL,
             UNIQUE(key_id, list_type, model),
             FOREIGN KEY(key_id) REFERENCES keys(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_key_model_rules_lookup
             ON key_model_rules (key_id, list_type, model);",
    )?;
    ensure_keys_capture_column(connection)
}

fn count_keys(connection: &Connection) -> Result<i64, SqlError> {
    connection.query_row("SELECT COUNT(*) FROM keys", [], |row| row.get(0))
}

fn seed_bootstrap_admin(connection: &Connection) -> Result<(), SqlError> {
    let token = Uuid::new_v4().to_string();
    connection.execute(
        "INSERT OR IGNORE INTO keys (name, value, role, capture)
         VALUES (?1, ?2, ?3, 0)",
        params!["admin", token, Role::Admin.as_str()],
    )?;
    log::warn("created initial admin key in sqlite database");
    log::warn(format!("initial admin name=admin token={token}"));
    Ok(())
}

fn ensure_keys_capture_column(connection: &Connection) -> Result<(), SqlError> {
    let mut statement = connection.prepare("PRAGMA table_info(keys)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "capture" {
            return Ok(());
        }
    }
    connection.execute(
        "ALTER TABLE keys ADD COLUMN capture INTEGER NOT NULL DEFAULT 0",
        [],
    )?;
    Ok(())
}

fn fetch_key_by_id(connection: &Connection, id: i64) -> Result<Option<KeyRecord>, SqlError> {
    let mut statement = connection.prepare(
        "SELECT id, name, value, role, capture
         FROM keys
         WHERE id = ?1",
    )?;
    let record = statement
        .query_row(params![id], |row| {
            let role: String = row.get(3)?;
            Ok(KeyRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                token: row.get(2)?,
                role: parse_role(&role).ok_or(SqlError::InvalidQuery)?,
                whitelist_models: Vec::new(),
                blacklist_models: Vec::new(),
                capture: row.get(4)?,
            })
        })
        .optional()?;
    let Some(mut record) = record else {
        return Ok(None);
    };
    record.whitelist_models = list_model_rules(connection, record.id, ModelListType::Whitelist)?;
    record.blacklist_models = list_model_rules(connection, record.id, ModelListType::Blacklist)?;
    Ok(Some(record))
}

fn parse_role(value: &str) -> Option<Role> {
    match value {
        "Admin" => Some(Role::Admin),
        "Client" => Some(Role::Client),
        "Worker" => Some(Role::Worker),
        _ => None,
    }
}

#[derive(Copy, Clone)]
enum ModelListType {
    Whitelist,
    Blacklist,
}

impl ModelListType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Whitelist => "whitelist",
            Self::Blacklist => "blacklist",
        }
    }
}

fn insert_model_rules(
    connection: &Connection,
    key_id: i64,
    list_type: ModelListType,
    models: &[String],
) -> Result<(), SqlError> {
    let mut statement = connection.prepare(
        "INSERT OR IGNORE INTO key_model_rules (key_id, list_type, model)
         VALUES (?1, ?2, ?3)",
    )?;

    for model in normalize_models(models) {
        statement.execute(params![key_id, list_type.as_str(), model])?;
    }

    Ok(())
}

fn list_model_rules(
    connection: &Connection,
    key_id: i64,
    list_type: ModelListType,
) -> Result<Vec<String>, SqlError> {
    let mut statement = connection.prepare(
        "SELECT model
         FROM key_model_rules
         WHERE key_id = ?1 AND list_type = ?2
         ORDER BY id ASC",
    )?;
    let rows = statement.query_map(params![key_id, list_type.as_str()], |row| row.get(0))?;
    let mut models = Vec::new();
    for row in rows {
        models.push(row?);
    }
    Ok(models)
}

fn normalize_models(values: &[String]) -> Vec<String> {
    let mut models = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if models.iter().any(|existing| existing == trimmed) {
            continue;
        }
        models.push(trimmed.to_string());
    }
    models
}

fn to_io_error(error: SqlError) -> io::Error {
    if let SqlError::SqliteFailure(err, _) = &error {
        if err.code == rusqlite::ErrorCode::ConstraintViolation {
            return io::Error::new(io::ErrorKind::AlreadyExists, error);
        }
    }
    io::Error::other(error)
}
