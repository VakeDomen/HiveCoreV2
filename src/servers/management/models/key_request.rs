use serde::Deserialize;

use crate::auth::Role;

#[derive(Deserialize)]
pub struct KeyRequest {
    pub role: Role,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub whitelist_models: Vec<String>,
    #[serde(default)]
    pub blacklist_models: Vec<String>,
}

fn default_name() -> String {
    "generated".to_string()
}
