//! Repo pages: model card, file browser, blob view, commit history.

use crate::error::{ApiError, ApiResult};
use crate::git::read::TreeEntry;
use crate::models::{load_repo, repo_access, Access, Caller, RepoRow};
use crate::routes::Req;
use crate::service::{self, card_tags, with_lfs};
use crate::web::{ago, base_ctx, ctx, render, render_status};
use axum::http::StatusCode;
use axum::response::Response;
use minijinja::context;
use serde_json::{json, Value};

pub const PAGE: usize = 50;

pub async fn load(st: &crate::state::AppState, rtype: &str, ns: &str, name: &str, me: &Option<Caller>) -> ApiResult<(RepoRow, Access)> {
    load_repo(st, rtype, ns, name, me.as_ref(), Access::Read).await
}

/// Header context shared by every repo page.
pub async fn header(r: &Req, repo: &RepoRow, me: &Option<Caller>, access: Access) -> ApiResult<Value> {
    let st = &r.st;
    let card = repo.card_json();
    let tags = card_tags(repo, &card);
    let git = st.git(&repo.disk_id);
    let file_count = match &repo.head_sha {
        Some(h) => git.ls_tree(h, "", true).await?.iter().filter(|e| e.kind == "file").count(),
        None => 0,
    };
    let liked = match me {
        Some(c) => {
            let (uid, rid) = (c.user.id, repo.id);
            st.db(move |db| {
                Ok(db
                    .query_row("SELECT 1 FROM likes WHERE user_id = ?1 AND repo_id = ?2", rusqlite::params![uid, rid], |_| Ok(()))
                    .ok()
                    .is_some())
            })
            .await?
        }
        None => false,
    };
    let rid = repo.id;
    let n_discussions = st.db(move |c| {
        Ok(c.query_row("SELECT COUNT(*) FROM discussions WHERE repo_id = ?1", [rid], |r| r.get::<_, i64>(0))?)
    }).await?;
    Ok(ctx(r, me, &[json!({ "r": crate::web::repo_card(repo), "tags": tags, "card": card, "file_count": file_count, "liked": liked, "n_discussions": n_discussions, "can_admin": access >= Access::Admin, "can_write": access >= Access::Write, "is_owner": me.as_ref().is_some_and(|c| c.user.id == repo.owner_id) })]))
}

pub async fn route(r: &mut Req, rtype: &str, ns: &str, name: &str, rest: &[&str]) -> ApiResult<Response> {
    let me = r.web_caller().await;
    let (repo, access) = load(&r.st, rtype, ns, name, &me).await?;
    match rest {
        [] => card_page(r, repo, me, access).await,
        ["tree", rev, path @ ..] => tree_page(r, repo, me, access, rev, &path.join("/")).await,
        ["blob", rev, path @ ..] => blob_page(r, repo, me, access, rev, &path.join("/")).await,
        ["raw", rev, path @ ..] => raw_file(r, repo, rev, &path.join("/")).await,
        ["commits", rev, path @ ..] => commits_page(r, repo, me, access, rev, &path.join("/")).await,
        ["commit", sha] => commit_page(r, repo, me, access, sha).await,
        ["community"] => community_page(r, repo, me, access).await,
        ["community", "new"] => new_discussion(r, repo, me, access).await,
        ["discussions", num] => discussion_page(r, repo, me, access, num).await,
        ["discussions", num, action] => super::actions::discussion_action(r, repo, me, access, num, action).await,
        ["settings"] => super::actions::settings_page(r, repo, me, access, None).await,
        ["settings", "rename"] => super::actions::rename(r, repo, me, access).await,
        ["settings", "delete"] => super::actions::delete(r, repo, me, access).await,
        // segments() drops the trailing empty piece, so "/app" and "/app/"
        // both arrive as ["app"]; the raw URI tells them apart.
        ["app"] if r.uri.path().ends_with('/') => space_app(r, repo, "").await,
        ["app"] => Ok(crate::web::redirect(&format!("/{}/app/", repo.web_path()))),
        ["app", path @ ..] => space_app(r, repo, &path.join("/")).await,
        ["like"] => super::actions::like(r, repo, me).await,
        ["upload", rev] => super::actions::upload_page(r, repo, me, access, rev).await,
        ["new-file", rev] => super::actions::new_file_page(r, repo, me, access, rev).await,
        ["edit", rev, path @ ..] => super::actions::edit_page(r, repo, me, access, rev, &path.join("/")).await,
        ["resolve", ..] => Err(ApiError::not_found("Not found")),
        _ => Ok(crate::web::not_found(r).await),
    }
}

