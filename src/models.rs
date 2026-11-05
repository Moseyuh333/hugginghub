//! Accounts and repos as loaded from SQLite, plus permission checks.

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub fullname: String,
    pub email: Option<String>,
    pub bio: String,
    pub website: String,
    pub avatar: Option<String>,
    pub is_admin: bool,
    pub created_at: String,
}

pub const ACCOUNT_COLS: &str = "id, name, kind, fullname, email, bio, website, avatar, is_admin, created_at";

impl Account {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Account {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            fullname: r.get(3)?,
            email: r.get(4)?,
            bio: r.get(5)?,
            website: r.get(6)?,
            avatar: r.get(7)?,
            is_admin: r.get::<_, i64>(8)? != 0,
            created_at: r.get(9)?,
        })
    }

    pub fn display_name(&self) -> &str {
        if self.fullname.is_empty() { &self.name } else { &self.fullname }
    }

    pub fn avatar_url(&self, base: &str) -> String {
        match &self.avatar {
            Some(_) => format!("{base}/avatars/{}", self.name),
            None => format!("{base}/avatars/{}.svg", self.name),
        }
    }

    pub fn by_name(c: &Connection, name: &str) -> rusqlite::Result<Option<Account>> {
        c.query_row(&format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE name = ?1"), [name], Account::from_row)
            .optional()
    }

    pub fn by_id(c: &Connection, id: i64) -> rusqlite::Result<Option<Account>> {
        c.query_row(&format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE id = ?1"), [id], Account::from_row)
            .optional()
    }

    /// Role of `user_id` inside this org (None if not a member / not an org).
    pub fn org_role(c: &Connection, org_id: i64, user_id: i64) -> rusqlite::Result<Option<String>> {
        c.query_row(
            "SELECT role FROM org_members WHERE org_id = ?1 AND user_id = ?2",
            params![org_id, user_id],
            |r| r.get(0),
        )
        .optional()
    }

    pub fn orgs_of(c: &Connection, user_id: i64) -> rusqlite::Result<Vec<(Account, String)>> {
        let mut st = c.prepare(&format!(
            "SELECT {}, m.role FROM accounts a JOIN org_members m ON m.org_id = a.id WHERE m.user_id = ?1 ORDER BY a.name",
            ACCOUNT_COLS.split(", ").map(|c| format!("a.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let rows = st.query_map([user_id], |r| Ok((Account::from_row(r)?, r.get::<_, String>(10)?)))?;
        rows.collect()
    }
}

/// The authenticated caller, if any.
#[derive(Clone, Debug)]
pub struct Caller {
    pub user: Account,
    /// "write" or "read" (token role); web sessions are "write".
    pub role: String,
    pub token_name: Option<String>,
}

impl Caller {
    pub fn can_write_token(&self) -> bool {
        self.role == "write"
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoRow {
    pub id: i64,
    pub rtype: String,
    pub owner_id: i64,
    pub owner: String,
    pub owner_kind: String,
    pub name: String,
    pub private: bool,
    pub gated: Option<String>,
    pub sdk: Option<String>,
    pub disk_id: String,
    pub downloads: i64,
    pub head_sha: Option<String>,
    pub card: String,
    pub used_storage: i64,
    pub discussions_disabled: bool,
    pub created_at: String,
    pub updated_at: String,
    pub likes: i64,
    pub app_url: Option<String>,
}

pub const REPO_SELECT: &str = "SELECT r.id, r.type, r.owner_id, a.name, a.kind, r.name, r.private, r.gated, r.sdk,
    r.disk_id, r.downloads, r.head_sha, r.card, r.used_storage, r.discussions_disabled, r.created_at, r.updated_at,
    (SELECT COUNT(*) FROM likes l WHERE l.repo_id = r.id), r.app_url
    FROM repos r JOIN accounts a ON a.id = r.owner_id";

impl RepoRow {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(RepoRow {
            id: r.get(0)?,
            rtype: r.get(1)?,
            owner_id: r.get(2)?,
            owner: r.get(3)?,
            owner_kind: r.get(4)?,
            name: r.get(5)?,
            private: r.get::<_, i64>(6)? != 0,
            gated: r.get(7)?,
            sdk: r.get(8)?,
            disk_id: r.get(9)?,
            downloads: r.get(10)?,
            head_sha: r.get(11)?,
            card: r.get(12)?,
            used_storage: r.get(13)?,
            discussions_disabled: r.get::<_, i64>(14)? != 0,
            created_at: r.get(15)?,
            updated_at: r.get(16)?,
            likes: r.get(17)?,
            app_url: r.get(18).ok(),
        })
    }

    pub fn full_id(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Path part of the web URL: `user/repo`, `datasets/user/repo`, `spaces/user/repo`.
    pub fn web_path(&self) -> String {
        format!("{}{}", crate::util::url_prefix(&self.rtype), self.full_id())
    }

    pub fn card_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.card).unwrap_or(serde_json::json!({}))
    }

    pub fn find(c: &Connection, rtype: &str, owner: &str, name: &str) -> rusqlite::Result<Option<RepoRow>> {
        c.query_row(
            &format!("{REPO_SELECT} WHERE r.type = ?1 AND a.name = ?2 AND r.name = ?3"),
            params![rtype, owner, name],
            RepoRow::from_row,
        )
        .optional()
    }

    pub fn by_id(c: &Connection, id: i64) -> rusqlite::Result<Option<RepoRow>> {
        c.query_row(&format!("{REPO_SELECT} WHERE r.id = ?1"), [id], RepoRow::from_row).optional()
    }

    /// Look up a repo by name alone (canonical, namespace-less URL like `gpt2`).
    /// Ambiguous names (same repo name under several owners) are not resolved.
    pub fn find_by_name(c: &Connection, rtype: &str, name: &str) -> rusqlite::Result<Option<RepoRow>> {
        let mut st = c.prepare(&format!("{REPO_SELECT} WHERE r.type = ?1 AND r.name = ?2 LIMIT 2"))?;
        let rows = st.query_map(params![rtype, name], RepoRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(if rows.len() == 1 { rows.into_iter().next() } else { None })
    }
}

/// Access level of a caller on a repo.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum Access {
    None,
    Read,
    Write,
    Admin,
}

pub fn repo_access(c: &Connection, repo: &RepoRow, caller: Option<&Caller>) -> rusqlite::Result<Access> {
    let base = if repo.private { Access::None } else { Access::Read };
    let Some(caller) = caller else { return Ok(base) };
    let u = &caller.user;
    let mut acc = if u.is_admin || repo.owner_id == u.id {
        Access::Admin
    } else if repo.owner_kind == "org" {
        match Account::org_role(c, repo.owner_id, u.id)?.as_deref() {
            Some("admin") => Access::Admin,
            Some("write") => Access::Write,
            Some(_) => Access::Read,
            None => base,
        }
    } else {
        base
    };
    if !caller.can_write_token() && acc > Access::Read {
        acc = Access::Read;
    }
    Ok(acc)
}

/// Whether `caller` may create repos under `namespace`.
pub fn can_write_namespace(c: &Connection, ns: &Account, caller: &Caller) -> rusqlite::Result<bool> {
    if !caller.can_write_token() {
        return Ok(false);
    }
    if caller.user.is_admin || ns.id == caller.user.id {
        return Ok(true);
    }
    if ns.kind == "org" {
        return Ok(matches!(Account::org_role(c, ns.id, caller.user.id)?.as_deref(), Some("admin" | "write")));
    }
    Ok(false)
}

/// Load a repo and check the caller can at least `need` it. Hidden repos yield RepoNotFound.
pub async fn load_repo(
    st: &AppState,
    rtype: &str,
    owner: &str,
    name: &str,
    caller: Option<&Caller>,
    need: Access,
) -> ApiResult<(RepoRow, Access)> {
    let (rtype, owner, name, caller_c) = (rtype.to_string(), owner.to_string(), name.to_string(), caller.cloned());
    let res = st
        .db(move |c| {
            let found = if owner.is_empty() {
                RepoRow::find_by_name(c, &rtype, &name)?
            } else {
                RepoRow::find(c, &rtype, &owner, &name)?
            };
            let Some(repo) = found else { return Ok(None) };
            let acc = repo_access(c, &repo, caller_c.as_ref())?;
            Ok(Some((repo, acc)))
        })
        .await?;
    match res {
        None => Err(ApiError::repo_not_found()),
        Some((_, Access::None)) => Err(ApiError::repo_not_found()),
        Some((repo, acc)) if acc >= need => Ok((repo, acc)),
        Some(_) => match caller {
            None => Err(ApiError::unauthorized()),
            Some(_) => Err(ApiError::forbidden("You don't have write access to this repository")),
        },
    }
}
