//! Repo-level logic shared by the API, the git endpoints and the web UI.

use crate::error::{ApiError, ApiResult};
use crate::git::lfs::{parse_pointer, Pointer};
use crate::git::read::TreeEntry;
use crate::git::write::{Op, Signature};
use crate::git::Repo as GitRepo;
use crate::models::{Account, RepoRow};
use crate::state::AppState;
use crate::util::now_iso;
use rusqlite::params;
use serde_json::{json, Value};

pub const DEFAULT_GITATTRIBUTES: &str = "*.7z filter=lfs diff=lfs merge=lfs -text
*.arrow filter=lfs diff=lfs merge=lfs -text
*.bin filter=lfs diff=lfs merge=lfs -text
*.bz2 filter=lfs diff=lfs merge=lfs -text
*.ckpt filter=lfs diff=lfs merge=lfs -text
*.ftz filter=lfs diff=lfs merge=lfs -text
*.gguf filter=lfs diff=lfs merge=lfs -text
*.gz filter=lfs diff=lfs merge=lfs -text
*.h5 filter=lfs diff=lfs merge=lfs -text
*.joblib filter=lfs diff=lfs merge=lfs -text
*.lfs.* filter=lfs diff=lfs merge=lfs -text
*.mlmodel filter=lfs diff=lfs merge=lfs -text
*.model filter=lfs diff=lfs merge=lfs -text
*.msgpack filter=lfs diff=lfs merge=lfs -text
*.npy filter=lfs diff=lfs merge=lfs -text
*.npz filter=lfs diff=lfs merge=lfs -text
*.onnx filter=lfs diff=lfs merge=lfs -text
*.ot filter=lfs diff=lfs merge=lfs -text
*.parquet filter=lfs diff=lfs merge=lfs -text
*.pb filter=lfs diff=lfs merge=lfs -text
*.pickle filter=lfs diff=lfs merge=lfs -text
*.pkl filter=lfs diff=lfs merge=lfs -text
*.pt filter=lfs diff=lfs merge=lfs -text
*.pth filter=lfs diff=lfs merge=lfs -text
*.rar filter=lfs diff=lfs merge=lfs -text
*.safetensors filter=lfs diff=lfs merge=lfs -text
saved_model/**/* filter=lfs diff=lfs merge=lfs -text
*.tar.* filter=lfs diff=lfs merge=lfs -text
*.tar filter=lfs diff=lfs merge=lfs -text
*.tflite filter=lfs diff=lfs merge=lfs -text
*.tgz filter=lfs diff=lfs merge=lfs -text
*.wasm filter=lfs diff=lfs merge=lfs -text
*.xz filter=lfs diff=lfs merge=lfs -text
*.zip filter=lfs diff=lfs merge=lfs -text
*.zst filter=lfs diff=lfs merge=lfs -text
*tfevents* filter=lfs diff=lfs merge=lfs -text
";

pub fn signature(user: &Account) -> Signature {
    Signature {
        name: user.name.clone(),
        email: user.email.clone().unwrap_or_else(|| format!("{}@users.noreply.hugginghub", user.name)),
    }
}

/// Create the DB row and the bare repo with an initial commit.
pub async fn create_repo(
    st: &AppState,
    rtype: &str,
    owner: &Account,
    name: &str,
    private: bool,
    sdk: Option<String>,
    creator: &Account,
) -> ApiResult<RepoRow> {
    let disk_id = format!("{}-{}", rtype, crate::util::object_id());
    let (rt, oid, nm, dk, sd) = (rtype.to_string(), owner.id, name.to_string(), disk_id.clone(), sdk.clone());
    let repo_id = st
        .db(move |c| {
            let now = now_iso();
            let exists: i64 = c.query_row(
                "SELECT COUNT(*) FROM repos WHERE type = ?1 AND owner_id = ?2 AND name = ?3",
                params![rt, oid, nm],
                |r| r.get(0),
            )?;
            if exists > 0 {
                return Ok(None);
            }
            c.execute(
                "INSERT INTO repos (type, owner_id, name, private, sdk, disk_id, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
                params![rt, oid, nm, private as i64, sd, dk, now],
            )?;
            Ok(Some(c.last_insert_rowid()))
        })
        .await?;
    let Some(repo_id) = repo_id else {
        return Err(ApiError::conflict("You already created this repo"));
    };
    let path = st.cfg.repos_dir().join(format!("{disk_id}.git"));
    let git = match GitRepo::init(&st.cfg.git, &path).await {
        Ok(g) => g,
        Err(e) => {
            st.db(move |c| c.execute("DELETE FROM repos WHERE id = ?1", [repo_id])).await?;
            return Err(e.into());
        }
    };
    let mut ops = vec![Op::Put { path: ".gitattributes".into(), data: DEFAULT_GITATTRIBUTES.as_bytes().to_vec() }];
    if rtype == "space" {
        let sdk = sdk.unwrap_or_else(|| "static".into());
        let readme = format!(
            "---\ntitle: {name}\nemoji: 🤗\ncolorFrom: yellow\ncolorTo: red\nsdk: {sdk}\npinned: false\n---\n\n# {name}\n"
        );
        ops.push(Op::Put { path: "README.md".into(), data: readme.into_bytes() });
    }
    let commit = git.build_commit(None, &ops, "initial commit", &signature(creator)).await?;
    git.update_ref("refs/heads/main", &commit, None).await?;
    let repo = st.db(move |c| RepoRow::by_id(c, repo_id)).await?.ok_or_else(ApiError::repo_not_found)?;
    refresh_meta(st, &repo).await
}

