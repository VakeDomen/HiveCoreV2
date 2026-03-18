use std::collections::HashMap;

#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub protocol: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn bearer_token(&self) -> Option<&str> {
        let header = self.header("authorization")?;
        header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
    }
}
