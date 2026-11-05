//! whoami, user/org overviews, followers, members, likes, tokens.

use crate::error::{ApiError, ApiResult};
use crate::models::{load_repo, repo_access, Access, Account, Caller, RepoRow, ACCOUNT_COLS, REPO_SELECT};
use crate::routes::{json, Req};
use crate::util::now_iso;
use axum::http::Method;
use axum::response::Response;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub async fn whoami(r: &mut Req) -> ApiResult<Response> {
    let caller = r.require().await?;
    let base = r.base.clone();
    let c2 = caller.clone();
    let v = r
        .st
        .db(move |c| {
            let orgs = Account::orgs_of(c, c2.user.id)?;
            let u = &c2.user;
            let orgs_json: Vec<Value> = orgs
                .iter()
                .map(|(o, role)| {
                    json!({
                        "type": "org", "id": format!("{:024x}", o.id), "name": o.name, "fullname": o.display_name(),
                        "email": Value::Null, "canPay": false, "avatarUrl": o.avatar_url(&base), "roleInOrg": role,
                        "isEnterprise": false, "periodEnd": Value::Null,
                    })
                })
                .collect();
            let mut v = json!({
                "type": "user",
                "id": format!("{:024x}", u.id),
                "name": u.name,
                "fullname": u.display_name(),
                "email": u.email,
                "emailVerified": true,
                "canPay": false,
                "periodEnd": Value::Null,
                "isPro": false,
                "avatarUrl": u.avatar_url(&base),
                "orgs": orgs_json,
                "isAdmin": u.is_admin,
            });
            v["auth"] = match &c2.token_name {
                Some(name) => json!({
                    "type": "access_token",
                    "accessToken": { "displayName": name, "role": c2.role, "createdAt": u.created_at },
                }),
                None => json!({ "type": "session" }),
            };
            Ok(v)
        })
        .await?;
    Ok(json(v))
}

fn count(c: &Connection, sql: &str, id: i64) -> rusqlite::Result<i64> {
    c.query_row(sql, [id], |r| r.get(0))
}

pub fn overview_json(c: &Connection, a: &Account, base: &str, viewer: Option<&Caller>) -> rusqlite::Result<Value> {
    let n = |t: &str| -> rusqlite::Result<i64> {
        c.query_row("SELECT COUNT(*) FROM repos WHERE owner_id = ?1 AND type = ?2 AND private = 0", params![a.id, t], |r| r.get(0))
    };
    let followers = count(c, "SELECT COUNT(*) FROM follows WHERE target_id = ?1", a.id)?;
    let following = viewer
        .map(|v| {
            c.query_row(
                "SELECT COUNT(*) FROM follows WHERE follower_id = ?1 AND target_id = ?2",
                params![v.user.id, a.id],
                |r| r.get::<_, i64>(0),
            )
        })
        .transpose()?
        .map(|x| x > 0);
    let mut v = json!({
        "_id": format!("{:024x}", a.id),
        "avatarUrl": a.avatar_url(base),
        "fullname": a.display_name(),
        "name": a.name,
        "type": a.kind,
        "isFollowing": following,
        "numModels": n("model")?,
        "numDatasets": n("dataset")?,
        "numSpaces": n("space")?,
        "numFollowers": followers,
        "details": a.bio,
        "createdAt": a.created_at,
    });
    if a.kind == "user" {
        v["user"] = json!(a.name);
        v["isPro"] = json!(false);
        v["numLikes"] = json!(count(c, "SELECT COUNT(*) FROM likes WHERE user_id = ?1", a.id)?);
        v["numFollowing"] = json!(count(c, "SELECT COUNT(*) FROM follows WHERE follower_id = ?1", a.id)?);
        v["numDiscussions"] = json!(count(c, "SELECT COUNT(*) FROM discussions WHERE author_id = ?1", a.id)?);
        v["numPapers"] = json!(0);
        v["numUpvotes"] = json!(0);
        let orgs: Vec<Value> = Account::orgs_of(c, a.id)?
            .into_iter()
            .map(|(o, _)| json!({ "name": o.name, "fullname": o.display_name(), "avatarUrl": o.avatar_url(base) }))
            .collect();
        v["orgs"] = json!(orgs);
    } else {
        v["numUsers"] = json!(count(c, "SELECT COUNT(*) FROM org_members WHERE org_id = ?1", a.id)?);
        v["isVerified"] = json!(false);
        v["isEnterprise"] = json!(false);
        v["numPapers"] = json!(0);
    }
    Ok(v)
}

