//! API errors, mapped to the status codes / `X-Error-Code` headers that
//! `huggingface_hub.utils.hf_raise_for_status` understands.

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: Option<&'static str>,
    pub msg: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self { status, code: None, msg: msg.into() }
    }
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }
    pub fn repo_not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Repository not found").with_code("RepoNotFound")
    }
    pub fn revision_not_found(rev: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("Invalid rev id: {rev}")).with_code("RevisionNotFound")
    }
    pub fn entry_not_found(path: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("Entry not found: {path}")).with_code("EntryNotFound")
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, msg)
    }
    pub fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Invalid credentials in Authorization header")
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, msg)
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, msg)
    }
    pub fn internal(msg: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {msg}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Internal error: {msg}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = (self.status, Json(serde_json::json!({ "error": self.msg }))).into_response();
        let h = resp.headers_mut();
        if let Some(code) = self.code {
            h.insert("X-Error-Code", HeaderValue::from_static(code));
        }
        let safe: String = self
            .msg
            .chars()
            .map(|c| if c.is_ascii() && !c.is_ascii_control() { c } else { ' ' })
            .take(400)
            .collect();
        if let Ok(v) = HeaderValue::from_str(&safe) {
            h.insert("X-Error-Message", v);
        }
        resp
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        Self::internal(e)
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e)
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e)
    }
}
impl From<minijinja::Error> for ApiError {
    fn from(e: minijinja::Error) -> Self {
        Self::internal(format!("{e:#}"))
    }
}
