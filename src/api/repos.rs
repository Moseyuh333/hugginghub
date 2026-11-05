//! Repo management + info endpoints.

use crate::error::{ApiError, ApiResult};
use crate::models::{can_write_namespace, load_repo, repo_access, Access, Account, RepoRow, REPO_SELECT};
use crate::routes::{json, json_paged, Req};
use crate::service::{self, repo_json, with_lfs};
use crate::util::{now_iso, valid_name};
use axum::response::Response;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    organization: Option<String>,
    #[serde(rename = "type")]
    rtype: Option<String>,
    private: Option<bool>,
    visibility: Option<String>,
    sdk: Option<String>,
}

pub async fn create(r: &mut Req) -> ApiResult<Response> {
    let caller = r.require().await?;
    let b: CreateBody = r.json().await?;
    let rtype = b.rtype.clone().unwrap_or_else(|| "model".into());
    if !matches!(rtype.as_str(), "model" | "dataset" | "space") {
        return Err(ApiError::bad(format!("Unsupported repo type: {rtype}")));
    }
    // `name` may be "ns/name" for older clients
    let (ns, name) = match (b.organization.clone(), b.name.split_once('/')) {
        (Some(o), _) => (o, b.name.clone()),
        (None, Some((n, m))) => (n.to_string(), m.to_string()),
        (None, None) => (caller.user.name.clone(), b.name.clone()),
    };
    if !valid_name(&name) {
        return Err(ApiError::bad("Repository name is invalid: use letters, digits, '-', '_' and '.'"));
    }
    let private = b.private.unwrap_or(false) || matches!(b.visibility.as_deref(), Some("private" | "protected"));
    let (ns2, c2) = (ns.clone(), caller.clone());
    let owner = r
        .st
        .db(move |c| {
            let Some(acc) = Account::by_name(c, &ns2)? else { return Ok(Err(ApiError::not_found("Namespace not found"))) };
            if !can_write_namespace(c, &acc, &c2)? {
                return Ok(Err(ApiError::forbidden(format!("You don't have the rights to create a repo under the namespace \"{ns2}\""))));
            }
            Ok(Ok(acc))
        })
        .await??;
    let repo = match service::create_repo(&r.st, &rtype, &owner, &name, private, b.sdk.clone(), &caller.user).await {
        Ok(repo) => repo,
        Err(e) if e.status == axum::http::StatusCode::CONFLICT => {
            // the client parses this body even with exist_ok=True, so keep `url`
            let path = format!("{}{}/{}", crate::util::url_prefix(&rtype), owner.name, name);
            return Ok(crate::routes::json_status(
                axum::http::StatusCode::CONFLICT,
                json!({ "error": "You already created this repo", "url": format!("{}/{path}", r.base), "success": false }),
            ));
        }
        Err(e) => return Err(e),
    };
    Ok(json(json!({
        "url": format!("{}/{}", r.base, repo.web_path()),
        "name": repo.full_id(),
        "id": format!("{:024x}", repo.id),
    })))
}

#[derive(Deserialize)]
struct DeleteBody {
    name: String,
    organization: Option<String>,
    #[serde(rename = "type")]
    rtype: Option<String>,
}

pub async fn delete(r: &mut Req) -> ApiResult<Response> {
    let caller = r.require().await?;
    let b: DeleteBody = r.json().await?;
    let rtype = b.rtype.unwrap_or_else(|| "model".into());
    let (ns, name) = match (b.organization, b.name.split_once('/')) {
        (Some(o), _) => (o, b.name.clone()),
        (None, Some((n, m))) => (n.to_string(), m.to_string()),
        (None, None) => (caller.user.name.clone(), b.name.clone()),
    };
    let (repo, _) = load_repo(&r.st, &rtype, &ns, &name, Some(&caller), Access::Admin).await?;
    delete_repo(&r.st, &repo).await?;
    Ok(json(json!({})))
}

pub async fn delete_repo(st: &crate::state::AppState, repo: &RepoRow) -> ApiResult<()> {
    let id = repo.id;
    st.db(move |c| c.execute("DELETE FROM repos WHERE id = ?1", [id])).await?;
    let path = st.cfg.repos_dir().join(format!("{}.git", repo.disk_id));
    let _ = tokio::fs::remove_dir_all(path).await;
    Ok(())
}