/// Parse YAML front matter of a README into JSON.
pub fn parse_card(readme: &str) -> (Value, String) {
    let text = readme.trim_start_matches('\u{feff}');
    if let Some(rest) = text.strip_prefix("---") {
        let rest = rest.trim_start_matches('\r').strip_prefix('\n').unwrap_or(rest);
        let end = rest.find("\n---").map(|i| (i, i + 4));
        if let Some((yend, bstart)) = end {
            let yaml = &rest[..yend];
            let body = rest[bstart..].trim_start_matches(|c| c != '\n').trim_start_matches('\n');
            let val: Value = serde_yaml::from_str::<serde_yaml::Value>(yaml)
                .ok()
                .and_then(|v| serde_json::to_value(v).ok())
                .filter(|v| v.is_object())
                .unwrap_or(json!({}));
            return (val, body.to_string());
        }
    }
    (json!({}), text.to_string())
}

/// Recompute cached card metadata when `main` moved (after API commits or git pushes).
pub async fn refresh_meta(st: &AppState, repo: &RepoRow) -> ApiResult<RepoRow> {
    let git = st.git(&repo.disk_id);
    let head = git.resolve("main").await?;
    if head == repo.head_sha {
        return Ok(repo.clone());
    }
    let mut card = json!({});
    if let Some(h) = &head {
        if let Some(e) = git.stat_path(h, "README.md").await? {
            if e.size < 2_000_000 {
                let data = git.cat_blob(&e.oid).await?;
                card = parse_card(&String::from_utf8_lossy(&data)).0;
            }
        }
    }
    let usage = git.disk_usage().await;
    let id = repo.id;
    let (h2, cs) = (head.clone(), card.to_string());
    st.db(move |c| {
        let lfs: i64 = c.query_row("SELECT COALESCE(SUM(size),0) FROM lfs_objects WHERE repo_id = ?1", [id], |r| r.get(0))?;
        c.execute(
            "UPDATE repos SET head_sha = ?1, card = ?2, used_storage = ?3, updated_at = ?4 WHERE id = ?5",
            params![h2, cs, usage + lfs, now_iso(), id],
        )?;
        RepoRow::by_id(c, id)
    })
    .await?
    .ok_or_else(ApiError::repo_not_found)
}

/// A tree entry with its LFS pointer resolved (if any).
pub struct FileInfo {
    pub entry: TreeEntry,
    pub lfs: Option<Pointer>,
    pub pointer_size: i64,
}

impl FileInfo {
    pub fn size(&self) -> i64 {
        self.lfs.as_ref().map(|p| p.size).unwrap_or(self.entry.size)
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "type": self.entry.kind,
            "oid": self.entry.oid,
            "size": if self.entry.kind == "file" { self.size() } else { 0 },
            "path": self.entry.path,
        });
        if let Some(p) = &self.lfs {
            v["lfs"] = json!({ "oid": p.oid, "size": p.size, "pointerSize": self.pointer_size });
        }
        v
    }
}

/// Detect LFS pointers among tree entries (only small blobs can be pointers).
pub async fn with_lfs(git: &GitRepo, entries: Vec<TreeEntry>) -> ApiResult<Vec<FileInfo>> {
    let small: Vec<String> = entries
        .iter()
        .filter(|e| e.kind == "file" && e.size > 0 && (e.size as usize) <= crate::git::lfs::POINTER_MAX)
        .map(|e| e.oid.clone())
        .collect();
    let heads = git.blob_heads(&small).await?;
    let mut map = std::collections::HashMap::new();
    for (oid, data) in small.iter().zip(heads.iter()) {
        if let Some(p) = parse_pointer(data) {
            map.insert(oid.clone(), p);
        }
    }
    Ok(entries
        .into_iter()
        .map(|e| {
            let lfs = if e.kind == "file" { map.get(&e.oid).cloned() } else { None };
            let ps = e.size;
            FileInfo { entry: e, lfs, pointer_size: ps }
        })
        .collect())
}

