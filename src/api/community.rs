//! Discussions & pull requests (`/api/{type}s/{repo}/discussions/...`).
//! PR commits live on `refs/pr/{num}`, exactly like on the Hub.

use crate::error::{ApiError, ApiResult};
use crate::models::{load_repo, Access, Account, Caller, RepoRow};
use crate::routes::{json, Req};
use crate::service;
use crate::state::AppState;
use crate::util::{now_iso, object_id};
use axum::http::Method;
use axum::response::Response;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct Disc {
    pub id: i64,
    pub num: i64,
    pub title: String,
    pub author: Option<Account>,
    pub is_pr: bool,
    pub status: String,
    pub target_ref: Option<String>,
    pub merge_oid: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub num_comments: i64,
}

const DISC_SELECT: &str = "SELECT d.id, d.num, d.title, d.author_id, d.is_pr, d.status, d.target_ref, d.merge_oid,
    d.created_at, d.updated_at, (SELECT COUNT(*) FROM events e WHERE e.discussion_id = d.id AND e.type = 'comment')
    FROM discussions d";

fn disc_from_row(c: &Connection, r: &rusqlite::Row) -> rusqlite::Result<Disc> {
    let author_id: Option<i64> = r.get(3)?;
    Ok(Disc {
        id: r.get(0)?,
        num: r.get(1)?,
        title: r.get(2)?,
        author: match author_id { Some(a) => Account::by_id(c, a)?, None => None },
        is_pr: r.get::<_, i64>(4)? != 0,
        status: r.get(5)?,
        target_ref: r.get(6)?,
        merge_oid: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        num_comments: r.get(10)?,
    })
}

pub fn list_discs(c: &Connection, repo_id: i64) -> rusqlite::Result<Vec<Disc>> {
    let mut st = c.prepare(&format!("{DISC_SELECT} WHERE d.repo_id = ?1 ORDER BY d.num DESC"))?;
    let mut rows = st.query([repo_id])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        out.push(disc_from_row(c, r)?);
    }
    Ok(out)
}

pub fn get_disc(c: &Connection, repo_id: i64, num: i64) -> rusqlite::Result<Option<Disc>> {
    let mut st = c.prepare(&format!("{DISC_SELECT} WHERE d.repo_id = ?1 AND d.num = ?2"))?;
    let mut rows = st.query(params![repo_id, num])?;
    match rows.next()? {
        Some(r) => Ok(Some(disc_from_row(c, r)?)),
        None => Ok(None),
    }
}

pub fn author_json(a: &Option<Account>, base: &str) -> Value {
    match a {
        Some(a) => json!({
            "_id": format!("{:024x}", a.id), "name": a.name, "fullname": a.display_name(), "avatarUrl": a.avatar_url(base),
            "type": "user", "isPro": false, "isHf": false, "isOwner": false, "isOrgMember": false,
        }),
        None => json!({ "name": "deleted", "type": "user" }),
    }
}

fn disc_summary(d: &Disc, repo: &RepoRow, base: &str) -> Value {
    json!({
        "num": d.num,
        "title": d.title,
        "status": d.status,
        "createdAt": d.created_at,
        "isPullRequest": d.is_pr,
        "numComments": d.num_comments,
        "pinned": false,
        "author": author_json(&d.author, base),
        "repo": { "name": repo.full_id(), "type": repo.rtype },
        "topReactions": [],
        "numReactionUsers": 0,
    })
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: String,
    pub etype: String,
    pub author: Option<Account>,
    pub data: Value,
    pub created_at: String,
}

pub fn events(c: &Connection, disc_id: i64) -> rusqlite::Result<Vec<Event>> {
    let mut st = c.prepare("SELECT id, type, author_id, data, created_at FROM events WHERE discussion_id = ?1 ORDER BY created_at, rowid")?;
    let rows = st.query_map([disc_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))
    })?;
    let mut out = vec![];
    for row in rows {
        let (id, etype, aid, data, created_at) = row?;
        out.push(Event {
            id,
            etype,
            author: match aid { Some(a) => Account::by_id(c, a)?, None => None },
            data: serde_json::from_str(&data).unwrap_or(json!({})),
            created_at,
        });
    }
    Ok(out)
}

pub fn event_json(e: &Event, base: &str) -> Value {
    json!({
        "id": e.id,
        "type": e.etype,
        "createdAt": e.created_at,
        "author": author_json(&e.author, base),
        "data": e.data,
    })
}

