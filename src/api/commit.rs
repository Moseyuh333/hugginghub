//! Uploads: `preupload` (regular vs LFS decision) and the NDJSON `commit` endpoint
//! (header / file / lfsFile / deletedFile / deletedFolder lines), incl. `create_pr=1`.

use crate::error::{ApiError, ApiResult};
use crate::git::lfs::{parse_pointer, pointer_text, valid_oid};
use crate::git::write::{clean_path, Op};
use crate::models::{load_repo, Access, Caller, RepoRow};
use crate::routes::{json, Req};
use crate::service;
use crate::state::AppState;
use axum::response::Response;
use base64::Engine;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rusqlite::params;
use serde_json::{json, Value};

/// Patterns from `.gitattributes` lines marked `filter=lfs`.
fn lfs_patterns(attrs: &str) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for line in attrs.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.contains("filter=lfs") {
            continue;
        }
        let Some(pat) = line.split_whitespace().next() else { continue };
        let pat = if pat.contains('/') { pat.trim_start_matches('/').to_string() } else { format!("**/{pat}") };
        if let Ok(g) = Glob::new(&pat) {
            b.add(g);
        }
        if let Some(stripped) = pat.strip_prefix("**/") {
            if let Ok(g) = Glob::new(stripped) {
                b.add(g);
            }
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Simple `.gitignore` matcher (glob lines, no negation).
fn ignore_patterns(gi: &str) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for line in gi.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
            continue;
        }
        let l = l.trim_end_matches('/');
        let pats = if l.starts_with('/') {
            vec![l.trim_start_matches('/').to_string(), format!("{}/**", l.trim_start_matches('/'))]
        } else {
            vec![l.to_string(), format!("**/{l}"), format!("{l}/**"), format!("**/{l}/**")]
        };
        for p in pats {
            if let Ok(g) = Glob::new(&p) {
                b.add(g);
            }
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Resolve the commit a write targets; `None` for an empty repo on `main`.
async fn base_commit(git: &crate::git::Repo, rev: &str) -> ApiResult<Option<String>> {
    match git.resolve(rev).await? {
        Some(s) => Ok(Some(s)),
        None if rev == "main" && git.is_empty().await? => Ok(None),
        None => Err(ApiError::revision_not_found(rev)),
    }
}

async fn read_file_at(git: &crate::git::Repo, sha: &Option<String>, path: &str) -> ApiResult<Option<String>> {
    let Some(sha) = sha else { return Ok(None) };
    match git.stat_path(sha, path).await? {
        Some(e) if e.kind == "file" && e.size < 1_000_000 => {
            Ok(Some(String::from_utf8_lossy(&git.cat_blob(&e.oid).await?).into_owned()))
        }
        _ => Ok(None),
    }
}

pub async fn preupload(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: &str) -> ApiResult<Response> {
    let caller = r.require().await?;
    let (repo, _) = load_repo(&r.st, rtype, ns, name, Some(&caller), Access::Write).await?;
    let b: Value = r.json().await?;
    let git = r.st.git(&repo.disk_id);
    let rev = if rev.is_empty() { "main" } else { rev };
    let sha = if r.q_bool("create_pr") { base_commit(&git, rev).await.unwrap_or(None) } else { base_commit(&git, rev).await? };
    let attrs = read_file_at(&git, &sha, ".gitattributes").await?.unwrap_or_else(|| service::DEFAULT_GITATTRIBUTES.into());
    let lfs = lfs_patterns(&attrs);
    let gitignore = match b.get("gitIgnore").and_then(|v| v.as_str()) {
        Some(g) => Some(g.to_string()),
        None => read_file_at(&git, &sha, ".gitignore").await?,
    };
    let ignore = gitignore.as_deref().map(ignore_patterns);
    let threshold = r.st.cfg.lfs_threshold;
    let mut out = vec![];
    for f in b.get("files").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let size = f.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
        let sample = f
            .get("sample")
            .and_then(|v| v.as_str())
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .unwrap_or_default();
        // binary sniff on the first 512 bytes, like the Hub
        let binary = sample.contains(&0u8);
        let mode = if size >= threshold || lfs.is_match(&path) || (binary && size > 0) { "lfs" } else { "regular" };
        let should_ignore = ignore.as_ref().is_some_and(|g| g.is_match(&path));
        // oid of the current remote file (sha256 for LFS, git sha1 otherwise) so unchanged files are skipped
        let mut oid = Value::Null;
        if let Some(s) = &sha {
            if let Some(e) = git.stat_path(s, &path).await? {
                if e.kind == "file" {
                    let info = service::with_lfs(&git, vec![e]).await?.remove(0);
                    oid = match (&info.lfs, mode) {
                        (Some(p), "lfs") => json!(p.oid),
                        (None, "regular") => json!(info.entry.oid),
                        _ => Value::Null,
                    };
                }
            }
        }
        out.push(json!({ "path": path, "uploadMode": mode, "shouldIgnore": should_ignore, "oid": oid }));
    }
    Ok(json(json!({ "files": out, "commitOid": sha })))
}

