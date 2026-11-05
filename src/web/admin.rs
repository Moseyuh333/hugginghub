//! Instance administration: users, repos, orgs and server info.
//!
//! Every handler starts with `require_admin`: a non-admin gets a 403 and a
//! signed-out visitor is redirected to the login form.

use crate::auth::hash_password;
use crate::error::{ApiError, ApiResult};
use crate::models::{Account, Caller, RepoRow, REPO_SELECT};
use crate::routes::Req;
use crate::util::{now_iso, valid_account_name};
use crate::web::{ctx, form, render};
use axum::response::Response;
use rusqlite::params;
use serde_json::json;

/// Ok(None) means "not signed in" — callers redirect to the login form.
fn require_admin(me: &Option<Caller>) -> ApiResult<Option<Caller>> {
    match me {
        Some(c) if c.user.is_admin => Ok(Some(c.clone())),
        Some(_) => Err(ApiError::forbidden("This page is for instance administrators")),
        None => Ok(None),
    }
}

fn admin_ctx(r: &Req, me: &Option<Caller>, section: &str, extra: serde_json::Value) -> serde_json::Value {
    let title = match section {
        "users" => "Users",
        "repos" => "Repositories",
        "orgs" => "Organizations",
        "server" => "Server",
        _ => "Admin",
    };
    ctx(r, me, &[json!({ "title": format!("{title} · Admin"), "section": section, "admin": true }), extra])
}

/// GET /admin -> overview
pub async fn index(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let (n_users, n_orgs, n_repos, n_private, n_discs, storage, newest) = r
        .st
        .db(|c| {
            let n_users: i64 = c.query_row("SELECT COUNT(*) FROM accounts WHERE kind = 'user'", [], |r| r.get(0))?;
            let n_orgs: i64 = c.query_row("SELECT COUNT(*) FROM accounts WHERE kind = 'org'", [], |r| r.get(0))?;
            let n_repos: i64 = c.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))?;
            let n_private: i64 = c.query_row("SELECT COUNT(*) FROM repos WHERE private = 1", [], |r| r.get(0))?;
            let n_discs: i64 = c.query_row("SELECT COUNT(*) FROM discussions", [], |r| r.get(0))?;
            let storage: i64 = c.query_row("SELECT COALESCE(SUM(used_storage), 0) FROM repos", [], |r| r.get(0))?;
            let mut st = c.prepare(&format!("{REPO_SELECT} ORDER BY r.created_at DESC LIMIT 8"))?;
            let newest: Vec<serde_json::Value> = st
                .query_map([], RepoRow::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .map(crate::web::repo_card)
                .collect();
            Ok((n_users, n_orgs, n_repos, n_private, n_discs, storage, newest))
        })
        .await?;
    let c = admin_ctx(
        r,
        &me,
        "overview",
        json!({
            "n_users": n_users, "n_orgs": n_orgs, "n_repos": n_repos, "n_private": n_private,
            "n_discs": n_discs, "storage": crate::util::human_size(storage), "newest": newest,
        }),
    );
    render(&r.st, "admin/index.html", c)
}

