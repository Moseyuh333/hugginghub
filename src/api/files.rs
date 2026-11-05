//! Read access to repo contents: tree listing, paths-info and `resolve` downloads.

use crate::error::{ApiError, ApiResult};
use crate::models::{load_repo, Access};
use crate::routes::{json, json_paged, Req};
use crate::service::{with_lfs, FileInfo};
use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rusqlite::params;
use serde_json::{json, Value};

const TREE_PAGE: usize = 1000;

async fn add_last_commits(git: &crate::git::Repo, sha: &str, files: &[FileInfo], out: &mut [Value]) -> ApiResult<()> {
    for (f, v) in files.iter().zip(out.iter_mut()) {
        if let Some(c) = git.last_commit(sha, &f.entry.path).await? {
            v["lastCommit"] = json!({ "id": c.id, "title": c.title, "date": c.date });
        }
    }
    Ok(())
}

/// GET /api/{type}s/{ns}/{name}/tree/{rev}/{path}
pub async fn tree(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: &str, path: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let git = r.st.git(&repo.disk_id);
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let path = path.trim_matches('/');
    if !path.is_empty() {
        match git.stat_path(&sha, path).await? {
            Some(e) if e.kind == "directory" => {}
            _ => return Err(ApiError::entry_not_found(path)),
        }
    }
    let recursive = r.q_bool("recursive");
    let expand = r.q_bool("expand");
    let mut entries = git.ls_tree(&sha, path, recursive).await?;
    // directories first, like the Hub
    entries.sort_by(|a, b| (a.kind != "directory").cmp(&(b.kind != "directory")).then(a.path.cmp(&b.path)));
    let offset: usize = r.q("cursor").and_then(|v| v.parse().ok()).unwrap_or(0);
    let page_size = if expand { 100 } else { TREE_PAGE };
    let total = entries.len();
    let page: Vec<_> = entries.into_iter().skip(offset).take(page_size).collect();
    let files = with_lfs(&git, page).await?;
    let mut out: Vec<Value> = files.iter().map(|f| f.to_json()).collect();
    if expand {
        add_last_commits(&git, &sha, &files, &mut out).await?;
    }
    let next = (offset + page_size < total).then(|| r.url_with(&[("cursor", (offset + page_size).to_string())]));
    Ok(json_paged(json!(out), next))
}

/// POST /api/{type}s/{ns}/{name}/paths-info/{rev}  (form or JSON body: paths[], expand)
pub async fn paths_info(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let is_json = r
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("json"));
    let body = r.bytes(16 * 1024 * 1024).await?;
    let (paths, expand) = if is_json {
        let v: Value = serde_json::from_slice(&body).map_err(|e| ApiError::bad(e.to_string()))?;
        let paths = match v.get("paths") {
            Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => vec![],
        };
        (paths, v.get("expand").and_then(|x| x.as_bool()).unwrap_or(false))
    } else {
        let kv: Vec<(String, String)> = serde_urlencoded::from_bytes(&body).unwrap_or_default();
        let paths = kv.iter().filter(|(k, _)| k == "paths" || k == "paths[]").map(|(_, v)| v.clone()).collect();
        let expand = kv.iter().any(|(k, v)| k == "expand" && matches!(v.to_lowercase().as_str(), "true" | "1"));
        (paths, expand)
    };
    let git = r.st.git(&repo.disk_id);
    let rev = if rev.is_empty() { "main" } else { rev };
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let mut entries = vec![];
    for p in paths {
        if let Some(e) = git.stat_path(&sha, &p).await? {
            entries.push(e);
        }
    }
    let files = with_lfs(&git, entries).await?;
    let mut out: Vec<Value> = files.iter().map(|f| f.to_json()).collect();
    if expand {
        add_last_commits(&git, &sha, &files, &mut out).await?;
    }
    Ok(json(json!(out)))
}