#[derive(Deserialize)]
struct MoveBody {
    #[serde(rename = "fromRepo")]
    from: String,
    #[serde(rename = "toRepo")]
    to: String,
    #[serde(rename = "type")]
    rtype: Option<String>,
}

pub async fn move_repo(r: &mut Req) -> ApiResult<Response> {
    let caller = r.require().await?;
    let b: MoveBody = r.json().await?;
    let rtype = b.rtype.unwrap_or_else(|| "model".into());
    let (fns, fname) = b.from.split_once('/').ok_or_else(|| ApiError::bad("Invalid fromRepo"))?;
    let (tns, tname) = b.to.split_once('/').ok_or_else(|| ApiError::bad("Invalid toRepo"))?;
    if !valid_name(tname) {
        return Err(ApiError::bad("Invalid repo name"));
    }
    let (repo, _) = load_repo(&r.st, &rtype, fns, fname, Some(&caller), Access::Admin).await?;
    let (tns, tname, rt, c2) = (tns.to_string(), tname.to_string(), rtype.clone(), caller.clone());
    r.st.db(move |c| {
        let Some(acc) = Account::by_name(c, &tns)? else { return Ok(Err(ApiError::not_found("Namespace not found"))) };
        if !can_write_namespace(c, &acc, &c2)? {
            return Ok(Err(ApiError::forbidden("You can't move repos to this namespace")));
        }
        if RepoRow::find(c, &rt, &tns, &tname)?.is_some() {
            return Ok(Err(ApiError::conflict("A repo with this name already exists")));
        }
        c.execute("UPDATE repos SET owner_id = ?1, name = ?2, updated_at = ?3 WHERE id = ?4", params![acc.id, tname, now_iso(), repo.id])?;
        Ok(Ok(()))
    })
    .await??;
    Ok(json(json!({})))
}

pub async fn validate_yaml(r: &mut Req) -> ApiResult<Response> {
    let b: Value = r.json().await?;
    let content = b.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let text = content.trim_start_matches('\u{feff}');
    if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            if let Err(e) = serde_yaml::from_str::<serde_yaml::Value>(&rest[..end]) {
                return Ok(crate::routes::json_status(
                    axum::http::StatusCode::BAD_REQUEST,
                    json!({ "errors": [{ "message": format!("Invalid YAML: {e}") }], "warnings": [] }),
                ));
            }
        }
    }
    Ok(json(json!({ "errors": [], "warnings": [] })))
}

/// GET /api/{type}s/{ns}/{name}[/revision/{rev}]
pub async fn info(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: Option<&str>) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let git = r.st.git(&repo.disk_id);
    let rev = rev.unwrap_or("main");
    let sha = match git.resolve(rev).await? {
        Some(s) => Some(s),
        None if rev == "main" && git.is_empty().await? => None,
        None => return Err(ApiError::revision_not_found(rev)),
    };
    let blobs = r.q_bool("blobs");
    let files = match &sha {
        Some(s) => {
            let entries = git.ls_tree(s, "", true).await?;
            let entries: Vec<_> = entries.into_iter().filter(|e| e.kind == "file").collect();
            if blobs { with_lfs(&git, entries).await? } else { entries.into_iter().map(|e| service::FileInfo { pointer_size: e.size, entry: e, lfs: None }).collect() }
        }
        None => vec![],
    };
    let mut v = repo_json(&repo, &r.base, sha.as_deref(), Some(&files), blobs);
    let expand = r.q_all("expand[]").into_iter().chain(r.q_all("expand")).collect::<Vec<_>>();
    if !expand.is_empty() {
        filter_expand(&mut v, &expand);
    }
    Ok(json(v))
}

/// Keep only the requested properties (plus id) when `expand[]` is given.
fn filter_expand(v: &mut Value, keep: &[String]) {
    if let Some(o) = v.as_object_mut() {
        o.retain(|k, _| k == "id" || k == "_id" || keep.iter().any(|x| x == k));
    }
}

