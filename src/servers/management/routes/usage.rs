use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate};
use serde_json::{Value, json};

use crate::app::AppState;
use crate::shared::http::{HttpRequest, HttpResponse};
use crate::shared::log;
use crate::shared::sqlite::usage_tracking::{
    DailyUsageRow, UsageTrackingDb, WorkerUsageRow, current_local_day,
};

pub fn get_usage(state: &AppState, request: &HttpRequest) -> HttpResponse {
    let (from, to) = match date_range(&request.uri) {
        Ok(range) => range,
        Err(message) => return HttpResponse::new(400, "Bad Request", message.into_bytes()),
    };

    let db = match UsageTrackingDb::open(&state.config.database_url) {
        Ok(db) => db,
        Err(err) => {
            log::error(format!("failed to open usage tracking db: {err}"));
            return HttpResponse::new(500, "Internal Server Error", Vec::new());
        }
    };

    let (rows, worker_rows) = match db.usage_report_for_range(from, to) {
        Ok(report) => report,
        Err(err) => {
            log::error(format!("failed to read usage report: {err}"));
            return HttpResponse::new(500, "Internal Server Error", Vec::new());
        }
    };

    json_response(json!({
        "from": from.format("%Y-%m-%d").to_string(),
        "to": to.format("%Y-%m-%d").to_string(),
        "summary": aggregate_daily(&rows),
        "days": aggregate_by_day(&rows),
        "keys": aggregate_by_key(&rows),
        "models": aggregate_by_model(&rows),
        "workers": aggregate_by_worker(&worker_rows),
        "backends": aggregate_by_backend(&worker_rows),
        "rows": rows,
        "worker_rows": worker_rows,
    }))
}

fn date_range(uri: &str) -> Result<(NaiveDate, NaiveDate), String> {
    let today = current_local_day();
    let query = uri.split_once('?').map(|(_, query)| query).unwrap_or("");
    let mut from = None;
    let mut to = None;
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| format!("invalid date for {key}; expected YYYY-MM-DD"))?;
        match key {
            "from" => from = Some(date),
            "to" => to = Some(date),
            _ => {}
        }
    }

    let from = from.unwrap_or(today);
    let to = to.unwrap_or(from);
    if to < from {
        return Err("to must be on or after from".to_string());
    }
    if to.signed_duration_since(from) > Duration::days(90) {
        return Err("usage range is limited to 90 days".to_string());
    }
    Ok((from, to))
}

fn aggregate_daily(rows: &[DailyUsageRow]) -> Value {
    let mut summary = UsageSummary::default();
    for row in rows {
        summary.add_daily(row);
    }
    summary.to_json()
}

fn aggregate_by_day(rows: &[DailyUsageRow]) -> Vec<Value> {
    let mut map = BTreeMap::<String, UsageSummary>::new();
    for row in rows {
        map.entry(row.usage_day.clone()).or_default().add_daily(row);
    }
    map.into_iter()
        .map(|(day, summary)| {
            let mut value = summary.to_json();
            value["day"] = json!(day);
            value
        })
        .collect()
}

fn aggregate_by_key(rows: &[DailyUsageRow]) -> Vec<Value> {
    let mut map = BTreeMap::<String, (Option<i64>, String, UsageSummary)>::new();
    for row in rows {
        let key = row
            .key_id
            .map(|id| format!("key:{id}"))
            .unwrap_or_else(|| format!("name:{}", row.key_name));
        let entry = map
            .entry(key)
            .or_insert_with(|| (row.key_id, row.key_name.clone(), UsageSummary::default()));
        entry.0 = row.key_id.or(entry.0);
        entry.1 = row.key_name.clone();
        entry.2.add_daily(row);
    }
    map.into_values()
        .map(|(key_id, key_name, summary)| {
            let mut value = summary.to_json();
            value["key_id"] = json!(key_id);
            value["key_name"] = json!(key_name);
            value
        })
        .collect()
}

fn aggregate_by_model(rows: &[DailyUsageRow]) -> Vec<Value> {
    let mut map = BTreeMap::<String, UsageSummary>::new();
    for row in rows {
        map.entry(row.model.clone()).or_default().add_daily(row);
    }
    map.into_iter()
        .map(|(model, summary)| {
            let mut value = summary.to_json();
            value["model"] = json!(model);
            value
        })
        .collect()
}

fn aggregate_by_worker(rows: &[WorkerUsageRow]) -> Vec<Value> {
    let mut map = BTreeMap::<String, (String, String, UsageSummary)>::new();
    for row in rows {
        let entry = map.entry(row.worker_name.clone()).or_insert_with(|| {
            (
                row.worker_name.clone(),
                row.backend.clone(),
                UsageSummary::default(),
            )
        });
        entry.1 = row.backend.clone();
        entry.2.add_worker(row);
    }
    map.into_values()
        .map(|(worker_name, backend, summary)| {
            let mut value = summary.to_json();
            value["worker_name"] = json!(worker_name);
            value["backend"] = json!(backend);
            value
        })
        .collect()
}

fn aggregate_by_backend(rows: &[WorkerUsageRow]) -> Vec<Value> {
    let mut map = BTreeMap::<String, UsageSummary>::new();
    for row in rows {
        map.entry(row.backend.clone()).or_default().add_worker(row);
    }
    map.into_iter()
        .map(|(backend, summary)| {
            let mut value = summary.to_json();
            value["backend"] = json!(backend);
            value
        })
        .collect()
}

#[derive(Default)]
struct UsageSummary {
    request_count: u64,
    success_count: u64,
    error_count: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    queue_ms: u64,
    worker_ms: u64,
    total_ms: u64,
    duration_ms: u64,
}

impl UsageSummary {
    fn add_daily(&mut self, row: &DailyUsageRow) {
        self.add(
            row.request_count,
            row.success_count,
            row.error_count,
            row.prompt_tokens,
            row.completion_tokens,
            row.total_tokens,
            row.queue_ms,
            row.worker_ms,
            row.total_ms,
            row.duration_ms,
        );
    }

    fn add_worker(&mut self, row: &WorkerUsageRow) {
        self.add(
            row.request_count,
            row.success_count,
            row.error_count,
            row.prompt_tokens,
            row.completion_tokens,
            row.total_tokens,
            row.queue_ms,
            row.worker_ms,
            row.total_ms,
            row.duration_ms,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        request_count: u64,
        success_count: u64,
        error_count: u64,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        queue_ms: u64,
        worker_ms: u64,
        total_ms: u64,
        duration_ms: u64,
    ) {
        self.request_count += request_count;
        self.success_count += success_count;
        self.error_count += error_count;
        self.prompt_tokens += prompt_tokens;
        self.completion_tokens += completion_tokens;
        self.total_tokens += total_tokens;
        self.queue_ms += queue_ms;
        self.worker_ms += worker_ms;
        self.total_ms += total_ms;
        self.duration_ms += duration_ms;
    }

    fn to_json(&self) -> Value {
        json!({
            "requests": self.request_count,
            "success": self.success_count,
            "errors": self.error_count,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens,
            "queue_ms": self.queue_ms,
            "worker_ms": self.worker_ms,
            "total_ms": self.total_ms,
            "duration_ms": self.duration_ms,
        })
    }
}

fn json_response(value: Value) -> HttpResponse {
    match serde_json::to_string(&value) {
        Ok(body) => HttpResponse::json(200, "OK", body),
        Err(_) => HttpResponse::new(500, "Internal Server Error", Vec::new()),
    }
}
