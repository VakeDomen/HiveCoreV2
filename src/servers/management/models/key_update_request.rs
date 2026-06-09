use serde::Deserialize;

#[derive(Deserialize)]
pub struct KeyUpdateRequest {
    pub id: i64,
    pub name: Option<String>,
    pub capture: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::KeyUpdateRequest;

    #[test]
    fn key_update_allows_name_only() {
        let request: KeyUpdateRequest =
            serde_json::from_str(r#"{"id":42,"name":"bob"}"#).expect("valid json");

        assert_eq!(request.id, 42);
        assert_eq!(request.name.as_deref(), Some("bob"));
        assert_eq!(request.capture, None);
    }

    #[test]
    fn key_update_allows_capture_only() {
        let request: KeyUpdateRequest =
            serde_json::from_str(r#"{"id":42,"capture":false}"#).expect("valid json");

        assert_eq!(request.id, 42);
        assert_eq!(request.name, None);
        assert_eq!(request.capture, Some(false));
    }
}