fn user_mini(a: &Account, base: &str) -> Value {
    json!({ "user": a.name, "fullname": a.display_name(), "avatarUrl": a.avatar_url(base), "type": a.kind, "isPro": false })
}

async fn account(r: &Req, name: &str, kind: Option<&'static str>) -> ApiResult<Account> {
    let n = name.to_string();
    let a = r.st.db(move |c| Account::by_name(c, &n)).await?;
    match a {
        Some(a) if kind.is_none_or(|k| a.kind == k) => Ok(a),
        _ => Err(ApiError::not_found(format!("{name} does not exist"))),
    }
}

fn list_accounts(c: &Connection, sql: &str, id: i64) -> rusqlite::Result<Vec<Account>> {
    let mut st = c.prepare(sql)?;
    let v = st.query_map([id], Account::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

fn prefixed_cols(p: &str) -> String {
    ACCOUNT_COLS.split(", ").map(|c| format!("{p}.{c}")).collect::<Vec<_>>().join(", ")
}

pub async fn user_route(r: &mut Req, name: &str, rest: &[&str]) -> ApiResult<Response> {
    let viewer = r.caller().await?;
    match (r.method.clone(), rest) {
        (Method::GET, ["overview"]) => {
            let a = account(r, name, Some("user")).await?;
            let base = r.base.clone();
            Ok(json(r.st.db(move |c| overview_json(c, &a, &base, viewer.as_ref())).await?))
        }
        (Method::GET, ["followers"]) | (Method::GET, ["following"]) => {
            let a = account(r, name, None).await?;
            let sql = if rest[0] == "followers" {
                format!("SELECT {} FROM accounts a JOIN follows f ON f.follower_id = a.id WHERE f.target_id = ?1 ORDER BY f.created_at DESC", prefixed_cols("a"))
            } else {
                format!("SELECT {} FROM accounts a JOIN follows f ON f.target_id = a.id WHERE f.follower_id = ?1 ORDER BY f.created_at DESC", prefixed_cols("a"))
            };
            let base = r.base.clone();
            let list = r.st.db(move |c| list_accounts(c, &sql, a.id)).await?;
            Ok(json(json!(list.iter().map(|a| user_mini(a, &base)).collect::<Vec<_>>())))
        }
        (Method::POST, ["follow"]) | (Method::DELETE, ["follow"]) => {
            let me = viewer.ok_or_else(ApiError::unauthorized)?;
            let a = account(r, name, None).await?;
            if a.id == me.user.id {
                return Err(ApiError::bad("You cannot follow yourself"));
            }
            let on = r.method == Method::POST;
            r.st.db(move |c| {
                if on {
                    c.execute("INSERT OR IGNORE INTO follows (follower_id, target_id, created_at) VALUES (?1, ?2, ?3)", params![me.user.id, a.id, now_iso()])
                } else {
                    c.execute("DELETE FROM follows WHERE follower_id = ?1 AND target_id = ?2", params![me.user.id, a.id])
                }
            })
            .await?;
            Ok(json(json!({})))
        }
        (Method::GET, ["likes"]) => {
            let a = account(r, name, Some("user")).await?;
            let base = r.base.clone();
            let v = r
                .st
                .db(move |c| {
                    let mut st = c.prepare(&format!(
                        "{REPO_SELECT} JOIN likes lk ON lk.repo_id = r.id WHERE lk.user_id = ?1 ORDER BY lk.created_at DESC"
                    ))?;
                    let repos = st.query_map([a.id], RepoRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
                    let mut out = vec![];
                    for repo in repos {
                        if repo_access(c, &repo, viewer.as_ref())? == Access::None {
                            continue;
                        }
                        out.push(json!({
                            "createdAt": repo.created_at,
                            "repo": { "name": repo.full_id(), "type": repo.rtype },
                            "repoData": crate::service::repo_json(&repo, &base, None, None, false),
                        }));
                    }
                    Ok(out)
                })
                .await?;
            Ok(json(json!(v)))
        }
        _ => Err(ApiError::not_found("Not found")),
    }
}

pub async fn org_route(r: &mut Req, name: &str, rest: &[&str]) -> ApiResult<Response> {
    let viewer = r.caller().await?;
    match (r.method.clone(), rest) {
        (Method::GET, ["overview"]) => {
            let a = account(r, name, Some("org")).await?;
            let base = r.base.clone();
            Ok(json(r.st.db(move |c| overview_json(c, &a, &base, viewer.as_ref())).await?))
        }
        (Method::GET, ["members"]) => {
            let a = account(r, name, Some("org")).await?;
            let base = r.base.clone();
            let sql = format!(
                "SELECT {}, m.role FROM accounts a JOIN org_members m ON m.user_id = a.id WHERE m.org_id = ?1 ORDER BY a.name",
                prefixed_cols("a")
            );
            let v = r
                .st
                .db(move |c| {
                    let mut st = c.prepare(&sql)?;
                    let rows = st.query_map([a.id], |row| Ok((Account::from_row(row)?, row.get::<_, String>(10)?)))?;
                    let mut out = vec![];
                    for row in rows {
                        let (u, role) = row?;
                        let mut m = user_mini(&u, &base);
                        m["role"] = json!(role);
                        out.push(m);
                    }
                    Ok(out)
                })
                .await?;
            Ok(json(json!(v)))
        }
        (Method::GET, ["followers"]) => user_route(r, name, rest).await,
        _ => Err(ApiError::not_found("Not found")),
    }
}

pub async fn like(r: &mut Req, rtype: &str, ns: &str, name: &str, on: bool) -> ApiResult<Response> {
    let caller = r.require().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, Some(&caller), Access::Read).await?;
    r.st.db(move |c| {
        if on {
            c.execute("INSERT OR IGNORE INTO likes (user_id, repo_id, created_at) VALUES (?1, ?2, ?3)", params![caller.user.id, repo.id, now_iso()])
        } else {
            c.execute("DELETE FROM likes WHERE user_id = ?1 AND repo_id = ?2", params![caller.user.id, repo.id])
        }
    })
    .await?;
    Ok(json(json!({})))
}

pub async fn likers(r: &mut Req, rtype: &str, ns: &str, name: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let base = r.base.clone();
    let sql = format!("SELECT {} FROM accounts a JOIN likes l ON l.user_id = a.id WHERE l.repo_id = ?1 ORDER BY l.created_at DESC", prefixed_cols("a"));
    let list = r.st.db(move |c| list_accounts(c, &sql, repo.id)).await?;
    Ok(json(json!(list.iter().map(|a| user_mini(a, &base)).collect::<Vec<_>>())))
}

/// Minimal token management API (`/api/settings/tokens`), handy for scripts.
pub async fn tokens_route(r: &mut Req, rest: &[&str]) -> ApiResult<Response> {
    let caller = r.require().await?;
    if caller.token_name.is_some() && caller.role != "write" {
        return Err(ApiError::forbidden("Write token required"));
    }
    match (r.method.clone(), rest) {
        (Method::GET, []) => {
            let uid = caller.user.id;
            let v = r
                .st
                .db(move |c| {
                    let mut st = c.prepare("SELECT id, name, prefix, role, created_at, last_used FROM tokens WHERE user_id = ?1 ORDER BY id DESC")?;
                    let v = st
                        .query_map([uid], |r| {
                            Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "prefix": r.get::<_, String>(2)?,
                                "role": r.get::<_, String>(3)?, "createdAt": r.get::<_, String>(4)?, "lastUsedAt": r.get::<_, Option<String>>(5)? }))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(v)
                })
                .await?;
            Ok(json(json!(v)))
        }
        (Method::POST, []) => {
            let b: Value = r.json().await?;
            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("token").to_string();
            let role = match b.get("role").and_then(|v| v.as_str()) { Some("read") => "read", _ => "write" };
            let uid = caller.user.id;
            let tok = r.st.db(move |c| crate::auth::new_token(c, uid, &name, role)).await?;
            Ok(json(json!({ "token": tok, "role": role })))
        }
        (Method::DELETE, [id]) => {
            let id: i64 = id.parse().map_err(|_| ApiError::bad("Invalid id"))?;
            let uid = caller.user.id;
            r.st.db(move |c| c.execute("DELETE FROM tokens WHERE id = ?1 AND user_id = ?2", params![id, uid])).await?;
            Ok(json(json!({})))
        }
        _ => Err(ApiError::not_found("Not found")),
    }
}
