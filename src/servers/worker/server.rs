use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app::AppState;
use crate::auth::Role;
use crate::servers::proxy::models::response_target::ResponseTarget;
use crate::servers::proxy::models::worker_http_response::WorkerHttpResponse;
use crate::servers::worker::models::worker_phase::WorkerPhase;
use crate::servers::worker::models::worker_status::WorkerStatus;
use crate::shared::http::{
    HttpRequest, TokenUsage, UsageEvent, read_request_from_reader, serialize_request,
    write_framed_request,
};
use crate::shared::log;

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", state.config.node_connection_port))?;
    log::info(format!(
        "worker listener bound on 0.0.0.0:{}",
        state.config.node_connection_port
    ));
    for connection in listener.incoming() {
        let state = Arc::clone(&state);
        match connection {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = handle_connection(state, stream);
                });
            }
            Err(err) => log::error(format!("worker accept error: {err}")),
        }
    }
    Ok(())
}

fn handle_connection(state: Arc<AppState>, stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(
        state.config.working_node_connection_timeout,
    )))?;
    stream.set_write_timeout(Some(Duration::from_millis(state.config.proxy_timeout_ms)))?;

    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;

    let auth_request = read_request_from_reader(&mut reader)?;
    let worker_name = authenticate_worker(&state, &mut writer, &auth_request)?;
    log::success(format!(
        "worker authenticated name={}",
        log::bold(&worker_name)
    ));

    loop {
        let request = match read_request_from_reader(&mut reader) {
            Ok(request) => request,
            Err(err) => {
                remove_worker(&state, &worker_name);
                log::warn(format!(
                    "worker disconnected name={} error={err}",
                    worker_name
                ));
                return Err(err);
            }
        };

        match request.method.as_str() {
            "POLL" => handle_poll(&state, &worker_name, &request, &mut writer, &mut reader)?,
            "PING" => touch_worker(&state, &worker_name, None, WorkerPhase::Polling),
            _ => touch_worker(&state, &worker_name, None, WorkerPhase::Polling),
        }
    }
}

fn authenticate_worker(
    state: &Arc<AppState>,
    writer: &mut TcpStream,
    request: &HttpRequest,
) -> io::Result<String> {
    if request.protocol != "HIVE" || request.method != "AUTH" {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "worker must authenticate first",
        ));
    }

    let parts: Vec<_> = request.uri.split(';').collect();
    if parts.len() != 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "AUTH uri must contain key;nonce;hiveVersion;ollamaVersion",
        ));
    }

    let token = parts[0];
    let nonce = parts[1].to_string();
    let hive_version = parts[2].to_string();
    let ollama_version = parts[3].to_string();

    let Some(record) = state.keys.verify(token, &[Role::Admin, Role::Worker]) else {
        log::warn("rejected worker authentication");
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "worker key rejected",
        ));
    };

    let worker_name = record.name.clone();
    {
        let mut guard = state
            .workers
            .write()
            .map_err(|_| io::Error::other("worker registry poisoned"))?;
        if let Some(existing) = guard.get(&worker_name) {
            if existing.nonce != nonce {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "worker name already in use with a different nonce",
                ));
            }
        }
        guard.insert(
            worker_name.clone(),
            WorkerStatus {
                name: worker_name.clone(),
                nonce,
                hive_version,
                ollama_version,
                tags: Vec::new(),
                state: WorkerPhase::Authenticating,
                last_ping: Instant::now(),
                last_poll: Instant::now(),
                model_catalog: None,
                running_models: None,
                version_payload: None,
            },
        );
    }

    let response = HttpRequest {
        method: "AUTH".to_string(),
        uri: worker_name.clone(),
        protocol: "HIVE".to_string(),
        headers: Default::default(),
        body: Vec::new(),
    };
    let bytes = serialize_request(&response);
    write_framed_request(writer, &bytes)?;
    touch_worker(state, &worker_name, None, WorkerPhase::Polling);
    log::info(format!(
        "worker auth accepted name={} role={} hive_version={} ollama_version={}",
        log::bold(&worker_name),
        record.role.as_str(),
        log::bold(parts[2]),
        log::bold(parts[3])
    ));
    Ok(worker_name)
}

