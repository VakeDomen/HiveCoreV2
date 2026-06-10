use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

pub mod models;

pub use models::http_request::HttpRequest;
pub use models::http_response::HttpResponse;

/// Token counts extracted from an LLM response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Usage event sent to the stats worker via the mpsc channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageEvent {
    pub key_id: Option<i64>,
    pub key_name: String,
    pub worker_name: String,
    pub backend: String,
    pub model: String,
    pub status_code: u16,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub queue_ms: u64,
    pub worker_ms: u64,
    pub total_ms: u64,
    pub duration_ms: u64,
    pub created_at: u64, // unix timestamp in seconds
}

/// Try to parse token usage from a JSON string (last SSE chunk or full body).
/// Supports OpenAI format (`usage.prompt_tokens`, `usage.completion_tokens`)
/// and Ollama format (`prompt_eval_count`, `eval_count`). Completion/output
/// tokens are optional because embedding responses only report input tokens.
pub fn parse_usage_json(text: &str) -> Option<TokenUsage> {
    if let Some(usage) = parse_usage_value(text) {
        return Some(usage);
    }

    for line in text.lines().rev() {
        if let Some(usage) = parse_usage_value(line) {
            return Some(usage);
        }
    }

    None
}

pub fn request_usage_model_name(request: &HttpRequest) -> Option<String> {
    match (request.method.as_str(), request.uri.as_str()) {
        ("POST", "/api/generate")
        | ("POST", "/api/chat")
        | ("POST", "/api/embed")
        | ("POST", "/api/embeddings")
        | ("POST", "/v1/chat/completions")
        | ("POST", "/v1/chat/completions/batch")
        | ("POST", "/v1/completions")
        | ("POST", "/v1/embeddings")
        | ("POST", "/v2/embed")
        | ("POST", "/score")
        | ("POST", "/v1/score")
        | ("POST", "/rerank")
        | ("POST", "/v1/rerank")
        | ("POST", "/v2/rerank")
        | ("POST", "/tokenize")
        | ("POST", "/detokenize") => extract_json_value(&request.body, "model"),
        _ => None,
    }
}

pub fn ensure_openai_stream_usage(request: &mut HttpRequest) -> bool {
    if !matches!(
        (request.method.as_str(), request.uri.as_str()),
        ("POST", "/v1/chat/completions") | ("POST", "/v1/completions")
    ) {
        return false;
    }

    let Ok(mut body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
        return false;
    };
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    if !object
        .get("stream")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }

    let stream_options = object
        .entry("stream_options".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(stream_options) = stream_options.as_object_mut() else {
        return false;
    };

    if stream_options
        .get("include_usage")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return false;
    }

    stream_options.insert("include_usage".to_string(), serde_json::Value::Bool(true));
    let Ok(body) = serde_json::to_vec(&body) else {
        return false;
    };

    request.body = body;
    request
        .headers
        .insert("content-length".to_string(), request.body.len().to_string());
    true
}

pub fn request_model_name(request: &HttpRequest) -> Option<String> {
    match (request.method.as_str(), request.uri.as_str()) {
        ("POST", "/api/generate")
        | ("POST", "/api/chat")
        | ("POST", "/api/embed")
        | ("POST", "/api/embeddings")
        | ("POST", "/v1/chat/completions")
        | ("POST", "/v1/chat/completions/batch")
        | ("POST", "/v1/completions")
        | ("POST", "/v1/embeddings")
        | ("POST", "/v2/embed")
        | ("POST", "/score")
        | ("POST", "/v1/score")
        | ("POST", "/rerank")
        | ("POST", "/v1/rerank")
        | ("POST", "/v2/rerank")
        | ("POST", "/tokenize")
        | ("POST", "/detokenize")
        | ("POST", "/api/show")
        | ("POST", "/api/pull")
        | ("POST", "/api/push")
        | ("DELETE", "/api/delete") => extract_json_value(&request.body, "model"),
        ("POST", "/api/copy") => extract_json_value(&request.body, "source"),
        ("POST", "/api/create") => extract_json_value(&request.body, "from"),
        _ => None,
    }
}

