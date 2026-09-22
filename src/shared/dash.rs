use rust_embed::Embed;

use super::http::HttpResponse;

#[derive(Embed)]
#[folder = "dash/"]
#[include = "index.html"]
#[include = "app.js"]
#[include = "styles.css"]
struct DashAssets;

const MIME_TYPES: &[(&str, &str)] = &[
    (".html", "text/html; charset=utf-8"),
    (".js", "application/javascript; charset=utf-8"),
    (".css", "text/css; charset=utf-8"),
];

pub fn serve_file(uri: &str) -> Option<HttpResponse> {
    let clean_path = if uri == "/" || uri.is_empty() {
        "index.html"
    } else {
        let p = uri.trim_start_matches('/');
        // Only serve known static files with allowed extensions
        let allowed = [".html", ".js", ".css"];
        if !allowed.iter().any(|ext| p.ends_with(ext)) {
            return None;
        }
        p
    };

    let asset = DashAssets::get(clean_path)?;
    let data = asset.data.to_vec();

    let content_type = MIME_TYPES
        .iter()
        .find(|(ext, _)| clean_path.ends_with(ext))
        .map(|(_, mime)| *mime)
        .unwrap_or("application/octet-stream");

    let mut response = HttpResponse::new(200, "OK", data);
    response
        .headers
        .push(("Content-Type".to_string(), content_type.to_string()));
    response
        .headers
        .push(("Cache-Control".to_string(), "no-store".to_string()));
    Some(response)
}

pub fn serve_config_json(_proxy_port: u16, management_port: u16) -> HttpResponse {
    let body = serde_json::json!({
        "proxyEndpoint": "https://hivecore.famnit.upr.si",
        "managementEndpoint": format!("http://127.0.0.1:{}", management_port),
        "key": "", // no auto-login; user enters their key in the browser
    });
    let body_str = serde_json::to_string(&body).unwrap_or_default();
    let mut response = HttpResponse::json(200, "OK", body_str);
    response
        .headers
        .push(("Access-Control-Allow-Origin".to_string(), "*".to_string()));
    response
}
