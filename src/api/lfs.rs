//! Git LFS Batch API (`{repo}.git/info/lfs/objects/batch`), basic transfer
//! adapter (PUT/GET with self-authorizing tickets), verify, and listing.
//! Used both by `huggingface_hub` uploads and by the `git-lfs` CLI.

use crate::error::{ApiError, ApiResult};
use crate::git::lfs::valid_oid;
use crate::models::{load_repo, Access};
use crate::routes::{json, json_status, Req};
use axum::body::Body;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const LFS_CT: &str = "application/vnd.git-lfs+json";

fn lfs_json(status: StatusCode, v: Value) -> Response {
    let mut r = json_status(status, v);
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(LFS_CT));
    r
}

fn lfs_err(status: StatusCode, msg: &str) -> Response {
    lfs_json(status, json!({ "message": msg }))
}

pub async fn git_lfs(r: &mut Req, rtype: &str, ns: &str, name: &str, rest: &[&str]) -> ApiResult<Response> {
    match (r.method.clone(), rest) {
        (Method::POST, ["objects", "batch"]) => batch(r, rtype, ns, name).await,
        (Method::PUT, ["objects", oid]) => upload(r, rtype, ns, name, oid).await,
        (Method::GET, ["objects", oid]) => download(r, rtype, ns, name, oid).await,
        (Method::POST, ["objects", "verify"]) | (Method::POST, ["verify"]) => verify(r, rtype, ns, name).await,
        (Method::POST, ["locks", "verify"]) => Ok(lfs_json(StatusCode::OK, json!({ "ours": [], "theirs": [] }))),
        (Method::GET, ["locks"]) => Ok(lfs_json(StatusCode::OK, json!({ "locks": [] }))),
        (_, ["locks", ..]) => Ok(lfs_err(StatusCode::NOT_IMPLEMENTED, "Locking is not supported")),
        _ => Err(ApiError::not_found("Not found")),
    }
}

async fn batch(r: &mut Req, rtype: &str, ns: &str, name: &str) -> ApiResult<Response> {
    let caller = match r.caller().await {
        Ok(c) => c,
        Err(_) => return Ok(unauthorized()),
    };
    let b: Value = r.json().await?;
    let op = b.get("operation").and_then(|v| v.as_str()).unwrap_or("download").to_string();
    let need = if op == "upload" { Access::Write } else { Access::Read };
    let repo = match load_repo(&r.st, rtype, ns, name, caller.as_ref(), need).await {
        Ok((repo, _)) => repo,
        Err(e) if caller.is_none() && e.status == StatusCode::NOT_FOUND || e.status == StatusCode::UNAUTHORIZED => {
            return Ok(unauthorized())
        }
        Err(e) if e.status == StatusCode::FORBIDDEN => return Ok(lfs_err(StatusCode::FORBIDDEN, &e.msg)),
        Err(e) => return Ok(lfs_err(StatusCode::NOT_FOUND, &e.msg)),
    };
    let base_href = format!("{}/{}.git/info/lfs/objects", r.base, repo.web_path());
    let mut objects = vec![];
    for o in b.get("objects").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let oid = o.get("oid").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
        let size = o.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
        if !valid_oid(&oid) {
            objects.push(json!({ "oid": oid, "size": size, "error": { "code": 422, "message": "Invalid oid" } }));
            continue;
        }
        let exists = r.st.storage.size(&oid).await?.is_some_and(|s| s as i64 == size);
        if op == "upload" {
            if exists {
                crate::api::commit::register_lfs(&r.st, repo.id, &[(oid.clone(), size)]).await?;
                objects.push(json!({ "oid": oid, "size": size, "authenticated": true }));
            } else {
                let t = r.st.issue_ticket(repo.id, &oid, true);
                let href = format!("{base_href}/{oid}?ticket={t}&size={size}");
                objects.push(json!({
                    "oid": oid, "size": size, "authenticated": true,
                    "actions": {
                        "upload": { "href": href, "header": { "X-HH-Ticket": t }, "expires_in": 21600 },
                        "verify": { "href": format!("{base_href}/verify?ticket={t}"), "header": { "X-HH-Ticket": t }, "expires_in": 21600 },
                    }
                }));
            }
        } else {
            let known = {
                let (id, o2) = (repo.id, oid.clone());
                r.st.db(move |c| {
                    c.query_row("SELECT 1 FROM lfs_objects WHERE repo_id = ?1 AND oid = ?2", params![id, o2], |_| Ok(()))
                        .optional()
                })
                .await?
                .is_some()
            };
            if !exists || !known {
                objects.push(json!({ "oid": oid, "size": size, "error": { "code": 404, "message": "Object does not exist" } }));
                continue;
            }
            let t = r.st.issue_ticket(repo.id, &oid, false);
            objects.push(json!({
                "oid": oid, "size": size, "authenticated": true,
                "actions": { "download": { "href": format!("{base_href}/{oid}?ticket={t}"), "header": { "X-HH-Ticket": t }, "expires_in": 21600 } }
            }));
        }
    }
    Ok(lfs_json(StatusCode::OK, json!({ "transfer": "basic", "objects": objects, "hash_algo": "sha256" })))
}

