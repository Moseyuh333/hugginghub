//! Read-side git operations: resolving revisions, trees, blobs, logs and refs.

use super::Repo;
use anyhow::Result;

#[derive(Clone, Debug)]
pub struct TreeEntry {
    /// "file" or "directory"
    pub kind: &'static str,
    pub path: String,
    pub oid: String,
    pub size: i64,
    pub mode: String,
}

#[derive(Clone, Debug)]
pub struct CommitMeta {
    pub id: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub date: String,
    pub title: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct RefInfo {
    pub name: String,
    pub full: String,
    pub target: String,
}

fn safe_rev(rev: &str) -> bool {
    !rev.is_empty() && !rev.starts_with('-') && !rev.contains("..") && !rev.contains(['\0', '\n', ' ', '~', '^', ':'])
}

impl Repo {
    /// Resolve a branch, tag, `refs/pr/N` or (abbreviated) sha to a full commit id.
    pub async fn resolve(&self, rev: &str) -> Result<Option<String>> {
        if !safe_rev(rev) {
            return Ok(None);
        }
        let candidates = if rev.starts_with("refs/") {
            vec![rev.to_string()]
        } else {
            vec![format!("refs/heads/{rev}"), format!("refs/tags/{rev}"), rev.to_string()]
        };
        for c in candidates {
            let spec = format!("{c}^{{commit}}");
            let o = self.run(&["rev-parse", "--verify", "-q", &spec], None).await?;
            if o.ok {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.len() == 40 {
                    return Ok(Some(s));
                }
            }
        }
        Ok(None)
    }

    pub async fn is_empty(&self) -> Result<bool> {
        let o = self.run(&["for-each-ref", "--count=1", "refs/heads"], None).await?;
        Ok(o.stdout.is_empty())
    }