pub async fn card_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access) -> ApiResult<Response> {
    let git = r.st.git(&repo.disk_id);
    let mut readme_html = String::new();
    let mut last_commit = Value::Null;
    if let Some(h) = &repo.head_sha {
        if let Some(e) = git.stat_path(h, "README.md").await? {
            let data = git.cat_blob(&e.oid).await?;
            let text = String::from_utf8_lossy(&data);
            let (_, body) = service::parse_card(&text);
            readme_html = super::markdown::render(&body);
        }
        if let Some(c) = git.last_commit(h, "").await? {
            last_commit = json!({ "id": c.id, "title": c.title, "date": c.date, "author": c.author_name });
        }
    }
    let hdr = header(r, &repo, &me, access).await?;
    let app_src = space_app_src(r, &repo).await?;
    let ctx = ctx(
        r,
        &me,
        &[hdr, json!({ "title": repo.full_id(), "tab": "card", "readme_html": readme_html, "last_commit": last_commit, "app_src": app_src })],
    );
    render(&r.st, "repo/card.html", ctx)
}

fn crumbs(path: &str) -> Vec<Value> {
    let mut out = vec![];
    let mut acc = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        out.push(json!({ "name": seg, "path": acc }));
    }
    out
}

pub async fn tree_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, rev: &str, path: &str) -> ApiResult<Response> {
    let git = r.st.git(&repo.disk_id);
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let path = path.trim_matches('/').to_string();
    if !path.is_empty() {
        match git.stat_path(&sha, &path).await? {
            Some(e) if e.kind == "directory" => {}
            Some(_) => return super::actions::blob_redirect(r, &repo, rev, &path).await,
            None => return Err(ApiError::entry_not_found(&path)),
        }
    }
    let entries = git.ls_tree(&sha, &path, false).await?;
    let entries: Vec<TreeEntry> = entries.into_iter().filter(|e| e.kind != "directory" || true).collect();
    let files = with_lfs(&git, entries).await?;
    let mut out = vec![];
    for f in &files {
        let mut v = f.to_json();
        v["name"] = json!(f.entry.path.rsplit('/').next().unwrap_or(&f.entry.path));
        if let Some(c) = git.last_commit(&sha, &f.entry.path).await? {
            v["lastCommit"] = json!({ "id": c.id, "title": c.title, "date": c.date });
        }
        out.push(v);
    }
    let last = git.last_commit(&sha, &path).await?.map(|c| json!({ "id": c.id, "title": c.title, "date": c.date, "author": c.author_name }));
    let branches = git.refs("refs/heads/").await?;
    let tags = git.refs("refs/tags/").await?;
    let prs = git.refs("refs/pr/").await?;
    let parent_path = path.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default();
    let hdr = header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("{}/{} at {rev}", repo.full_id(), path), "tab": "files", "rev": rev, "path": path, "parent_path": parent_path, "crumbs": crumbs(&path), "files": out, "last_commit": last.unwrap_or(Value::Null), "branches": branches.iter().map(|b| json!({"name": b.name})).collect::<Vec<_>>(), "git_tags": tags.iter().map(|b| json!({"name": b.name})).collect::<Vec<_>>(), "prs": prs.iter().map(|b| json!({"name": b.name})).collect::<Vec<_>>() })]);
    render(&r.st, "repo/tree.html", ctx)
}