/// GET /api/models|datasets|spaces
pub async fn list(r: &mut Req, rtype: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let search = r.q("search").map(|s| s.to_lowercase());
    let author = r.q("author").map(String::from);
    let filters: Vec<String> = r.q_all("filter");
    let sort = r.q("sort").unwrap_or("lastModified").to_string();
    let limit: usize = r.q("limit").and_then(|v| v.parse().ok()).unwrap_or(1000).min(10_000);
    let offset: usize = r.q("cursor").and_then(|v| v.parse().ok()).unwrap_or(0);
    let full = r.q_bool("full");
    let card = r.q_bool("cardData") || full;
    let expand = r.q_all("expand[]").into_iter().chain(r.q_all("expand")).collect::<Vec<_>>();
    let rt = rtype.to_string();
    let c2 = caller.clone();
    let repos: Vec<RepoRow> = r
        .st
        .db(move |c| {
            let order = match sort.as_str() {
                "likes" => "(SELECT COUNT(*) FROM likes l WHERE l.repo_id = r.id) DESC",
                "downloads" => "r.downloads DESC",
                "createdAt" | "created_at" => "r.created_at DESC",
                "id" | "modelId" => "a.name ASC, r.name ASC",
                _ => "r.updated_at DESC",
            };
            let mut st = c.prepare(&format!("{REPO_SELECT} WHERE r.type = ?1 ORDER BY {order}"))?;
            let rows = st.query_map([&rt], RepoRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = vec![];
            for repo in rows {
                if repo_access(c, &repo, c2.as_ref())? == Access::None {
                    continue;
                }
                out.push(repo);
            }
            Ok(out)
        })
        .await?;
    let filtered: Vec<RepoRow> = repos
        .into_iter()
        .filter(|repo| author.as_ref().is_none_or(|a| repo.owner.eq_ignore_ascii_case(a)))
        .filter(|repo| search.as_ref().is_none_or(|s| repo.full_id().to_lowercase().contains(s)))
        .filter(|repo| {
            if filters.is_empty() {
                return true;
            }
            let tags = service::card_tags(repo, &repo.card_json());
            filters.iter().all(|f| tags.iter().any(|t| t == f))
        })
        .collect();
    let total = filtered.len();
    let page: Vec<Value> = filtered
        .iter()
        .skip(offset)
        .take(limit)
        .map(|repo| {
            let mut v = repo_json(repo, &r.base, None, None, false);
            if !card {
                if let Some(o) = v.as_object_mut() {
                    o.remove("cardData");
                }
            }
            if !expand.is_empty() {
                filter_expand(&mut v, &expand);
            }
            v
        })
        .collect();
    let next = (r.q("limit").is_none() && offset + limit < total).then(|| r.url_with(&[("cursor", (offset + limit).to_string())]));
    Ok(json_paged(json!(page), next))
}

pub async fn refs(r: &mut Req, rtype: &str, ns: &str, name: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let git = r.st.git(&repo.disk_id);
    let fmt = |v: Vec<crate::git::read::RefInfo>| -> Vec<Value> {
        v.into_iter().map(|x| json!({ "name": x.name, "ref": x.full, "targetCommit": x.target })).collect()
    };
    let mut out = json!({
        "branches": fmt(git.refs("refs/heads/").await?),
        "tags": fmt(git.refs("refs/tags/").await?),
        "converts": fmt(git.refs("refs/convert/").await?),
    });
    if r.q_bool("include_prs") {
        let prs: Vec<Value> = git
            .refs("refs/pr/")
            .await?
            .into_iter()
            .map(|x| json!({ "name": x.full.trim_start_matches("refs/"), "ref": x.full, "targetCommit": x.target }))
            .collect();
        out["pullRequests"] = json!(prs);
    }
    Ok(json(out))
}

pub async fn commits(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let git = r.st.git(&repo.disk_id);
    let rev = if rev.is_empty() { "main" } else { rev };
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    let per: usize = r.q("limit").and_then(|v| v.parse().ok()).unwrap_or(50).clamp(1, 1000);
    let page: usize = r.q("p").and_then(|v| v.parse().ok()).unwrap_or(0);
    let list = git.log(&sha, None, page * per, per + 1).await?;
    let has_more = list.len() > per;
    let commits = list.into_iter().take(per);
    let st = r.st.clone();
    let mut out = vec![];
    for c in commits {
        let author = c.author_name.clone();
        let exists = {
            let a = author.clone();
            st.db(move |db| Account::by_name(db, &a)).await?
        };
        let author_json = match exists {
            Some(a) => json!({ "user": a.name, "avatar": a.avatar_url(&r.base) }),
            None => json!({ "user": author, "avatar": Value::Null }),
        };
        let mut v = json!({
            "id": c.id,
            "title": c.title,
            "message": c.message,
            "date": c.date,
            "authors": [author_json],
            "parents": c.parents,
        });
        if r.q_all("expand[]").iter().any(|e| e == "formatted") {
            v["formatted"] = json!({ "title": html_escape(&c.title), "message": html_escape(&c.message) });
        }
        out.push(v);
    }
    let next = has_more.then(|| r.url_with(&[("p", (page + 1).to_string())]));
    Ok(json_paged(json!(out), next))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub async fn compare(r: &mut Req, rtype: &str, ns: &str, name: &str, spec: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    let git = r.st.git(&repo.disk_id);
    let (a, b) = spec.split_once("..").ok_or_else(|| ApiError::bad("Expected base..head"))?;
    let a = git.resolve(a).await?.ok_or_else(|| ApiError::revision_not_found(a))?;
    let b = git.resolve(b).await?.ok_or_else(|| ApiError::revision_not_found(b))?;
    let diff = git.diff(&a, &b).await?;
    Ok(axum::response::IntoResponse::into_response(diff))
}

async fn writable(r: &Req, rtype: &str, ns: &str, name: &str) -> ApiResult<(RepoRow, crate::models::Caller)> {
    let caller = r.require().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, Some(&caller), Access::Write).await?;
    Ok((repo, caller))
}

pub fn valid_ref_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with(['-', '/', '.'])
        && !n.ends_with(['/', '.'])
        && !n.ends_with(".lock")
        && !n.contains("..")
        && !n.contains("//")
        && !n.contains("@{")
        && !n.chars().any(|c| c.is_ascii_control() || " ~^:?*[\\".contains(c))
}

