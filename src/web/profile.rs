//! User and organization profile pages.

use crate::error::{ApiError, ApiResult};
use crate::models::{repo_access, Access, Account, Caller, RepoRow, REPO_SELECT};
use crate::routes::Req;
use crate::web::{base_ctx, ctx, render};
use minijinja::context;
use serde_json::json;

pub async fn profile(r: &mut Req, name: &str) -> ApiResult<axum::response::Response> {
    let me = r.web_caller().await;
    let n = name.to_string();
    let acc = r.st.db(move |c| Account::by_name(c, &n)).await?;
    let Some(u) = acc else { return Ok(crate::web::not_found(r).await) };
    let (uid, me2) = (u.id, me.clone());
    let repos: Vec<RepoRow> = r
        .st
        .db(move |c| {
            let mut st = c.prepare(&format!("{REPO_SELECT} WHERE r.owner_id = ?1 ORDER BY r.updated_at DESC"))?;
            let rows = st.query_map([uid], RepoRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = vec![];
            for repo in rows {
                if repo_access(c, &repo, me2.as_ref())? != Access::None {
                    out.push(repo);
                }
            }
            Ok(out)
        })
        .await?;
    let cards = |t: &str| -> Vec<serde_json::Value> { repos.iter().filter(|x| x.rtype == t).map(crate::web::repo_card).collect() };
    let (models, datasets, spaces) = (cards("model"), cards("dataset"), cards("space"));
    let (uid, me2, base) = (u.id, me.clone(), r.base.clone());
    let (num_followers, num_following, following, orgs, members) = r
        .st
        .db(move |c| {
            let followers: i64 = c.query_row("SELECT COUNT(*) FROM follows WHERE target_id = ?1", [uid], |r| r.get(0))?;
            let following: i64 = c.query_row("SELECT COUNT(*) FROM follows WHERE follower_id = ?1", [uid], |r| r.get(0))?;
            let is_following = match &me2 {
                Some(v) => c
                    .query_row(
                        "SELECT 1 FROM follows WHERE follower_id = ?1 AND target_id = ?2",
                        rusqlite::params![v.user.id, uid],
                        |_| Ok(()),
                    )
                    .is_ok(),
                None => false,
            };
            let orgs: Vec<serde_json::Value> = Account::orgs_of(c, uid)?
                .into_iter()
                .map(|(o, _)| json!({ "name": o.name, "fullname": o.display_name(), "avatar": o.avatar_url(&base) }))
                .collect();
            let mut members = vec![];
            let mut st = c.prepare(
                "SELECT a.id, a.name, a.kind, a.fullname, a.email, a.bio, a.website, a.avatar, a.is_admin, a.created_at, m.role
                 FROM accounts a JOIN org_members m ON m.user_id = a.id WHERE m.org_id = ?1 ORDER BY a.name",
            )?;
            let rows = st.query_map([uid], |row| Ok((Account::from_row(row)?, row.get::<_, String>(10)?)))?;
            for row in rows {
                let (a, role) = row?;
                members.push(json!({ "name": a.name, "fullname": a.display_name(), "avatar": a.avatar_url(&base), "role": role }));
            }
            Ok((followers, following, is_following, orgs, members))
        })
        .await?;
    let can_admin_org = match &me {
        Some(c) => c.user.id == u.id || c.user.is_admin,
        None => false,
    };
    let avatar = u.avatar_url(&r.base);
    let u_v = json!({ "name": u.name, "fullname": u.display_name(), "kind": u.kind,
        "bio": u.bio, "website": u.website, "avatar": avatar });
    let ctx = ctx(r, &me, &[json!({ "title": u.display_name(), "u": u_v, "models": models, "datasets": datasets, "spaces": spaces, "num_followers": num_followers, "num_following": num_following, "following": following, "orgs": orgs, "members": members, "can_admin_org": can_admin_org })]);
    render(&r.st, "profile.html", ctx)
}

/// POST /{user}/follow
pub async fn follow(r: &mut Req, name: &str) -> ApiResult<axum::response::Response> {
    let Some(caller) = r.web_caller().await else { return Ok(crate::web::login_redirect(r)) };
    let n = name.to_string();
    let target = r.st.db(move |c| Account::by_name(c, &n)).await?.ok_or_else(|| ApiError::not_found("User not found"))?;
    if target.id != caller.user.id {
        let (f, t) = (caller.user.id, target.id);
        r.st.db(move |c| {
            let has = c.query_row("SELECT 1 FROM follows WHERE follower_id = ?1 AND target_id = ?2", rusqlite::params![f, t], |_| Ok(())).is_ok();
            if has {
                c.execute("DELETE FROM follows WHERE follower_id = ?1 AND target_id = ?2", rusqlite::params![f, t])?;
            } else {
                c.execute("INSERT OR IGNORE INTO follows (follower_id, target_id, created_at) VALUES (?1, ?2, ?3)", rusqlite::params![f, t, crate::util::now_iso()])?;
            }
            Ok(())
        })
        .await?;
    }
    Ok(crate::web::redirect(&format!("/{name}")))
}

/// POST /{org}/members
pub async fn add_member(r: &mut Req, name: &str) -> ApiResult<axum::response::Response> {
    let Some(caller) = r.web_caller().await else { return Ok(crate::web::login_redirect(r)) };
    let n = name.to_string();
    let org = r.st.db(move |c| Account::by_name(c, &n)).await?.ok_or_else(|| ApiError::not_found("Org not found"))?;
    let allowed = {
        let c2 = caller.clone();
        let oid = org.id;
        r.st.db(move |c| {
            Ok(c2.user.is_admin
                || c2.user.id == oid
                || matches!(Account::org_role(c, oid, c2.user.id)?.as_deref(), Some("admin")))
        })
        .await?
    };
    if !allowed {
        return Err(ApiError::forbidden("Only org admins can add members"));
    }
    let f = crate::web::form(r).await?;
    let user = f.get("user").cloned().unwrap_or_default();
    let role = f.get("role").cloned().unwrap_or_else(|| "read".into());
    if !matches!(role.as_str(), "admin" | "write" | "contributor" | "read") {
        return Err(ApiError::bad("Invalid role"));
    }
    let oid = org.id;
    r.st.db(move |c| {
        let Some(u) = Account::by_name(c, &user)? else { return Ok(Err(ApiError::not_found("No such user"))) };
        if u.kind != "user" {
            return Ok(Err(ApiError::bad("Can only add users")));
        }
        c.execute("INSERT OR REPLACE INTO org_members (org_id, user_id, role) VALUES (?1, ?2, ?3)", rusqlite::params![oid, u.id, role])?;
        Ok(Ok(()))
    })
    .await??;
    Ok(crate::web::redirect(&format!("/{name}")))
}