pub struct ParsedCommit {
    pub summary: String,
    pub description: String,
    pub parent: Option<String>,
    pub ops: Vec<Op>,
    pub lfs_refs: Vec<(String, i64)>,
}

async fn parse_ndjson(st: &AppState, repo: &RepoRow, git: &crate::git::Repo, base: &Option<String>, body: &[u8]) -> ApiResult<ParsedCommit> {
    let mut pc = ParsedCommit { summary: String::new(), description: String::new(), parent: None, ops: vec![], lfs_refs: vec![] };
    let b64 = base64::engine::general_purpose::STANDARD;
    for line in body.split(|b| *b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let v: Value = serde_json::from_slice(line).map_err(|e| ApiError::bad(format!("Invalid NDJSON line: {e}")))?;
        let key = v.get("key").and_then(|k| k.as_str()).unwrap_or("");
        let val = v.get("value").cloned().unwrap_or(Value::Null);
        let path = || -> ApiResult<String> {
            let p = val.get("path").and_then(|p| p.as_str()).unwrap_or("");
            clean_path(p).ok_or_else(|| ApiError::bad(format!("Invalid path: {p:?}")))
        };
        match key {
            "header" => {
                pc.summary = val.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
                pc.description = val.get("description").and_then(|s| s.as_str()).unwrap_or("").to_string();
                pc.parent = val.get("parentCommit").and_then(|s| s.as_str()).map(String::from);
            }
            "file" => {
                let content = val.get("content").and_then(|c| c.as_str()).unwrap_or("");
                let data = match val.get("encoding").and_then(|e| e.as_str()) {
                    Some("base64") => b64.decode(content).map_err(|_| ApiError::bad("Invalid base64 content"))?,
                    _ => content.as_bytes().to_vec(),
                };
                pc.ops.push(Op::Put { path: path()?, data });
            }
            "lfsFile" => {
                let p = path()?;
                let oid = val.get("oid").and_then(|o| o.as_str()).unwrap_or("").to_ascii_lowercase();
                if !valid_oid(&oid) {
                    return Err(ApiError::bad(format!("Invalid LFS oid for {p}")));
                }
                let stored = st
                    .storage
                    .size(&oid)
                    .await?
                    .ok_or_else(|| ApiError::bad(format!("LFS object {oid} for '{p}' was not uploaded")))?;
                let size = val.get("size").and_then(|s| s.as_i64()).unwrap_or(stored as i64);
                pc.lfs_refs.push((oid.clone(), size));
                pc.ops.push(Op::Put { path: p, data: pointer_text(&oid, size).into_bytes() });
            }
            "deletedFile" => {
                let p = path()?;
                let exists = match base {
                    Some(s) => git.stat_path(s, &p).await?.is_some_and(|e| e.kind == "file"),
                    None => false,
                };
                if !exists {
                    return Err(ApiError::entry_not_found(&p));
                }
                pc.ops.push(Op::Delete { path: p });
            }
            "deletedFolder" => pc.ops.push(Op::DeleteFolder { path: path()? }),
            _ => return Err(ApiError::bad(format!("Unknown commit operation: {key}"))),
        }
    }
    let _ = repo;
    Ok(pc)
}

pub fn commit_message(summary: &str, description: &str) -> String {
    let s = if summary.trim().is_empty() { "Upload files" } else { summary.trim() };
    if description.trim().is_empty() { s.to_string() } else { format!("{s}\n\n{}", description.trim()) }
}

