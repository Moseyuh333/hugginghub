//! Pluggable LFS object storage: local disk or any S3-compatible bucket
//! (AWS S3, MinIO, Cloudflare R2, Backblaze B2, Wasabi, Ceph RGW, ...).
//!
//! Git repositories themselves (small: pointers + text) always live on local
//! disk; every LFS object (weights, datasets, ...) goes through this layer.

pub mod s3;

use anyhow::Result;
use axum::body::Body;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub enum Storage {
    Local(PathBuf),
    S3(s3::S3),
}

/// Object key / relative path for an oid: `lfs/aa/bb/<oid>`.
pub fn rel_key(oid: &str) -> String {
    format!("lfs/{}/{}/{}", &oid[0..2], &oid[2..4], oid)
}

impl Storage {
    pub fn from_config(cfg: &crate::config::Config) -> Result<Storage> {
        match cfg.storage.as_str() {
            "s3" => Ok(Storage::S3(s3::S3::from_env()?)),
            _ => Ok(Storage::Local(cfg.data.clone())),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Storage::Local(p) => format!("local disk ({})", p.join("lfs").display()),
            Storage::S3(s) => format!("s3 ({}/{})", s.endpoint, s.bucket),
        }
    }

    fn local_path(base: &Path, oid: &str) -> PathBuf {
        base.join(rel_key(oid))
    }

    /// Size of a stored object, `None` if absent.
    pub async fn size(&self, oid: &str) -> Result<Option<u64>> {
        match self {
            Storage::Local(b) => Ok(tokio::fs::metadata(Self::local_path(b, oid)).await.ok().map(|m| m.len())),
            Storage::S3(s) => s.head(&rel_key(oid)).await,
        }
    }

    /// Store a fully written + verified temp file under `oid` (the temp file is consumed).
    pub async fn put_file(&self, oid: &str, tmp: &Path, size: u64) -> Result<()> {
        match self {
            Storage::Local(b) => {
                let dst = Self::local_path(b, oid);
                if let Some(p) = dst.parent() {
                    tokio::fs::create_dir_all(p).await?;
                }
                tokio::fs::rename(tmp, &dst).await?;
                Ok(())
            }
            Storage::S3(s) => {
                let res = s.put_file(&rel_key(oid), tmp, size).await;
                let _ = tokio::fs::remove_file(tmp).await;
                res
            }
        }
    }

    /// Stream `len` bytes starting at `start`. `None` if absent.
    pub async fn get(&self, oid: &str, start: u64, len: u64) -> Result<Option<Body>> {
        match self {
            Storage::Local(b) => {
                let Ok(mut f) = tokio::fs::File::open(Self::local_path(b, oid)).await else { return Ok(None) };
                f.seek(std::io::SeekFrom::Start(start)).await?;
                Ok(Some(Body::from_stream(tokio_util::io::ReaderStream::with_capacity(f.take(len), 256 * 1024))))
            }
            Storage::S3(s) => s.get_range(&rel_key(oid), start, len).await,
        }
    }

    /// Presigned direct-download URL (S3 with `HH_S3_REDIRECT=1` only), like the Hub's CDN redirects.
    pub fn presigned_get(&self, oid: &str, filename: &str) -> Option<String> {
        match self {
            Storage::S3(s) if s.redirect => Some(s.presign_get(&rel_key(oid), 3600, Some(filename))),
            _ => None,
        }
    }
}
