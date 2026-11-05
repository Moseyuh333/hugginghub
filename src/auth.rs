//! Authentication: `Authorization: Bearer hf_...` tokens (also accepted as the
//! password in HTTP Basic auth for git), and cookie sessions for the web UI.

use crate::error::{ApiError, ApiResult};
use crate::models::{Account, Caller, ACCOUNT_COLS};
use crate::state::AppState;
use crate::util::{now_iso, random_token, sha256_hex};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use base64::Engine;
use rusqlite::{params, OptionalExtension};

pub const SESSION_COOKIE: &str = "hh_session";

pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(pw.as_bytes(), &salt).expect("argon2").to_string()
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false)
}

/// Create a new access token; returns the plaintext (shown once).
pub fn new_token(c: &rusqlite::Connection, user_id: i64, name: &str, role: &str) -> rusqlite::Result<String> {
    let tok = format!("hf_{}", random_token(34));
    c.execute(
        "INSERT INTO tokens (user_id, name, hash, prefix, role, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![user_id, name, sha256_hex(tok.as_bytes()), &tok[..7], role, now_iso()],
    )?;
    Ok(tok)
}

pub fn new_session(c: &rusqlite::Connection, user_id: i64) -> rusqlite::Result<String> {
    let sid = random_token(40);
    c.execute(
        "INSERT INTO sessions (id, user_id, created_at) VALUES (?1, ?2, ?3)",
        params![sha256_hex(sid.as_bytes()), user_id, now_iso()],
    )?;
    Ok(sid)
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all("cookie").iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// Pull a token out of Bearer or Basic auth.
fn header_token(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("authorization")?.to_str().ok()?.trim();
    if let Some(t) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
        return Some(t.trim().to_string());
    }
    if let Some(b) = v.strip_prefix("Basic ").or_else(|| v.strip_prefix("basic ")) {
        let dec = base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()?;
        let s = String::from_utf8(dec).ok()?;
        let (_user, pass) = s.split_once(':')?;
        return Some(pass.to_string());
    }
    None
}

pub enum Resolved {
    Anonymous,
    Caller(Caller),
    /// A credential was presented but is invalid.
    Invalid,
}

/// Resolve the caller from headers. Basic auth may also carry username+password.
pub async fn resolve(st: &AppState, headers: &HeaderMap) -> ApiResult<Resolved> {
    let tok = header_token(headers);
    let basic = basic_credentials(headers);
    let sid = cookie(headers, SESSION_COOKIE);
    if tok.is_none() && sid.is_none() {
        return Ok(Resolved::Anonymous);
    }
    st.db(move |c| {
        if let Some(tok) = tok {
            if tok.starts_with("hf_") {
                let row: Option<(i64, String, String)> = c
                    .query_row(
                        "SELECT user_id, role, name FROM tokens WHERE hash = ?1",
                        [sha256_hex(tok.as_bytes())],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                if let Some((uid, role, name)) = row {
                    c.execute("UPDATE tokens SET last_used = ?1 WHERE hash = ?2", params![now_iso(), sha256_hex(tok.as_bytes())])?;
                    if let Some(user) = Account::by_id(c, uid)? {
                        return Ok(Resolved::Caller(Caller { user, role, token_name: Some(name) }));
                    }
                }
                return Ok(Resolved::Invalid);
            }
            // Basic auth with username + account password (git over HTTP convenience).
            if let Some((user, pass)) = basic {
                let row: Option<(String, Option<String>)> = c
                    .query_row(
                        &format!("SELECT {ACCOUNT_COLS}, password FROM accounts WHERE name = ?1 AND kind = 'user'"),
                        [&user],
                        |r| Ok((r.get::<_, String>(1)?, r.get(10)?)),
                    )
                    .optional()?;
                if let Some((_, Some(hash))) = row {
                    if verify_password(&pass, &hash) {
                        let acc = Account::by_name(c, &user)?.unwrap();
                        return Ok(Resolved::Caller(Caller { user: acc, role: "write".into(), token_name: None }));
                    }
                }
            }
            return Ok(Resolved::Invalid);
        }
        if let Some(sid) = sid {
            let uid: Option<i64> = c
                .query_row("SELECT user_id FROM sessions WHERE id = ?1", [sha256_hex(sid.as_bytes())], |r| r.get(0))
                .optional()?;
            if let Some(uid) = uid {
                if let Some(user) = Account::by_id(c, uid)? {
                    return Ok(Resolved::Caller(Caller { user, role: "write".into(), token_name: None }));
                }
            }
        }
        Ok(Resolved::Anonymous)
    })
    .await
}

fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers.get("authorization")?.to_str().ok()?.trim();
    let b = v.strip_prefix("Basic ").or_else(|| v.strip_prefix("basic "))?;
    let s = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()?).ok()?;
    let (u, p) = s.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

/// Extractor: optional caller. Invalid credentials -> 401 (like the Hub).
pub struct MaybeUser(pub Option<Caller>);

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        match resolve(st, &parts.headers).await? {
            Resolved::Anonymous => Ok(MaybeUser(None)),
            Resolved::Caller(c) => Ok(MaybeUser(Some(c))),
            Resolved::Invalid => Err(ApiError::unauthorized()),
        }
    }
}

/// Extractor: required caller.
pub struct User(pub Caller);

impl FromRequestParts<AppState> for User {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        match MaybeUser::from_request_parts(parts, st).await?.0 {
            Some(c) => Ok(User(c)),
            None => Err(ApiError::unauthorized()),
        }
    }
}

impl OptionalFromRequestParts<AppState> for User {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Option<Self>, Self::Rejection> {
        Ok(<MaybeUser as FromRequestParts<AppState>>::from_request_parts(parts, st).await?.0.map(User))
    }
}

/// Extractor for web pages: a session-only caller; bad cookies are just ignored.
pub struct WebUser(pub Option<Caller>);

impl FromRequestParts<AppState> for WebUser {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        match resolve(st, &parts.headers).await {
            Ok(Resolved::Caller(c)) => Ok(WebUser(Some(c))),
            _ => Ok(WebUser(None)),
        }
    }
}
