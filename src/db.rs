//! SQLite storage. One connection behind a mutex is plenty for a self-hosted hub;
//! all heavy lifting (git, file IO) happens outside the lock.

use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

pub type Db = Arc<Mutex<Connection>>;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- users and organizations share one namespace table
CREATE TABLE IF NOT EXISTS accounts (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE COLLATE NOCASE,
    kind         TEXT NOT NULL CHECK (kind IN ('user','org')),
    fullname     TEXT NOT NULL DEFAULT '',
    email        TEXT,
    password     TEXT,
    bio          TEXT NOT NULL DEFAULT '',
    website      TEXT NOT NULL DEFAULT '',
    avatar       TEXT,
    is_admin     INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS org_members (
    org_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    role     TEXT NOT NULL DEFAULT 'read' CHECK (role IN ('admin','write','contributor','read')),
    PRIMARY KEY (org_id, user_id)
);

CREATE TABLE IF NOT EXISTS tokens (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    hash       TEXT NOT NULL UNIQUE,
    prefix     TEXT NOT NULL,
    role       TEXT NOT NULL DEFAULT 'write' CHECK (role IN ('read','write')),
    created_at TEXT NOT NULL,
    last_used  TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
    id         TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS repos (
    id           INTEGER PRIMARY KEY,
    type         TEXT NOT NULL CHECK (type IN ('model','dataset','space')),
    owner_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name         TEXT NOT NULL COLLATE NOCASE,
    private      INTEGER NOT NULL DEFAULT 0,
    gated        TEXT,
    sdk          TEXT,
    app_url      TEXT,
    disk_id      TEXT NOT NULL UNIQUE,
    downloads    INTEGER NOT NULL DEFAULT 0,
    head_sha     TEXT,
    card         TEXT NOT NULL DEFAULT '{}',
    used_storage INTEGER NOT NULL DEFAULT 0,
    discussions_disabled INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (type, owner_id, name)
);

CREATE TABLE IF NOT EXISTS likes (
    user_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    repo_id    INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, repo_id)
);

CREATE TABLE IF NOT EXISTS follows (
    follower_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (follower_id, target_id)
);

-- discussions and pull requests
CREATE TABLE IF NOT EXISTS discussions (
    id          INTEGER PRIMARY KEY,
    repo_id     INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    num         INTEGER NOT NULL,
    title       TEXT NOT NULL,
    author_id   INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    is_pr       INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','closed','merged','draft')),
    target_ref  TEXT,
    merge_oid   TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (repo_id, num)
);

-- events: comment | status-change | commit | title-change
CREATE TABLE IF NOT EXISTS events (
    id            TEXT PRIMARY KEY,
    discussion_id INTEGER NOT NULL REFERENCES discussions(id) ON DELETE CASCADE,
    type          TEXT NOT NULL,
    author_id     INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    data          TEXT NOT NULL,
    created_at    TEXT NOT NULL
);

-- LFS objects known per repo (content lives in data/lfs/<aa>/<bb>/<oid>)
CREATE TABLE IF NOT EXISTS lfs_objects (
    repo_id    INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    oid        TEXT NOT NULL,
    size       INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (repo_id, oid)
);

CREATE INDEX IF NOT EXISTS idx_repos_owner ON repos(owner_id);
CREATE INDEX IF NOT EXISTS idx_events_disc ON events(discussion_id);
"#;

pub fn open(path: &Path) -> anyhow::Result<Db> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(Arc::new(Mutex::new(conn)))
}

/// Post-schema column additions. SQLite has no `ADD COLUMN IF NOT EXISTS`, so
/// each new column is checked against PRAGMA table_info first.
fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let cols: std::collections::HashSet<String> = conn
        .prepare("PRAGMA table_info(repos)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    if !cols.contains("app_url") {
        conn.execute_batch("ALTER TABLE repos ADD COLUMN app_url TEXT")?;
    }
    Ok(())
}
