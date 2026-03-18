use crate::auth::KeyRecord;

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
