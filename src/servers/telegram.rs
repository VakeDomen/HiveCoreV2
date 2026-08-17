use std::collections::{BTreeMap, HashSet};
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
use crate::shared::capture::{CaptureCompressionReport, compress_capture_day};
use crate::shared::log;
use crate::shared::sqlite::usage_tracking::{DailyUsageRow, UsageTrackingDb, current_local_day};

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    let Some(settings) = TelegramSettings::from_state(&state) else {
        return Ok(());
    };

    log::info(format!(
        "telegram bot enabled for target={}",
        log::bold(settings.target.label())
    ));

    let client = TelegramClient::new(settings.bot_token)?;
    let db = UsageTrackingDb::open(&state.config.database_url)?;
    let mut offset = 0_i64;
    let mut current_day = current_local_day();

    loop {
        if let Err(err) =
            send_daily_report_if_needed(&client, &db, &state, settings.target, &mut current_day)
        {
            log::warn(format!("telegram daily report failed: {err}"));
        }

        match client.get_updates(offset, 15) {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;
                    if let Some(message) = update.message {
                        if !is_allowed_message(&message, settings.target) {
                            continue;
                        }
                        if let Err(err) =
                            handle_command(&client, &db, &state, settings.target, &message)
                        {
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
    target: TelegramTarget,
}

impl TelegramSettings {
    fn from_state(state: &AppState) -> Option<Self> {
        Some(Self {
            bot_token: state.config.telegram_bot_token.clone()?,
            target: TelegramTarget {
                chat_id: state.config.telegram_user_id?,
                message_thread_id: state.config.telegram_message_thread_id,
            },
        })
    }
}

#[derive(Clone, Copy)]
struct TelegramTarget {
    chat_id: i64,
    message_thread_id: Option<i64>,
}

impl TelegramTarget {
    fn label(self) -> String {
        match self.message_thread_id {
            Some(thread_id) => format!("{}:{}", self.chat_id, thread_id),
            None => self.chat_id.to_string(),
        }
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
        let body = response
            .json::<TelegramResponse<Vec<TelegramUpdate>>>()
            .map_err(io::Error::other)?;
        if body.ok {
            Ok(body.result)
        } else {
            Err(io::Error::other("telegram getUpdates returned not ok"))
        }
    }

    fn send_message(&self, target: TelegramTarget, text: &str) -> io::Result<()> {
        let mut payload = json!({
            "chat_id": target.chat_id,
            "text": text,
            "parse_mode": "HTML",
        });
        if let Some(thread_id) = target.message_thread_id {
            payload["message_thread_id"] = json!(thread_id);
        }
        let response = self
            .http
            .post(format!("{}/sendMessage", self.base_url))
            .json(&payload)
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

fn is_allowed_message(message: &TelegramMessage, target: TelegramTarget) -> bool {
    if message.chat.id != target.chat_id {
        return false;
    }
    if let Some(thread_id) = target.message_thread_id {
        return message.message_thread_id == Some(thread_id);
    }
    message.from.as_ref().map(|user| user.id) == Some(target.chat_id)
}

fn handle_command(
    client: &TelegramClient,
    db: &UsageTrackingDb,
    state: &Arc<AppState>,
    target: TelegramTarget,
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

    client.send_message(target, &response)
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
            "{} | {} | {} | models={}",
            worker.name,
            worker.backend.as_str(),
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
        return format!(
            "Usage report for {}:\nNo usage recorded.",
            day.format("%Y-%m-%d")
        );
    }

    let total_requests = rows.iter().map(|row| row.request_count).sum::<u64>();
    let total_prompt = rows.iter().map(|row| row.prompt_tokens).sum::<u64>();
    let total_completion = rows.iter().map(|row| row.completion_tokens).sum::<u64>();
    let total_duration = rows.iter().map(|row| row.duration_ms).sum::<u64>();

    let mut lines = vec![
        format!("<b>Usage report for {}</b>", day.format("%Y-%m-%d")),
        String::new(),
        "<b>📊 Overall</b>".to_string(),
        format_summary_block(
            total_requests,
            total_prompt,
            total_completion,
            total_duration,
        ),
    ];

    let mut grouped = BTreeMap::<&str, Vec<&DailyUsageRow>>::new();
    for row in rows {
        grouped.entry(&row.key_name).or_default().push(row);
    }

    for (user, entries) in grouped {
        let user_requests = entries.iter().map(|row| row.request_count).sum::<u64>();
        let user_prompt = entries.iter().map(|row| row.prompt_tokens).sum::<u64>();
        let user_completion = entries.iter().map(|row| row.completion_tokens).sum::<u64>();
        let user_duration = entries.iter().map(|row| row.duration_ms).sum::<u64>();

        lines.push(String::new());
        lines.push(format_user_heading(user));
        lines.push(format_summary_block(
            user_requests,
            user_prompt,
            user_completion,
            user_duration,
        ));
        lines.push("  Models:".to_string());

        for row in entries {
            lines.push(format!("  • <code>{}</code>", escape_html(&row.model)));
            lines.push(format!(
                "    {} req | {} in | {} out | {}",
                row.request_count,
                format_count(row.prompt_tokens),
                format_count(row.completion_tokens),
                format_duration_short(row.duration_ms)
            ));
        }
    }

    lines.join("\n")
}

fn format_summary_block(
    request_count: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    duration_ms: u64,
) -> String {
    [
        format!("  Requests: {}", request_count),
        format!("  Input:    {}", format_count(prompt_tokens)),
        format!("  Output:   {}", format_count(completion_tokens)),
        format!("  Time:     {}", format_duration_short(duration_ms)),
    ]
    .join("\n")
}

fn format_user_heading(user: &str) -> String {
    if user == "Unauthenticated" {
        "<b>🔓 Unauthenticated</b>".to_string()
    } else {
        format!("<b>👤 {}</b>", escape_html(user))
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn format_count(value: u64) -> String {
    if value >= 1_000_000 {
        let whole = value / 1_000_000;
        let decimal = (value % 1_000_000) / 100_000;
        format!("{whole}.{decimal}M")
    } else if value >= 1_000 {
        let whole = value / 1_000;
        let decimal = (value % 1_000) / 100;
        format!("{whole}.{decimal}k")
    } else {
        value.to_string()
    }
}

fn format_duration_short(duration_ms: u64) -> String {
    let duration = Duration::from_millis(duration_ms);
    let secs = duration.as_secs();
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        log::format_duration(duration)
    }
}

fn format_bytes(value: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut amount = value as f64;
    let mut unit = 0_usize;
    while amount >= 1024.0 && unit + 1 < UNITS.len() {
        amount /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", value, UNITS[unit])
    } else {
        format!("{amount:.1} {}", UNITS[unit])
    }
}

fn format_signed_bytes(value: i64) -> String {
    if value < 0 {
        format!("-{}", format_bytes(value.unsigned_abs()))
    } else {
        format_bytes(value as u64)
    }
}

fn send_daily_report_if_needed(
    client: &TelegramClient,
    db: &UsageTrackingDb,
    state: &AppState,
    target: TelegramTarget,
    current_day: &mut NaiveDate,
) -> io::Result<()> {
    let now_day = Local::now().date_naive();
    if now_day == *current_day {
        return Ok(());
    }

    let previous_day = *current_day;
    *current_day = now_day;
    let usage_report = format_usage_report(previous_day, &db.usage_report_for_day(previous_day)?);
    let capture_report = match compress_capture_day(&state.config.capture_dir, previous_day) {
        Ok(report) => report,
        Err(err) => {
            log::warn(format!(
                "capture compression failed for day={previous_day}: {err}"
            ));
            CaptureCompressionReport {
                day: previous_day,
                errors: vec![err.to_string()],
                ..CaptureCompressionReport::default()
            }
        }
    };
    let report = format_daily_report(&usage_report, &capture_report);
    client.send_message(target, &report)
}

fn format_daily_report(usage_report: &str, capture_report: &CaptureCompressionReport) -> String {
    [
        usage_report.to_string(),
        String::new(),
        format_capture_compression_report(capture_report),
    ]
    .join("\n")
}

fn format_capture_compression_report(report: &CaptureCompressionReport) -> String {
    let mut lines = vec![
        format!(
            "<b>Capture compression for {}</b>",
            report.day.format("%Y-%m-%d")
        ),
        format!("  Captures: {}", report.captures),
        format!("  Files compressed: {}", report.files_compressed),
        format!("  Already compressed: {}", report.files_already_compressed),
        format!("  Original: {}", format_bytes(report.original_bytes)),
        format!("  Compressed: {}", format_bytes(report.compressed_bytes)),
        format!("  Saved: {}", format_signed_bytes(report.bytes_saved())),
    ];
    if let Some(available) = report.disk_available_bytes {
        lines.push(format!("  Disk free: {}", format_bytes(available)));
    }
    if !report.errors.is_empty() {
        lines.push(format!("  Errors: {}", report.errors.len()));
        for error in report.errors.iter().take(3) {
            lines.push(format!("    {}", escape_html(error)));
        }
    }
    lines.join("\n")
}

fn worker_phase_name(phase: WorkerPhase) -> &'static str {
    match phase {
        WorkerPhase::Authenticating => "auth",
        WorkerPhase::Polling => "polling",
        WorkerPhase::Working => "working",
    }
}

fn unique_tag_count(tags: &[String]) -> usize {
    tags.iter()
        .map(String::as_str)
        .collect::<HashSet<_>>()
        .len()
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

#[derive(Clone, Deserialize)]
struct TelegramMessage {
    text: Option<String>,
    chat: TelegramChat,
    from: Option<TelegramUser>,
    message_thread_id: Option<i64>,
}

#[derive(Clone, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Clone, Deserialize)]
struct TelegramUser {
    id: i64,
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use crate::shared::capture::CaptureCompressionReport;
    use crate::shared::sqlite::usage_tracking::DailyUsageRow;

    use super::{
        TelegramChat, TelegramMessage, TelegramTarget, TelegramUser,
        format_capture_compression_report, format_usage_report, is_allowed_message,
    };

    #[test]
    fn usage_report_formats_totals_and_rows() {
        let report = format_usage_report(
            NaiveDate::from_ymd_opt(2026, 4, 28).expect("valid date"),
            &[DailyUsageRow {
                usage_day: "2026-04-28".to_string(),
                key_id: Some(1),
                key_name: "alice".to_string(),
                model: "bge-m3".to_string(),
                request_count: 2,
                success_count: 2,
                error_count: 0,
                prompt_tokens: 17,
                completion_tokens: 5,
                total_tokens: 22,
                queue_ms: 0,
                worker_ms: 35,
                total_ms: 35,
                duration_ms: 35,
            }],
        );

        assert!(report.contains("Usage report for 2026-04-28"));
        assert!(report.contains("📊 Overall"));
        assert!(report.contains("  Requests: 2"));
        assert!(report.contains("👤 alice"));
        assert!(report.contains("  Models:"));
        assert!(report.contains("  • <code>bge-m3</code>"));
        assert!(report.contains("    2 req | 17 in | 5 out"));
    }

    #[test]
    fn capture_compression_report_formats_summary() {
        let report = format_capture_compression_report(&CaptureCompressionReport {
            day: NaiveDate::from_ymd_opt(2026, 4, 28).expect("valid date"),
            files_compressed: 2,
            files_already_compressed: 1,
            captures: 42,
            original_bytes: 4096,
            compressed_bytes: 1024,
            disk_available_bytes: Some(10 * 1024 * 1024),
            errors: Vec::new(),
        });

        assert!(report.contains("Capture compression for 2026-04-28"));
        assert!(report.contains("  Captures: 42"));
        assert!(report.contains("  Files compressed: 2"));
        assert!(report.contains("  Already compressed: 1"));
        assert!(report.contains("  Original: 4.0 KB"));
        assert!(report.contains("  Compressed: 1.0 KB"));
        assert!(report.contains("  Saved: 3.0 KB"));
        assert!(report.contains("  Disk free: 10.0 MB"));
    }

    #[test]
    fn topic_target_accepts_only_matching_chat_and_thread() {
        let target = TelegramTarget {
            chat_id: -1003996209253,
            message_thread_id: Some(2),
        };
        let matching = TelegramMessage {
            text: Some("/status".to_string()),
            chat: TelegramChat { id: -1003996209253 },
            from: Some(TelegramUser { id: 12345 }),
            message_thread_id: Some(2),
        };
        let wrong_thread = TelegramMessage {
            message_thread_id: Some(3),
            ..matching.clone()
        };

        assert!(is_allowed_message(&matching, target));
        assert!(!is_allowed_message(&wrong_thread, target));
    }

    #[test]
    fn direct_target_keeps_user_id_check() {
        let target = TelegramTarget {
            chat_id: 12345,
            message_thread_id: None,
        };
        let matching = TelegramMessage {
            text: Some("/status".to_string()),
            chat: TelegramChat { id: 12345 },
            from: Some(TelegramUser { id: 12345 }),
            message_thread_id: None,
        };
        let wrong_sender = TelegramMessage {
            from: Some(TelegramUser { id: 999 }),
            ..matching.clone()
        };

        assert!(is_allowed_message(&matching, target));
        assert!(!is_allowed_message(&wrong_sender, target));
    }
}
