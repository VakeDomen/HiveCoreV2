use std::sync::Arc;

use serde_json::{Value, json};

use crate::app::{AppState, RateLimitTier};
use crate::auth::KeyRecord;
use crate::shared::http::HttpResponse;
use crate::shared::log;
use crate::shared::sqlite::usage_tracking::UsageTrackingDb;

/// Returns the current key's record (tier, name, role, id).
pub fn get_me(state: &Arc<AppState>, key: &KeyRecord) -> HttpResponse {
    let _tier = state
        .rate_limiter
        .resolve_tier(&key.rate_limit_tier);
    let tier_config = state
        .config
        .rate_limit_tiers
        .get(&key.rate_limit_tier)
        .cloned()
        .unwrap_or_else(RateLimitTier::unlimited);
    let snapshot = state.rate_limiter.snapshot(key.id);

    json_response(200, json!({
        "id": key.id,
        "name": key.name,
        "role": key.role,
        "rate_limit_tier": key.rate_limit_tier,
        "tier": tier_config,
        "limits": snapshot,
    }))
}

/// Returns usage stats for the current key.
pub fn get_me_usage(state: &Arc<AppState>, key: &KeyRecord, query: &str) -> HttpResponse {
    let _today = chrono::Local::now().date_naive();
    let (from, to) = match date_range(query) {
        Ok(range) => range,
        Err(msg) => {
            return HttpResponse::new(400, "Bad Request", msg.into_bytes());
        }
    };

    let db = match UsageTrackingDb::open(&state.config.database_url) {
        Ok(db) => db,
        Err(err) => {
            log::error(format!("failed to open usage tracking db: {err}"));
            return HttpResponse::new(500, "Internal Server Error", Vec::new());
        }
    };

    match db.usage_for_key(key.id, key.name.clone(), from, to) {
        Ok(rows) => json_response(200, json!({
            "from": from.format("%Y-%m-%d").to_string(),
            "to": to.format("%Y-%m-%d").to_string(),
            "rows": rows,
        })),
        Err(err) => {
            log::error(format!("failed to read usage for key {}: {err}", key.id));
            HttpResponse::new(500, "Internal Server Error", Vec::new())
        }
    }
}

/// Returns the live rate limit snapshot for the current key.
pub fn get_me_limits(state: &Arc<AppState>, key: &KeyRecord) -> HttpResponse {
    let _tier = state
        .rate_limiter
        .resolve_tier(&key.rate_limit_tier);
    let tier_config = state
        .config
        .rate_limit_tiers
        .get(&key.rate_limit_tier)
        .cloned()
        .unwrap_or_else(RateLimitTier::unlimited);
    let snapshot = state.rate_limiter.snapshot(key.id);

    json_response(200, json!({
        "tier_name": key.rate_limit_tier,
        "tier": tier_config,
        "snapshot": snapshot,
    }))
}

fn json_response(status: u16, value: Value) -> HttpResponse {
    let body = serde_json::to_vec(&value).unwrap_or_default();
    HttpResponse::json(status, if status == 200 { "OK" } else { "Error" }, String::from_utf8_lossy(&body).into_owned())
}

fn date_range(query: &str) -> Result<(chrono::NaiveDate, chrono::NaiveDate), String> {
    let today = chrono::Local::now().date_naive();
    let mut from = None;
    let mut to = None;
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let Some((key, value)) = part.split_once('=') else { continue; };
        let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
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
    if to.signed_duration_since(from) > chrono::Duration::days(90) {
        return Err("usage range is limited to 90 days".to_string());
    }
    Ok((from, to))
}
