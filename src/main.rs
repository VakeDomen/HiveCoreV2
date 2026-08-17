mod app;
mod auth;
mod servers;
mod shared;

use std::io;
use std::sync::Arc;
use std::thread;

use app::AppState;
use app::Config;

fn main() -> io::Result<()> {
    let config = Config::load_or_create("config.ini")?;
    shared::log::info(format!(
        "starting hive_core_v2 proxy_port={} worker_port={} management_port={} database_url={} user_authentication={}",
        shared::log::bold(config.proxy_port.to_string()),
        shared::log::bold(config.node_connection_port.to_string()),
        shared::log::bold(config.management_connection_port.to_string()),
        shared::log::bold(&config.database_url),
        shared::log::bold(config.user_authentication.to_string())
    ));
    let (stats_tx, stats_rx): (
        std::sync::mpsc::Sender<shared::http::UsageEvent>,
        std::sync::mpsc::Receiver<shared::http::UsageEvent>,
    ) = std::sync::mpsc::channel();
    let (capture_tx, capture_rx): (
        std::sync::mpsc::Sender<shared::capture::CaptureEvent>,
        std::sync::mpsc::Receiver<shared::capture::CaptureEvent>,
    ) = std::sync::mpsc::channel();
    let state = Arc::new(AppState::new(config.clone(), stats_tx, capture_tx)?);
    let stats_database_url = config.database_url.clone();
    let capture_dir = config.capture_dir.clone();

    let client_state = Arc::clone(&state);
    let worker_state = Arc::clone(&state);
    let management_state = Arc::clone(&state);
    let overseer_state = Arc::clone(&state);
    let stats_thread = thread::spawn(move || {
        shared::sqlite::usage_tracking::run_usage_tracking_worker(stats_database_url, stats_rx)
    });
    let capture_thread =
        thread::spawn(move || shared::capture::run_capture_writer(capture_dir, capture_rx));
    let telegram_state = Arc::clone(&state);
    let telegram_enabled = config.telegram_bot_token.is_some() && config.telegram_user_id.is_some();

    let client_thread = thread::spawn(move || servers::proxy::server::run(client_state));
    let worker_thread = thread::spawn(move || servers::worker::server::run(worker_state));
    let management_thread =
        thread::spawn(move || servers::management::server::run(management_state));
    let overseer_thread = thread::spawn(move || servers::worker::overseer::run(overseer_state));
    let telegram_thread =
        telegram_enabled.then(|| thread::spawn(move || servers::telegram::run(telegram_state)));

    join_server("client", client_thread)?;
    join_server("worker", worker_thread)?;
    join_server("management", management_thread)?;
    join_server("overseer", overseer_thread)?;
    if let Some(handle) = telegram_thread {
        join_server("telegram", handle)?;
    }
    join_server("stats", stats_thread)?;
    join_server("capture", capture_thread)?;

    Ok(())
}

fn join_server(name: &str, handle: thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    match handle.join() {
        Ok(result) => result,
        Err(_) => {
            shared::log::error(format!("{name} thread panicked"));
            Err(io::Error::other(format!("{name} thread panicked")))
        }
    }
}