pub async fn create_branch(r: &mut Req, rtype: &str, ns: &str, name: &str, branch: &str) -> ApiResult<Response> {
    let (repo, caller) = writable(r, rtype, ns, name).await?;
    if !valid_ref_name(branch) || branch.starts_with("refs/") {
        return Err(ApiError::bad("Invalid branch name"));
    }
    let b: Value = r.json().await?;
    let git = r.st.git(&repo.disk_id);
    let refname = format!("refs/heads/{branch}");
    if git.resolve(&refname).await?.is_some() {
        if b.get("overwrite").and_then(|v| v.as_bool()) != Some(true) {
            return Err(ApiError::conflict(format!("Reference already exists: {refname}")));
        }
    }
    let start = b.get("startingPoint").and_then(|v| v.as_str()).unwrap_or("main");
    let target = if b.get("emptyBranch").and_then(|v| v.as_bool()) == Some(true) {
        let tree = git.ok_str(&["hash-object", "-t", "tree", "-w", "/dev/null"]).await?;
        git.commit_tree(&tree, vec![], "Initial empty commit", &service::signature(&caller.user)).await?
    } else {
        git.resolve(start).await?.ok_or_else(|| ApiError::revision_not_found(start))?
    };
    git.ok(&["update-ref", &refname, &target], None, &[]).await?;
    Ok(json(json!({})))
}

pub async fn delete_branch(r: &mut Req, rtype: &str, ns: &str, name: &str, branch: &str) -> ApiResult<Response> {
    let (repo, _) = writable(r, rtype, ns, name).await?;
    if branch == "main" {
        return Err(ApiError::forbidden("Cannot delete the default branch"));
    }
    let git = r.st.git(&repo.disk_id);
    let refname = format!("refs/heads/{branch}");
    if git.resolve(&refname).await?.is_none() {
        return Err(ApiError::revision_not_found(branch));
    }
    git.delete_ref(&refname).await?;
    Ok(json(json!({})))
}