fn comment_data(raw: &str, author: &Account, hidden: bool, edited: bool, history: Vec<Value>, base: &str) -> Value {
    let latest = json!({
        "raw": raw,
        "html": crate::web::markdown::render(raw),
        "updatedAt": now_iso(),
        "author": author_json(&Some(author.clone()), base),
    });
    let mut hist = history;
    hist.insert(0, latest.clone());
    json!({ "edited": edited, "hidden": hidden, "latest": latest, "numEdits": hist.len() - 1, "history": hist, "editors": [author.name], "editorAvatarUrls": [], "reactions": [], "identifiedLanguage": { "language": "en", "probability": 1.0 } })
}

pub fn insert_event(c: &Connection, disc_id: i64, etype: &str, author_id: i64, data: &Value) -> rusqlite::Result<String> {
    let id = object_id();
    c.execute(
        "INSERT INTO events (id, discussion_id, type, author_id, data, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, disc_id, etype, author_id, data.to_string(), now_iso()],
    )?;
    c.execute("UPDATE discussions SET updated_at = ?1 WHERE id = ?2", params![now_iso(), disc_id])?;
    Ok(id)
}

fn event_by_id(c: &Connection, id: &str) -> rusqlite::Result<Option<Event>> {
    let row: Option<(String, Option<i64>, String, String)> = c
        .query_row("SELECT type, author_id, data, created_at FROM events WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((etype, aid, data, created_at)) = row else { return Ok(None) };
    Ok(Some(Event {
        id: id.to_string(),
        etype,
        author: match aid { Some(a) => Account::by_id(c, a)?, None => None },
        data: serde_json::from_str(&data).unwrap_or(json!({})),
        created_at,
    }))
}

/// Create a discussion (and its opening comment); returns its number.
pub fn create_disc(c: &Connection, repo_id: i64, author: &Account, title: &str, description: &str, is_pr: bool, base: &str) -> rusqlite::Result<(i64, i64)> {
    let num: i64 = c.query_row("SELECT COALESCE(MAX(num), 0) + 1 FROM discussions WHERE repo_id = ?1", [repo_id], |r| r.get(0))?;
    let now = now_iso();
    c.execute(
        "INSERT INTO discussions (repo_id, num, title, author_id, is_pr, status, target_ref, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
        params![repo_id, num, title, author.id, is_pr as i64, if is_pr { "draft" } else { "open" }, if is_pr { Some("refs/heads/main") } else { None }, now],
    )?;
    let did = c.last_insert_rowid();
    insert_event(c, did, "comment", author.id, &comment_data(description, author, false, false, vec![], base))?;
    Ok((did, num))
}

/// Called by `commit?create_pr=1`: open a PR whose branch points at `commit`.
pub async fn open_pr_from_commit(st: &AppState, repo: &RepoRow, caller: &Caller, target: &str, commit: &str, title: &str, desc: &str) -> ApiResult<i64> {
    let (rid, user, t, d) = (repo.id, caller.user.clone(), title.to_string(), desc.to_string());
    let target_ref = if target.starts_with("refs/") { target.to_string() } else { format!("refs/heads/{target}") };
    let base = st.cfg.public_url.clone().unwrap_or_default();
    let (did, num) = st
        .db(move |c| {
            let (did, num) = create_disc(c, rid, &user, if t.trim().is_empty() { "Upload files" } else { t.trim() }, &d, true, &base)?;
            c.execute("UPDATE discussions SET status = 'open', target_ref = ?1 WHERE id = ?2", params![target_ref, did])?;
            Ok((did, num))
        })
        .await?;
    let git = st.git(&repo.disk_id);
    git.update_ref(&format!("refs/pr/{num}"), commit, None).await?;
    record_commit_event(st, &git, did, &caller.user, commit).await?;
    Ok(num)
}

async fn record_commit_event(st: &AppState, git: &crate::git::Repo, did: i64, author: &Account, oid: &str) -> ApiResult<()> {
    let subject = git.log(oid, None, 0, 1).await?.into_iter().next().map(|c| c.title).unwrap_or_default();
    let (aid, oid) = (author.id, oid.to_string());
    st.db(move |c| insert_event(c, did, "commit", aid, &json!({ "subject": subject, "oid": oid }))).await?;
    Ok(())
}

pub async fn route(r: &mut Req, rtype: &str, ns: &str, name: &str, rest: &[&str]) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, access) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    match (r.method.clone(), rest) {
        (Method::GET, []) => list(r, &repo).await,
        (Method::POST, []) => {
            let caller = caller.ok_or_else(ApiError::unauthorized)?;
            create(r, &repo, &caller).await
        }
        (Method::GET, [num]) => details(r, &repo, parse_num(num)?).await,
        (Method::DELETE, [num]) => {
            if access < Access::Admin {
                return Err(ApiError::forbidden("Only admins can delete discussions"));
            }
            let (rid, n) = (repo.id, parse_num(num)?);
            r.st.db(move |c| c.execute("DELETE FROM discussions WHERE repo_id = ?1 AND num = ?2", params![rid, n])).await?;
            let _ = r.st.git(&repo.disk_id).delete_ref(&format!("refs/pr/{n}")).await;
            Ok(json(json!({})))
        }
        (Method::POST, [num, action @ ..]) => {
            let caller = caller.ok_or_else(ApiError::unauthorized)?;
            if !caller.can_write_token() {
                return Err(ApiError::forbidden("A write token is required"));
            }
            let num = parse_num(num)?;
            let d = { let rid = repo.id; r.st.db(move |c| get_disc(c, rid, num)).await? }.ok_or_else(|| ApiError::not_found("Discussion not found"))?;
            mutate(r, &repo, access, &caller, &d, action).await
        }
        _ => Err(ApiError::not_found("Not found")),
    }
}

