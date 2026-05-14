use serde::Deserialize;

use crate::auth::Role;

#[derive(Deserialize)]
pub struct KeyRequest {
    pub role: Role,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_capture")]
    pub capture: bool,
    #[serde(default)]
    pub whitelist_models: Vec<String>,
    #[serde(default)]
    pub blacklist_models: Vec<String>,
}

fn default_name() -> String {
    "generated".to_string()
}

fn default_capture() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use crate::auth::Role;

    use super::KeyRequest;

    #[test]
    fn capture_defaults_to_true_when_creating_key() {
        let request: KeyRequest =
            serde_json::from_str(r#"{"role":"Client","name":"alice"}"#).expect("valid key json");

        assert_eq!(request.role, Role::Client);
        assert!(request.capture);
    }

    #[test]
    fn capture_can_be_disabled_when_creating_key() {
        let request: KeyRequest =
            serde_json::from_str(r#"{"role":"Client","capture":false}"#).expect("valid key json");

        assert!(!request.capture);
    }
}