pub async fn create_tag(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: &str) -> ApiResult<Response> {
    let (repo, caller) = writable(r, rtype, ns, name).await?;
    let b: Value = r.json().await?;
    let tag = b.get("tag").and_then(|v| v.as_str()).ok_or_else(|| ApiError::bad("Missing tag"))?;
    if !valid_ref_name(tag) {
        return Err(ApiError::bad("Invalid tag name"));
    }
    let git = r.st.git(&repo.disk_id);
    let rev = if rev.is_empty() { "main" } else { rev };
    let sha = git.resolve(rev).await?.ok_or_else(|| ApiError::revision_not_found(rev))?;
    if git.resolve(&format!("refs/tags/{tag}")).await?.is_some() {
        return Err(ApiError::conflict(format!("Tag already exists: {tag}")));
    }
    git.create_tag(tag, &sha, b.get("message").and_then(|v| v.as_str()), &service::signature(&caller.user)).await?;
    Ok(json(json!({})))
}

pub async fn delete_tag(r: &mut Req, rtype: &str, ns: &str, name: &str, tag: &str) -> ApiResult<Response> {
    let (repo, _) = writable(r, rtype, ns, name).await?;
    let git = r.st.git(&repo.disk_id);
    let refname = format!("refs/tags/{tag}");
    if git.resolve(&refname).await?.is_none() {
        return Err(ApiError::revision_not_found(tag));
    }
    git.delete_ref(&refname).await?;
    Ok(json(json!({})))
}

pub async fn settings(r: &mut Req, rtype: &str, ns: &str, name: &str) -> ApiResult<Response> {
    let caller = r.require().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, Some(&caller), Access::Admin).await?;
    let b: Value = r.json().await?;
    let private = match (b.get("private").and_then(|v| v.as_bool()), b.get("visibility").and_then(|v| v.as_str())) {
        (_, Some(v)) => Some(v != "public"),
        (Some(p), None) => Some(p),
        _ => None,
    };
    let gated = b.get("gated").map(|g| match g {
        Value::String(s) if s == "auto" || s == "manual" => Some(s.clone()),
        _ => None,
    });
    let disc = b.get("discussionsDisabled").and_then(|v| v.as_bool());
    let id = repo.id;
    r.st.db(move |c| {
        if let Some(p) = private {
            c.execute("UPDATE repos SET private = ?1 WHERE id = ?2", params![p as i64, id])?;
        }
        if let Some(g) = gated {
            c.execute("UPDATE repos SET gated = ?1 WHERE id = ?2", params![g, id])?;
        }
        if let Some(d) = disc {
            c.execute("UPDATE repos SET discussions_disabled = ?1 WHERE id = ?2", params![d as i64, id])?;
        }
        Ok(())
    })
    .await?;
    Ok(json(json!({})))
}

pub async fn super_squash(r: &mut Req, rtype: &str, ns: &str, name: &str, branch: &str) -> ApiResult<Response> {
    let (repo, caller) = writable(r, rtype, ns, name).await?;
    let b: Value = r.json().await?;
    let msg = b.get("message").and_then(|v| v.as_str()).unwrap_or("Super-squash branch using huggingface_hub");
    let git = r.st.git(&repo.disk_id);
    let branch = if branch.is_empty() { "main" } else { branch };
    let refname = format!("refs/heads/{branch}");
    let lock = r.st.repo_lock(repo.id);
    let _g = lock.lock().await;
    let head = git.resolve(&refname).await?.ok_or_else(|| ApiError::revision_not_found(branch))?;
    let tree = git.tree_of(&head).await?;
    let new = git.commit_tree(&tree, vec![], msg, &service::signature(&caller.user)).await?;
    git.update_ref(&refname, &new, Some(&head)).await?;
    service::refresh_meta(&r.st, &repo).await?;
    Ok(json(json!({ "commitId": new })))
}

pub async fn auth_check(r: &mut Req, rtype: &str, ns: &str, name: &str) -> ApiResult<Response> {
    let caller = r.caller().await?;
    load_repo(&r.st, rtype, ns, name, caller.as_ref(), Access::Read).await?;
    Ok(json(json!({})))
}