/// GET /admin/users
pub async fn users(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let base = r.base.clone();
    let list = r
        .st
        .db(move |c| {
            let mut st = c.prepare(
                "SELECT a.id, a.name, a.kind, a.fullname, a.email, a.bio, a.website, a.avatar, a.is_admin, a.created_at,
                        (SELECT COUNT(*) FROM repos r WHERE r.owner_id = a.id),
                        (SELECT COUNT(*) FROM tokens t WHERE t.user_id = a.id)
                 FROM accounts a ORDER BY a.kind, a.name",
            )?;
            let v = st
                .query_map([], |row| {
                    let acc = Account::from_row(row)?;
                    Ok(json!({
                        "name": acc.name, "fullname": acc.display_name(), "kind": acc.kind,
                        "email": acc.email, "is_admin": acc.is_admin, "created_at": acc.created_at,
                        "avatar": acc.avatar_url(&base),
                        "num_repos": row.get::<_, i64>(10)?, "num_tokens": row.get::<_, i64>(11)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    let flash = r.q("created").map(|n| format!("User {n} created"));
    let c = admin_ctx(r, &me, "users", json!({ "users": list, "flash": flash }));
    render(&r.st, "admin/users.html", c)
}

/// POST /admin/users/new
pub async fn create_user(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let f = form(r).await?;
    let name = f.get("name").cloned().unwrap_or_default().trim().to_string();
    let password = f.get("password").cloned().unwrap_or_default();
    let is_admin = f.get("is_admin").map(|v| v == "1").unwrap_or(false);
    if !valid_account_name(&name) {
        return Err(ApiError::bad("Invalid username: use letters, digits, '-', '_', '.'"));
    }
    if password.len() < 8 {
        return Err(ApiError::bad("Password must be at least 8 characters"));
    }
    let hash = hash_password(&password);
    let n = name.clone();
    let created = r
        .st
        .db(move |c| {
            if Account::by_name(c, &n)?.is_some() {
                return Ok(false);
            }
            c.execute(
                "INSERT INTO accounts (name, kind, fullname, password, created_at, is_admin) VALUES (?1, 'user', ?1, ?2, ?3, ?4)",
                params![n, hash, now_iso(), is_admin as i64],
            )?;
            Ok(true)
        })
        .await?;
    if !created {
        return Err(ApiError::conflict("That username is already taken"));
    }
    Ok(crate::web::redirect(&format!("/admin/users?created={name}")))
}

/// POST /admin/users/{name}/admin — toggle administrator rights.
pub async fn toggle_admin(r: &mut Req, me: Option<Caller>, name: &str) -> ApiResult<Response> {
    let Some(caller) = require_admin(&me)? else {
        return Ok(crate::web::login_redirect(r));
    };
    let n = name.to_string();
    let target = r.st.db(move |c| Account::by_name(c, &n)).await?.ok_or_else(|| ApiError::not_found("No such user"))?;
    if target.kind != "user" {
        return Err(ApiError::bad("Only users can be administrators"));
    }
    if target.id == caller.user.id {
        return Err(ApiError::bad("You cannot remove your own administrator rights"));
    }
    let (id, new) = (target.id, !target.is_admin);
    r.st.db(move |c| c.execute("UPDATE accounts SET is_admin = ?1 WHERE id = ?2", params![new as i64, id])).await?;
    Ok(crate::web::redirect("/admin/users"))
}

/// POST /admin/users/{name}/password — set a new password (also revokes sessions).
pub async fn set_password(r: &mut Req, me: Option<Caller>, name: &str) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let f = form(r).await?;
    let password = f.get("password").cloned().unwrap_or_default();
    if password.len() < 8 {
        return Err(ApiError::bad("Password must be at least 8 characters"));
    }
    let n = name.to_string();
    let target = r.st.db(move |c| Account::by_name(c, &n)).await?.ok_or_else(|| ApiError::not_found("No such user"))?;
    let hash = hash_password(&password);
    let id = target.id;
    r.st.db(move |c| {
        c.execute("UPDATE accounts SET password = ?1 WHERE id = ?2", params![hash, id])?;
        c.execute("DELETE FROM sessions WHERE user_id = ?1", params![id])?;
        Ok(())
    })
    .await?;
    Ok(crate::web::redirect("/admin/users"))
}

/// POST /admin/users/{name}/delete — removes the account and everything it owns.
pub async fn delete_user(r: &mut Req, me: Option<Caller>, name: &str) -> ApiResult<Response> {
    let Some(caller) = require_admin(&me)? else {
        return Ok(crate::web::login_redirect(r));
    };
    let f = form(r).await?;
    let n = name.to_string();
    let target = r.st.db(move |c| Account::by_name(c, &n)).await?.ok_or_else(|| ApiError::not_found("No such user"))?;
    if target.id == caller.user.id {
        return Err(ApiError::bad("You cannot delete your own account"));
    }
    if f.get("confirm").map(|s| s.trim()) != Some(target.name.as_str()) {
        return Err(ApiError::bad("Type the account name to confirm deletion"));
    }
    let id = target.id;
    let repos: Vec<RepoRow> = r
        .st
        .db(move |c| {
            let mut st = c.prepare(&format!("{REPO_SELECT} WHERE r.owner_id = ?1"))?;
            let v = st.query_map([id], RepoRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    for repo in &repos {
        crate::api::repos::delete_repo(&r.st, repo).await?;
    }
    let (id, kind) = (target.id, target.kind.clone());
    r.st.db(move |c| {
        if kind == "org" {
            c.execute("DELETE FROM org_members WHERE org_id = ?1", params![id])?;
        } else {
            c.execute("DELETE FROM org_members WHERE user_id = ?1", params![id])?;
        }
        for sql in [
            "DELETE FROM tokens WHERE user_id = ?1",
            "DELETE FROM sessions WHERE user_id = ?1",
            "DELETE FROM likes WHERE user_id = ?1",
            "DELETE FROM follows WHERE follower_id = ?1 OR target_id = ?1",
            "DELETE FROM discussions WHERE author_id = ?1",
        ] {
            c.execute(sql, params![id])?;
        }
        c.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
        Ok(())
    })
    .await?;
    Ok(crate::web::redirect("/admin/users"))
}

/// GET /admin/repos
pub async fn repos(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let q = r.q("q").unwrap_or("").to_lowercase();
    let list = r
        .st
        .db(move |c| {
            let mut st = c.prepare(&format!("{REPO_SELECT} ORDER BY r.updated_at DESC"))?;
            let rows = st.query_map([], RepoRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut v = vec![];
            for repo in rows {
                if !q.is_empty() && !repo.full_id().to_lowercase().contains(&q) {
                    continue;
                }
                let mut card = crate::web::repo_card(&repo);
                card["head_sha"] = json!(repo.head_sha);
                v.push(card);
            }
            Ok(v)
        })
        .await?;
    let c = admin_ctx(r, &me, "repos", json!({ "repos": list, "q": r.q("q").unwrap_or("") }));
    render(&r.st, "admin/repos.html", c)
}

/// POST /admin/repos/delete
pub async fn delete_repo(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let f = form(r).await?;
    let full_id = f.get("repo").cloned().unwrap_or_default();
    let fi = full_id.clone();
    let repo = r
        .st
        .db(move |c| {
            let mut st = c.prepare(&format!("{REPO_SELECT} WHERE (a.name || '/' || r.name) = ?1"))?;
            Ok(st.query_row([fi], RepoRow::from_row).ok())
        })
        .await?
        .ok_or_else(|| ApiError::not_found("No such repository"))?;
    crate::api::repos::delete_repo(&r.st, &repo).await?;
    Ok(crate::web::redirect("/admin/repos"))
}

/// GET /admin/orgs
pub async fn orgs(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let base = r.base.clone();
    let list = r
        .st
        .db(move |c| {
            let mut st = c.prepare(
                "SELECT a.id, a.name, a.fullname, a.avatar, a.created_at,
                        (SELECT COUNT(*) FROM org_members m WHERE m.org_id = a.id),
                        (SELECT COUNT(*) FROM repos r WHERE r.owner_id = a.id)
                 FROM accounts a WHERE a.kind = 'org' ORDER BY a.name",
            )?;
            let v = st
                .query_map([], |row| {
                    let name: String = row.get(1)?;
                    Ok(json!({
                        "name": name, "fullname": row.get::<_, String>(2)?,
                        "avatar": format!("{base}/avatars/{name}.svg"),
                        "created_at": row.get::<_, String>(4)?,
                        "num_members": row.get::<_, i64>(5)?, "num_repos": row.get::<_, i64>(6)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    let c = admin_ctx(r, &me, "orgs", json!({ "orgs": list }));
    render(&r.st, "admin/orgs.html", c)
}

/// GET /admin/server
pub async fn server(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    if require_admin(&me)?.is_none() {
        return Ok(crate::web::login_redirect(r));
    }
    let cfg = &r.st.cfg;
    let db_size = std::fs::metadata(cfg.data.join("hub.db")).map(|m| m.len()).unwrap_or(0);
    let (repo_count, token_count, session_count) = r
        .st
        .db(|c| {
            let r: i64 = c.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))?;
            let t: i64 = c.query_row("SELECT COUNT(*) FROM tokens", [], |r| r.get(0))?;
            let s: i64 = c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
            Ok((r, t, s))
        })
        .await?;
    let c = admin_ctx(
        r,
        &me,
        "server",
        json!({
            "info": {
                "version": env!("CARGO_PKG_VERSION"),
                "bind": cfg.bind,
                "data_dir": cfg.data.display().to_string(),
                "public_url": cfg.public_url.clone().unwrap_or_else(|| r.base.clone()),
                "storage": r.st.storage.describe(),
                "lfs_threshold": crate::util::human_size(cfg.lfs_threshold),
                "open_signup": cfg.open_signup,
                "git": cfg.git,
                "db_size": crate::util::human_size(db_size as i64),
                "repo_count": repo_count,
                "token_count": token_count,
                "session_count": session_count,
                "site_name": cfg.site_name,
            }
        }),
    );
    render(&r.st, "admin/server.html", c)
}


