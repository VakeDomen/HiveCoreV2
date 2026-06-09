use std::collections::HashMap;
use std::io;
use std::sync::RwLock;

use crate::auth::{KeyRecord, Role};
use crate::shared::log;
use crate::shared::sqlite::SqliteKeyStore;

pub struct KeyStore {
    cache: RwLock<HashMap<String, KeyRecord>>,
    database: SqliteKeyStore,
}

impl KeyStore {
    pub fn new(database_url: &str) -> io::Result<Self> {
        let store = Self {
            cache: RwLock::new(HashMap::new()),
            database: SqliteKeyStore::open(database_url)?,
        };
        store.refresh_cache()?;
        Ok(store)
    }

    pub fn verify(&self, token: &str, allowed_roles: &[Role]) -> Option<KeyRecord> {
        if let Some(record) = self
            .cache
            .read()
            .ok()
            .and_then(|guard| guard.get(token).cloned())
        {
            return allowed_roles.contains(&record.role).then_some(record);
        }

        let record = match self.fetch_by_token(token) {
            Ok(Some(record)) => record,
            Ok(None) => return None,
            Err(err) => {
                log::error(format!("key lookup failed for token={token}: {err}"));
                return None;
            }
        };

        if let Ok(mut guard) = self.cache.write() {
            guard.insert(record.token.clone(), record.clone());
        }
        allowed_roles.contains(&record.role).then_some(record)
    }

    pub fn insert(
        &self,
        token: String,
        role: Role,
        name: String,
        capture: bool,
        whitelist_models: Vec<String>,
        blacklist_models: Vec<String>,
    ) -> io::Result<KeyRecord> {
        let record = self.database.insert_key(
            token,
            role,
            name,
            capture,
            whitelist_models,
            blacklist_models,
        )?;
        if let Ok(mut guard) = self.cache.write() {
            guard.insert(record.token.clone(), record.clone());
        }
        Ok(record)
    }

    pub fn list(&self) -> io::Result<Vec<KeyRecord>> {
        self.database.list_keys()
    }

    pub fn update_key(
        &self,
        id: i64,
        name: Option<String>,
        capture: Option<bool>,
    ) -> io::Result<Option<KeyRecord>> {
        let record = self.database.update_key(id, name, capture)?;
        self.refresh_cache()?;
        Ok(record)
    }

    pub fn delete(&self, id: i64) -> io::Result<bool> {
        let deleted = self.database.delete_key(id)?;
        if deleted {
            self.refresh_cache()?;
        }
        Ok(deleted)
    }

    fn refresh_cache(&self) -> io::Result<()> {
        let records = self.list()?;
        let cache = records
            .into_iter()
            .map(|record| (record.token.clone(), record))
            .collect::<HashMap<_, _>>();
        let mut guard = self
            .cache
            .write()
            .map_err(|_| io::Error::other("key cache poisoned"))?;
        *guard = cache;
        Ok(())
    }

    fn fetch_by_token(&self, token: &str) -> io::Result<Option<KeyRecord>> {
        self.database.fetch_key_by_token(token)
    }
}

#[cfg(test)]
mod tests {
    use super::KeyStore;
    use crate::auth::{KeyRecord, Role};
    use rusqlite::{Connection, params};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn temp_db_path(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hive_core_v2_{test_name}_{}.db", Uuid::new_v4()))
    }

    fn cleanup(path: &PathBuf) {
        let _ = fs::remove_file(path);
    }

