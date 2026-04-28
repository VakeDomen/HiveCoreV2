use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use chrono::{Days, Local, NaiveDate};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::servers::worker::models::worker_phase::WorkerPhase;
use crate::shared::log;
use crate::shared::sqlite::usage_tracking::{DailyUsageRow, UsageTrackingDb, current_local_day};

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    let Some(settings) = TelegramSettings::from_state(&state) else {
        return Ok(());
    };

    log::info(format!(
        "telegram bot enabled for user={}",
        log::bold(settings.user_id.to_string())
    ));

    let client = TelegramClient::new(settings.bot_token)?;
    let db = UsageTrackingDb::open(&state.config.database_url)?;
    let mut offset = 0_i64;
    let mut current_day = current_local_day();

    loop {
        if let Err(err) = send_daily_report_if_needed(&client, &db, settings.user_id, &mut current_day)
        {
            log::warn(format!("telegram daily report failed: {err}"));
        }

        match client.get_updates(offset, 15) {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;
                    if let Some(message) = update.message {
                        if !is_allowed_message(&message, settings.user_id) {
                            continue;
                        }
                        if let Err(err) = handle_command(&client, &db, &state, settings.user_id, &message) {
                            log::warn(format!("telegram command failed: {err}"));
                        }
                    }
                }
            }
            Err(err) => {
                log::warn(format!("telegram polling failed: {err}"));
                thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

struct TelegramSettings {
    bot_token: String,
    user_id: i64,
}

impl TelegramSettings {
    fn from_state(state: &AppState) -> Option<Self> {
        Some(Self {
            bot_token: state.config.telegram_bot_token.clone()?,
            user_id: state.config.telegram_user_id?,
        })
    }
}

struct TelegramClient {
    base_url: String,
    http: Client,
}

impl TelegramClient {
    fn new(bot_token: String) -> io::Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            base_url: format!("https://api.telegram.org/bot{bot_token}"),
            http,
        })
    }

    fn get_updates(&self, offset: i64, timeout_secs: u64) -> io::Result<Vec<TelegramUpdate>> {
        let response = self
            .http
            .post(format!("{}/getUpdates", self.base_url))
            .json(&json!({
                "offset": offset,
                "timeout": timeout_secs,
                "allowed_updates": ["message"],
            }))
            .send()
            .map_err(io::Error::other)?;
        let body = response.json::<TelegramResponse<Vec<TelegramUpdate>>>().map_err(io::Error::other)?;
        if body.ok {
            Ok(body.result)
        } else {
            Err(io::Error::other("telegram getUpdates returned not ok"))
        }
    }

    fn send_message(&self, user_id: i64, text: &str) -> io::Result<()> {
        let response = self
            .http
            .post(format!("{}/sendMessage", self.base_url))
            .json(&json!({
                "chat_id": user_id,
                "text": text,
            }))
            .send()
            .map_err(io::Error::other)?;
        let body = response
            .json::<TelegramResponse<serde_json::Value>>()
            .map_err(io::Error::other)?;
        if body.ok {
            Ok(())
        } else {
            Err(io::Error::other("telegram sendMessage returned not ok"))
        }
    }
}

fn is_allowed_message(message: &TelegramMessage, user_id: i64) -> bool {
    message.chat.id == user_id && message.from.as_ref().map(|user| user.id) == Some(user_id)
}

fn handle_command(
    client: &TelegramClient,
    db: &UsageTrackingDb,
    state: &Arc<AppState>,
    user_id: i64,
    message: &TelegramMessage,
) -> io::Result<()> {
    let text = message.text.as_deref().unwrap_or("").trim();
    if text.is_empty() {
        return Ok(());
    }

    let response = match text.split_whitespace().next().unwrap_or("") {
        "/help" | "/start" => help_text(),
        "/status" => status_text(state),
        "/workers" => workers_text(state),
        "/queue" => queue_text(state),
        "/today" => usage_text(db, current_local_day())?,
        "/yesterday" => usage_text(
            db,
            current_local_day()
                .checked_sub_days(Days::new(1))
                .unwrap_or_else(current_local_day),
        )?,
        _ => "Unknown command. Use /help.".to_string(),
    };

    client.send_message(user_id, &response)
}

fn help_text() -> String {
    [
        "HiveCore Telegram commands:",
        "/status - worker and queue summary",
        "/workers - connected worker list",
        "/queue - queued request counts",
        "/today - usage report for today",
        "/yesterday - usage report for yesterday",
    ]
    .join("\n")
}