    /// List a directory (or the whole tree when `recursive`). `dir` is "" for root.
    pub async fn ls_tree(&self, commit: &str, dir: &str, recursive: bool) -> Result<Vec<TreeEntry>> {
        let mut args = vec!["ls-tree", "-l", "-z", "--full-tree"];
        if recursive {
            args.push("-r");
            args.push("-t");
        }
        let spec;
        if dir.is_empty() {
            spec = commit.to_string();
        } else {
            spec = format!("{commit}:{dir}");
        }
        args.push(&spec);
        let o = self.run(&args, None).await?;
        if !o.ok {
            return Ok(vec![]);
        }
        let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_end_matches('/')) };
        let mut out = vec![];
        for rec in o.stdout.split(|b| *b == 0) {
            if rec.is_empty() {
                continue;
            }
            let rec = String::from_utf8_lossy(rec);
            let Some((meta, name)) = rec.split_once('\t') else { continue };
            let parts: Vec<&str> = meta.split_whitespace().collect();
            if parts.len() < 4 {
                continue;
            }
            let kind = match parts[1] {
                "blob" => "file",
                "tree" => "directory",
                _ => continue, // submodules
            };
            out.push(TreeEntry {
                kind,
                path: format!("{prefix}{name}"),
                oid: parts[2].to_string(),
                size: parts[3].parse().unwrap_or(0),
                mode: parts[0].to_string(),
            });
        }
        Ok(out)
    }

    /// Info about a single path at a commit (None if absent).
    pub async fn stat_path(&self, commit: &str, path: &str) -> Result<Option<TreeEntry>> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok(None);
        }
        let o = self.run(&["ls-tree", "-l", "-z", "--full-tree", commit, "--", path], None).await?;
        if !o.ok {
            return Ok(None);
        }
        for rec in o.stdout.split(|b| *b == 0) {
            let rec = String::from_utf8_lossy(rec);
            let Some((meta, name)) = rec.split_once('\t') else { continue };
            if name != path {
                continue;
            }
            let parts: Vec<&str> = meta.split_whitespace().collect();
            let kind = match parts.get(1) {
                Some(&"blob") => "file",
                Some(&"tree") => "directory",
                _ => continue,
            };
            return Ok(Some(TreeEntry {
                kind,
                path: name.to_string(),
                oid: parts[2].to_string(),
                size: parts[3].parse().unwrap_or(0),
                mode: parts[0].to_string(),
            }));
        }
        Ok(None)
    }

    pub async fn cat_blob(&self, oid: &str) -> Result<Vec<u8>> {
        self.ok(&["cat-file", "blob", oid], None, &[]).await
    }

    /// Read the first `n` bytes of many blobs at once (used to detect LFS pointers).
    pub async fn blob_heads(&self, oids: &[String]) -> Result<Vec<Vec<u8>>> {
        if oids.is_empty() {
            return Ok(vec![]);
        }
        let input = oids.join("\n") + "\n";
        let out = self.ok(&["cat-file", "--batch"], Some(input.as_bytes()), &[]).await?;
        let mut res = Vec::with_capacity(oids.len());
        let mut i = 0usize;
        while i < out.len() && res.len() < oids.len() {
            let nl = match out[i..].iter().position(|b| *b == b'\n') {
                Some(p) => i + p,
                None => break,
            };
            let header = String::from_utf8_lossy(&out[i..nl]).to_string();
            i = nl + 1;
            let parts: Vec<&str> = header.split(' ').collect();
            if parts.len() < 3 {
                res.push(vec![]);
                continue;
            }
            let size: usize = parts[2].parse().unwrap_or(0);
            res.push(out[i..(i + size).min(out.len())].to_vec());
            i += size + 1;
        }
        Ok(res)
    }

    /// Commits reachable from `rev`, newest first, optionally limited to a path.
    pub async fn log(&self, rev: &str, path: Option<&str>, skip: usize, limit: usize) -> Result<Vec<CommitMeta>> {
        let skip_s = format!("--skip={skip}");
        let max_s = format!("--max-count={limit}");
        let mut args = vec!["log", "-z", "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%B", &skip_s, &max_s, rev];
        if let Some(p) = path {
            args.push("--");
            args.push(p);
        }
        let o = self.run(&args, None).await?;
        if !o.ok {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for rec in o.stdout.split(|b| *b == 0) {
            let rec = String::from_utf8_lossy(rec);
            let f: Vec<&str> = rec.splitn(6, '\x1f').collect();
            if f.len() < 6 {
                continue;
            }
            let body = f[5].trim_end().to_string();
            let (title, message) = match body.split_once('\n') {
                Some((t, m)) => (t.trim().to_string(), m.trim().to_string()),
                None => (body.trim().to_string(), String::new()),
            };
            out.push(CommitMeta {
                id: f[0].trim().to_string(),
                parents: f[1].split_whitespace().map(String::from).collect(),
                author_name: f[2].to_string(),
                author_email: f[3].to_string(),
                date: crate::util::to_hub_date(f[4]),
                title,
                message,
            });
        }
        Ok(out)
    }

    pub async fn commit_count(&self, rev: &str) -> Result<i64> {
        let s = self.ok_str(&["rev-list", "--count", rev]).await.unwrap_or_default();
        Ok(s.parse().unwrap_or(0))
    }

    pub async fn last_commit(&self, rev: &str, path: &str) -> Result<Option<CommitMeta>> {
        let p = if path.is_empty() { None } else { Some(path) };
        Ok(self.log(rev, p, 0, 1).await?.into_iter().next())
    }

    /// All refs under a namespace, e.g. "refs/heads/".
    pub async fn refs(&self, ns: &str) -> Result<Vec<RefInfo>> {
        let o = self
            .run(&["for-each-ref", "--format=%(refname)%09%(objectname)%09%(*objectname)", ns], None)
            .await?;
        let mut out = vec![];
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 2 {
                continue;
            }
            // annotated tags: prefer the peeled commit
            let target = if f.len() > 2 && !f[2].is_empty() { f[2] } else { f[1] };
            out.push(RefInfo {
                name: f[0].trim_start_matches(ns).to_string(),
                full: f[0].to_string(),
                target: target.to_string(),
            });
        }
        Ok(out)
    }

    pub async fn diff(&self, base: &str, head: &str) -> Result<String> {
        let o = self.run(&["diff", "--no-color", "--no-ext-diff", base, head], None).await?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    pub async fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>> {
        let o = self.run(&["merge-base", a, b], None).await?;
        Ok(if o.ok { Some(String::from_utf8_lossy(&o.stdout).trim().to_string()) } else { None })
    }

    pub async fn is_ancestor(&self, a: &str, b: &str) -> Result<bool> {
        Ok(self.run(&["merge-base", "--is-ancestor", a, b], None).await?.ok)
    }

    /// Total bytes of objects in the repo (approximate "usedStorage").
    pub async fn disk_usage(&self) -> i64 {
        let s = self.ok_str(&["count-objects", "-v"]).await.unwrap_or_default();
        let mut kb = 0i64;
        for l in s.lines() {
            if let Some(v) = l.strip_prefix("size: ").or_else(|| l.strip_prefix("size-pack: ")) {
                kb += v.trim().parse::<i64>().unwrap_or(0);
            }
        }
        kb * 1024
    }
}
