use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub protocol: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// Look up a header by name (case-sensitive, must be lowercase).
    ///
    /// Header names are normalized to lowercase at parse time
    /// (`read_request_from_reader` inserts `to_ascii_lowercase()`, and the
    /// hardcoded headers are already lowercase), so callers pass the
    /// pre-lowercased constant and we avoid a `String` allocation per lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        debug_assert_eq!(
            name,
            &name.to_ascii_lowercase(),
            "header names are stored lowercase; pass a lowercase name to avoid allocation"
        );
        self.headers.get(name).map(String::as_str)
    }

    pub fn bearer_token(&self) -> Option<&str> {
        if let Some(header) = self.header("authorization") {
            if let Some(token) = header
                .strip_prefix("Bearer ")
                .or_else(|| header.strip_prefix("bearer "))
            {
                return Some(token);
            }
        }

        self.header("api-key")
    }
}

#[cfg(test)]
mod tests {
    use super::HttpRequest;
    use std::collections::HashMap;

    fn request_with(headers: Vec<(&str, &str)>) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: Vec::new(),
        }
    }

    #[test]
    fn header_finds_lowercased_key_without_allocating() {
        let req = request_with(vec![("authorization", "Bearer abc123")]);
        assert_eq!(req.header("authorization"), Some("Bearer abc123"));
        assert_eq!(req.header("missing"), None);
    }

    #[test]
    fn bearer_token_reads_authorization_bearer() {
        let req = request_with(vec![("authorization", "Bearer sk-1234")]);
        assert_eq!(req.bearer_token(), Some("sk-1234"));
    }

    #[test]
    fn bearer_token_reads_lowercase_bearer_prefix() {
        let req = request_with(vec![("authorization", "bearer sk-5678")]);
        assert_eq!(req.bearer_token(), Some("sk-5678"));
    }

    #[test]
    fn bearer_token_falls_back_to_api_key() {
        let req = request_with(vec![("api-key", "key-9999")]);
        assert_eq!(req.bearer_token(), Some("key-9999"));
    }

    #[test]
    fn bearer_token_returns_none_without_auth() {
        let req = request_with(vec![]);
        assert_eq!(req.bearer_token(), None);
    }
}
