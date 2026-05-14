use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;

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
