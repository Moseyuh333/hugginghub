//! Write-side git operations on bare repos: building commits from in-memory
//! operations through a throwaway index, updating refs atomically, merging.

use super::{Repo, ZERO_OID};
use anyhow::{anyhow, Result};

pub enum Op {
    /// Add or replace a file with these exact bytes (an LFS pointer for LFS files).
    Put { path: String, data: Vec<u8> },
    Delete { path: String },
    DeleteFolder { path: String },
}

#[derive(Clone)]
pub struct Signature {
    pub name: String,
    pub email: String,
}

impl Signature {
    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("GIT_AUTHOR_NAME", self.name.clone()),
            ("GIT_AUTHOR_EMAIL", self.email.clone()),
            ("GIT_COMMITTER_NAME", self.name.clone()),
            ("GIT_COMMITTER_EMAIL", self.email.clone()),
        ]
    }
}

pub fn clean_path(p: &str) -> Option<String> {
    let p = p.trim().trim_start_matches("./").trim_matches('/');
    if p.is_empty() || p.contains('\0') || p.contains('\n') || p.contains('\\') {
        return None;
    }
    if p.split('/').any(|seg| seg.is_empty() || seg == "." || seg == ".." || seg == ".git") {
        return None;
    }
    Some(p.to_string())
}

impl Repo {
    pub async fn hash_blob(&self, data: &[u8]) -> Result<String> {
        let out = self.ok(&["hash-object", "-w", "--stdin"], Some(data), &[]).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Apply `ops` on top of `parent` (None = new root) and write a commit.
    /// Returns the new commit id; does NOT move any ref.
    pub async fn build_commit(
        &self,
        parent: Option<&str>,
        ops: &[Op],
        message: &str,
        sig: &Signature,
    ) -> Result<String> {
        let tmp = tempfile_path(&self.path, "index");
        let idx = tmp.to_string_lossy().to_string();
        let env = [("GIT_INDEX_FILE", idx.as_str())];
        let result = async {
            match parent {
                Some(p) => self.ok(&["read-tree", p], None, &env).await?,
                None => self.ok(&["read-tree", "--empty"], None, &env).await?,
            };
            let mut info = String::new();
            for op in ops {
                match op {
                    Op::Put { path, data } => {
                        let oid = self.hash_blob(data).await?;
                        // drop any directory that occupies this path, and any file where a dir must go
                        info.push_str(&format!("100644 {oid}\t{path}\n"));
                    }
                    Op::Delete { path } => {
                        info.push_str(&format!("0 {ZERO_OID}\t{path}\n"));
                    }
                    Op::DeleteFolder { path } => {
                        let pre = format!("{}/", path.trim_end_matches('/'));
                        let files = self.ok(&["ls-files", "-z", "--", &pre], None, &env).await?;
                        for f in files.split(|b| *b == 0).filter(|f| !f.is_empty()) {
                            info.push_str(&format!("0 {ZERO_OID}\t{}\n", String::from_utf8_lossy(f)));
                        }
                    }
                }
            }
            if !info.is_empty() {
                self.ok(&["update-index", "--index-info"], Some(info.as_bytes()), &env).await?;
            }
            let tree = String::from_utf8_lossy(&self.ok(&["write-tree"], None, &env).await?).trim().to_string();
            self.commit_tree(&tree, parent.into_iter().collect(), message, sig).await
        }
        .await;
        let _ = std::fs::remove_file(&tmp);
        result
    }

    pub async fn commit_tree(&self, tree: &str, parents: Vec<&str>, message: &str, sig: &Signature) -> Result<String> {
        let mut args = vec!["commit-tree", tree];
        for p in &parents {
            args.push("-p");
            args.push(p);
        }
        let envs = sig.env();
        let env: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let msg = if message.trim().is_empty() { "Update" } else { message };
        let out = self.ok(&args, Some(msg.as_bytes()), &env).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    pub async fn tree_of(&self, commit: &str) -> Result<String> {
        self.ok_str(&["rev-parse", &format!("{commit}^{{tree}}")]).await
    }

    /// Compare-and-swap a ref. `old` = None means "must not exist".
    pub async fn update_ref(&self, refname: &str, new: &str, old: Option<&str>) -> Result<()> {
        let old = old.unwrap_or(ZERO_OID);
        self.ok(&["update-ref", refname, new, old], None, &[]).await.map(|_| ())
    }

    pub async fn delete_ref(&self, refname: &str) -> Result<()> {
        self.ok(&["update-ref", "-d", refname], None, &[]).await.map(|_| ())
    }

    pub async fn create_tag(&self, name: &str, commit: &str, message: Option<&str>, sig: &Signature) -> Result<()> {
        let envs = sig.env();
        let env: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        match message.filter(|m| !m.trim().is_empty()) {
            Some(m) => self.ok(&["tag", "-a", name, commit, "-m", m], None, &env).await?,
            None => self.ok(&["tag", name, commit], None, &env).await?,
        };
        Ok(())
    }

    /// Three-way merge of `theirs` into `ours`.
    /// Ok(Ok(tree)) on success, Ok(Err(conflicting paths)) on conflict.
    pub async fn merge_trees(&self, ours: &str, theirs: &str) -> Result<std::result::Result<String, Vec<String>>> {
        let o = self
            .run(&["merge-tree", "--write-tree", "--name-only", "--no-messages", ours, theirs], None)
            .await?;
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        let mut lines = text.lines();
        let tree = lines.next().unwrap_or("").trim().to_string();
        if o.ok && tree.len() == 40 {
            return Ok(Ok(tree));
        }
        if o.stdout.is_empty() {
            return Err(anyhow!("merge-tree failed: {}", o.stderr.trim()));
        }
        Ok(Err(lines.map(|s| s.to_string()).filter(|s| !s.is_empty()).collect()))
    }
}

fn tempfile_path(base: &std::path::Path, kind: &str) -> std::path::PathBuf {
    base.join(format!("hh-{kind}-{}", crate::util::random_token(16)))
}