fn parse_num(s: &str) -> ApiResult<i64> {
    s.parse().map_err(|_| ApiError::bad("Invalid discussion number"))
}

async fn list(r: &mut Req, repo: &RepoRow) -> ApiResult<Response> {
    let rid = repo.id;
    let mut all = r.st.db(move |c| list_discs(c, rid)).await?;
    if let Some(t) = r.q("type") {
        match t {
            "pull_request" => all.retain(|d| d.is_pr),
            "discussion" => all.retain(|d| !d.is_pr),
            _ => {}
        }
    }
    if let Some(s) = r.q("status") {
        if s != "all" {
            all.retain(|d| d.status == s);
        }
    }
    if let Some(a) = r.q("author") {
        all.retain(|d| d.author.as_ref().is_some_and(|x| x.name.eq_ignore_ascii_case(a)));
    }
    if let Some(s) = r.q("search") {
        let s = s.to_lowercase();
        all.retain(|d| d.title.to_lowercase().contains(&s));
    }
    let per = 50usize;
    let page: usize = r.q("p").and_then(|v| v.parse().ok()).unwrap_or(0);
    let start = page * per;
    let items: Vec<Value> = all.iter().skip(start).take(per).map(|d| disc_summary(d, repo, &r.base)).collect();
    Ok(json(json!({ "discussions": items, "count": all.len(), "start": start, "numClosedDiscussions": all.iter().filter(|d| d.status != "open").count() })))
}

async fn create(r: &mut Req, repo: &RepoRow, caller: &Caller) -> ApiResult<Response> {
    if repo.discussions_disabled {
        return Err(ApiError::forbidden("Discussions are disabled for this repo"));
    }
    if !caller.can_write_token() {
        return Err(ApiError::forbidden("A write token is required"));
    }
    let b: Value = r.json().await?;
    let title = b.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if title.len() < 3 {
        return Err(ApiError::bad("Title must be at least 3 characters"));
    }
    let desc = b.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let is_pr = b.get("pullRequest").and_then(|v| v.as_bool()).unwrap_or(false);
    let (rid, user, base) = (repo.id, caller.user.clone(), r.base.clone());
    let (_, num) = r.st.db(move |c| create_disc(c, rid, &user, &title, &desc, is_pr, &base)).await?;
    if is_pr {
        // the PR starts as a draft pointing at main; pushes to refs/pr/N add commits
        let git = r.st.git(&repo.disk_id);
        if let Some(head) = git.resolve("main").await? {
            git.update_ref(&format!("refs/pr/{num}"), &head, None).await?;
        }
    }
    Ok(json(json!({ "num": num, "pullRequest": is_pr, "url": format!("{}/{}/discussions/{num}", r.base, repo.web_path()) })))
}

