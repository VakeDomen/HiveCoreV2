use crate::auth::Role;

#[derive(Clone, Debug)]
pub struct KeyRecord {
    pub id: i64,
    pub token: String,
    pub role: Role,
    pub name: String,
    pub whitelist_models: Vec<String>,
    pub blacklist_models: Vec<String>,
    pub capture: bool,
}