fn parse_usage_value(text: &str) -> Option<TokenUsage> {
    let candidate = text.trim();
    if candidate.is_empty() || candidate == "[DONE]" {
        return None;
    }
    let candidate = candidate
        .strip_prefix("data:")
        .map(str::trim)
        .unwrap_or(candidate);
    if candidate.is_empty() || candidate == "[DONE]" {
        return None;
    }

    let parsed: serde_json::Value = serde_json::from_str(candidate).ok()?;

    // Try OpenAI format first
    if let Some(usage) = parsed.get("usage") {
        return parse_openai_usage(usage);
    }

    if let Some(usage) = parse_openai_usage(&parsed) {
        return Some(usage);
    }

    // Fall back to Ollama format
    let prompt_tokens = parsed.get("prompt_eval_count")?.as_u64()?;
    let completion_tokens = parsed
        .get("eval_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    })
}

fn parse_openai_usage(usage: &serde_json::Value) -> Option<TokenUsage> {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))?
        .as_u64()?;
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            usage
                .get("total_tokens")
                .and_then(serde_json::Value::as_u64)
                .map(|total| total.saturating_sub(prompt_tokens))
        })
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .or_else(|| usage.get("totalTokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(prompt_tokens + completion_tokens);

    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    })
}

impl HttpResponse {
    pub fn new(status_code: u16, reason: &'static str, body: Vec<u8>) -> Self {
        Self {
            status_code,
            reason,
            headers: vec![
                ("Content-Length".to_string(), body.len().to_string()),
                ("Connection".to_string(), "close".to_string()),
            ],
            body,
        }
    }

    pub fn json(status_code: u16, reason: &'static str, body: String) -> Self {
        let mut response = Self::new(status_code, reason, body.into_bytes());
        response
            .headers
            .push(("Content-Type".to_string(), "application/json".to_string()));
        response
    }

    pub fn write_to(&self, stream: &mut TcpStream) -> io::Result<()> {
        write!(stream, "HTTP/1.1 {} {}\r\n", self.status_code, self.reason)?;
        for (name, value) in &self.headers {
            write!(stream, "{}: {}\r\n", name, value)?;
        }
        write!(stream, "\r\n")?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

pub fn read_request(stream: &TcpStream) -> io::Result<HttpRequest> {
    let cloned = stream.try_clone()?;
    let mut reader = BufReader::new(cloned);
    read_request_from_reader(&mut reader)
}

pub fn read_request_from_reader<R: BufRead>(reader: &mut R) -> io::Result<HttpRequest> {
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if request_line.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty request line",
        ));
    }
    let parts: Vec<_> = request_line.trim_end().split_whitespace().collect();
    if parts.len() != 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request line",
        ));
    }

    let mut headers = HashMap::new();
    let protocol = parts[2].to_string();
    if protocol != "HIVE" {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
    }

    let mut body = Vec::new();
    if protocol != "HIVE" {
        if let Some(content_length) = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
        {
            let mut limited = reader.take(content_length as u64);
            limited.read_to_end(&mut body)?;
        }
    }

    Ok(HttpRequest {
        method: parts[0].to_string(),
        uri: parts[1].to_string(),
        protocol,
        headers,
        body,
    })
}

pub fn write_framed_request(stream: &mut TcpStream, request_bytes: &[u8]) -> io::Result<()> {
    let length = request_bytes.len() as u32;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(request_bytes)?;
    stream.flush()
}

pub fn serialize_request(request: &HttpRequest) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        format!(
            "{} {} {}\r\n",
            request.method, request.uri, request.protocol
        )
        .as_bytes(),
    );
    if request.protocol != "HIVE" {
        for (name, value) in &request.headers {
            bytes.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        bytes.extend_from_slice(&request.body);
    }
    bytes
}