pub async fn details_json(st: &AppState, repo: &RepoRow, num: i64, base: &str, with_diff: bool) -> ApiResult<(Disc, Value)> {
    let rid = repo.id;
    let (d, evs) = st
        .db(move |c| {
            let Some(d) = get_disc(c, rid, num)? else { return Ok(None) };
            let evs = events(c, d.id)?;
            Ok(Some((d, evs)))
        })
        .await?
        .ok_or_else(|| ApiError::not_found("Discussion not found"))?;
    let mut v = disc_summary(&d, repo, base);
    v["events"] = json!(evs.iter().map(|e| event_json(e, base)).collect::<Vec<_>>());
    if d.is_pr {
        let git = st.git(&repo.disk_id);
        let target = d.target_ref.clone().unwrap_or_else(|| "refs/heads/main".into());
        let head = git.resolve(&format!("refs/pr/{num}")).await?;
        let base_sha = git.resolve(&target).await?;
        let mut conflicts: Vec<String> = vec![];
        let mut diff = String::new();
        if let (Some(h), Some(b)) = (&head, &base_sha) {
            let mb = git.merge_base(b, h).await?.unwrap_or_else(|| b.clone());
            if with_diff {
                diff = git.diff(&mb, h).await?;
            }
            if d.status == "open" || d.status == "draft" {
                if let Ok(Err(paths)) = git.merge_trees(b, h).await {
                    conflicts = paths;
                }
            }
        }
        v["changes"] = json!({ "base": target, "mergeCommitId": d.merge_oid, "headCommitId": head });
        v["filesWithConflicts"] = json!(conflicts);
        v["diff"] = if with_diff { json!(diff) } else { Value::Null };
        v["targetBranch"] = json!(target);
    }
    Ok((d, v))
}

async fn details(r: &mut Req, repo: &RepoRow, num: i64) -> ApiResult<Response> {
    let with_diff = r.q_bool("diff");
    let (_, v) = details_json(&r.st, repo, num, &r.base, with_diff).await?;
    Ok(json(v))
}