fn handle_poll(
    state: &Arc<AppState>,
    worker_name: &str,
    poll_request: &HttpRequest,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) -> io::Result<()> {
    let incoming_tags = if poll_request.uri == "-" {
        None
    } else {
        Some(
            poll_request
                .uri
                .split(';')
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>(),
        )
    };
    touch_worker(state, worker_name, incoming_tags, WorkerPhase::Polling);

    let tags = state
        .workers
        .read()
        .ok()
        .and_then(|guard| guard.get(worker_name).map(|worker| worker.tags.clone()))
        .unwrap_or_default();

    if let Some(task) = state.request_queue.dequeue_for_worker(worker_name, &tags) {
        touch_worker(state, worker_name, None, WorkerPhase::Working);
        let request_id = task.id;
        let request_method = task.request.method.clone();
        let request_uri = task.request.uri.clone();
        let queue_wait = task.queue_wait();
        let user_name = task
            .context
            .key_name
            .as_deref()
            .unwrap_or("Unauthenticated");
        let started_at = Instant::now();
        let bytes = serialize_request(&task.request);
        log::info(format!(
            "forwarding request id={} to worker={} user={} method={} uri={} queue_wait={}",
            log::bold(request_id.to_string()),
            log::bold(worker_name),
            log::bold(user_name),
            request_method,
            request_uri,
            log::bold(log::format_duration(queue_wait))
        ));
        write_framed_request(writer, &bytes)?;

        let (status_code, token_usage): (u16, Option<TokenUsage>) = match task.response_target {
            ResponseTarget::ProxyClient(mut client_stream) => {
                proxy_worker_response(reader, &mut client_stream)?
            }
            ResponseTarget::Capture(sender) => {
                let response = capture_worker_response(reader)?;
                let status_code = response.status_code;
                let _ = sender.send(response);
                (status_code, None)
            }
            ResponseTarget::Ignore => discard_worker_response(reader).map(|sc| (sc, None))?,
        };

        let worker_time = started_at.elapsed();

        // Record every successful request. Token counts may be unavailable on some streamed
        // OpenAI-compatible responses, in which case we still count the request with zero usage.
        if status_code < 400 && task.context.model.is_some() {
            let tu = token_usage.unwrap_or(TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
            });
            let model = task.context.model.clone().unwrap_or_default();
            let key_name = task
                .context
                .key_name
                .clone()
                .unwrap_or_else(|| "Unauthenticated".to_string());
            let duration_ms = worker_time.as_millis() as u64;
            let created_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let usage_event = UsageEvent {
                key_name,
                model,
                prompt_tokens: tu.prompt_tokens,
                completion_tokens: tu.completion_tokens,
                duration_ms,
                created_at,
            };
            let _ = state.stats_tx.send(usage_event);
        }

        touch_worker(state, worker_name, None, WorkerPhase::Polling);
        let total_time = queue_wait + worker_time;
        log::success(format!(
            "resolved request id={} user={} worker={} method={} uri={} status={} wait={} worker_time={} total={}",
            log::bold(request_id.to_string()),
            log::bold(user_name),
            log::bold(worker_name),
            request_method,
            request_uri,
            log::bold(status_code.to_string()),
            log::bold(log::format_duration(queue_wait)),
            log::bold(log::format_duration(worker_time)),
            log::bold(log::format_duration(total_time))
        ));
        return Ok(());
    }

    let pong = HttpRequest {
        method: "PONG".to_string(),
        uri: "/".to_string(),
        protocol: "HIVE".to_string(),
        headers: Default::default(),
        body: Vec::new(),
    };
    let bytes = serialize_request(&pong);
    write_framed_request(writer, &bytes)
}

