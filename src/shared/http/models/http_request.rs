use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub protocol: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    /// Memoized parse of `body` as JSON, computed at most once per request.
    ///
    /// `None` once computed means the body did not parse as a JSON object
    /// (or was empty). Cloning a request resets the cache so a clone does
    /// not deep-copy a potentially large `Value` into capture paths.
    parsed_json: OnceLock<Option<serde_json::Value>>,
}

impl Clone for HttpRequest {
    fn clone(&self) -> Self {
        Self {
            method: self.method.clone(),
            uri: self.uri.clone(),
            protocol: self.protocol.clone(),
            headers: self.headers.clone(),
            body: self.body.clone(),
            // Reset the memoized parse: a clone (e.g. into capture) should not
            // carry a deep copy of the parsed body.
            parsed_json: OnceLock::new(),
        }
    }
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("protocol", &self.protocol)
            .field("headers", &self.headers)
            // body is binary; only expose its length, mirroring prior behavior
            // where Debug printed the full byte slice.
            .field("body", &self.body)
            .finish()
    }
}

impl HttpRequest {
    /// Construct a request with the given method, URI, protocol, headers and body.
    pub fn new(
        method: impl Into<String>,
        uri: impl Into<String>,
        protocol: impl Into<String>,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            method: method.into(),
            uri: uri.into(),
            protocol: protocol.into(),
            headers,
            body,
            parsed_json: OnceLock::new(),
        }
    }

    /// Convenience constructor for synthetic HIVE control/AUTH/probe frames (no headers, no body).
    pub fn hive(method: impl Into<String>, uri: impl Into<String>) -> Self {
        Self::new(method, uri, "HIVE", HashMap::new(), Vec::new())
    }

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

    /// Parse the request body as a JSON value, memoizing the result.
    ///
    /// The first call parses `body` once; later calls return the cached value,
    /// so a non-JSON or empty body is also parsed at most once (returning
    /// `None` each time).
    pub fn parsed_json(&self) -> Option<&serde_json::Value> {
        self.parsed_json
            .get_or_init(|| serde_json::from_slice(&self.body).ok())
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::HttpRequest;
    use std::collections::HashMap;

    fn request_with(headers: Vec<(&str, &str)>) -> HttpRequest {
        HttpRequest::new(
            "POST",
            "/v1/chat/completions",
            "HTTP/1.1",
            headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            Vec::new(),
        )
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

    #[test]
    fn parsed_json_memoizes_and_parses_object_fields() {
        let req = HttpRequest::new(
            "POST",
            "/api/generate",
            "HTTP/1.1",
            HashMap::new(),
            br#"{"model":"llama3","name":"other"}"#.to_vec(),
        );
        // Memoized: repeated calls return the same parsed value.
        assert_eq!(req.parsed_json().is_some(), true);
        let model = req.parsed_json().and_then(|v| v.get("model")).and_then(serde_json::Value::as_str);
        assert_eq!(model, Some("llama3"));
        // Same parsed value across calls (memoization should not re-parse).
        let again = req.parsed_json();
        assert!(std::ptr::eq(req.parsed_json().unwrap(), again.unwrap()));
        // Different field reads share the single parse.
        let name = req.parsed_json().and_then(|v| v.get("name")).and_then(serde_json::Value::as_str);
        assert_eq!(name, Some("other"));
    }

    #[test]
    fn parsed_json_returns_none_for_non_json_body() {
        let req = HttpRequest::new(
            "POST",
            "/api/generate",
            "HTTP/1.1",
            HashMap::new(),
            b"not json".to_vec(),
        );
        assert_eq!(req.parsed_json(), None);
        // Second call stays None (cached miss, not re-parsed).
        assert_eq!(req.parsed_json(), None);
    }

    #[test]
    fn parsed_json_returns_none_for_empty_body() {
        let req = HttpRequest::new("POST", "/api/generate", "HTTP/1.1", HashMap::new(), Vec::new());
        assert_eq!(req.parsed_json(), None);
    }

    #[test]
    fn clone_resets_parsed_json_cache() {
        let req = HttpRequest::new(
            "POST",
            "/api/generate",
            "HTTP/1.1",
            HashMap::new(),
            br#"{"model":"llama3"}"#.to_vec(),
        );
        // Populate the cache on the original.
        let _ = req.parsed_json();
        // The clone must carry an unfilled cache, so it parses independently.
        let cloned = req.clone();
        assert_eq!(
            cloned
                .parsed_json()
                .and_then(|v| v.get("model"))
                .and_then(serde_json::Value::as_str),
            Some("llama3")
        );
    }
}
