use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

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

#[derive(Debug)]
pub struct HttpResponse {
    pub status_code: u16,
    pub reason: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
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
    let body = String::from_utf8_lossy(body);
    let needle = format!("\"{field}\"");
    let start = body.find(&needle)?;
    let remainder = &body[start + needle.len()..];
    let colon = remainder.find(':')?;
    let value_part = remainder[colon + 1..].trim_start();
    if !value_part.starts_with('"') {
        return None;
    }
    let rest = &value_part[1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub fn extract_json_array_strings(body: &[u8], field: &str) -> Option<Vec<String>> {
    let body = String::from_utf8_lossy(body);
    let needle = format!("\"{field}\"");
    let start = body.find(&needle)?;
    let remainder = &body[start + needle.len()..];
    let colon = remainder.find(':')?;
    let value_part = remainder[colon + 1..].trim_start();
    if !value_part.starts_with('[') {
        return None;
    }
    let end = value_part.find(']')?;
    let inner = &value_part[1..end];
    let values = inner
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.trim_matches('"').to_string())
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    Some(values)
}

pub fn escape_json(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}