/// Tags derived from card metadata, Hub style.
pub fn card_tags(repo: &RepoRow, card: &Value) -> Vec<String> {
    let mut tags: Vec<String> = vec![];
    let push_list = |tags: &mut Vec<String>, v: Option<&Value>, prefix: &str| match v {
        Some(Value::String(s)) => tags.push(format!("{prefix}{s}")),
        Some(Value::Array(a)) => {
            for x in a {
                if let Some(s) = x.as_str() {
                    tags.push(format!("{prefix}{s}"));
                }
            }
        }
        _ => {}
    };
    if let Some(l) = card.get("library_name").and_then(|v| v.as_str()) {
        tags.push(l.to_string());
    }
    push_list(&mut tags, card.get("tags"), "");
    if let Some(p) = card.get("pipeline_tag").and_then(|v| v.as_str()) {
        tags.push(p.to_string());
    }
    push_list(&mut tags, card.get("language"), "");
    push_list(&mut tags, card.get("datasets"), "dataset:");
    push_list(&mut tags, card.get("base_model"), "base_model:");
    push_list(&mut tags, card.get("license"), "license:");
    if repo.rtype == "space" {
        if let Some(s) = &repo.sdk {
            tags.push(s.clone());
        }
    }
    let mut seen = std::collections::HashSet::new();
    tags.retain(|t| seen.insert(t.clone()));
    tags
}

/// The Hub's repo-info JSON (ModelInfo / DatasetInfo / SpaceInfo compatible).
pub fn repo_json(repo: &RepoRow, base: &str, sha: Option<&str>, files: Option<&[FileInfo]>, blobs: bool) -> Value {
    let card = repo.card_json();
    let mut v = json!({
        "_id": format!("{:024x}", repo.id),
        "id": repo.full_id(),
        "author": repo.owner,
        "sha": sha.or(repo.head_sha.as_deref()),
        "lastModified": repo.updated_at,
        "createdAt": repo.created_at,
        "private": repo.private,
        "gated": match repo.gated.as_deref() { Some(g @ ("auto" | "manual")) => json!(g), _ => json!(false) },
        "disabled": false,
        "likes": repo.likes,
        "downloads": repo.downloads,
        "tags": card_tags(repo, &card),
        "usedStorage": repo.used_storage,
        "cardData": card,
    });
    if let Some(files) = files {
        let sib: Vec<Value> = files
            .iter()
            .filter(|f| f.entry.kind == "file")
            .map(|f| {
                let mut s = json!({ "rfilename": f.entry.path });
                if blobs {
                    s["size"] = json!(f.size());
                    s["blobId"] = json!(f.entry.oid);
                    if let Some(p) = &f.lfs {
                        s["lfs"] = json!({ "sha256": p.oid, "size": p.size, "pointerSize": f.pointer_size });
                    }
                }
                s
            })
            .collect();
        v["siblings"] = json!(sib);
    }
    match repo.rtype.as_str() {
        "model" => {
            v["modelId"] = json!(repo.full_id());
            if let Some(l) = card.get("library_name") {
                v["library_name"] = l.clone();
            }
            if let Some(p) = card.get("pipeline_tag") {
                v["pipeline_tag"] = p.clone();
            }
            v["spaces"] = json!([]);
            v["widgetData"] = Value::Null;
        }
        "dataset" => {
            v["description"] = json!("");
            v["citation"] = json!("");
        }
        "space" => {
            v["sdk"] = json!(repo.sdk);
            v["subdomain"] = json!(format!("{}-{}", repo.owner, repo.name).to_lowercase().replace(['_', '.'], "-"));
            v["host"] = json!(format!("{base}/{}", repo.web_path()));
            v["runtime"] = json!({
                "stage": "NO_APP_FILE",
                "hardware": { "current": null, "requested": null },
                "storage": null,
                "gcTimeout": null,
                "replicas": { "current": 0, "requested": 0 },
                "devMode": false,
                "domains": [],
            });
            v["models"] = json!([]);
            v["datasets"] = json!([]);
        }
        _ => {}
    }
    v
}