    fn create_legacy_database(path: &PathBuf) -> io::Result<()> {
        let connection = Connection::open(path).map_err(io::Error::other)?;
        connection
            .execute_batch(
                "CREATE TABLE keys (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL UNIQUE,
                    value TEXT NOT NULL UNIQUE,
                    role TEXT NOT NULL
                );",
            )
            .map_err(io::Error::other)?;
        Ok(())
    }

    #[test]
    fn new_database_seeds_exactly_one_admin_key_with_uuid() -> io::Result<()> {
        let path = temp_db_path("seed_admin");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let keys = store.list()?;

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name, "admin");
        assert_eq!(keys[0].role, Role::Admin);
        assert!(Uuid::parse_str(&keys[0].token).is_ok());

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn existing_database_is_not_reseeded() -> io::Result<()> {
        let path = temp_db_path("no_reseed");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let initial = store.list()?;
        drop(store);

        let reopened = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let reopened_keys = reopened.list()?;

        assert_eq!(initial.len(), 1);
        assert_eq!(reopened_keys.len(), 1);
        assert_eq!(initial[0].token, reopened_keys[0].token);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn legacy_database_opens_without_rewriting_existing_keys() -> io::Result<()> {
        let path = temp_db_path("legacy_upgrade");
        create_legacy_database(&path)?;
        let token = Uuid::new_v4().to_string();

        {
            let connection = Connection::open(&path).map_err(io::Error::other)?;
            connection
                .execute(
                    "INSERT INTO keys (name, value, role) VALUES (?1, ?2, ?3)",
                    params!["legacy-worker", token, Role::Worker.as_str()],
                )
                .map_err(io::Error::other)?;
        }

        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let keys = store.list()?;

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name, "legacy-worker");
        assert_eq!(keys[0].token, token);
        assert_eq!(keys[0].role, Role::Worker);
        assert!(keys[0].whitelist_models.is_empty());
        assert!(keys[0].blacklist_models.is_empty());

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn legacy_database_gets_model_rule_table_without_reseeding() -> io::Result<()> {
        let path = temp_db_path("legacy_rules");
        create_legacy_database(&path)?;

        {
            let connection = Connection::open(&path).map_err(io::Error::other)?;
            connection
                .execute(
                    "INSERT INTO keys (name, value, role) VALUES (?1, ?2, ?3)",
                    params![
                        "legacy-admin",
                        Uuid::new_v4().to_string(),
                        Role::Admin.as_str()
                    ],
                )
                .map_err(io::Error::other)?;
        }

        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let inserted = store.insert(
            Uuid::new_v4().to_string(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["qwen3:0.6b".to_string()],
            vec!["hidden-model".to_string()],
        )?;

        assert_eq!(inserted.whitelist_models, vec!["qwen3:0.6b"]);
        assert_eq!(inserted.blacklist_models, vec!["hidden-model"]);

        let keys = store.list()?;
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|key| key.name == "legacy-admin"));
        let alice = keys
            .iter()
            .find(|key| key.name == "alice")
            .expect("inserted key should be listed");
        assert_eq!(alice.whitelist_models, vec!["qwen3:0.6b"]);
        assert_eq!(alice.blacklist_models, vec!["hidden-model"]);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn inserted_keys_round_trip_with_filters_and_roles() -> io::Result<()> {
        let path = temp_db_path("round_trip");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let token = Uuid::new_v4().to_string();
        let inserted = store.insert(
            token.clone(),
            Role::Worker,
            "worker-a".to_string(),
            true,
            vec!["llama3".to_string(), "mistral".to_string()],
            vec!["deepseek".to_string()],
        )?;

        assert_eq!(inserted.name, "worker-a");
        assert_eq!(inserted.role, Role::Worker);
        assert!(inserted.capture);

        let verified = store
            .verify(&token, &[Role::Worker])
            .expect("worker key should verify");
        assert_eq!(verified.name, "worker-a");
        assert_eq!(verified.whitelist_models, vec!["llama3", "mistral"]);
        assert_eq!(verified.blacklist_models, vec!["deepseek"]);
        assert!(verified.capture);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn update_capture_changes_database_and_cache() -> io::Result<()> {
        let path = temp_db_path("update_capture");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let token = Uuid::new_v4().to_string();
        let inserted = store.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;

        assert!(!inserted.capture);
        let updated = store
            .update_key(inserted.id, None, Some(true))?
            .expect("key should exist");
        assert!(updated.capture);
        let verified = store
            .verify(&token, &[Role::Client])
            .expect("key should verify");
        assert!(verified.capture);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn update_key_renames_key_and_refreshes_cache() -> io::Result<()> {
        let path = temp_db_path("rename_key");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let token = Uuid::new_v4().to_string();
        let inserted = store.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;

        let updated = store
            .update_key(inserted.id, Some("bob".to_string()), None)?
            .expect("key should exist");
        assert_eq!(updated.name, "bob");
        let verified = store
            .verify(&token, &[Role::Client])
            .expect("key should verify");
        assert_eq!(verified.name, "bob");

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn update_key_rejects_duplicate_name() -> io::Result<()> {
        let path = temp_db_path("rename_duplicate");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let first = store.insert(
            Uuid::new_v4().to_string(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;
        store.insert(
            Uuid::new_v4().to_string(),
            Role::Client,
            "bob".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;

        let result = store.update_key(first.id, Some("bob".to_string()), None);
        assert!(matches!(
            result,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists
        ));

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn delete_removes_key_from_database_and_cache() -> io::Result<()> {
        let path = temp_db_path("delete_key");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let token = Uuid::new_v4().to_string();
        let inserted = store.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;

        assert!(store.verify(&token, &[Role::Client]).is_some());
        assert!(store.delete(inserted.id)?);
        assert!(store.verify(&token, &[Role::Client]).is_none());
        assert!(!store.list()?.iter().any(|key| key.id == inserted.id));

        let connection = Connection::open(&path).map_err(io::Error::other)?;
        let deleted: bool = connection
            .query_row(
                "SELECT deleted FROM keys WHERE id = ?1",
                params![inserted.id],
                |row| row.get(0),
            )
            .map_err(io::Error::other)?;
        assert!(deleted);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn delete_returns_false_for_unknown_key() -> io::Result<()> {
        let path = temp_db_path("delete_missing_key");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;

        assert!(!store.delete(999_999)?);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn duplicate_key_name_is_rejected() -> io::Result<()> {
        let path = temp_db_path("duplicate_name");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        store.insert(
            Uuid::new_v4().to_string(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;

        let duplicate = store.insert(
            Uuid::new_v4().to_string(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        );

        assert!(matches!(
            duplicate,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists
        ));

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn verify_requires_an_allowed_role() -> io::Result<()> {
        let path = temp_db_path("verify_role");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let token = Uuid::new_v4().to_string();
        store.insert(
            token.clone(),
            Role::Worker,
            "worker-a".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;

        assert!(store.verify(&token, &[Role::Worker]).is_some());
        assert!(store.verify(&token, &[Role::Admin, Role::Client]).is_none());

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn verify_populates_cache_and_survives_repeated_calls() -> io::Result<()> {
        let path = temp_db_path("verify_cache");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let token = Uuid::new_v4().to_string();
        store.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;

        let first = store.verify(&token, &[Role::Client]).expect("first verify");
        let second = store
            .verify(&token, &[Role::Client])
            .expect("second verify");

        assert_eq!(first.name, "alice");
        assert_eq!(second.name, "alice");
        assert_eq!(first.whitelist_models, vec!["llama3"]);
        assert_eq!(second.whitelist_models, vec!["llama3"]);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn blacklist_overrides_whitelist() {
        let record = KeyRecord {
            id: 1,
            token: Uuid::new_v4().to_string(),
            role: Role::Client,
            name: "alice".to_string(),
            whitelist_models: vec!["llama3".to_string(), "mistral".to_string()],
            blacklist_models: vec!["mistral".to_string()],
            capture: false,
        };

        assert!(record.allows_model("llama3"));
        assert!(!record.allows_model("mistral"));
        assert!(!record.allows_model("deepseek"));
    }

    #[test]
    fn empty_whitelist_allows_any_non_blacklisted_model() {
        let record = KeyRecord {
            id: 1,
            token: Uuid::new_v4().to_string(),
            role: Role::Client,
            name: "alice".to_string(),
            whitelist_models: Vec::new(),
            blacklist_models: vec!["forbidden".to_string()],
            capture: false,
        };

        assert!(record.allows_model("llama3"));
        assert!(!record.allows_model("forbidden"));
    }

    #[test]
    fn bare_rule_matches_tagged_variants() {
        let record = KeyRecord {
            id: 1,
            token: Uuid::new_v4().to_string(),
            role: Role::Client,
            name: "alice".to_string(),
            whitelist_models: vec!["bge-m3".to_string()],
            blacklist_models: Vec::new(),
            capture: false,
        };

        assert!(record.allows_model("bge-m3"));
        assert!(record.allows_model("bge-m3:latest"));
    }

    #[test]
    fn latest_rule_matches_bare_alias() {
        let record = KeyRecord {
            id: 1,
            token: Uuid::new_v4().to_string(),
            role: Role::Client,
            name: "alice".to_string(),
            whitelist_models: vec!["bge-m3:latest".to_string()],
            blacklist_models: Vec::new(),
            capture: false,
        };

        assert!(record.allows_model("bge-m3"));
        assert!(record.allows_model("bge-m3:latest"));
        assert!(!record.allows_model("bge-m3:fp16"));
    }

    #[test]
    fn bare_blacklist_blocks_tagged_variants() {
        let record = KeyRecord {
            id: 1,
            token: Uuid::new_v4().to_string(),
            role: Role::Client,
            name: "alice".to_string(),
            whitelist_models: Vec::new(),
            blacklist_models: vec!["bge-m3".to_string()],
            capture: false,
        };

        assert!(!record.allows_model("bge-m3"));
        assert!(!record.allows_model("bge-m3:latest"));
    }
}
