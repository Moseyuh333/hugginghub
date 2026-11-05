//! Shared application state.

use crate::config::Config;
use crate::db::Db;
use crate::git::Repo as GitRepo;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

pub struct Inner {
    pub cfg: Config,
    pub db: Db,
    pub tmpl: minijinja::Environment<'static>,
    pub storage: crate::storage::Storage,
    /// Per-repo write locks so concurrent commits to one repo serialize.
    pub repo_locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    /// Short-lived LFS transfer tickets: ticket -> (repo_id, oid, write, expiry).
    /// huggingface_hub PUTs LFS content WITHOUT auth headers, so action hrefs are self-authorizing.
    pub lfs_tickets: Mutex<HashMap<String, Ticket>>,
    /// Content hash of the embedded CSS/JS, used to version asset URLs so a
    /// deploy is never served stale from a browser cache.
    pub asset_ver: String,
}

#[derive(Clone)]
pub struct Ticket {
    pub repo_id: i64,
    pub oid: String,
    pub write: bool,
    pub expires: std::time::Instant,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn git(&self, disk_id: &str) -> GitRepo {
        GitRepo::new(&self.cfg.git, self.cfg.repos_dir().join(format!("{disk_id}.git")))
    }

    pub fn issue_ticket(&self, repo_id: i64, oid: &str, write: bool) -> String {
        let t = crate::util::random_token(32);
        let mut m = self.lfs_tickets.lock();
        let now = std::time::Instant::now();
        m.retain(|_, v| v.expires > now);
        m.insert(
            t.clone(),
            Ticket { repo_id, oid: oid.to_string(), write, expires: now + std::time::Duration::from_secs(6 * 3600) },
        );
        t
    }

    pub fn check_ticket(&self, ticket: &str, repo_id: i64, oid: &str, write: bool) -> bool {
        let m = self.lfs_tickets.lock();
        m.get(ticket).is_some_and(|t| {
            t.repo_id == repo_id && t.oid == oid && (t.write || !write) && t.expires > std::time::Instant::now()
        })
    }

    pub fn repo_lock(&self, repo_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        self.repo_locks.lock().entry(repo_id).or_default().clone()
    }

    /// Run a closure with the DB connection on the blocking pool.
    pub async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, crate::error::ApiError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let c = db.lock();
            f(&c)
        })
        .await
        .map_err(crate::error::ApiError::internal)?
        .map_err(Into::into)
    }
}
