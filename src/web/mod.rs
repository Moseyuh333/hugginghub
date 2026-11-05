//! Server-rendered web UI modelled on huggingface.co.

pub mod account;
pub mod actions;
pub mod admin;
pub mod assets;
pub mod markdown;
pub mod profile;
pub mod repo;

use crate::error::{ApiError, ApiResult};
use crate::models::{repo_access, Access, Caller, RepoRow, REPO_SELECT};
use crate::routes::Req;
use crate::state::AppState;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use minijinja::Environment;
use serde_json::json;

pub fn templates() -> Environment<'static> {
    let mut env = Environment::new();
    for (name, src) in assets::TEMPLATES {
        env.add_template(name, src).expect("template");
    }
    env.add_filter("human_size", |n: i64| crate::util::human_size(n));
    env.add_filter("ago", |s: String| ago(&s));
    env.add_filter("short", |s: String| s.chars().take(7).collect::<String>());
    env.add_filter("compact", |n: i64| compact(n));
    env
}

pub fn compact(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M")
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k")
    } else {
        n.to_string()
    }
}

/// "about 3 hours ago" style, like the Hub.
pub fn ago(iso: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(iso) else { return iso.to_string() };
    let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds().max(0);
    let (n, unit) = match secs {
        0..=59 => return "just now".into(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86399 => (secs / 3600, "hour"),
        86400..=2591999 => (secs / 86400, "day"),
        2592000..=31535999 => (secs / 2592000, "month"),
        _ => (secs / 31536000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// Common template context: current user, site name.
pub fn base_ctx(r: &Req, me: &Option<Caller>) -> serde_json::Value {
    let me_v = me.as_ref().map(|c| {
        json!({ "name": c.user.name, "fullname": c.user.display_name(), "avatar": c.user.avatar_url(&r.base), "is_admin": c.user.is_admin })
    });
    json!({
        "me": me_v, "site_name": r.st.cfg.site_name, "base": r.base, "path": r.uri.path(),
        "asset_ver": r.st.asset_ver,
        "open_signup": r.st.cfg.open_signup,
    })
}

/// Merge context objects left-to-right over the base context.
pub fn ctx(r: &Req, me: &Option<Caller>, parts: &[serde_json::Value]) -> serde_json::Value {
    let mut out = base_ctx(r, me);
    for p in parts {
        if let (Some(dst), Some(src)) = (out.as_object_mut(), p.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

pub fn render(st: &AppState, name: &str, ctx: impl serde::Serialize) -> ApiResult<Response> {
    let t = st.tmpl.get_template(name)?;
    Ok(Html(t.render(ctx)?).into_response())
}

pub fn render_status(st: &AppState, status: StatusCode, name: &str, ctx: impl serde::Serialize) -> ApiResult<Response> {
    let mut r = render(st, name, ctx)?;
    *r.status_mut() = status;
    Ok(r)
}

pub async fn not_found(r: &Req) -> Response {
    let me = r.web_caller().await;
    let c = ctx(r, &me, &[json!({ "title": "404" })]);
    render_status(&r.st, StatusCode::NOT_FOUND, "404.html", c).unwrap_or_else(|e| e.into_response())
}

pub fn redirect(to: &str) -> Response {
    Redirect::to(to).into_response()
}

pub fn login_redirect(r: &Req) -> Response {
    let next = percent_encoding::utf8_percent_encode(r.uri.path(), percent_encoding::NON_ALPHANUMERIC).to_string();
    redirect(&format!("/login?next={next}"))
}

/// Repo card used in listings and profiles.
pub fn repo_card(repo: &RepoRow) -> serde_json::Value {
    let card = repo.card_json();
    json!({
        "id": repo.full_id(),
        "full_id": repo.full_id(),
        "web_path": repo.web_path(),
        "owner": repo.owner,
        "owner_kind": repo.owner_kind,
        "name": repo.name,
        "type": repo.rtype,
        "path": format!("/{}", repo.web_path()),
        "private": repo.private,
        "likes": repo.likes,
        "downloads": repo.downloads,
        "used_storage": repo.used_storage,
        "discussions_disabled": repo.discussions_disabled,
        "updated": repo.updated_at,
        "created": repo.created_at,
        "pipeline": card.get("pipeline_tag").and_then(|v| v.as_str()),
        "sdk": repo.sdk,
        "app_url": repo.app_url,
        "emoji": card.get("emoji").and_then(|v| v.as_str()).unwrap_or("🤗"),
        "color_from": card.get("colorFrom").and_then(|v| v.as_str()).unwrap_or("yellow"),
        "color_to": card.get("colorTo").and_then(|v| v.as_str()).unwrap_or("red"),
        "title": card.get("title").and_then(|v| v.as_str()).unwrap_or(&repo.name),
        "short_description": card.get("short_description").and_then(|v| v.as_str()),
    })
}

/// Visible repos with optional filters, used by listing pages and the home page.
pub async fn visible_repos(st: &AppState, me: &Option<Caller>, rtype: Option<&str>, owner_id: Option<i64>, sort: &str) -> ApiResult<Vec<RepoRow>> {
    let (rt, me2, sort) = (rtype.map(String::from), me.clone(), sort.to_string());
    st.db(move |c| {
        let order = match sort.as_str() {
            "likes" => "(SELECT COUNT(*) FROM likes l WHERE l.repo_id = r.id) DESC",
            "downloads" => "r.downloads DESC",
            "created" => "r.created_at DESC",
            "alphabetical" => "a.name, r.name",
            _ => "r.updated_at DESC",
        };
        let mut st = c.prepare(&format!(
            "{REPO_SELECT} WHERE (?1 IS NULL OR r.type = ?1) AND (?2 IS NULL OR r.owner_id = ?2) ORDER BY {order}"
        ))?;
        let rows = st.query_map(rusqlite::params![rt, owner_id], RepoRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = vec![];
        for repo in rows {
            if repo_access(c, &repo, me2.as_ref())? != Access::None {
                out.push(repo);
            }
        }
        Ok(out)
    })
    .await
}

/// GET / — landing page (signed out) or feed of recent repos (signed in).
pub async fn home(r: &mut Req) -> ApiResult<Response> {
    let me = r.web_caller().await;
    let models = visible_repos(&r.st, &me, Some("model"), None, "updated").await?;
    let datasets = visible_repos(&r.st, &me, Some("dataset"), None, "updated").await?;
    let spaces = visible_repos(&r.st, &me, Some("space"), None, "updated").await?;
    let take = |v: &[RepoRow]| v.iter().take(6).map(repo_card).collect::<Vec<_>>();
    let mine = match &me {
        Some(c) => visible_repos(&r.st, &me, None, Some(c.user.id), "updated").await?.iter().take(12).map(repo_card).collect(),
        None => vec![],
    };
    let c = ctx(r, &me, &[json!({
        "title": "",
        "models": take(&models), "datasets": take(&datasets), "spaces": take(&spaces), "mine": mine,
        "n_models": models.len(), "n_datasets": datasets.len(), "n_spaces": spaces.len(),
    })]);
    render(&r.st, "home.html", c)
}

/// GET /models | /datasets | /spaces
pub async fn listing(r: &mut Req, rtype: &str) -> ApiResult<Response> {
    let me = r.web_caller().await;
    let sort = r.q("sort").unwrap_or("trending").to_string();
    let search = r.q("search").unwrap_or("").to_string();
    let mut repos = visible_repos(&r.st, &me, Some(rtype), None, if sort == "trending" { "likes" } else { &sort }).await?;
    if !search.is_empty() {
        let s = search.to_lowercase();
        repos.retain(|x| x.full_id().to_lowercase().contains(&s));
    }
    let filter = r.q("pipeline_tag").or(r.q("sdk")).map(String::from);
    if let Some(f) = &filter {
        repos.retain(|x| x.card_json().get("pipeline_tag").and_then(|v| v.as_str()) == Some(f.as_str()) || x.sdk.as_deref() == Some(f.as_str()));
    }
    let per = 30;
    let page: usize = r.q("p").and_then(|v| v.parse().ok()).unwrap_or(0);
    let total = repos.len();
    let cards: Vec<_> = repos.iter().skip(page * per).take(per).map(repo_card).collect();
    let (title, plural) = match rtype { "dataset" => ("Datasets", "datasets"), "space" => ("Spaces", "spaces"), _ => ("Models", "models") };
    let c = ctx(r, &me, &[json!({
        "title": title, "plural": plural, "rtype": rtype, "repos": cards, "total": total, "sort": sort, "search": search,
        "filter": filter, "page": page, "pages": total.div_ceil(per),
    })]);
    render(&r.st, "listing.html", c)
}

/// Serve /avatars/{name}[.svg] — an uploaded image or a generated initial.
pub async fn avatar(r: &mut Req, name: &str) -> ApiResult<Response> {
    let name = name.trim_end_matches(".svg").to_string();
    let n2 = name.clone();
    let acc = r.st.db(move |c| crate::models::Account::by_name(c, &n2)).await?;
    if let Some(a) = &acc {
        if a.avatar.is_some() {
            let p = r.st.cfg.avatars_dir().join(&a.name);
            if let Ok(data) = tokio::fs::read(&p).await {
                let ct = if data.starts_with(b"\x89PNG") { "image/png" } else if data.starts_with(b"GIF") { "image/gif" } else if data.starts_with(b"RIFF") { "image/webp" } else { "image/jpeg" };
                return Ok(([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "public, max-age=300")], data).into_response());
            }
        }
    }
    let is_org = acc.as_ref().is_some_and(|a| a.kind == "org");
    let hue = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    let initial = name.chars().next().unwrap_or('?').to_ascii_uppercase();
    let radius = if is_org { "12" } else { "50" };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl({hue},75%,62%)"/><stop offset="1" stop-color="hsl({},70%,52%)"/></linearGradient></defs><rect width="100" height="100" rx="{radius}" fill="url(#g)"/><text x="50" y="50" dy=".35em" text-anchor="middle" font-family="Inter,system-ui,sans-serif" font-size="46" font-weight="600" fill="#fff">{initial}</text></svg>"##,
        (hue + 40) % 360
    );
    Ok(([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "public, max-age=300")], svg).into_response())
}

pub fn bad_form(msg: &str) -> ApiError {
    ApiError::bad(msg.to_string())
}

/// Parse an `application/x-www-form-urlencoded` body.
pub async fn form(r: &mut Req) -> ApiResult<std::collections::HashMap<String, String>> {
    let b = r.bytes(1024 * 1024).await?;
    Ok(serde_urlencoded::from_bytes::<Vec<(String, String)>>(&b).unwrap_or_default().into_iter().collect())
}

/// One uploaded form field.
pub struct Upload {
    pub name: String,
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

/// Parse a multipart/form-data body.
pub async fn multipart(r: &mut Req) -> ApiResult<Vec<Upload>> {
    use axum::extract::{FromRequest, Multipart, Request};
    let ct = r.headers.get(axum::http::header::CONTENT_TYPE).cloned();
    let body = r.take_body();
    let mut b = Request::builder();
    if let Some(ct) = ct {
        b = b.header(axum::http::header::CONTENT_TYPE, ct);
    }
    let req = b.body(body).map_err(ApiError::internal)?;
    let mut mp = Multipart::from_request(req, &r.st).await.map_err(|e| ApiError::bad(format!("Invalid form: {e}")))?;
    let mut out = vec![];
    while let Some(field) = mp.next_field().await.map_err(|e| ApiError::bad(format!("Invalid form: {e}")))? {
        let name = field.name().unwrap_or("").to_string();
        let filename = field.file_name().map(|f| f.to_string());
        let data = field.bytes().await.map_err(|e| ApiError::bad(format!("Invalid form: {e}")))?;
        out.push(Upload { name, filename, data: data.to_vec() });
    }
    Ok(out)
}

/// The `Link`-less pagination used by the web UI.
pub fn page_of(r: &Req) -> usize {
    r.q("p").and_then(|v| v.parse().ok()).unwrap_or(0)
}