pub fn extract_json_value(body: &[u8], field: &str) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        HttpRequest, TokenUsage, ensure_openai_stream_usage, parse_usage_json, request_model_name,
        request_usage_model_name,
    };

    #[test]
    fn parses_ollama_usage_from_raw_json() {
        assert_eq!(
            parse_usage_json(r#"{"prompt_eval_count":12,"eval_count":34}"#),
            Some(TokenUsage {
                prompt_tokens: 12,
                completion_tokens: 34,
                total_tokens: 46,
            })
        );
    }

    #[test]
    fn parses_openai_usage_from_stream_frame() {
        assert_eq!(
            parse_usage_json(
                "data: {\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":4}}\n\ndata: [DONE]\n"
            ),
            Some(TokenUsage {
                prompt_tokens: 9,
                completion_tokens: 4,
                total_tokens: 13,
            })
        );
    }

    #[test]
    fn ignores_openai_reasoning_token_details_for_normalized_usage() {
        assert_eq!(
            parse_usage_json(
                r#"{"usage":{"prompt_tokens":9,"completion_tokens":14,"total_tokens":23,"completion_tokens_details":{"reasoning_tokens":10}}}"#
            ),
            Some(TokenUsage {
                prompt_tokens: 9,
                completion_tokens: 14,
                total_tokens: 23,
            })
        );
    }

    #[test]
    fn parses_openai_embedding_usage_without_completion_tokens() {
        assert_eq!(
            parse_usage_json(r#"{"usage":{"prompt_tokens":21,"total_tokens":21}}"#),
            Some(TokenUsage {
                prompt_tokens: 21,
                completion_tokens: 0,
                total_tokens: 21,
            })
        );
    }

    #[test]
    fn parses_ollama_input_only_usage() {
        assert_eq!(
            parse_usage_json(r#"{"prompt_eval_count":13}"#),
            Some(TokenUsage {
                prompt_tokens: 13,
                completion_tokens: 0,
                total_tokens: 13,
            })
        );
    }

    #[test]
    fn resolves_request_model_name_for_supported_routes() {
        let request = HttpRequest {
            method: "POST".to_string(),
            uri: "/api/copy".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: Default::default(),
            body: br#"{"source":"base-model","destination":"copy"}"#.to_vec(),
        };

        assert_eq!(request_model_name(&request).as_deref(), Some("base-model"));
    }

    #[test]
    fn resolves_usage_model_name_only_for_inference_routes() {
        let show_request = HttpRequest {
            method: "POST".to_string(),
            uri: "/api/show".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: Default::default(),
            body: br#"{"model":"llama3"}"#.to_vec(),
        };
        let chat_request = HttpRequest {
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: Default::default(),
            body: br#"{"model":"llama3"}"#.to_vec(),
        };

        assert_eq!(request_usage_model_name(&show_request), None);
        assert_eq!(
            request_usage_model_name(&chat_request).as_deref(),
            Some("llama3")
        );
    }

    #[test]
    fn resolves_usage_model_name_for_vllm_model_routes() {
        let request = HttpRequest {
            method: "POST".to_string(),
            uri: "/rerank".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: Default::default(),
            body: br#"{"model":"reranker"}"#.to_vec(),
        };

        assert_eq!(
            request_usage_model_name(&request).as_deref(),
            Some("reranker")
        );
    }

    #[test]
    fn resolves_model_name_from_top_level_json_field() {
        let request = HttpRequest {
            method: "POST".to_string(),
            uri: "/api/generate".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: Default::default(),
            body: br#"{"messages":[{"model":"inner"}],"model":"outer"}"#.to_vec(),
        };

        assert_eq!(request_usage_model_name(&request).as_deref(), Some("outer"));
    }

    #[test]
    fn adds_usage_request_to_openai_streaming_calls() {
        let mut request = HttpRequest {
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: Default::default(),
            body: br#"{"model":"llama3","stream":true,"messages":[]}"#.to_vec(),
        };
        request
            .headers
            .insert("content-length".to_string(), request.body.len().to_string());

        assert!(ensure_openai_stream_usage(&mut request));
        let parsed: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
        let expected_len = request.body.len().to_string();
        assert_eq!(parsed["stream_options"]["include_usage"], true);
        assert_eq!(request.headers.get("content-length"), Some(&expected_len));
    }

    #[test]
    fn leaves_non_streaming_openai_calls_unchanged() {
        let mut request = HttpRequest {
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: Default::default(),
            body: br#"{"model":"llama3","stream":false,"messages":[]}"#.to_vec(),
        };

        assert!(!ensure_openai_stream_usage(&mut request));
    }
}
