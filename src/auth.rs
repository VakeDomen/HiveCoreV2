use std::collections::HashMap;
use std::io;
use std::sync::RwLock;

use crate::db::SqliteKeyStore;
use crate::log;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Admin,
    Client,
    Worker,
}

#[derive(Clone, Debug)]
pub struct KeyRecord {
    pub id: i64,
    pub token: String,
    pub role: Role,
    pub name: String,
    pub whitelist_models: Vec<String>,
    pub blacklist_models: Vec<String>,
}

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
        whitelist_models: Vec<String>,
        blacklist_models: Vec<String>,
    ) -> io::Result<KeyRecord> {
        let record = self
            .database
            .insert_key(token, role, name, whitelist_models, blacklist_models)?;
        if let Ok(mut guard) = self.cache.write() {
            guard.insert(record.token.clone(), record.clone());
        }
        Ok(record)
    }

    pub fn list(&self) -> io::Result<Vec<KeyRecord>> {
        self.database.list_keys()
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

impl KeyRecord {
    pub fn allows_model(&self, model: &str) -> bool {
        if self.blacklist_models.iter().any(|entry| entry == model) {
            return false;
        }
        if self.whitelist_models.is_empty() {
            return true;
        }
        self.whitelist_models.iter().any(|entry| entry == model)
    }
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "Admin",
            Role::Client => "Client",
            Role::Worker => "Worker",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyStore, Role};
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
    fn inserted_keys_round_trip_with_filters_and_roles() -> io::Result<()> {
        let path = temp_db_path("round_trip");
        let store = KeyStore::new(path.to_str().expect("utf8 path"))?;
        let token = Uuid::new_v4().to_string();
        let inserted = store.insert(
            token.clone(),
            Role::Worker,
            "worker-a".to_string(),
            vec!["llama3".to_string(), "mistral".to_string()],
            vec!["deepseek".to_string()],
        )?;

        assert_eq!(inserted.name, "worker-a");
        assert_eq!(inserted.role, Role::Worker);

        let verified = store
            .verify(&token, &[Role::Worker])
            .expect("worker key should verify");
        assert_eq!(verified.name, "worker-a");
        assert_eq!(verified.whitelist_models, vec!["llama3", "mistral"]);
        assert_eq!(verified.blacklist_models, vec!["deepseek"]);

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
            Vec::new(),
            Vec::new(),
        )?;

        let duplicate = store.insert(
            Uuid::new_v4().to_string(),
            Role::Client,
            "alice".to_string(),
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
            vec!["llama3".to_string()],
            Vec::new(),
        )?;

        let first = store.verify(&token, &[Role::Client]).expect("first verify");
        let second = store.verify(&token, &[Role::Client]).expect("second verify");

        assert_eq!(first.name, "alice");
        assert_eq!(second.name, "alice");
        assert_eq!(first.whitelist_models, vec!["llama3"]);
        assert_eq!(second.whitelist_models, vec!["llama3"]);

        cleanup(&path);
        Ok(())
    }

    #[test]
    fn blacklist_overrides_whitelist() {
        let record = super::KeyRecord {
            id: 1,
            token: Uuid::new_v4().to_string(),
            role: Role::Client,
            name: "alice".to_string(),
            whitelist_models: vec!["llama3".to_string(), "mistral".to_string()],
            blacklist_models: vec!["mistral".to_string()],
        };

        assert!(record.allows_model("llama3"));
        assert!(!record.allows_model("mistral"));
        assert!(!record.allows_model("deepseek"));
    }

    #[test]
    fn empty_whitelist_allows_any_non_blacklisted_model() {
        let record = super::KeyRecord {
            id: 1,
            token: Uuid::new_v4().to_string(),
            role: Role::Client,
            name: "alice".to_string(),
            whitelist_models: Vec::new(),
            blacklist_models: vec!["forbidden".to_string()],
        };

        assert!(record.allows_model("llama3"));
        assert!(!record.allows_model("forbidden"));
    }
}
