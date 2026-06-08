use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::shared::http::HttpRequest;
use crate::shared::log;

#[derive(Clone, Debug, Serialize)]
pub struct CaptureEvent {
    pub schema_version: u8,
    pub request_id: u64,
    pub key_id: i64,
    pub key_name: String,
    pub worker_name: String,
    pub created_at: String,
    pub completed_at: String,
    pub method: String,
    pub uri: String,
    pub model: Option<String>,
    pub status_code: u16,
    pub timing: CaptureTiming,
    pub client_request: CapturedHttpRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarded_request: Option<CapturedHttpRequest>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proxy_mutations: Vec<String>,
    pub response: CapturedResponse,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureTiming {
    pub queue_ms: u64,
    pub worker_ms: u64,
    pub total_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapturedHttpRequest {
    pub method: String,
    pub uri: String,
    pub protocol: String,
    pub headers: HashMap<String, String>,
    pub body: CapturedBody,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapturedBody {
    pub encoding: &'static str,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapturedResponse {
    pub status_line: String,
    pub status_code: u16,
    pub headers: Vec<(String, String)>,
    pub transfer: ResponseTransfer,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<CapturedBody>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<CapturedStreamEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseTransfer {
    FixedLength,
    Chunked,
    UntilEof,
    Empty,
}

impl Default for ResponseTransfer {
    fn default() -> Self {
        Self::Empty
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CapturedStreamEvent {
    pub index: usize,
    pub received_at: String,
    pub chunk_size: usize,
    pub body: CapturedBody,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureCompressionReport {
    pub day: NaiveDate,
    pub files_compressed: u64,
    pub files_already_compressed: u64,
    pub captures: u64,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub errors: Vec<String>,
}

impl CaptureCompressionReport {
    pub fn bytes_saved(&self) -> i64 {
        self.original_bytes as i64 - self.compressed_bytes as i64
    }
}

pub fn capture_request(request: &HttpRequest) -> CapturedHttpRequest {
    CapturedHttpRequest {
        method: request.method.clone(),
        uri: request.uri.clone(),
        protocol: request.protocol.clone(),
        headers: sanitize_headers(&request.headers),
        body: capture_body(&request.body),
    }
}

pub fn capture_body(bytes: &[u8]) -> CapturedBody {
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        return CapturedBody {
            encoding: "utf8",
            data: String::from_utf8_lossy(bytes).into_owned(),
            json: Some(value),
        };
    }

    match std::str::from_utf8(bytes) {
        Ok(text) => CapturedBody {
            encoding: "utf8",
            data: text.to_string(),
            json: None,
        },
        Err(_) => CapturedBody {
            encoding: "hex",
            data: encode_hex(bytes),
            json: None,
        },
    }
}

pub fn redact_embedding_vectors(response: &mut CapturedResponse) {
    if let Some(body) = response.body.as_mut() {
        redact_embedding_body(body);
    }
    for event in &mut response.events {
        redact_embedding_body(&mut event.body);
    }
}

pub fn utc_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn run_capture_writer(capture_dir: String, rx: Receiver<CaptureEvent>) -> io::Result<()> {
    let root = PathBuf::from(capture_dir);
    fs::create_dir_all(&root)?;
    let mut failed_writes = 0_u64;

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                if let Err(err) = write_event(&root, &event) {
                    failed_writes += 1;
                    log::error(format!(
                        "failed to capture interaction id={} key={} failures={} error={err}",
                        event.request_id, event.key_name, failed_writes
                    ));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

pub fn compress_capture_day(
    capture_dir: &str,
    day: NaiveDate,
) -> io::Result<CaptureCompressionReport> {
    let root = Path::new(capture_dir);
    let mut report = CaptureCompressionReport {
        day,
        ..CaptureCompressionReport::default()
    };
    if !root.exists() {
        return Ok(report);
    }

    let day_name = day.format("%Y-%m-%d").to_string();
    for key_entry in fs::read_dir(root)? {
        let key_entry = match key_entry {
            Ok(entry) => entry,
            Err(err) => {
                report.errors.push(format!("failed to read capture entry: {err}"));
                continue;
            }
        };
        let day_dir = key_entry.path().join(&day_name);
        if !day_dir.is_dir() {
            continue;
        }
        let path = day_dir.join("interactions.jsonl");
        let compressed_path = day_dir.join("interactions.jsonl.gz");

        if path.exists() {
            match compress_capture_file(&path, &compressed_path) {
                Ok(summary) => {
                    report.files_compressed += 1;
                    report.captures += summary.captures;
                    report.original_bytes += summary.original_bytes;
                    report.compressed_bytes += summary.compressed_bytes;
                }
                Err(err) => {
                    report.errors.push(format!("{}: {err}", path.display()));
                }
            }
        } else if compressed_path.exists() {
            report.files_already_compressed += 1;
            if let Ok(metadata) = fs::metadata(&compressed_path) {
                report.compressed_bytes += metadata.len();
            }
        }
    }

    Ok(report)
}

fn write_event(root: &Path, event: &CaptureEvent) -> io::Result<()> {
    let day = DateTime::parse_from_rfc3339(&event.created_at)
        .map(|value| value.with_timezone(&Local).date_naive())
        .unwrap_or_else(|_| Local::now().date_naive())
        .format("%Y-%m-%d")
        .to_string();
    let dir = root.join(format!("key_{}", event.key_id)).join(day);
    fs::create_dir_all(&dir)?;
    let path = dir.join("interactions.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    serde_json::to_writer(&mut file, event).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    log::info(format!(
        "captured interaction id={} key={} path={}",
        event.request_id,
        event.key_name,
        path.display()
    ));
    Ok(())
}

struct CompressionSummary {
    captures: u64,
    original_bytes: u64,
    compressed_bytes: u64,
}

fn compress_capture_file(path: &Path, compressed_path: &Path) -> io::Result<CompressionSummary> {
    let original_bytes = fs::metadata(path)?.len();
    let captures = count_lines(path)?;
    let status = Command::new("gzip")
        .arg("-f")
        .arg("-n")
        .arg(path)
        .status()
        .map_err(io::Error::other)?;
    if !status.success() {
        return Err(io::Error::other(format!("gzip exited with status {status}")));
    }
    let compressed_bytes = fs::metadata(compressed_path)?.len();
    Ok(CompressionSummary {
        captures,
        original_bytes,
        compressed_bytes,
    })
}

fn count_lines(path: &Path) -> io::Result<u64> {
    let file = File::open(path)?;
    let mut count = 0_u64;
    for line in BufReader::new(file).lines() {
        line?;
        count += 1;
    }
    Ok(count)
}

fn sanitize_headers(headers: &HashMap<String, String>) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let sanitized = if sensitive_header(name) {
                "[redacted]".to_string()
            } else {
                value.clone()
            };
            (name.clone(), sanitized)
        })
        .collect()
}

fn sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "api-key" | "x-api-key"
    )
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn redact_embedding_body(body: &mut CapturedBody) {
    let Some(mut value) = body.json.take() else {
        return;
    };
    redact_embedding_value(&mut value);
    body.data = serde_json::to_string(&value).unwrap_or_else(|_| body.data.clone());
    body.json = Some(value);
}

fn redact_embedding_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for key in ["embedding", "embeddings"] {
                if let Some(vectors) = object.get_mut(key) {
                    *vectors = embedding_redaction(vectors);
                }
            }
            for nested in object.values_mut() {
                redact_embedding_value(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                redact_embedding_value(nested);
            }
        }
        _ => {}
    }
}

fn embedding_redaction(value: &Value) -> Value {
    match value {
        Value::Array(values) => {
            let dimensions = values
                .first()
                .and_then(|first| match first {
                    Value::Array(inner) => Some(inner.len()),
                    Value::Number(_) => Some(values.len()),
                    _ => None,
                })
                .unwrap_or(0);
            json!({
                "redacted": true,
                "count": if values.first().is_some_and(Value::is_array) {
                    values.len()
                } else {
                    1
                },
                "dimensions": dimensions
            })
        }
        _ => json!({ "redacted": true }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;

    use chrono::NaiveDate;
    use uuid::Uuid;

    use super::compress_capture_day;

    #[test]
    fn compress_capture_day_gzips_interactions_and_counts_records() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("hive_capture_{}", Uuid::new_v4()));
        let day = NaiveDate::from_ymd_opt(2026, 5, 14).expect("valid day");
        let dir = root.join("key_42").join("2026-05-14");
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("interactions.jsonl"),
            "{\"request_id\":1}\n{\"request_id\":2}\n",
        )?;

        let report = compress_capture_day(root.to_str().expect("utf8 path"), day)?;

        assert_eq!(report.files_compressed, 1);
        assert_eq!(report.captures, 2);
        assert!(report.original_bytes > 0);
        assert!(report.compressed_bytes > 0);
        assert!(!dir.join("interactions.jsonl").exists());
        assert!(dir.join("interactions.jsonl.gz").exists());

        fs::remove_dir_all(root)?;
        Ok(())
    }
}