pub async fn commit(r: &mut Req, rtype: &str, ns: &str, name: &str, rev: &str) -> ApiResult<Response> {
    let caller = r.require().await?;
    let create_pr = r.q_bool("create_pr");
    // anyone with read access may open a PR; direct commits need write
    let need = if create_pr { Access::Read } else { Access::Write };
    let (repo, _) = load_repo(&r.st, rtype, ns, name, Some(&caller), need).await?;
    if create_pr && !caller.can_write_token() {
        return Err(ApiError::forbidden("A write token is required to open a pull request"));
    }
    let body = r.bytes(2 * 1024 * 1024 * 1024).await?;
    let rev = if rev.is_empty() { "main".to_string() } else { rev.to_string() };
    let git = r.st.git(&repo.disk_id);
    let lock = r.st.repo_lock(repo.id);
    let _guard = lock.lock().await;

    let base = base_commit(&git, &rev).await?;
    let pc = parse_ndjson(&r.st, &repo, &git, &base, &body).await?;
    let parent = match &pc.parent {
        Some(p) => Some(git.resolve(p).await?.ok_or_else(|| ApiError::revision_not_found(p))?),
        None => base.clone(),
    };
    let msg = commit_message(&pc.summary, &pc.description);
    let sig = service::signature(&caller.user);
    let new = git.build_commit(parent.as_deref(), &pc.ops, &msg, &sig).await?;
    register_lfs(&r.st, repo.id, &pc.lfs_refs).await?;

    if create_pr {
        let pr = crate::api::community::open_pr_from_commit(&r.st, &repo, &caller, &rev, &new, &pc.summary, &pc.description).await?;
        let url = format!("{}/{}/discussions/{}", r.base, repo.web_path(), pr);
        return Ok(json(json!({
            "success": true,
            "commitOid": new,
            "commitUrl": format!("{}/{}/commit/{new}", r.base, repo.web_path()),
            "pullRequestUrl": url,
            "hookOutput": "",
        })));
    }

    // fast-forward only if the branch did not move; with parentCommit the base may differ (Hub rejects non-ff)
    let refname = if rev.starts_with("refs/") { rev.clone() } else { format!("refs/heads/{rev}") };
    if let (Some(p), Some(b)) = (&parent, &base) {
        if p != b && !git.is_ancestor(p, b).await? {
            return Err(ApiError::bad("parentCommit is not an ancestor of the branch head"));
        }
        if p != b {
            // parent is behind head: rebase-free 3-way merge onto current head
            let theirs = new.clone();
            match git.merge_trees(b, &theirs).await? {
                Ok(tree) => {
                    let merged = git.commit_tree(&tree, vec![b.as_str()], &msg, &sig).await?;
                    git.update_ref(&refname, &merged, Some(b)).await?;
                    return finish(r, &repo, &merged).await;
                }
                Err(paths) => return Err(ApiError::conflict(format!("Conflicts on: {}", paths.join(", ")))),
            }
        }
    }
    git.update_ref(&refname, &new, base.as_deref())
        .await
        .map_err(|e| ApiError::conflict(format!("Branch moved during commit, retry: {e}")))?;
    finish(r, &repo, &new).await
}

async fn finish(r: &Req, repo: &RepoRow, oid: &str) -> ApiResult<Response> {
    service::refresh_meta(&r.st, repo).await?;
    Ok(json(json!({
        "success": true,
        "commitOid": oid,
        "commitUrl": format!("{}/{}/commit/{oid}", r.base, repo.web_path()),
        "hookOutput": "",
    })))
}

pub async fn register_lfs(st: &AppState, repo_id: i64, refs: &[(String, i64)]) -> ApiResult<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let refs = refs.to_vec();
    st.db(move |c| {
        let now = crate::util::now_iso();
        for (oid, size) in refs {
            c.execute(
                "INSERT OR IGNORE INTO lfs_objects (repo_id, oid, size, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![repo_id, oid, size, now],
            )?;
        }
        Ok(())
    })
    .await
}

/// Used by git push: register any LFS pointers newly referenced.
pub async fn scan_pointers(st: &AppState, repo: &RepoRow, commit: &str) -> ApiResult<()> {
    let git = st.git(&repo.disk_id);
    let entries = git.ls_tree(commit, "", true).await?;
    let small: Vec<String> = entries
        .iter()
        .filter(|e| e.kind == "file" && (e.size as usize) <= crate::git::lfs::POINTER_MAX)
        .map(|e| e.oid.clone())
        .collect();
    let heads = git.blob_heads(&small).await?;
    let refs: Vec<(String, i64)> = heads.iter().filter_map(|d| parse_pointer(d)).map(|p| (p.oid, p.size)).collect();
    register_lfs(st, repo.id, &refs).await
}

#[allow(dead_code)]
fn _caller_unused(_: &Caller) {}
