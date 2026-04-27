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
        "starting hive_core_v2 proxy_port={} worker_port={} management_port={} database_url={}",
        shared::log::bold(config.proxy_port.to_string()),
        shared::log::bold(config.node_connection_port.to_string()),
        shared::log::bold(config.management_connection_port.to_string()),
        shared::log::bold(&config.database_url)
    ));
    let (stats_tx, _stats_rx): (
        std::sync::mpsc::Sender<shared::http::UsageEvent>,
        std::sync::mpsc::Receiver<shared::http::UsageEvent>,
    ) = std::sync::mpsc::channel();
   let state = Arc::new(AppState::new(config.clone(), stats_tx)?);

    let client_state = Arc::clone(&state);
    let worker_state = Arc::clone(&state);
    let management_state = Arc::clone(&state);
    let overseer_state = Arc::clone(&state);

    let client_thread = thread::spawn(move || servers::proxy::server::run(client_state));
    let worker_thread = thread::spawn(move || servers::worker::server::run(worker_state));
    let management_thread = thread::spawn(move || servers::management::server::run(management_state));
    let overseer_thread = thread::spawn(move || servers::worker::overseer::run(overseer_state));

    join_server("client", client_thread)?;
    join_server("worker", worker_thread)?;
    join_server("management", management_thread)?;
    join_server("overseer", overseer_thread)?;

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