/// MIME types for files served *as a web app* (Spaces). Unlike `content_type`,
/// which is for downloads/previews, html and assets get their real types here.
pub fn app_content_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "json" => "application/json",
        "md" | "txt" | "py" | "yaml" | "yml" | "toml" | "cfg" | "csv" | "tsv" | "jsonl" | "gitattributes" => {
            "text/plain; charset=utf-8"
        }
        "html" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Parse a single `bytes=a-b` range against `size`.
fn parse_range(h: Option<&HeaderValue>, size: u64) -> Option<(u64, u64)> {
    let s = h?.to_str().ok()?.strip_prefix("bytes=")?;
    if s.contains(',') {
        return None;
    }
    let (a, b) = s.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (size.saturating_sub(n), size.saturating_sub(1))
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { size.saturating_sub(1) } else { b.parse::<u64>().ok()?.min(size.saturating_sub(1)) };
        (start, end)
    };
    (start <= end && start < size).then_some((start, end))
}

/// HEAD/GET /{repo}/resolve/{rev}/{path}
pub async fn resolve(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: &str, path: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let git = r.st.git(&repo.disk_id);
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let entry = match git.stat_path(&sha, path).await? {
        Some(e) if e.kind == "file" => e,
        _ => {
            let mut resp = ApiError::entry_not_found(path).into_response();
            resp.headers_mut().insert("X-Repo-Commit", HeaderValue::from_str(&sha).unwrap());
            return Ok(resp);
        }
    };
    let info = with_lfs(&git, vec![entry]).await?.remove(0);
    let is_head = r.method == axum::http::Method::HEAD;

    // download counter: one per "snapshot" (config.json for models, README.md otherwise)
    let counted = if rtype == "model" { "config.json" } else { "README.md" };
    if path == counted {
        let id = repo.id;
        r.st.db(move |c| c.execute("UPDATE repos SET downloads = downloads + 1 WHERE id = ?1", params![id])).await?;
    }

    let size = info.size() as u64;
    let etag = match &info.lfs {
        Some(p) => format!("\"{}\"", p.oid),
        None => format!("\"{}\"", info.entry.oid),
    };
    let range = parse_range(r.headers.get(header::RANGE), size);
    let mut b = Response::builder()
        .header("X-Repo-Commit", &sha)
        .header(header::ETAG, &etag)
        .header("X-Linked-Etag", &etag)
        .header("X-Linked-Size", size.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, content_type(path))
        .header("Access-Control-Expose-Headers", "X-Repo-Commit, X-Linked-Etag, X-Linked-Size, ETag, Content-Range");
    if r.q_bool("download") {
        let fname = path.rsplit('/').next().unwrap_or(path).replace('"', "");
        b = b.header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{fname}\""));
    }
    let (status, start, len) = match range {
        Some((s, e)) => {
            b = b.header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{size}"));
            (StatusCode::PARTIAL_CONTENT, s, e - s + 1)
        }
        None => (StatusCode::OK, 0, size),
    };
    b = b.status(status).header(header::CONTENT_LENGTH, len.to_string());
    if is_head {
        return Ok(b.body(Body::empty()).unwrap());
    }
    let body = match &info.lfs {
        Some(p) => {
            // S3 with HH_S3_REDIRECT=1: send the client straight to the bucket (like the Hub's CDN)
            let fname = path.rsplit('/').next().unwrap_or(path);
            if let Some(url) = r.st.storage.presigned_get(&p.oid, fname) {
                return Ok(Response::builder()
                    .status(StatusCode::FOUND)
                    .header(header::LOCATION, url)
                    .header("X-Repo-Commit", &sha)
                    .header(header::ETAG, &etag)
                    .header("X-Linked-Etag", &etag)
                    .header("X-Linked-Size", size.to_string())
                    .body(Body::empty())
                    .unwrap());
            }
            r.st
                .storage
                .get(&p.oid, start, len)
                .await?
                .ok_or_else(|| ApiError::not_found("LFS object missing on server"))?
        }
        None => {
            let data = git.cat_blob(&info.entry.oid).await?;
            let s = start as usize;
            Body::from(data[s..(s + len as usize).min(data.len())].to_vec())
        }
    };
    Ok(b.body(body).unwrap())
}
