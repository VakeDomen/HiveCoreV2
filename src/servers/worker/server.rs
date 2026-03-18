use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::app::AppState;
use crate::auth::Role;
use crate::servers::proxy::models::response_target::ResponseTarget;
use crate::servers::proxy::models::worker_http_response::WorkerHttpResponse;
use crate::servers::worker::models::worker_phase::WorkerPhase;
use crate::servers::worker::models::worker_status::WorkerStatus;
use crate::shared::http::{
    HttpRequest, HttpResponse, read_request_from_reader, serialize_request, write_framed_request,
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
    log::success(format!("worker authenticated name={}", log::bold(&worker_name)));

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
        let started_at = Instant::now();
        let bytes = serialize_request(&task.request);
        log::info(format!(
            "forwarding request id={} to worker={} method={} uri={} queue_wait={}",
            log::bold(request_id.to_string()),
            log::bold(worker_name),
            request_method,
            request_uri,
            log::bold(log::format_duration(queue_wait))
        ));
        write_framed_request(writer, &bytes)?;

        let status_code = match task.response_target {
            ResponseTarget::ProxyClient(mut client_stream) => {
                proxy_worker_response(reader, &mut client_stream)?
            }
            ResponseTarget::Capture(sender) => {
                let response = capture_worker_response(reader)?;
                let status_code = response.status_code;
                let _ = sender.send(response);
                status_code
            }
            ResponseTarget::Ignore => discard_worker_response(reader)?,
        };

        touch_worker(state, worker_name, None, WorkerPhase::Polling);
        let worker_time = started_at.elapsed();
        let total_time = queue_wait + worker_time;
        log::success(format!(
            "resolved request id={} worker={} method={} uri={} status={} wait={} worker_time={} total={}",
            log::bold(request_id.to_string()),
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

fn proxy_worker_response(
    reader: &mut BufReader<TcpStream>,
    client_stream: &mut TcpStream,
) -> io::Result<u16> {
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    if status_line.trim().is_empty() {
        log::warn("worker returned empty status line");
        return reject_request(client_stream.try_clone()?, 502, "Bad Gateway");
    }

    let mut headers = Vec::new();
    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
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
            if lower == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
                chunked = true;
            }
            headers.push((name.trim().to_string(), value));
        }
    }

    client_stream.write_all(status_line.as_bytes())?;
    for (name, value) in &headers {
        client_stream.write_all(format!("{name}: {value}\r\n").as_bytes())?;
    }
    client_stream.write_all(b"\r\n")?;

    if chunked {
        loop {
            let mut size_line = String::new();
            reader.read_line(&mut size_line)?;
            client_stream.write_all(size_line.as_bytes())?;
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                let mut trailing = [0_u8; 2];
                reader.read_exact(&mut trailing)?;
                client_stream.write_all(&trailing)?;
                break;
            }
            let mut chunk = vec![0_u8; size + 2];
            reader.read_exact(&mut chunk)?;
            client_stream.write_all(&chunk)?;
        }
    } else if let Some(length) = content_length {
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;
        client_stream.write_all(&body)?;
    } else {
        let mut body = Vec::new();
        reader.read_to_end(&mut body)?;
        client_stream.write_all(&body)?;
    }

    client_stream.flush()?;

    Ok(parse_status_code(&status_line))
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
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
            if name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }

    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            reader.read_line(&mut size_line)?;
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                let mut trailing = [0_u8; 2];
                reader.read_exact(&mut trailing)?;
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

fn reject_request(mut stream: TcpStream, status: u16, reason: &'static str) -> io::Result<u16> {
    HttpResponse::new(status, reason, Vec::new()).write_to(&mut stream)?;
    Ok(status)
}

fn parse_status_code(status_line: &str) -> u16 {
    status_line
        .trim_end()
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(500)
}
