//! Git smart-HTTP (clone / fetch / push) by proxying to `git http-backend` (CGI).

use crate::models::{load_repo, Access};
use crate::routes::Req;
use crate::service;
use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn challenge(msg: &str) -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, msg.to_string()).into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"HuggingHub\""));
    r
}

pub async fn serve(r: &mut Req, rtype: &str, ns: &str, name: &str, sub: &str) -> Response {
    let is_push = sub == "git-receive-pack" || r.q("service") == Some("git-receive-pack");
    let caller = match r.caller().await {
        Ok(c) => c,
        Err(_) => return challenge("Invalid credentials"),
    };
    let need = if is_push { Access::Write } else { Access::Read };
    let repo = match load_repo(&r.st, rtype, ns, name, caller.as_ref(), need).await {
        Ok((repo, _)) => repo,
        Err(_) if caller.is_none() => return challenge("Authentication required"),
        Err(e) => return e.into_response(),
    };
    let git = r.st.git(&repo.disk_id);
    let before = if is_push { git.resolve("main").await.ok().flatten() } else { None };

    let resp = cgi(r, &repo.disk_id, sub, caller.as_ref().map(|c| c.user.name.clone())).await;

    if is_push && sub == "git-receive-pack" {
        // post-receive: refresh cached metadata + register LFS pointers
        let st = r.st.clone();
        let repo2 = repo.clone();
        let after = git.resolve("main").await.ok().flatten();
        if after != before {
            if let Some(a) = &after {
                let _ = crate::api::commit::scan_pointers(&st, &repo2, a).await;
            }
            let _ = service::refresh_meta(&st, &repo2).await;
        }
    }
    resp
}

async fn cgi(r: &mut Req, disk_id: &str, sub: &str, user: Option<String>) -> Response {
    let st = r.st.clone();
    let path_info = format!("/{disk_id}.git/{sub}");
    let mut cmd = tokio::process::Command::new(&st.cfg.git);
    cmd.arg("http-backend")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("GIT_PROJECT_ROOT", st.cfg.repos_dir())
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("REQUEST_METHOD", r.method.as_str())
        .env("PATH_INFO", &path_info)
        .env("QUERY_STRING", r.uri.query().unwrap_or(""))
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("REMOTE_USER", user.as_deref().unwrap_or(""))
        .env("CONTENT_TYPE", r.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or(""))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = r.headers.get("git-protocol").and_then(|v| v.to_str().ok()) {
        cmd.env("GIT_PROTOCOL", p);
    }
    if let Some(enc) = r.headers.get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()) {
        cmd.env("HTTP_CONTENT_ENCODING", enc);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("git http-backend: {e}")).into_response(),
    };
    let mut stdin = child.stdin.take().unwrap();
    let mut body = r.take_body().into_data_stream();
    let feeder = tokio::spawn(async move {
        while let Some(Ok(chunk)) = body.next().await {
            if stdin.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = stdin.shutdown().await;
    });
    let mut stderr = child.stderr.take().unwrap();
    tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        if !s.trim().is_empty() {
            tracing::warn!("git http-backend: {}", s.trim());
        }
    });
    let mut out = BufReader::new(child.stdout.take().unwrap());

    // parse CGI headers
    let mut status = StatusCode::OK;
    let mut b = Response::builder();
    loop {
        let mut line = String::new();
        match out.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            if k.eq_ignore_ascii_case("status") {
                if let Some(code) = v.split_whitespace().next().and_then(|c| c.parse::<u16>().ok()) {
                    status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
                }
            } else {
                b = b.header(k.trim(), v);
            }
        }
    }
    // stream the rest; for pushes we must finish before post-receive work, so buffer receive-pack
    if sub == "git-receive-pack" {
        let mut rest = vec![];
        let _ = out.read_to_end(&mut rest).await;
        let _ = feeder.await;
        let _ = child.wait().await;
        return b.status(status).body(Body::from(rest)).unwrap();
    }
    tokio::spawn(async move {
        let _ = feeder.await;
        let _ = child.wait().await;
    });
    b.status(status).body(Body::from_stream(tokio_util::io::ReaderStream::new(out))).unwrap()
}