pub async fn blob_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, rev: &str, path: &str) -> ApiResult<Response> {
    let git = r.st.git(&repo.disk_id);
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let entry = match git.stat_path(&sha, path).await? {
        Some(e) if e.kind == "file" => e,
        Some(_) => return tree_page(r, repo, me, access, rev, path).await,
        None => return Err(ApiError::entry_not_found(path)),
    };
    let info = with_lfs(&git, vec![entry]).await?.remove(0);
    let size = info.size();
    let binary = info.lfs.is_some();
    let mut lines: Vec<String> = vec![];
    let mut content_html = String::new();
    let is_markdown = path.to_ascii_lowercase().ends_with(".md") || path.to_ascii_lowercase().ends_with(".mdx");
    if !binary && size < 2_000_000 {
        let data = git.cat_blob(&info.entry.oid).await?;
        let text = String::from_utf8_lossy(&data);
        let looks_binary = text.chars().take(4000).any(|c| c == '\u{0}');
        if looks_binary {
            // binary but stored as a regular blob
        } else if is_markdown {
            let (_, body) = service::parse_card(&text);
            content_html = super::markdown::render(&body);
        } else {
            lines = text.lines().map(|l| l.replace('\t', "    ")).collect();
        }
        let last = git.last_commit(&sha, path).await?;
        let hdr = header(r, &repo, &me, access).await?;
        let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("{path} at {rev}"), "tab": "files", "rev": rev, "path": path, "crumbs": crumbs(path), "size": size, "lines": lines, "content_html": content_html, "is_markdown": is_markdown, "binary": false, "lfs": info.lfs.as_ref().map(|p| p.oid.clone()), "last_commit": last.map(|c| json!({"id": c.id, "title": c.title, "date": c.date, "author": c.author_name})).unwrap_or(Value::Null), "branches": git.refs("refs/heads/").await?.iter().map(|b| json!({"name": b.name})).collect::<Vec<_>>() })]);
        return render(&r.st, "repo/blob.html", ctx);
    }
    let last = git.last_commit(&sha, path).await?;
    let hdr = header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("{path} at {rev}"), "tab": "files", "rev": rev, "path": path, "crumbs": crumbs(path), "size": size, "lines": Vec::<String>::new(), "content_html": "", "is_markdown": false, "binary": true, "lfs": info.lfs.as_ref().map(|p| p.oid.clone()), "last_commit": last.map(|c| json!({"id": c.id, "title": c.title, "date": c.date, "author": c.author_name})).unwrap_or(Value::Null), "branches": git.refs("refs/heads/").await?.iter().map(|b| json!({"name": b.name})).collect::<Vec<_>>() })]);
    render(&r.st, "repo/blob.html", ctx)
}

pub async fn raw_file(r: &mut Req, repo: RepoRow, rev: &str, path: &str) -> ApiResult<Response> {
    let git = r.st.git(&repo.disk_id);
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let entry = git.stat_path(&sha, path).await?.ok_or_else(|| ApiError::entry_not_found(path))?;
    let info = with_lfs(&git, vec![entry]).await?.remove(0);
    if let Some(p) = info.lfs {
        let body = r.st.storage.get(&p.oid, 0, p.size as u64).await?;
        return Ok(axum::response::Response::builder()
            .header("content-type", "application/octet-stream")
            .body(body.unwrap_or_default())
            .unwrap());
    }
    let data = git.cat_blob(&info.entry.oid).await?;
    Ok(([("content-type", "text/plain; charset=utf-8")], data).into_response())
}

/// Serve a Space's app files at `/{space}/app[/path]`. Files come from HEAD;
/// directories fall back to `index.html`. This is what a "static" SDK Space
/// runs in — the app iframe points here. gradio/streamlit spaces embed an
/// external `app_url` instead (set in Settings).
pub async fn space_app(r: &mut Req, repo: RepoRow, path: &str) -> ApiResult<Response> {
    if repo.rtype != "space" {
        return Err(ApiError::not_found("Not found"));
    }
    let git = r.st.git(&repo.disk_id);
    let sha = git.resolve("main").await?.ok_or_else(|| ApiError::not_found("This Space has no files yet"))?;
    let mut path = path.trim_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".to_string();
    }
    // Directories need a trailing slash so relative links resolve inside them.
    match git.stat_path(&sha, &path).await? {
        Some(e) if e.kind == "dir" => {
            if !r.uri.path().ends_with('/') {
                return Ok(crate::web::redirect(&format!("/{}/app/{path}/", repo.web_path())));
            }
            path = format!("{path}/index.html");
        }
        _ => {}
    }
    let entry = git.stat_path(&sha, &path).await?.ok_or_else(|| ApiError::entry_not_found(&path))?;
    let ctype = crate::api::files::app_content_type(&entry.path);
    let info = with_lfs(&git, vec![entry]).await?.remove(0);
    let mut resp = if let Some(p) = info.lfs {
        let body = r.st.storage.get(&p.oid, 0, p.size as u64).await?.unwrap_or_default();
        Response::builder().header("content-type", ctype).body(body).unwrap()
    } else {
        let data = git.cat_blob(&info.entry.oid).await?;
        Response::builder().header("content-type", ctype).body(data.into()).unwrap()
    };
    resp.headers_mut().insert("x-repo-commit", axum::http::HeaderValue::from_str(&sha).unwrap());
    resp.headers_mut().insert("cache-control", axum::http::HeaderValue::from_static("no-cache"));
    Ok(resp)
}