fn status_text(state: &AppState) -> String {
    let queue = state.request_queue.snapshot();
    let workers = state
        .workers
        .read()
        .ok()
        .map(|guard| guard.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut phases = [0_u64; 3];
    for worker in &workers {
        match worker.state {
            WorkerPhase::Authenticating => phases[0] += 1,
            WorkerPhase::Polling => phases[1] += 1,
            WorkerPhase::Working => phases[2] += 1,
        }
    }

    format!(
        "Workers: {}\nAuthenticating: {}\nPolling: {}\nWorking: {}\nModel queue: {}\nNode queue: {}",
        workers.len(),
        phases[0],
        phases[1],
        phases[2],
        queue.model_queue.values().sum::<usize>(),
        queue.node_queue.values().sum::<usize>()
    )
}

fn workers_text(state: &AppState) -> String {
    let workers = state
        .workers
        .read()
        .ok()
        .map(|guard| guard.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if workers.is_empty() {
        return "No connected workers.".to_string();
    }

    let mut lines = vec![format!("Connected workers: {}", workers.len())];
    for worker in workers {
        lines.push(format!(
            "{} | {} | models={}",
            worker.name,
            worker_phase_name(worker.state),
            unique_tag_count(&worker.tags)
        ));
    }
    lines.join("\n")
}

fn queue_text(state: &AppState) -> String {
    let queue = state.request_queue.snapshot();
    if queue.model_queue.is_empty() && queue.node_queue.is_empty() {
        return "Queue is empty.".to_string();
    }

    let mut lines = vec!["Queue snapshot:".to_string()];
    if !queue.model_queue.is_empty() {
        lines.push("Model routes:".to_string());
        let mut entries = queue.model_queue.into_iter().collect::<Vec<_>>();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (model, count) in entries {
            lines.push(format!("{} = {}", model, count));
        }
    }
    if !queue.node_queue.is_empty() {
        lines.push("Node routes:".to_string());
        let mut entries = queue.node_queue.into_iter().collect::<Vec<_>>();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (node, count) in entries {
            lines.push(format!("{} = {}", node, count));
        }
    }
    lines.join("\n")
}

fn usage_text(db: &UsageTrackingDb, day: NaiveDate) -> io::Result<String> {
    let rows = db.usage_report_for_day(day)?;
    Ok(format_usage_report(day, &rows))
}

fn format_usage_report(day: NaiveDate, rows: &[DailyUsageRow]) -> String {
    if rows.is_empty() {
        return format!("Usage report for {}:\nNo usage recorded.", day.format("%Y-%m-%d"));
    }

    let total_requests = rows.iter().map(|row| row.request_count).sum::<u64>();
    let total_prompt = rows.iter().map(|row| row.prompt_tokens).sum::<u64>();
    let total_completion = rows.iter().map(|row| row.completion_tokens).sum::<u64>();
    let total_duration = rows.iter().map(|row| row.duration_ms).sum::<u64>();

    let mut lines = vec![
        format!("Usage report for {}:", day.format("%Y-%m-%d")),
        format!(
            "Totals: requests={} prompt={} completion={} duration={}",
            total_requests,
            total_prompt,
            total_completion,
            log::format_duration(Duration::from_millis(total_duration))
        ),
    ];

    for row in rows {
        lines.push(format!(
            "{} | {} | req={} prompt={} completion={} duration={}",
            row.key_name,
            row.model,
            row.request_count,
            row.prompt_tokens,
            row.completion_tokens,
            log::format_duration(Duration::from_millis(row.duration_ms))
        ));
    }

    lines.join("\n")
}

fn send_daily_report_if_needed(
    client: &TelegramClient,
    db: &UsageTrackingDb,
    user_id: i64,
    current_day: &mut NaiveDate,
) -> io::Result<()> {
    let now_day = Local::now().date_naive();
    if now_day == *current_day {
        return Ok(());
    }

    let previous_day = *current_day;
    *current_day = now_day;
    let report = format_usage_report(previous_day, &db.usage_report_for_day(previous_day)?);
    client.send_message(user_id, &report)
}

fn worker_phase_name(phase: WorkerPhase) -> &'static str {
    match phase {
        WorkerPhase::Authenticating => "auth",
        WorkerPhase::Polling => "polling",
        WorkerPhase::Working => "working",
    }
}

fn unique_tag_count(tags: &[String]) -> usize {
    tags.iter().map(String::as_str).collect::<HashSet<_>>().len()
}

#[derive(Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: T,
}

#[derive(Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Deserialize)]
struct TelegramMessage {
    text: Option<String>,
    chat: TelegramChat,
    from: Option<TelegramUser>,
}

#[derive(Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Deserialize)]
struct TelegramUser {
    id: i64,
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use crate::shared::sqlite::usage_tracking::DailyUsageRow;

    use super::format_usage_report;

    #[test]
    fn usage_report_formats_totals_and_rows() {
        let report = format_usage_report(
            NaiveDate::from_ymd_opt(2026, 4, 28).expect("valid date"),
            &[DailyUsageRow {
                key_name: "alice".to_string(),
                model: "bge-m3".to_string(),
                request_count: 2,
                prompt_tokens: 17,
                completion_tokens: 5,
                duration_ms: 35,
            }],
        );

        assert!(report.contains("Usage report for 2026-04-28:"));
        assert!(report.contains("Totals: requests=2 prompt=17 completion=5"));
        assert!(report.contains("alice | bge-m3 | req=2"));
    }
}