fn unauthorized() -> Response {
    let mut r = lfs_err(StatusCode::UNAUTHORIZED, "Authentication required");
    r.headers_mut().insert("LFS-Authenticate", HeaderValue::from_static("Basic realm=\"HuggingHub\""));
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"HuggingHub\""));
    r
}

fn ticket_of(r: &Req) -> Option<String> {
    r.q("ticket")
        .map(String::from)
        .or_else(|| r.headers.get("x-hh-ticket").and_then(|v| v.to_str().ok()).map(String::from))
}

async fn repo_for_ticket(r: &Req, rtype: &str, ns: &str, name: &str, oid: &str, write: bool) -> ApiResult<crate::models::RepoRow> {
    let (rt, n1, n2) = (rtype.to_string(), ns.to_string(), name.to_string());
    let repo = r
        .st
        .db(move |c| crate::models::RepoRow::find(c, &rt, &n1, &n2))
        .await?
        .ok_or_else(ApiError::repo_not_found)?;
    let ok = ticket_of(r).is_some_and(|t| r.st.check_ticket(&t, repo.id, oid, write));
    if !ok {
        // fall back to normal auth (git-lfs may retry with credentials)
        let caller = r.caller().await?;
        let need = if write { Access::Write } else { Access::Read };
        load_repo(&r.st, rtype, ns, name, caller.as_ref(), need).await?;
    }
    Ok(repo)
}

async fn upload(r: &mut Req, rtype: &str, ns: &str, name: &str, oid: &str) -> ApiResult<Response> {
    let oid = oid.to_ascii_lowercase();
    if !valid_oid(&oid) {
        return Err(ApiError::bad("Invalid oid"));
    }
    let repo = repo_for_ticket(r, rtype, ns, name, &oid, true).await?;
    let tmp = r.st.cfg.tmp_dir().join(format!("lfs-{}", crate::util::random_token(20)));
    let mut f = tokio::fs::File::create(&tmp).await?;
    let mut hasher = Sha256::new();
    let mut size: i64 = 0;
    let mut stream = r.take_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(ApiError::bad(format!("Upload interrupted: {e}")));
            }
        };
        hasher.update(&chunk);
        size += chunk.len() as i64;
        f.write_all(&chunk).await?;
    }
    f.flush().await?;
    drop(f);
    let got = hex::encode(hasher.finalize());
    if got != oid {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(ApiError::bad(format!("SHA256 mismatch: expected {oid}, got {got}")));
    }
    if let Err(e) = r.st.storage.put_file(&oid, &tmp, size as u64).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    crate::api::commit::register_lfs(&r.st, repo.id, &[(oid, size)]).await?;
    Ok(StatusCode::OK.into_response())
}

async fn download(r: &mut Req, rtype: &str, ns: &str, name: &str, oid: &str) -> ApiResult<Response> {
    let oid = oid.to_ascii_lowercase();
    if !valid_oid(&oid) {
        return Err(ApiError::bad("Invalid oid"));
    }
    repo_for_ticket(r, rtype, ns, name, &oid, false).await?;
    let len = r.st.storage.size(&oid).await?.ok_or_else(|| ApiError::not_found("Object not found"))?;
    if let Some(url) = r.st.storage.presigned_get(&oid, &oid) {
        return Ok(Response::builder().status(StatusCode::FOUND).header(header::LOCATION, url).body(Body::empty()).unwrap());
    }
    let body = r.st.storage.get(&oid, 0, len).await?.ok_or_else(|| ApiError::not_found("Object not found"))?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len.to_string())
        .body(body)
        .unwrap())
}

async fn verify(r: &mut Req, rtype: &str, ns: &str, name: &str) -> ApiResult<Response> {
    let b: Value = r.json().await?;
    let oid = b.get("oid").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
    let size = b.get("size").and_then(|v| v.as_i64()).unwrap_or(-1);
    if !valid_oid(&oid) {
        return Err(ApiError::bad("Invalid oid"));
    }
    repo_for_ticket(r, rtype, ns, name, &oid, true).await?;
    match r.st.storage.size(&oid).await? {
        Some(s) if s as i64 == size => Ok(lfs_json(StatusCode::OK, json!({}))),
        Some(_) => Ok(lfs_err(StatusCode::UNPROCESSABLE_ENTITY, "Size mismatch")),
        None => Ok(lfs_err(StatusCode::NOT_FOUND, "Object not found")),
    }
}

/// GET /api/{type}s/{ns}/{name}/lfs-files
pub async fn list_lfs_files(r: &mut Req, rtype: &str, ns: &str, name: &str) -> ApiResult<Response> {
    let caller = r.require().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, Some(&caller), Access::Write).await?;
    let id = repo.id;
    let rows = r
        .st
        .db(move |c| {
            let mut st = c.prepare("SELECT oid, size, created_at FROM lfs_objects WHERE repo_id = ?1 ORDER BY created_at DESC")?;
            let v = st
                .query_map([id], |r| {
                    Ok(json!({ "oid": r.get::<_, String>(0)?, "size": r.get::<_, i64>(1)?, "pushedAt": r.get::<_, String>(2)?, "fileOid": r.get::<_, String>(0)? }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    Ok(json(json!(rows)))
}
