//! Login, sign-up, logout, settings, token management, org & repo creation.

use crate::auth::{hash_password, new_session, new_token, verify_password, SESSION_COOKIE};
use crate::error::{ApiError, ApiResult};
use crate::models::{Account, Caller};
use crate::routes::Req;
use crate::util::{now_iso, valid_account_name, valid_name};
use crate::web::{base_ctx, ctx, form, render, render_status};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use minijinja::context;
use rusqlite::params;
use serde_json::{json, Value};

fn with_cookie(mut resp: Response, sid: &str, clear: bool) -> Response {
    let val = if clear {
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
    } else {
        format!("{SESSION_COOKIE}={sid}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}", 60 * 60 * 24 * 90)
    };
    if let Ok(h) = HeaderValue::from_str(&val) {
        resp.headers_mut().append(header::SET_COOKIE, h);
    }
    resp
}

fn safe_next(n: &str) -> String {
    if n.starts_with('/') && !n.starts_with("//") {
        n.to_string()
    } else {
        "/".into()
    }
}

pub async fn login(r: &mut Req) -> ApiResult<Response> {
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let user = f.get("username").cloned().unwrap_or_default();
        let pass = f.get("password").cloned().unwrap_or_default();
        let next = safe_next(&f.get("next").cloned().unwrap_or_default());
        let u = {
            let user = user.clone();
            r.st.db(move |c| {
                Ok(c.query_row(
                    "SELECT id, password FROM accounts WHERE name = ?1 AND kind = 'user'",
                    [user],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .ok())
            })
            .await?
        };
        match u {
            Some((id, Some(hash))) if verify_password(&pass, &hash) => {
                let sid = r.st.db(move |c| new_session(c, id)).await?;
                return Ok(with_cookie(crate::web::redirect(&next), &sid, false));
            }
            _ => {
                let ctx = ctx(r, &None, &[json!({ "title": "Log In", "heading": "Log In", "action": "/login", "button": "Log In", "error": "Invalid username or password", "username": user, "next": next, "open_signup": r.st.cfg.open_signup, "username_fixed": false, "info": Value::Null })]);
                return render_status(&r.st, StatusCode::UNAUTHORIZED, "auth.html", ctx);
            }
        }
    }
    let next = r.q("next").unwrap_or("").to_string();
    let ctx = ctx(r, &None, &[json!({ "title": "Log In", "heading": "Log In", "action": "/login", "button": "Log In", "error": Value::Null, "username": "", "next": next, "username_fixed": false, "info": Value::Null })]);
    render(&r.st, "auth.html", ctx)
}

pub async fn join(r: &mut Req) -> ApiResult<Response> {
    if !r.st.cfg.open_signup {
        return Err(ApiError::forbidden("Sign-up is disabled on this instance"));
    }
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let username = f.get("username").cloned().unwrap_or_default();
        let password = f.get("password").cloned().unwrap_or_default();
        let fullname = f.get("fullname").cloned().unwrap_or_default();
        let email = f.get("email").cloned().unwrap_or_default();
        let err = |msg: &str| ctx(r, &None, &[json!({ "title": "Sign Up", "heading": "Create your account", "action": "/join", "button": "Create account", "error": msg, "username": username.clone(), "next": "", "username_fixed": false, "info": Value::Null })]);
        if !valid_account_name(&username) {
            return render_status(&r.st, StatusCode::BAD_REQUEST, "auth.html", err("Invalid username"));
        }
        if password.len() < 8 {
            return render_status(&r.st, StatusCode::BAD_REQUEST, "auth.html", err("Password must be at least 8 characters"));
        }
        let hash = hash_password(&password);
        let (u, f, e) = (username.clone(), fullname.clone(), email.clone());
        let created = r
            .st
            .db(move |c| {
                if Account::by_name(c, &u)?.is_some() {
                    return Ok(None);
                }
                let first: i64 = c.query_row("SELECT COUNT(*) FROM accounts WHERE kind = 'user'", [], |r| r.get(0))?;
                c.execute(
                    "INSERT INTO accounts (name, kind, fullname, email, password, created_at, is_admin) VALUES (?1, 'user', ?2, ?3, ?4, ?5, ?6)",
                    params![u, f, if e.is_empty() { None } else { Some(e) }, hash, now_iso(), (first == 0) as i64],
                )?;
                Ok(Some(c.last_insert_rowid()))
            })
            .await?;
        let Some(id) = created else {
            return render_status(&r.st, StatusCode::CONFLICT, "auth.html", err("This username is taken"));
        };
        let sid = r.st.db(move |c| new_session(c, id)).await?;
        return Ok(with_cookie(crate::web::redirect("/"), &sid, false));
    }
    let ctx = ctx(r, &None, &[json!({ "title": "Sign Up", "heading": "Create your account", "action": "/join", "button": "Create account", "error": Value::Null, "username": "", "next": "", "username_fixed": false, "info": Value::Null })]);
    render(&r.st, "auth.html", ctx)
}