/// Where the Space's app iframe points: external `app_url`, our own `/app`
/// server for static spaces with an index.html, or None (stopped).
async fn space_app_src(r: &Req, repo: &RepoRow) -> ApiResult<Option<String>> {
    if repo.rtype != "space" {
        return Ok(None);
    }
    if let Some(u) = &repo.app_url {
        if !u.trim().is_empty() {
            return Ok(Some(u.trim().to_string()));
        }
    }
    if repo.sdk.as_deref() == Some("static") {
        let git = r.st.git(&repo.disk_id);
        if let Some(sha) = git.resolve("main").await? {
            if git.stat_path(&sha, "index.html").await?.is_some() {
                return Ok(Some(format!("/{}/app/", repo.web_path())));
            }
        }
    }
    Ok(None)
}

use axum::response::IntoResponse;

pub async fn commits_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, rev: &str, path: &str) -> ApiResult<Response> {
    let git = r.st.git(&repo.disk_id);
    let rev = if rev.is_empty() { "main" } else { rev };
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let page = super::page_of(r);
    let p = if path.is_empty() { None } else { Some(path) };
    let list = git.log(&sha, p, page * PAGE, PAGE + 1).await?;
    let has_more = list.len() > PAGE;
    let commits: Vec<Value> = list
        .into_iter()
        .take(PAGE)
        .map(|c| json!({ "id": c.id, "title": c.title, "author": c.author_name, "date": c.date }))
        .collect();
    let hdr = header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("Commits · {}", repo.full_id()), "tab": "commits", "rev": rev, "path": path, "commits": commits, "page": page, "has_more": has_more })]);
    render(&r.st, "repo/commits.html", ctx)
}

pub async fn commit_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, sha: &str) -> ApiResult<Response> {
    let git = r.st.git(&repo.disk_id);
    let full = git.resolve(sha).await?.ok_or_else(|| ApiError::revision_not_found(sha))?;
    let c = git.log(&full, None, 0, 1).await?.into_iter().next().ok_or_else(|| ApiError::revision_not_found(sha))?;
    let parent = c.parents.first().cloned();
    // changed files with line counts
    let mut files = vec![];
    let diff_text = match &parent {
        Some(p) => git.diff(p, &full).await?,
        None => git.diff(&empty_tree(&git).await?, &full).await.unwrap_or_default(),
    };
    let mut cur: Option<Value> = None;
    let mut diff_html = String::new();
    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            if let Some(f) = cur.take() {
                files.push(f);
            }
            let p = line.split(" b/").nth(1).unwrap_or("").to_string();
            cur = Some(json!({ "path": p, "additions": 0, "deletions": 0, "status": "modified" }));
            diff_html.push_str(&format!("<div class=\"file\">{}</div>\n", html_escape(line)));
        } else if line.starts_with("new file") {
            if let Some(f) = cur.as_mut() {
                f["status"] = json!("added");
            }
            diff_html.push_str(&format!("<div class=\"file\">{}</div>\n", html_escape(line)));
        } else if line.starts_with("deleted file") {
            if let Some(f) = cur.as_mut() {
                f["status"] = json!("removed");
            }
        } else if line.starts_with("+++") || line.starts_with("---") || line.starts_with("index ") {
            continue;
        } else if line.starts_with('+') {
            if let Some(f) = cur.as_mut() {
                let n = f["additions"].as_i64().unwrap_or(0) + 1;
                f["additions"] = json!(n);
            }
            diff_html.push_str(&format!("<div class=\"add\">{}</div>\n", html_escape(line)));
        } else if line.starts_with('-') {
            if let Some(f) = cur.as_mut() {
                let n = f["deletions"].as_i64().unwrap_or(0) + 1;
                f["deletions"] = json!(n);
            }
            diff_html.push_str(&format!("<div class=\"del\">{}</div>\n", html_escape(line)));
        } else if line.starts_with("@@") {
            diff_html.push_str(&format!("<div class=\"hunk\">{}</div>\n", html_escape(line)));
        } else {
            diff_html.push_str(&format!("<div>{}</div>\n", html_escape(line)));
        }
    }
    if let Some(f) = cur.take() {
        files.push(f);
    }
    if diff_html.len() > 400_000 {
        diff_html = String::from("<div class=\"file\">Diff too large to display</div>");
    }
    let hdr = header(r, &repo, &me, access).await?;
    let commit_v = json!({ "id": c.id, "title": c.title, "author": c.author_name, "date": c.date,
        "message_html": super::markdown::render(&c.message) });
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("{} · {}", c.title, repo.full_id()), "tab": "commits", "commit": commit_v, "files": files, "diff_html": diff_html })]);
    render(&r.st, "repo/commit.html", ctx)
}

