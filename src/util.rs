//! Small shared helpers: timestamps, ids, names, base URL.

use axum::http::HeaderMap;
use rand::Rng;

pub fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Normalise any RFC3339 date (e.g. from `git log %aI`) to the Hub's `...%.3fZ` format.
pub fn to_hub_date(s: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(s.trim()) {
        Ok(d) => d.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        Err(_) => s.to_string(),
    }
}

/// 24-hex id, shaped like the Mongo ObjectIds the Hub returns.
pub fn object_id() -> String {
    let mut b = [0u8; 12];
    rand::thread_rng().fill(&mut b);
    hex::encode(b)
}

pub fn random_token(len: usize) -> String {
    const CH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut r = rand::thread_rng();
    (0..len).map(|_| CH[r.gen_range(0..CH.len())] as char).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

/// Names that would collide with top-level routes.
pub const RESERVED: &[&str] = &[
    "api", "datasets", "spaces", "models", "kernels", "new", "new-dataset", "new-space", "login", "logout",
    "join", "settings", "organizations", "static", "avatars", "docs", "search", "admin", "buckets",
    "collections", "papers", "hugginghub", "favicon.ico", "robots.txt", "notifications",
];

/// Hub-like name validation (users, orgs and repo names).
pub fn valid_name(name: &str) -> bool {
    let ok_chars = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    !name.is_empty()
        && name.len() <= 96
        && ok_chars
        && !name.starts_with(['-', '.'])
        && !name.ends_with(['-', '.'])
        && !name.contains("..")
        && !name.contains("--")
        && !name.ends_with(".git")
}

pub fn valid_account_name(name: &str) -> bool {
    valid_name(name) && !RESERVED.contains(&name.to_ascii_lowercase().as_str())
}

/// Public base URL: configured, or derived from the request's Host header.
pub fn base_url(cfg_public: &Option<String>, headers: &HeaderMap) -> String {
    if let Some(p) = cfg_public {
        return p.trim_end_matches('/').to_string();
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

pub fn human_size(n: i64) -> String {
    let units = ["Bytes", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} Bytes")
    } else if v >= 100.0 {
        format!("{v:.0} {}", units[i])
    } else {
        format!("{v:.1} {}", units[i]).replace(".0 ", " ")
    }
}

/// Repo type helpers. Internal type names: model | dataset | space.
pub fn type_from_plural(p: &str) -> Option<&'static str> {
    match p {
        "models" => Some("model"),
        "datasets" => Some("dataset"),
        "spaces" => Some("space"),
        _ => None,
    }
}

pub fn url_prefix(rtype: &str) -> &'static str {
    match rtype {
        "dataset" => "datasets/",
        "space" => "spaces/",
        _ => "",
    }
}
