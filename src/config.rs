//! Runtime configuration from environment variables.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// Address to bind, e.g. `0.0.0.0:8080` (HH_BIND).
    pub bind: String,
    /// Data dir holding `hub.db`, `repos/`, `lfs/`, `tmp/` (HH_DATA).
    pub data: PathBuf,
    /// Externally visible base URL, e.g. `https://hub.example.com` (HH_PUBLIC_URL).
    /// If unset it is derived from the request Host header.
    pub public_url: Option<String>,
    /// Allow open sign-up (HH_OPEN_SIGNUP, default true).
    pub open_signup: bool,
    /// Path to the `git` binary (HH_GIT).
    pub git: String,
    /// Files larger than this are stored through LFS on preupload (HH_LFS_THRESHOLD, default 10MB).
    pub lfs_threshold: i64,
    /// Instance name shown in the UI (HH_SITE_NAME).
    pub site_name: String,
    /// LFS object storage backend: `local` (default) or `s3` (HH_STORAGE). See `storage::s3` for S3 vars.
    pub storage: String,
}

impl Config {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Config {
            bind: env("HH_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()),
            data: PathBuf::from(env("HH_DATA").unwrap_or_else(|| "./data".into())),
            public_url: env("HH_PUBLIC_URL").map(|s| s.trim_end_matches('/').to_string()),
            open_signup: env("HH_OPEN_SIGNUP").map(|v| v != "0" && v != "false").unwrap_or(true),
            git: env("HH_GIT").unwrap_or_else(|| "git".into()),
            lfs_threshold: env("HH_LFS_THRESHOLD").and_then(|v| v.parse().ok()).unwrap_or(10 * 1024 * 1024),
            site_name: env("HH_SITE_NAME").unwrap_or_else(|| "HuggingHub".into()),
            storage: env("HH_STORAGE").unwrap_or_else(|| "local".into()),
        }
    }

    pub fn repos_dir(&self) -> PathBuf {
        self.data.join("repos")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.data.join("tmp")
    }
    pub fn avatars_dir(&self) -> PathBuf {
        self.data.join("avatars")
    }
}