pub async fn logout(r: &mut Req) -> ApiResult<Response> {
    if let Some(sid) = crate::auth::cookie(&r.headers, SESSION_COOKIE) {
        let h = crate::util::sha256_hex(sid.as_bytes());
        r.st.db(move |c| c.execute("DELETE FROM sessions WHERE id = ?1", [h])).await?;
    }
    Ok(with_cookie(crate::web::redirect("/"), "", true))
}

pub async fn docs(r: &mut Req) -> ApiResult<Response> {
    let me = r.web_caller().await;
    let ctx = ctx(r, &me, &[json!({ "title": "Docs" })]);
    render(&r.st, "docs.html", ctx)
}

fn need_user(r: &Req, me: Option<Caller>) -> ApiResult<Caller> {
    match me {
        Some(c) if c.can_write_token() => Ok(c),
        Some(_) => Err(ApiError::forbidden("A write token is required")),
        None => Err(ApiError::unauthorized()),
    }
}

pub async fn settings_profile(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    let caller = match me.clone() {
        Some(c) => c,
        None => return Ok(crate::web::login_redirect(r)),
    };
    let mut flash = None;
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let fullname = f.get("fullname").cloned().unwrap_or_default();
        let email = f.get("email").cloned().unwrap_or_default();
        let website = f.get("website").cloned().unwrap_or_default();
        let bio = f.get("bio").cloned().unwrap_or_default();
        let id = caller.user.id;
        r.st.db(move |c| {
            c.execute(
                "UPDATE accounts SET fullname = ?1, email = ?2, website = ?3, bio = ?4 WHERE id = ?5",
                params![fullname, if email.is_empty() { None } else { Some(email) }, website, bio, id],
            )?;
            Ok(())
        })
        .await?;
        flash = Some("Profile updated");
    }
    let (uid, base) = (caller.user.id, r.base.clone());
    let user = r.st.db(move |c| Account::by_id(c, uid)).await?.ok_or_else(|| ApiError::not_found("User not found"))?;
    let user_v = json!({
            "fullname": user.fullname, "email": user.email, "website": user.website, "bio": user.bio });
    let ctx = ctx(r, &me, &[json!({ "title": "Profile settings", "section": "profile", "flash": flash, "user": user_v, "base": base })]);
    render(&r.st, "settings/profile.html", ctx)
}

pub async fn settings_password(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    let caller = match me.clone() {
        Some(c) => c,
        None => return Ok(crate::web::login_redirect(r)),
    };
    let mut flash = None;
    let mut error = None;
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let cur = f.get("current").cloned().unwrap_or_default();
        let new = f.get("password").cloned().unwrap_or_default();
        let uid = caller.user.id;
        let hash: Option<String> = r
            .st
            .db(move |c| Ok(c.query_row("SELECT password FROM accounts WHERE id = ?1", [uid], |r| r.get(0)).ok()))
            .await?
            .flatten();
        if !hash.as_deref().is_some_and(|h| verify_password(&cur, h)) {
            error = Some("Current password is incorrect");
        } else if new.len() < 8 {
            error = Some("New password must be at least 8 characters");
        } else {
            let h = hash_password(&new);
            r.st.db(move |c| c.execute("UPDATE accounts SET password = ?1 WHERE id = ?2", params![h, uid])).await?;
            flash = Some("Password updated");
        }
    }
    let ctx = ctx(r, &me, &[json!({ "title": "Password", "section": "password", "flash": flash, "error": error })]);
    render(&r.st, "settings/password.html", ctx)
}