fn proxy_worker_response<R, W>(
    reader: &mut R,
    client_stream: &mut W,
) -> io::Result<(u16, Option<TokenUsage>)>
where
    R: BufRead,
    W: Write,
{
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    if status_line.trim().is_empty() {
        log::warn("worker returned empty status line");
        write_empty_response(client_stream, 502, "Bad Gateway")?;
        return Ok((502, None));
    }

    let mut headers = Vec::new();
    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worker response ended before headers completed",
            ));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let lower = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if lower == "content-length" {
                content_length = value.parse::<usize>().ok();
            }
            if lower == "transfer-encoding" && transfer_encoding_is_chunked(&value) {
                chunked = true;
            }
            headers.push((name.trim().to_string(), value));
        }
    }

    client_stream.write_all(status_line.as_bytes())?;
    for (name, value) in sanitize_response_headers(&headers, chunked) {
        client_stream.write_all(format!("{name}: {value}\r\n").as_bytes())?;
    }
    client_stream.write_all(b"\r\n")?;

    let observed_usage = if chunked {
        relay_chunked_body(reader, client_stream)?
    } else if let Some(length) = content_length {
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;
        client_stream.write_all(&body)?;
        crate::shared::http::parse_usage_json(&String::from_utf8_lossy(&body))
    } else {
        let mut body = Vec::new();
        reader.read_to_end(&mut body)?;
        client_stream.write_all(&body)?;
        crate::shared::http::parse_usage_json(&String::from_utf8_lossy(&body))
    };

    client_stream.flush()?;

    let status_code = parse_status_code(&status_line);
    Ok((status_code, observed_usage))
}

fn discard_worker_response(reader: &mut BufReader<TcpStream>) -> io::Result<u16> {
    let response = capture_worker_response(reader)?;
    Ok(response.status_code)
}

fn capture_worker_response(reader: &mut BufReader<TcpStream>) -> io::Result<WorkerHttpResponse> {
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    if status_line.trim().is_empty() {
        return Ok(WorkerHttpResponse {
            status_code: 502,
            body: Vec::new(),
        });
    }

    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worker response ended before headers completed",
            ));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
            if name.trim().eq_ignore_ascii_case("transfer-encoding")
                && transfer_encoding_is_chunked(value.trim())
            {
                chunked = true;
            }
        }
    }

    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "worker response ended before chunk size",
                ));
            }
            let size = parse_chunk_size(&size_line)?;
            if size == 0 {
                discard_chunk_trailers(reader)?;
                break;
            }
            let mut chunk = vec![0_u8; size];
            reader.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0_u8; 2];
            reader.read_exact(&mut crlf)?;
        }
    } else if let Some(length) = content_length {
        let mut fixed = vec![0_u8; length];
        reader.read_exact(&mut fixed)?;
        body = fixed;
    }

    Ok(WorkerHttpResponse {
        status_code: parse_status_code(&status_line),
        body,
    })
}

fn sanitize_response_headers(headers: &[(String, String)], chunked: bool) -> Vec<(String, String)> {
    let mut sanitized = Vec::with_capacity(headers.len());
    let mut transfer_encoding = None;

    for (name, value) in headers {
        if chunked && name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        if chunked && name.eq_ignore_ascii_case("transfer-encoding") {
            if transfer_encoding.is_none() {
                transfer_encoding = Some(value.clone());
            }
            continue;
        }
        sanitized.push((name.clone(), value.clone()));
    }

    if chunked {
        sanitized.push((
            "Transfer-Encoding".to_string(),
            transfer_encoding.unwrap_or_else(|| "chunked".to_string()),
        ));
    }

    sanitized
}

fn transfer_encoding_is_chunked(value: &str) -> bool {
    value
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
}

fn relay_chunked_body<R, W>(reader: &mut R, client_stream: &mut W) -> io::Result<Option<TokenUsage>>
where
    R: BufRead,
    W: Write,
{
    let mut observed_usage = None;

    loop {
        let mut size_line = String::new();
        if reader.read_line(&mut size_line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worker response ended before chunk size",
            ));
        }
        client_stream.write_all(size_line.as_bytes())?;

        let size = parse_chunk_size(&size_line)?;
        if size == 0 {
            relay_chunk_trailers(reader, client_stream)?;
            break;
        }

        let mut chunk = vec![0_u8; size];
        reader.read_exact(&mut chunk)?;
        client_stream.write_all(&chunk)?;

        let mut crlf = [0_u8; 2];
        reader.read_exact(&mut crlf)?;
        client_stream.write_all(&crlf)?;

        let chunk_text = String::from_utf8_lossy(&chunk);
        if let Some(usage) = crate::shared::http::parse_usage_json(chunk_text.as_ref()) {
            observed_usage = Some(usage);
        }
    }

    Ok(observed_usage)
}

