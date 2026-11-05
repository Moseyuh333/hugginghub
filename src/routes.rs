//! Request plumbing. Repo ids contain `/` and revisions arrive percent-encoded
//! (`refs%2Fpr%2F1`), so repo-scoped URLs are routed by hand on the *raw*
//! path segments instead of through axum's decoded wildcards.

use crate::auth::{self, Resolved};
use crate::error::{ApiError, ApiResult};
use crate::models::Caller;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use serde::de::DeserializeOwned;

pub struct Req {
    pub st: AppState,
    pub method: Method,
    pub headers: HeaderMap,
    pub uri: Uri,
    pub query: Vec<(String, String)>,
    /// Behind a Mutex so `&Req` stays `Sync` (Body is Send but not Sync).
    pub body: std::sync::Mutex<Option<Body>>,
    pub base: String,
}

impl Req {
    pub fn new(st: AppState, req: Request) -> Self {
        let (parts, body) = req.into_parts();
        let query = serde_urlencoded::from_str(parts.uri.query().unwrap_or("")).unwrap_or_default();
        let base = crate::util::base_url(&st.cfg.public_url, &parts.headers);
        Req { st, method: parts.method, headers: parts.headers, uri: parts.uri, query, body: std::sync::Mutex::new(Some(body)), base }
    }

    pub fn q(&self, k: &str) -> Option<&str> {
        self.query.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }

    pub fn q_all(&self, k: &str) -> Vec<String> {
        self.query.iter().filter(|(a, _)| a == k).map(|(_, v)| v.clone()).collect()
    }

    pub fn q_bool(&self, k: &str) -> bool {
        matches!(self.q(k).map(|s| s.to_ascii_lowercase()).as_deref(), Some("1" | "true" | "yes"))
    }

    pub fn take_body(&mut self) -> Body {
        self.body.lock().ok().and_then(|mut b| b.take()).unwrap_or_else(Body::empty)
    }

    pub async fn bytes(&mut self, limit: usize) -> ApiResult<bytes::Bytes> {
        let body = self.take_body();
        axum::body::to_bytes(body, limit).await.map_err(|e| ApiError::bad(format!("Could not read body: {e}")))
    }

    pub async fn json<T: DeserializeOwned>(&mut self) -> ApiResult<T> {
        let b = self.bytes(64 * 1024 * 1024).await?;
        let b = if b.is_empty() { bytes::Bytes::from_static(b"{}") } else { b };
        serde_json::from_slice(&b).map_err(|e| ApiError::bad(format!("Invalid JSON body: {e}")))
    }

    /// API caller: invalid credentials are a hard 401.
    pub async fn caller(&self) -> ApiResult<Option<Caller>> {
        match auth::resolve(&self.st, &self.headers).await? {
            Resolved::Anonymous => Ok(None),
            Resolved::Caller(c) => Ok(Some(c)),
            Resolved::Invalid => Err(ApiError::unauthorized()),
        }
    }

    pub async fn require(&self) -> ApiResult<Caller> {
        self.caller().await?.ok_or_else(ApiError::unauthorized)
    }

    /// Web caller: bad cookies are ignored.
    pub async fn web_caller(&self) -> Option<Caller> {
        match auth::resolve(&self.st, &self.headers).await {
            Ok(Resolved::Caller(c)) => Some(c),
            _ => None,
        }
    }

    /// URL to the same path with some query params replaced (for Link pagination).
    pub fn url_with(&self, set: &[(&str, String)]) -> String {
        let mut q: Vec<(String, String)> =
            self.query.iter().filter(|(k, _)| !set.iter().any(|(s, _)| s == k)).cloned().collect();
        for (k, v) in set {
            q.push((k.to_string(), v.clone()));
        }
        let qs = serde_urlencoded::to_string(&q).unwrap_or_default();
        format!("{}{}?{}", self.base, self.uri.path(), qs)
    }
}

pub fn json(v: serde_json::Value) -> Response {
    axum::Json(v).into_response()
}

pub fn json_status(status: StatusCode, v: serde_json::Value) -> Response {
    (status, axum::Json(v)).into_response()
}

/// JSON list with an optional `Link: <next>; rel="next"` header.
pub fn json_paged(v: serde_json::Value, next: Option<String>) -> Response {
    let mut r = json(v);
    if let Some(n) = next {
        if let Ok(h) = HeaderValue::from_str(&format!("<{n}>; rel=\"next\"")) {
            r.headers_mut().insert("link", h);
        }
    }
    r
}

pub fn ok_or_err(r: ApiResult<Response>) -> Response {
    r.unwrap_or_else(|e| e.into_response())
}

/// Decoded path segments (empty segments dropped).
pub fn segments(uri: &Uri) -> Vec<String> {
    uri.path()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
        .collect()
}

/// Fallback for everything not matched by a static route.
pub async fn dispatch(State(st): State<AppState>, req: Request) -> Response {
    let mut r = Req::new(st, req);
    let segs = segments(&r.uri);
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    match s.as_slice() {
        ["api", rest @ ..] => ok_or_err(crate::api::dispatch(&mut r, rest).await),
        [t @ ("datasets" | "spaces"), rest @ ..] if !rest.is_empty() => {
            let rtype = if *t == "datasets" { "dataset" } else { "space" };
            if rest.len() >= 2 && is_web_repo_action(rest[1]) {
                repo_route(&mut r, rtype, "", rest[0], &rest[1..]).await
            } else if rest.len() >= 2 {
                repo_route(&mut r, rtype, rest[0], rest[1], &rest[2..]).await
            } else {
                repo_route(&mut r, rtype, "", rest[0], &[]).await
            }
        }
        [first, rest @ ..] if is_web_repo_action(first) => repo_route(&mut r, "model", "", first, rest).await,
        [first, rest @ ..] => {
            // `/{repo}/<action>...` (bare, canonical id) vs `/{ns}/{repo}/...`
            if !rest.is_empty() && is_web_repo_action(rest[0]) {
                repo_route(&mut r, "model", "", first, rest).await
            } else if rest.is_empty() {
                ok_or_err(crate::web::profile::profile(&mut r, first).await)
            } else {
                repo_route(&mut r, "model", first, rest[0], &rest[1..]).await
            }
        }
        _ => crate::web::not_found(&r).await,
    }
}

/// Path segments that mean "this is a repo URL" rather than a profile URL.
pub fn is_web_repo_action(seg: &str) -> bool {
    matches!(
        seg,
        "resolve" | "tree" | "blob" | "raw" | "commits" | "commit" | "community" | "discussions" | "settings"
            | "upload" | "new-file" | "edit" | "like" | "app" | "info" | "git-upload-pack" | "git-receive-pack"
    )
}

async fn repo_route(r: &mut Req, rtype: &str, ns: &str, name: &str, rest: &[&str]) -> Response {
    let (name, dot_git) = match name.strip_suffix(".git") {
        Some(n) => (n, true),
        None => (name, false),
    };
    match rest {
        ["info", "lfs", lrest @ ..] => ok_or_err(crate::api::lfs::git_lfs(r, rtype, ns, name, lrest).await),
        ["info", "refs"] | ["git-upload-pack"] | ["git-receive-pack"] => {
            crate::githttp::serve(r, rtype, ns, name, &rest.join("/")).await
        }
        ["resolve", rev, path @ ..] if !path.is_empty() && !dot_git => {
            ok_or_err(crate::api::files::resolve(r, rtype, ns, name, rev, &path.join("/")).await)
        }
        _ if dot_git => crate::web::not_found(r).await,
        _ => ok_or_err(crate::web::repo::route(r, rtype, ns, name, rest).await),
    }
}
