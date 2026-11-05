//! Thin async wrapper around the `git` CLI operating on bare repositories.
//! Every hub repo is a bare repo at `data/repos/<disk_id>.git`.

pub mod lfs;
pub mod read;
pub mod write;

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub const ZERO_OID: &str = "0000000000000000000000000000000000000000";

#[derive(Clone, Debug)]
pub struct Repo {
    pub git: String,
    pub path: PathBuf,
}

pub struct Output {
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Repo {
    pub fn new(git: &str, path: impl Into<PathBuf>) -> Self {
        Repo { git: git.to_string(), path: path.into() }
    }

    pub fn cmd(&self) -> Command {
        let mut c = Command::new(&self.git);
        c.arg("--git-dir").arg(&self.path);
        c.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE");
        c
    }

    /// Run git with optional stdin and extra env; never fails on non-zero exit (see `Output::ok`).
    pub async fn run_env(&self, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Result<Output> {
        let mut c = self.cmd();
        c.args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().with_context(|| format!("spawning git {args:?}"))?;
        if let Some(data) = stdin {
            let mut si = child.stdin.take().unwrap();
            let data = data.to_vec();
            // write concurrently so large inputs cannot deadlock against stdout
            tokio::spawn(async move {
                let _ = si.write_all(&data).await;
                let _ = si.shutdown().await;
            });
        }
        let out = child.wait_with_output().await?;
        Ok(Output {
            ok: out.status.success(),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    pub async fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Output> {
        self.run_env(args, stdin, &[]).await
    }

    /// Run and require success, returning stdout.
    pub async fn ok(&self, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Result<Vec<u8>> {
        let o = self.run_env(args, stdin, env).await?;
        if !o.ok {
            return Err(anyhow!("git {} failed: {}", args.join(" "), o.stderr.trim()));
        }
        Ok(o.stdout)
    }

    pub async fn ok_str(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.ok(args, None, &[]).await?).trim().to_string())
    }

    /// `git init --bare` with hub-friendly defaults.
    pub async fn init(git: &str, path: &Path) -> Result<Repo> {
        let out = Command::new(git)
            .args(["init", "--bare", "-q", "-b", "main"])
            .arg(path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .await?;
        if !out.status.success() {
            return Err(anyhow!("git init failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        let r = Repo::new(git, path);
        for (k, v) in [
            ("http.receivepack", "true"),
            ("uploadpack.allowFilter", "true"),
            ("uploadpack.allowAnySHA1InWant", "true"),
            ("receive.denyNonFastForwards", "false"),
            ("gc.auto", "0"),
        ] {
            r.ok(&["config", k, v], None, &[]).await?;
        }
        Ok(r)
    }
}