fn parse_chunk_size(size_line: &str) -> io::Result<usize> {
    let token = size_line.trim_end().split(';').next().unwrap_or("").trim();

    if token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty chunk size",
        ));
    }

    usize::from_str_radix(token, 16).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid chunk size '{token}': {err}"),
        )
    })
}

fn relay_chunk_trailers<R, W>(reader: &mut R, client_stream: &mut W) -> io::Result<()>
where
    R: BufRead,
    W: Write,
{
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worker response ended before chunk trailers completed",
            ));
        }
        client_stream.write_all(line.as_bytes())?;
        if line.trim_end().is_empty() {
            return Ok(());
        }
    }
}

fn discard_chunk_trailers(reader: &mut BufReader<TcpStream>) -> io::Result<()> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worker response ended before chunk trailers completed",
            ));
        }
        if line.trim_end().is_empty() {
            return Ok(());
        }
    }
}

fn touch_worker(
    state: &Arc<AppState>,
    worker_name: &str,
    tags: Option<Vec<String>>,
    phase: WorkerPhase,
) {
    if let Ok(mut guard) = state.workers.write() {
        if let Some(worker) = guard.get_mut(worker_name) {
            if let Some(tags) = tags {
                worker.tags = tags;
            }
            worker.state = phase;
            worker.last_ping = Instant::now();
            worker.last_poll = Instant::now();
        }
    }
}

fn remove_worker(state: &Arc<AppState>, worker_name: &str) {
    if let Ok(mut guard) = state.workers.write() {
        guard.remove(worker_name);
        log::info(format!("removed worker={}", log::bold(worker_name)));
    }
}

fn write_empty_response<W: Write>(
    stream: &mut W,
    status: u16,
    reason: &'static str,
) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {status} {reason}\r\n")?;
    write!(stream, "Content-Length: 0\r\n")?;
    write!(stream, "Connection: close\r\n")?;
    write!(stream, "\r\n")?;
    stream.flush()
}

fn parse_status_code(status_line: &str) -> u16 {
    status_line
        .trim_end()
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(500)
}

#[cfg(test)]
mod tests {
    use std::io::{self, BufReader};

    use crate::shared::http::TokenUsage;

    use super::{parse_chunk_size, proxy_worker_response};

    #[test]
    fn proxy_worker_response_removes_content_length_from_chunked_response() -> io::Result<()> {
        let worker_response = b"HTTP/1.1 200 OK\r\n\
                                Content-Type: application/json\r\n\
                                Content-Length: 16\r\n\
                                Transfer-Encoding: chunked\r\n\
                                Connection: close\r\n\
                                \r\n\
                                10\r\n\
                                {\"message\":\"ok\"}\r\n\
                                0\r\n\
                                \r\n";
        let mut reader = BufReader::new(&worker_response[..]);
        let mut relayed = Vec::new();

        let (status_code, _) = proxy_worker_response(&mut reader, &mut relayed)?;
        let relayed = String::from_utf8(relayed).expect("relayed response should be utf8");

        assert_eq!(status_code, 200);
        assert!(relayed.contains("Transfer-Encoding: chunked\r\n"));
        assert!(!relayed.to_ascii_lowercase().contains("content-length:"));
        assert!(relayed.ends_with("0\r\n\r\n"));
        Ok(())
    }

    #[test]
    fn proxy_worker_response_observes_usage_from_chunked_response() -> io::Result<()> {
        let usage_payload = r#"data: {"usage":{"prompt_tokens":9,"completion_tokens":4}}"#;
        let worker_response = format!(
            "HTTP/1.1 200 OK\r\n\
             Transfer-Encoding: chunked\r\n\
             \r\n\
             {:x}\r\n\
             {}\r\n\
             0\r\n\
             \r\n",
            usage_payload.len(),
            usage_payload
        );
        let mut reader = BufReader::new(worker_response.as_bytes());
        let mut relayed = Vec::new();

        let (status_code, usage) = proxy_worker_response(&mut reader, &mut relayed)?;

        assert_eq!(status_code, 200);
        assert_eq!(
            usage,
            Some(TokenUsage {
                prompt_tokens: 9,
                completion_tokens: 4,
            })
        );
        Ok(())
    }

    #[test]
    fn parse_chunk_size_accepts_extensions() -> io::Result<()> {
        assert_eq!(parse_chunk_size("a;foo=bar\r\n")?, 10);
        Ok(())
    }
}
