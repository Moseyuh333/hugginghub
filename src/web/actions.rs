//! Repo write actions from the web UI: settings, uploads, discussions, likes.

use crate::error::{ApiError, ApiResult};
use crate::git::write::Op;
use crate::models::{load_repo, Access, Caller, RepoRow};
use crate::routes::Req;
use crate::service::{self, signature};
use crate::web::{base_ctx, ctx, form, multipart, render, render_status};
use axum::http::StatusCode;
use axum::response::Response;
use minijinja::context;
use serde_json::{json, Value};

async fn writable(r: &Req, rtype: &str, ns: &str, name: &str) -> ApiResult<(RepoRow, Caller)> {
    let me = r.web_caller().await.ok_or_else(|| ApiError::unauthorized())?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, Some(&me), Access::Write).await?;
    Ok((repo, me))
}

pub async fn settings_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, flash: Option<&str>) -> ApiResult<Response> {
    if access < Access::Admin {
        return Err(ApiError::forbidden("Only admins can change settings"));
    }
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let private = f.get("visibility").map(|v| v == "private").unwrap_or(repo.private);
        let disc = f.get("discussions").map(|v| v == "1").unwrap_or(!repo.discussions_disabled);
        // Only a space settings form carries app_url; keep it untouched otherwise.
        let app_url = f
            .get("app_url")
            .map(|v| if v.trim().is_empty() { None } else { Some(v.trim().to_string()) })
            .unwrap_or(repo.app_url.clone());
        let id = repo.id;
        r.st.db(move |c| {
            c.execute(
                "UPDATE repos SET private = ?1, discussions_disabled = ?2, app_url = ?3 WHERE id = ?4",
                rusqlite::params![private as i64, (!disc) as i64, app_url, id],
            )?;
            Ok(())
        })
        .await?;
        let (repo, access) = load_repo(&r.st, &repo.rtype, &repo.owner, &repo.name, me.as_ref(), Access::Read).await?;
        let hdr = crate::web::repo::header(r, &repo, &me, access).await?;
        let c = ctx(r, &me, &[hdr, json!({ "title": format!("Settings · {}", repo.full_id()), "tab": "settings", "flash": "Settings saved" })]);
        return render(&r.st, "repo/settings.html", c);
    }
    let hdr = crate::web::repo::header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("Settings · {}", repo.full_id()), "tab": "settings", "flash": flash })]);
    render(&r.st, "repo/settings.html", ctx)
}

