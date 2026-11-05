//! Static assets and templates, embedded in the binary.

pub const STYLE: &str = include_str!("../../static/style.css");
pub const STYLE2: &str = include_str!("../../static/style2.css");
pub const STYLE3: &str = include_str!("../../static/style3.css");
pub const STYLE4: &str = include_str!("../../static/style4.css");
pub const APP_JS: &str = include_str!("../../static/app.js");

pub const TEMPLATES: &[(&str, &str)] = &[
    ("base.html", include_str!("../../templates/base.html")),
    ("icons.html", include_str!("../../templates/icons.html")),
    ("macros.html", include_str!("../../templates/macros.html")),
    ("home.html", include_str!("../../templates/home.html")),
    ("listing.html", include_str!("../../templates/listing.html")),
    ("profile.html", include_str!("../../templates/profile.html")),
    ("auth.html", include_str!("../../templates/auth.html")),
    ("new.html", include_str!("../../templates/new.html")),
    ("orgs.html", include_str!("../../templates/orgs.html")),
    ("docs.html", include_str!("../../templates/docs.html")),
    ("404.html", include_str!("../../templates/404.html")),
    ("settings/base.html", include_str!("../../templates/settings/base.html")),
    ("settings/profile.html", include_str!("../../templates/settings/profile.html")),
    ("settings/tokens.html", include_str!("../../templates/settings/tokens.html")),
    ("settings/password.html", include_str!("../../templates/settings/password.html")),
    ("repo/base.html", include_str!("../../templates/repo/base.html")),
    ("repo/card.html", include_str!("../../templates/repo/card.html")),
    ("repo/tree.html", include_str!("../../templates/repo/tree.html")),
    ("repo/blob.html", include_str!("../../templates/repo/blob.html")),
    ("repo/commits.html", include_str!("../../templates/repo/commits.html")),
    ("repo/commit.html", include_str!("../../templates/repo/commit.html")),
    ("repo/community.html", include_str!("../../templates/repo/community.html")),
    ("repo/discussion.html", include_str!("../../templates/repo/discussion.html")),
    ("repo/new_discussion.html", include_str!("../../templates/repo/new_discussion.html")),
    ("repo/settings.html", include_str!("../../templates/repo/settings.html")),
    ("repo/upload.html", include_str!("../../templates/repo/upload.html")),
    ("repo/edit.html", include_str!("../../templates/repo/edit.html")),
    ("admin/base.html", include_str!("../../templates/admin/base.html")),
    ("admin/index.html", include_str!("../../templates/admin/index.html")),
    ("admin/users.html", include_str!("../../templates/admin/users.html")),
    ("admin/repos.html", include_str!("../../templates/admin/repos.html")),
    ("admin/orgs.html", include_str!("../../templates/admin/orgs.html")),
    ("admin/server.html", include_str!("../../templates/admin/server.html")),
];

/// Version string for asset URLs: changes whenever the embedded CSS/JS changes,
/// so browsers pick up a deploy instead of serving a cached stylesheet.
pub fn version() -> String {
    let mut all = String::with_capacity(200_000);
    for s in [STYLE, STYLE2, STYLE3, STYLE4, APP_JS] {
        all.push_str(s);
    }
    crate::util::sha256_hex(all.as_bytes())[..10].to_string()
}

pub async fn serve(name: &str, if_none_match: Option<&str>) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    let (body, ct) = match name {
        "style.css" => (STYLE, "text/css; charset=utf-8"),
        "style2.css" => (STYLE2, "text/css; charset=utf-8"),
        "style3.css" => (STYLE3, "text/css; charset=utf-8"),
        "style4.css" => (STYLE4, "text/css; charset=utf-8"),
        "app.js" => (APP_JS, "application/javascript; charset=utf-8"),
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };
    // ETag over the embedded bytes: assets are compiled in, so this changes
    // exactly when the binary does. Revalidation is cheap and deploys are never
    // served stale from a browser cache.
    let etag = format!("\"{}\"", &crate::util::sha256_hex(body.as_bytes())[..16]);
    if if_none_match.is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*")) {
        return axum::http::StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "public, max-age=0, must-revalidate"),
            (header::ETAG, etag.as_str()),
        ],
        body,
    )
        .into_response()
}