async fn empty_tree(git: &crate::git::Repo) -> ApiResult<String> {
    let out = git.ok(&["hash-object", "-t", "tree", "-w", "/dev/null"], None, &[]).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub async fn community_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access) -> ApiResult<Response> {
    let ftype = r.q("type").unwrap_or("all").to_string();
    let rid = repo.id;
    let mut discs = r.st.db(move |c| crate::api::community::list_discs(c, rid)).await?;
    match ftype.as_str() {
        "pull_request" => discs.retain(|d| d.is_pr),
        "discussion" => discs.retain(|d| !d.is_pr),
        _ => {}
    }
    let list: Vec<Value> = discs
        .iter()
        .map(|d| {
            json!({
                "num": d.num, "title": d.title, "status": d.status, "is_pr": d.is_pr,
                "author": d.author.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| "deleted".into()),
                "created_at": d.created_at, "num_comments": d.num_comments,
            })
        })
        .collect();
    let hdr = header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("Community · {}", repo.full_id()), "tab": "community", "discussions": list, "ftype": ftype })]);
    render(&r.st, "repo/community.html", ctx)
}

pub async fn new_discussion(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access) -> ApiResult<Response> {
    let Some(caller) = me.clone() else { return Ok(crate::web::login_redirect(r)) };
    let is_pr = r.q_bool("pr") || r.q("pr") == Some("1");
    if r.method == axum::http::Method::POST {
        let f = super::form(r).await?;
        let title = f.get("title").cloned().unwrap_or_default();
        let desc = f.get("description").cloned().unwrap_or_default();
        if title.trim().len() < 3 {
            let ctx = ctx(r, &me, &[json!({ "title": "New discussion", "r": crate::web::repo_card(&repo), "is_pr": is_pr, "error": "Title must be at least 3 characters" })]);
            return crate::web::render_status(&r.st, StatusCode::BAD_REQUEST, "repo/new_discussion.html", ctx);
        }
        let (rid, user, base) = (repo.id, caller.user.clone(), r.base.clone());
        let (_, num) = r
            .st
            .db(move |c| crate::api::community::create_disc(c, rid, &user, title.trim(), &desc, is_pr, &base))
            .await?;
        if is_pr {
            let git = r.st.git(&repo.disk_id);
            if let Some(head) = git.resolve("main").await? {
                let _ = git.update_ref(&format!("refs/pr/{num}"), &head, None).await;
            }
        }
        return Ok(crate::web::redirect(&format!("/{}/discussions/{num}", repo.web_path())));
    }
    let hdr = header(r, &repo, &me, access).await?;
    let ctx = ctx(r, &me, &[hdr, json!({ "title": "New discussion", "r": crate::web::repo_card(&repo), "is_pr": is_pr, "error": Value::Null })]);
    render(&r.st, "repo/new_discussion.html", ctx)
}

pub async fn discussion_page(r: &mut Req, repo: RepoRow, me: Option<Caller>, access: Access, num: &str) -> ApiResult<Response> {
    let num: i64 = num.parse().map_err(|_| ApiError::bad("Invalid discussion number"))?;
    let (d, v) = crate::api::community::details_json(&r.st, &repo, num, &r.base, true).await?;
    // simplify events for the template
    let events: Vec<Value> = v["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            let author = e["author"]["name"].as_str().unwrap_or("deleted").to_string();
            let avatar = e["author"]["avatarUrl"].as_str().map(String::from).unwrap_or_else(|| format!("/avatars/{author}.svg"));
            let etype = e["type"].as_str().unwrap_or("").to_string();
            let data = e["data"].clone();
            json!({
                "type": etype, "author": author, "avatar": avatar, "created_at": e["createdAt"],
                "html": data["latest"]["html"], "hidden": data["hidden"].as_bool().unwrap_or(false),
                "edited": data["edited"].as_bool().unwrap_or(false), "data": data,
                "subject": data["subject"], "oid": data["oid"],
            })
        })
        .collect();
    let files: Vec<Value> = Vec::new();
    let hdr = header(r, &repo, &me, access).await?;
    let d_v = json!({ "num": d.num, "title": d.title, "status": d.status, "is_pr": d.is_pr,
        "author": d.author.as_ref().map(|a| a.name.clone()), "created_at": d.created_at,
        "target_ref": d.target_ref });
    let ctx = ctx(r, &me, &[hdr, json!({ "title": format!("{} · {}", d.title, repo.full_id()), "tab": "community", "d": d_v, "events": events, "files": files })]);
    render(&r.st, "repo/discussion.html", ctx)
}