pub async fn rename(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access) -> ApiResult<Response> {
    if access < Access::Admin {
        return Err(ApiError::forbidden("Only admins can rename"));
    }
    let f = form(r).await?;
    let name = f.get("name").cloned().unwrap_or_default();
    if !crate::util::valid_name(&name) {
        return Err(ApiError::bad("Invalid repository name"));
    }
    let (id, rt, nm) = (repo.id, repo.rtype.clone(), name.clone());
    let new_name = name.clone();
    let exists = r
        .st
        .db(move |c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM repos WHERE owner_id = (SELECT owner_id FROM repos WHERE id = ?1) AND type = ?2 AND name = ?3 AND id != ?1",
                rusqlite::params![id, rt, nm],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
        .await?;
    if exists {
        return Err(ApiError::conflict("A repository with this name already exists"));
    }
    let id = repo.id;
    r.st.db(move |c| c.execute("UPDATE repos SET name = ?1 WHERE id = ?2", rusqlite::params![new_name, id])).await?;
    Ok(crate::web::redirect(&format!("/{}/{name}", repo.owner)))
}

pub async fn delete(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access) -> ApiResult<Response> {
    if access < Access::Admin {
        return Err(ApiError::forbidden("Only admins can delete"));
    }
    let f = form(r).await?;
    if f.get("confirm").map(|c| c.trim()) != Some(repo.name.as_str()) {
        return Err(ApiError::bad("Type the repository name to confirm deletion"));
    }
    crate::api::repos::delete_repo(&r.st, &repo).await?;
    Ok(crate::web::redirect(&format!("/{}", repo.owner)))
}

pub async fn like(r: &mut Req, repo: RepoRow, me: Option<Caller>) -> ApiResult<Response> {
    let Some(caller) = me.clone() else { return Ok(crate::web::login_redirect(r)) };
    let (uid, rid) = (caller.user.id, repo.id);
    r.st.db(move |c| {
        let has = c.query_row("SELECT 1 FROM likes WHERE user_id = ?1 AND repo_id = ?2", rusqlite::params![uid, rid], |_| Ok(())).is_ok();
        if has {
            c.execute("DELETE FROM likes WHERE user_id = ?1 AND repo_id = ?2", rusqlite::params![uid, rid])?;
        } else {
            c.execute("INSERT OR IGNORE INTO likes (user_id, repo_id, created_at) VALUES (?1, ?2, ?3)", rusqlite::params![uid, rid, crate::util::now_iso()])?;
        }
        Ok(())
    })
    .await?;
    Ok(crate::web::redirect(&format!("/{}", repo.web_path())))
}

pub async fn discussion_action(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, num: &str, action: &str) -> ApiResult<Response> {
    let Some(caller) = me.clone() else { return Ok(crate::web::login_redirect(r)) };
    let num: i64 = num.parse().map_err(|_| ApiError::bad("Invalid number"))?;
    let back = format!("/{}/discussions/{num}", repo.web_path());
    let d = {
        let rid = repo.id;
        r.st.db(move |c| crate::api::community::get_disc(c, rid, num)).await?
    }
    .ok_or_else(|| ApiError::not_found("Discussion not found"))?;
    match action {
        "comment" => {
            let f = form(r).await?;
            let text = f.get("comment").cloned().unwrap_or_default();
            if !text.trim().is_empty() {
                let (did, user, base) = (d.id, caller.user.clone(), r.base.clone());
                r.st.db(move |c| {
                    let data = json!({
                        "edited": false, "hidden": false,
                        "latest": { "raw": text, "html": crate::web::markdown::render(&text), "updatedAt": crate::util::now_iso(),
                                    "author": { "name": user.name, "fullname": user.display_name() } },
                        "numEdits": 0, "history": [], "editors": [user.name], "reactions": [],
                    });
                    crate::api::community::insert_event(c, did, "comment", user.id, &data)?;
                    Ok(())
                })
                .await?;
                let _ = base;
            }
        }
        "status" => {
            let f = form(r).await?;
            let status = f.get("status").cloned().unwrap_or_default();
            if status == "open" || status == "closed" {
                let (did, uid) = (d.id, caller.user.id);
                r.st.db(move |c| {
                    c.execute("UPDATE discussions SET status = ?1 WHERE id = ?2", rusqlite::params![status, did])?;
                    crate::api::community::insert_event(c, did, "status-change", uid, &json!({ "status": status }))?;
                    Ok(())
                })
                .await?;
            }
        }
        "merge" => {
            if access < Access::Write {
                return Err(ApiError::forbidden("You need write access to merge"));
            }
            if d.is_pr && (d.status == "open" || d.status == "draft") {
                let oid = crate::api::community::merge_pr(&r.st, &repo, &caller, &d).await?;
                let (did, uid) = (d.id, caller.user.id);
                r.st.db(move |c| {
                    c.execute("UPDATE discussions SET status = 'merged', merge_oid = ?1 WHERE id = ?2", rusqlite::params![oid, did])?;
                    crate::api::community::insert_event(c, did, "status-change", uid, &json!({ "status": "merged" }))?;
                    Ok(())
                })
                .await?;
            }
        }
        _ => return Err(ApiError::not_found("Not found")),
    }
    Ok(crate::web::redirect(&back))
}

pub async fn blob_redirect(r: &mut Req, repo: &RepoRow, rev: &str, path: &str) -> ApiResult<Response> {
    Ok(crate::web::redirect(&format!("/{}/blob/{rev}/{path}", repo.web_path())))
}

pub async fn upload_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, rev: &str) -> ApiResult<Response> {
    if access < Access::Write {
        return Err(ApiError::forbidden("You need write access to upload files"));
    }
    let Some(caller) = me.clone() else { return Ok(crate::web::login_redirect(r)) };
    let pr = r.q_bool("pr") || r.q("pr") == Some("1");
    if r.method == axum::http::Method::POST {
        let parts = multipart(r).await?;
        let mut ops = vec![];
        let mut lfs = vec![];
        let mut message = String::new();
        let mut description = String::new();
        let mut text_path: Option<(String, Vec<u8>)> = None;
        for p in parts {
            match p.name.as_str() {
                "message" => message = String::from_utf8_lossy(&p.data).to_string(),
                "description" => description = String::from_utf8_lossy(&p.data).to_string(),
                "path" => text_path = Some((String::from_utf8_lossy(&p.data).to_string(), vec![])),
                "content" => {
                    if let Some((_, data)) = text_path.as_mut() {
                        *data = p.data.clone();
                    }
                }
                "files" => {
                    if let Some(fname) = p.filename.clone() {
                        let clean = crate::git::write::clean_path(&fname).unwrap_or_else(|| fname.clone());
                        let threshold = r.st.cfg.lfs_threshold as u64;
                        if p.data.len() as u64 >= threshold {
                            let oid = crate::util::sha256_hex(&p.data);
                            let tmp = r.st.cfg.tmp_dir().join(format!("web-{}", crate::util::random_token(16)));
                            tokio::fs::write(&tmp, &p.data).await?;
                            r.st.storage.put_file(&oid, &tmp, p.data.len() as u64).await?;
                            lfs.push((oid.clone(), p.data.len() as i64));
                            ops.push(Op::Put { path: clean, data: crate::git::lfs::pointer_text(&oid, p.data.len() as i64).into_bytes() });
                        } else {
                            ops.push(Op::Put { path: clean, data: p.data });
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some((path, data)) = text_path {
            if let Some(clean) = crate::git::write::clean_path(&path) {
                ops.push(Op::Put { path: clean, data });
            }
        }
        if ops.is_empty() {
            return Err(ApiError::bad("No files to upload"));
        }
        let msg = crate::api::commit::commit_message(&message, &description);
        let git = r.st.git(&repo.disk_id);
        let lock = r.st.repo_lock(repo.id);
        let _g = lock.lock().await;
        let base = git.resolve(rev).await?;
        let new = git.build_commit(base.as_deref(), &ops, &msg, &signature(&caller.user)).await?;
        crate::api::commit::register_lfs(&r.st, repo.id, &lfs).await?;
        if pr {
            let num = crate::api::community::open_pr_from_commit(&r.st, &repo, &caller, rev, &new, &message, &description).await?;
            return Ok(crate::web::redirect(&format!("/{}/discussions/{num}", repo.web_path())));
        }
        let refname = format!("refs/heads/{rev}");
        git.update_ref(&refname, &new, base.as_deref()).await?;
        service::refresh_meta(&r.st, &repo).await?;
        return Ok(crate::web::redirect(&format!("/{}/tree/{rev}", repo.web_path())));
    }
    let hdr = crate::web::repo::header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": "Upload files", "r": crate::web::repo_card(&repo), "rev": rev, "pr": pr, "error": Value::Null, "path": "" })]);
    render(&r.st, "repo/upload.html", ctx)
}

pub async fn new_file_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, rev: &str) -> ApiResult<Response> {
    if r.method == axum::http::Method::POST {
        return upload_page(r, repo, me, access, rev).await;
    }
    upload_page(r, repo, me, access, rev).await
}

pub async fn edit_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, rev: &str, path: &str) -> ApiResult<Response> {
    if access < Access::Write {
        return Err(ApiError::forbidden("You need write access to edit files"));
    }
    let Some(caller) = me.clone() else { return Ok(crate::web::login_redirect(r)) };
    let git = r.st.git(&repo.disk_id);
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let entry = git.stat_path(&sha, path).await?.ok_or_else(|| ApiError::entry_not_found(path))?;
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let content = f.get("content").cloned().unwrap_or_default();
        let msg = f.get("message").cloned().unwrap_or_else(|| format!("Update {path}"));
        let lock = r.st.repo_lock(repo.id);
        let _g = lock.lock().await;
        let new = git.build_commit(Some(&sha), &[Op::Put { path: path.to_string(), data: content.into_bytes() }], &msg, &signature(&caller.user)).await?;
        git.update_ref(&format!("refs/heads/{rev}"), &new, Some(&sha)).await?;
        service::refresh_meta(&r.st, &repo).await?;
        return Ok(crate::web::redirect(&format!("/{}/blob/{rev}/{path}", repo.web_path())));
    }
    let data = git.cat_blob(&entry.oid).await?;
    let content = String::from_utf8_lossy(&data).to_string();
    let hdr = crate::web::repo::header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("Edit {path}"), "r": crate::web::repo_card(&repo), "rev": rev, "path": path, "content": content, "error": Value::Null })]);
    render_status(&r.st, StatusCode::OK, "repo/edit.html", ctx)
}