async fn mutate(r: &mut Req, repo: &RepoRow, access: Access, caller: &Caller, d: &Disc, action: &[&str]) -> ApiResult<Response> {
    let is_author = d.author.as_ref().is_some_and(|a| a.id == caller.user.id);
    let b: Value = r.json().await.unwrap_or(json!({}));
    let base = r.base.clone();
    let st = r.st.clone();
    let uid = caller.user.id;
    let user = caller.user.clone();
    let did = d.id;
    match action {
        ["comment"] => {
            let text = b.get("comment").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if text.trim().is_empty() {
                return Err(ApiError::bad("Comment cannot be empty"));
            }
            let ev = st
                .db(move |c| {
                    let id = insert_event(c, did, "comment", uid, &comment_data(&text, &user, false, false, vec![], &base))?;
                    event_by_id(c, &id)
                })
                .await?
                .unwrap();
            Ok(json(json!({ "newMessage": event_json(&ev, &r.base) })))
        }
        ["status"] => {
            if !(is_author || access >= Access::Write) {
                return Err(ApiError::forbidden("Not allowed to change status"));
            }
            let status = b.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if status != "open" && status != "closed" {
                return Err(ApiError::bad("Invalid status"));
            }
            let comment = b.get("comment").and_then(|v| v.as_str()).map(String::from);
            let ev = st
                .db(move |c| {
                    if let Some(cm) = comment.filter(|s| !s.trim().is_empty()) {
                        insert_event(c, did, "comment", uid, &comment_data(&cm, &user, false, false, vec![], &base))?;
                    }
                    c.execute("UPDATE discussions SET status = ?1 WHERE id = ?2", params![status, did])?;
                    let id = insert_event(c, did, "status-change", uid, &json!({ "status": status }))?;
                    event_by_id(c, &id)
                })
                .await?
                .unwrap();
            Ok(json(json!({ "newStatus": event_json(&ev, &r.base) })))
        }
        ["title"] => {
            if !(is_author || access >= Access::Write) {
                return Err(ApiError::forbidden("Not allowed to rename"));
            }
            let title = b.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if title.len() < 3 {
                return Err(ApiError::bad("Title must be at least 3 characters"));
            }
            let old = d.title.clone();
            let ev = st
                .db(move |c| {
                    c.execute("UPDATE discussions SET title = ?1 WHERE id = ?2", params![title, did])?;
                    let id = insert_event(c, did, "title-change", uid, &json!({ "from": old, "to": title }))?;
                    event_by_id(c, &id)
                })
                .await?
                .unwrap();
            Ok(json(json!({ "newTitle": event_json(&ev, &r.base) })))
        }
        ["comment", cid, op @ ("edit" | "hide")] => {
            let (cid, op) = (cid.to_string(), op.to_string());
            let content = b.get("content").and_then(|v| v.as_str()).map(String::from);
            let admin = access >= Access::Write;
            let res = st
                .db(move |c| {
                    let Some(ev) = event_by_id(c, &cid)? else { return Ok(Err(ApiError::not_found("Comment not found"))) };
                    let mine = ev.author.as_ref().is_some_and(|a| a.id == uid);
                    if ev.etype != "comment" || !(mine || (op == "hide" && admin)) {
                        return Ok(Err(ApiError::forbidden("Not allowed")));
                    }
                    let mut data = ev.data.clone();
                    if op == "edit" {
                        let text = content.unwrap_or_default();
                        let hist = data.get("history").and_then(|h| h.as_array()).cloned().unwrap_or_default();
                        data = comment_data(&text, &user, false, true, hist, &base);
                    } else {
                        data["hidden"] = json!(true);
                        data["latest"]["raw"] = json!("");
                        data["latest"]["html"] = json!("");
                        data["history"] = json!([]);
                    }
                    c.execute("UPDATE events SET data = ?1 WHERE id = ?2", params![data.to_string(), cid])?;
                    Ok(Ok(event_by_id(c, &cid)?.unwrap()))
                })
                .await??;
            Ok(json(json!({ "updatedComment": event_json(&res, &r.base) })))
        }
        ["merge"] => {
            if !d.is_pr {
                return Err(ApiError::bad("Not a pull request"));
            }
            if access < Access::Write {
                return Err(ApiError::forbidden("You need write access to merge"));
            }
            if d.status != "open" && d.status != "draft" {
                return Err(ApiError::bad("Pull request is not open"));
            }
            let comment = b.get("comment").and_then(|v| v.as_str()).map(String::from);
            let oid = merge_pr(&st, repo, caller, d).await?;
            let oid2 = oid.clone();
            st.db(move |c| {
                if let Some(cm) = comment.filter(|s| !s.trim().is_empty()) {
                    insert_event(c, did, "comment", uid, &comment_data(&cm, &user, false, false, vec![], &base))?;
                }
                c.execute("UPDATE discussions SET status = 'merged', merge_oid = ?1 WHERE id = ?2", params![oid2, did])?;
                insert_event(c, did, "status-change", uid, &json!({ "status": "merged" }))
            })
            .await?;
            Ok(json(json!({ "mergeCommitId": oid })))
        }
        _ => Err(ApiError::not_found("Not found")),
    }
}

/// Merge `refs/pr/N` into its target; fast-forward when possible, else a merge commit.
pub async fn merge_pr(st: &AppState, repo: &RepoRow, caller: &Caller, d: &Disc) -> ApiResult<String> {
    let git = st.git(&repo.disk_id);
    let lock = st.repo_lock(repo.id);
    let _g = lock.lock().await;
    let target = d.target_ref.clone().unwrap_or_else(|| "refs/heads/main".into());
    let head = git.resolve(&format!("refs/pr/{}", d.num)).await?.ok_or_else(|| ApiError::bad("PR has no commits"))?;
    let base = git.resolve(&target).await?;
    let new = match &base {
        None => head.clone(),
        Some(b) if git.is_ancestor(b, &head).await? => head.clone(),
        Some(b) => match git.merge_trees(b, &head).await? {
            Ok(tree) => {
                let msg = format!("Merge pull request #{} {}\n\n{}", d.num, d.title, "");
                git.commit_tree(&tree, vec![b.as_str(), head.as_str()], msg.trim(), &service::signature(&caller.user)).await?
            }
            Err(paths) => return Err(ApiError::conflict(format!("Merge conflicts in: {}", paths.join(", ")))),
        },
    };
    git.update_ref(&target, &new, base.as_deref()).await?;
    let fresh = { let id = repo.id; st.db(move |c| RepoRow::by_id(c, id)).await? };
    if let Some(fresh) = fresh {
        if target == "refs/heads/main" {
            crate::api::commit::scan_pointers(st, &fresh, &new).await?;
            service::refresh_meta(st, &fresh).await?;
        }
    }
    Ok(new)
}