pub async fn settings_tokens(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    let caller = match me.clone() {
        Some(c) => c,
        None => return Ok(crate::web::login_redirect(r)),
    };
    let mut new_token_value = None;
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let name = f.get("name").cloned().unwrap_or_else(|| "token".into());
        let role = f.get("role").cloned().unwrap_or_else(|| "write".into());
        let uid = caller.user.id;
        new_token_value = Some(r.st.db(move |c| new_token(c, uid, &name, &role)).await?);
    }
    let uid = caller.user.id;
    let tokens = r
        .st
        .db(move |c| {
            let mut st = c.prepare("SELECT id, name, prefix, role, created_at, last_used FROM tokens WHERE user_id = ?1 ORDER BY id DESC")?;
            let v = st
                .query_map([uid], |r| {
                    Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "prefix": r.get::<_, String>(2)?,
                        "role": r.get::<_, String>(3)?, "created_at": r.get::<_, String>(4)?, "last_used": r.get::<_, Option<String>>(5)? }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    let ctx = ctx(r, &me, &[json!({ "title": "Access tokens", "section": "tokens", "tokens": tokens, "new_token": new_token_value, "flash": Value::Null })]);
    render(&r.st, "settings/tokens.html", ctx)
}

pub async fn delete_token(r: &mut Req, me: Option<Caller>, id: &str) -> ApiResult<Response> {
    let caller = need_user(r, me)?;
    let id: i64 = id.parse().map_err(|_| ApiError::bad("Invalid id"))?;
    let uid = caller.user.id;
    r.st.db(move |c| c.execute("DELETE FROM tokens WHERE id = ?1 AND user_id = ?2", params![id, uid])).await?;
    Ok(crate::web::redirect("/settings/tokens"))
}

pub async fn orgs_list(r: &mut Req, me: Option<Caller>) -> ApiResult<Response> {
    let Some(caller) = me.clone() else { return Ok(crate::web::login_redirect(r)) };
    let (uid, base) = (caller.user.id, r.base.clone());
    let orgs = r
        .st
        .db(move |c| {
            let mut st = c.prepare(
                "SELECT a.id, a.name, a.fullname, a.avatar, m.role,
                   (SELECT COUNT(*) FROM org_members m2 WHERE m2.org_id = a.id)
                 FROM accounts a JOIN org_members m ON m.org_id = a.id WHERE m.user_id = ?1 ORDER BY a.name",
            )?;
            let v = st
                .query_map([uid], |row| {
                    let name: String = row.get(1)?;
                    Ok(json!({ "name": name, "fullname": row.get::<_, String>(2)?,
                        "avatar": format!("{base}/avatars/{name}.svg"), "role": row.get::<_, String>(4)?,
                        "num_members": row.get::<_, i64>(5)? }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    let ctx = ctx(r, &me, &[json!({ "title": "Organizations", "orgs": orgs })]);
    render(&r.st, "orgs.html", ctx)
}

/// GET/POST /new, /new-dataset, /new-space — create a repo.
pub async fn new_repo(r: &mut Req, rtype: &str) -> ApiResult<Response> {
    let Some(caller) = me_of(r).await else { return Ok(crate::web::login_redirect(r)) };
    let me = Some(caller.clone());
    let (uid, base) = (caller.user.id, r.base.clone());
    let my_orgs = r
        .st
        .db(move |c| {
            let mut st = c.prepare(
                "SELECT a.name FROM accounts a JOIN org_members m ON m.org_id = a.id WHERE m.user_id = ?1 AND m.role IN ('admin','write') ORDER BY a.name",
            )?;
            let v = st.query_map([uid], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    let label = match rtype { "dataset" => "dataset", "space" => "Space", _ => "model" };
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let name = f.get("name").cloned().unwrap_or_default();
        let owner = f.get("owner").cloned().unwrap_or_else(|| caller.user.name.clone());
        let private = f.get("visibility").map(|v| v == "private").unwrap_or(false);
        let sdk = f.get("sdk").cloned();
        let err = |msg: &str| ctx(r, &me, &[json!({ "title": format!("New {label}"), "heading": format!("Create a new {label}"), "action": "", "rtype": rtype, "error": msg, "owner_choice": true, "my_orgs": my_orgs.clone(), "org_name": Value::Null, "button": "Create", "me": Some(caller_view(&caller, &base)) })]);
        if !valid_name(&name) {
            return render_status(&r.st, StatusCode::BAD_REQUEST, "new.html", err("Invalid name: use letters, digits, '-', '_', '.'"));
        }
        let (o, n, c2) = (owner.clone(), name.clone(), caller.clone());
        let owner_acc = r
            .st
            .db(move |c| {
                let Some(acc) = Account::by_name(c, &o)? else { return Ok(None) };
                Ok(crate::models::can_write_namespace(c, &acc, &c2)?.then_some(acc))
            })
            .await?;
        let Some(owner_acc) = owner_acc else {
            return render_status(&r.st, StatusCode::FORBIDDEN, "new.html", err("You can't create repos in this namespace"));
        };
        let license = f.get("license").cloned().unwrap_or_default();
        match crate::service::create_repo(&r.st, rtype, &owner_acc, &name, private, sdk, &caller.user).await {
            Ok(repo) => {
                if !license.is_empty() {
                    let readme = format!("---\nlicense: {license}\n---\n");
                    let git = r.st.git(&repo.disk_id);
                    if let Ok(c) = git
                        .build_commit(
                            repo.head_sha.as_deref(),
                            &[crate::git::write::Op::Put { path: "README.md".into(), data: readme.into_bytes() }],
                            "Add license metadata",
                            &crate::service::signature(&caller.user),
                        )
                        .await
                    {
                        let _ = git.update_ref("refs/heads/main", &c, repo.head_sha.as_deref()).await;
                    }
                    let _ = crate::service::refresh_meta(&r.st, &repo).await;
                }
                return Ok(crate::web::redirect(&format!("/{}", repo.web_path())));
            }
            Err(e) if e.status == StatusCode::CONFLICT => {
                return render_status(&r.st, StatusCode::CONFLICT, "new.html", err("You already created this repo"));
            }
            Err(e) => return Err(e),
        }
    }
    let ctx = ctx(r, &me, &[json!({ "title": format!("New {label}"), "heading": format!("Create a new {label}"), "action": "", "rtype": rtype, "error": Value::Null, "owner_choice": true, "my_orgs": my_orgs, "org_name": Value::Null, "button": "Create", "me": Some(caller_view(&caller, &base)) })]);
    render(&r.st, "new.html", ctx)
}

pub fn caller_view(c: &Caller, base: &str) -> serde_json::Value {
    json!({ "name": c.user.name, "fullname": c.user.display_name(), "avatar": c.user.avatar_url(base), "is_admin": c.user.is_admin })
}

async fn me_of(r: &Req) -> Option<Caller> {
    r.web_caller().await
}

/// GET/POST /organizations/new
pub async fn new_org(r: &mut Req) -> ApiResult<Response> {
    let Some(caller) = me_of(r).await else { return Ok(crate::web::login_redirect(r)) };
    let me = Some(caller.clone());
    let base = r.base.clone();
    if r.method == axum::http::Method::POST {
        let f = form(r).await?;
        let name = f.get("name").cloned().unwrap_or_default();
        let err = |msg: &str| ctx(r, &me, &[json!({ "title": "New organization", "heading": "Create a new organization", "action": "/organizations/new", "rtype": Value::Null, "error": msg, "owner_choice": false, "my_orgs": Vec::<String>::new(), "org_name": Value::Null, "button": "Create organization", "me": Some(caller_view(&caller, &base)) })]);
        if !valid_account_name(&name) {
            return render_status(&r.st, StatusCode::BAD_REQUEST, "new.html", err("Invalid organization name"));
        }
        let (n, uid) = (name.clone(), caller.user.id);
        let ok = r
            .st
            .db(move |c| {
                if Account::by_name(c, &n)?.is_some() {
                    return Ok(false);
                }
                c.execute(
                    "INSERT INTO accounts (name, kind, fullname, created_at) VALUES (?1, 'org', ?2, ?3)",
                    params![n, n, now_iso()],
                )?;
                let oid = c.last_insert_rowid();
                c.execute("INSERT INTO org_members (org_id, user_id, role) VALUES (?1, ?2, 'admin')", params![oid, uid])?;
                Ok(true)
            })
            .await?;
        if !ok {
            return render_status(&r.st, StatusCode::CONFLICT, "new.html", err("This name is already taken"));
        }
        return Ok(crate::web::redirect(&format!("/{name}")));
    }
    let ctx = ctx(r, &me, &[json!({ "title": "New organization", "heading": "Create a new organization", "action": "/organizations/new", "rtype": Value::Null, "error": Value::Null, "owner_choice": false, "my_orgs": Vec::<String>::new(), "org_name": Value::Null, "button": "Create organization", "me": Some(caller_view(&caller, &base)) })]);
    render(&r.st, "new.html", ctx)
}

