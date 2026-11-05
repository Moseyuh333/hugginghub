//! HuggingHub — a self-hosted, Hugging Face Hub compatible server.

mod api;
mod auth;
mod config;
mod db;
mod error;
mod githttp;
mod git;
mod models;
mod routes;
mod service;
mod state;
mod storage;
mod util;
mod web;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, serve};
use state::{AppState, Inner};
use std::sync::Arc;

async fn static_asset(axum::extract::Path(name): axum::extract::Path<String>, headers: axum::http::HeaderMap) -> Response {
    let inm = headers.get(axum::http::header::IF_NONE_MATCH).and_then(|v| v.to_str().ok());
    web::assets::serve(&name, inm).await
}

async fn avatar(State(st): State<AppState>, axum::extract::Path(name): axum::extract::Path<String>, req: axum::extract::Request) -> Response {
    let mut r = routes::Req::new(st, req);
    routes::ok_or_err(web::avatar(&mut r, &name).await)
}

type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// Wrap a `&mut Req` page function into an axum handler.
macro_rules! page {
    ($f:path) => {{
        |State(st): State<AppState>, req: axum::extract::Request| async move {
            let mut r = routes::Req::new(st, req);
            routes::ok_or_err($f(&mut r).await)
        }
    }};
}

async fn home(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::home(r).await
}
async fn models_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::listing(r, "model").await
}
async fn datasets_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::listing(r, "dataset").await
}
async fn spaces_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::listing(r, "space").await
}
async fn docs_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::docs(r).await
}
async fn login_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::login(r).await
}
async fn join_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::join(r).await
}
async fn logout_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::logout(r).await
}
async fn profile_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    let segs = routes::segments(&r.uri);
    let name = segs.last().cloned().unwrap_or_default();
    web::profile::profile(r, &name).await
}
async fn follow_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    // POST /{user}/follow -- the username is the first segment
    let segs = routes::segments(&r.uri);
    let name = segs.first().cloned().unwrap_or_default();
    web::profile::follow(r, &name).await
}

// ---- admin panel ----
async fn admin_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::admin::index(r, me).await
}
async fn admin_users(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::admin::users(r, me).await
}
async fn admin_user_new(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::admin::create_user(r, me).await
}
async fn admin_user_admin(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    let name = routes::segments(&r.uri).get(2).cloned().unwrap_or_default();
    web::admin::toggle_admin(r, me, &name).await
}
async fn admin_user_password(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    let name = routes::segments(&r.uri).get(2).cloned().unwrap_or_default();
    web::admin::set_password(r, me, &name).await
}
async fn admin_user_delete(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    let name = routes::segments(&r.uri).get(2).cloned().unwrap_or_default();
    web::admin::delete_user(r, me, &name).await
}
async fn admin_repos(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::admin::repos(r, me).await
}
async fn admin_repo_delete(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::admin::delete_repo(r, me).await
}
async fn admin_orgs(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::admin::orgs(r, me).await
}
async fn admin_server(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::admin::server(r, me).await
}
async fn members_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    let segs = routes::segments(&r.uri);
    let name = segs[0].clone();
    web::profile::add_member(r, &name).await
}
async fn settings_profile(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::account::settings_profile(r, me).await
}
async fn settings_tokens(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::account::settings_tokens(r, me).await
}
async fn settings_password(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::account::settings_password(r, me).await
}
async fn token_delete(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    let segs = routes::segments(&r.uri);
    let id = segs.get(2).cloned().unwrap_or_default();
    web::account::delete_token(r, me, &id).await
}
async fn orgs_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    let me = r.web_caller().await;
    web::account::orgs_list(r, me).await
}
async fn new_org_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::new_org(r).await
}
async fn new_model_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::new_repo(r, "model").await
}
async fn new_dataset_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::new_repo(r, "dataset").await
}
async fn new_space_page(r: &mut routes::Req) -> error::ApiResult<Response> {
    web::account::new_repo(r, "space").await
}

fn app(st: AppState) -> Router {
    Router::new()
        .route("/", get(page!(home)))
        .route("/models", get(page!(models_page)))
        .route("/datasets", get(page!(datasets_page)))
        .route("/spaces", get(page!(spaces_page)))
        .route("/docs", get(page!(docs_page)))
        .route("/login", get(page!(login_page)).post(page!(login_page)))
        .route("/join", get(page!(join_page)).post(page!(join_page)))
        .route("/logout", post(page!(logout_page)))
        .route("/organizations", get(page!(orgs_page)))
        .route("/organizations/new", get(page!(new_org_page)).post(page!(new_org_page)))
        .route("/new", get(page!(new_model_page)).post(page!(new_model_page)))
        .route("/new-dataset", get(page!(new_dataset_page)).post(page!(new_dataset_page)))
        .route("/new-space", get(page!(new_space_page)).post(page!(new_space_page)))
        .route("/settings/profile", get(page!(settings_profile)).post(page!(settings_profile)))
        .route("/settings/tokens", get(page!(settings_tokens)).post(page!(settings_tokens)))
        .route("/settings/tokens/{id}/delete", post(page!(token_delete)))
        .route("/settings/password", get(page!(settings_password)).post(page!(settings_password)))
        .route("/admin", get(page!(admin_page)))
        .route("/admin/users", get(page!(admin_users)))
        .route("/admin/users/new", post(page!(admin_user_new)))
        .route("/admin/users/{name}/admin", post(page!(admin_user_admin)))
        .route("/admin/users/{name}/password", post(page!(admin_user_password)))
        .route("/admin/users/{name}/delete", post(page!(admin_user_delete)))
        .route("/admin/repos", get(page!(admin_repos)))
        .route("/admin/repos/delete", post(page!(admin_repo_delete)))
        .route("/admin/orgs", get(page!(admin_orgs)))
        .route("/admin/server", get(page!(admin_server)))
        .route("/static/{name}", get(static_asset))
        .route("/avatars/{name}", get(avatar))
        .route("/{name}/follow", post(page!(follow_page)))
        .route("/{org}/members", post(page!(members_page)))
        .route("/{name}", get(page!(profile_page)))
        .fallback(routes::dispatch)
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(st)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "create-user" {
        return cli_create_user(&args[2..]);
    }
    if args.len() > 1 && args[1] == "create-token" {
        return cli_create_token(&args[2..]);
    }
    if args.len() > 1 && args[1] == "info" {
        let cfg = config::Config::from_env();
        let storage = storage::Storage::from_config(&cfg)?;
        println!("data dir: {}", cfg.data.display());
        println!("repos:    {}", cfg.repos_dir().display());
        println!("lfs:      {}", storage.describe());
        return Ok(());
    }
    let cfg = config::Config::from_env();
    std::fs::create_dir_all(cfg.repos_dir())?;
    std::fs::create_dir_all(cfg.tmp_dir())?;
    std::fs::create_dir_all(cfg.avatars_dir())?;
    let storage = storage::Storage::from_config(&cfg)?;
    let database = db::open(&cfg.data.join("hub.db"))?;
    let st = AppState(Arc::new(Inner {
        cfg: cfg.clone(),
        db: database,
        tmpl: web::templates(),
        storage,
        repo_locks: parking_lot::Mutex::new(Default::default()),
        lfs_tickets: parking_lot::Mutex::new(Default::default()),
        asset_ver: web::assets::version(),
    }));
    let router = app(st);
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("HuggingHub listening on http://{} (data: {})", cfg.bind, cfg.data.display());
    serve(listener, router).await?;
    Ok(())
}

fn open_db() -> anyhow::Result<(config::Config, db::Db)> {
    let cfg = config::Config::from_env();
    std::fs::create_dir_all(&cfg.data)?;
    let database = db::open(&cfg.data.join("hub.db"))?;
    Ok((cfg, database))
}

fn cli_create_user(args: &[String]) -> anyhow::Result<()> {
    let (_, db) = open_db()?;
    let name = args.first().cloned().unwrap_or_else(|| "admin".into());
    let password = args.get(1).cloned().unwrap_or_else(|| util::random_token(12));
    let c = db.lock();
    let exists: i64 = c.query_row("SELECT COUNT(*) FROM accounts WHERE name = ?1", [&name], |r| r.get(0))?;
    if exists > 0 {
        anyhow::bail!("user {name} already exists");
    }
    let first: i64 = c.query_row("SELECT COUNT(*) FROM accounts WHERE kind = 'user'", [], |r| r.get(0))?;
    c.execute(
        "INSERT INTO accounts (name, kind, fullname, password, created_at, is_admin) VALUES (?1, 'user', ?1, ?2, ?3, ?4)",
        rusqlite::params![name, auth::hash_password(&password), util::now_iso(), (first == 0) as i64],
    )?;
    let uid = c.last_insert_rowid();
    let tok = auth::new_token(&c, uid, "cli", "write")?;
    println!("user:     {name}");
    println!("password: {password}");
    println!("token:    {tok}");
    Ok(())
}

fn cli_create_token(args: &[String]) -> anyhow::Result<()> {
    let (_, db) = open_db()?;
    let name = args.first().cloned().unwrap_or_else(|| "admin".into());
    let role = args.get(1).cloned().unwrap_or_else(|| "write".into());
    let c = db.lock();
    let uid: i64 = c
        .query_row("SELECT id FROM accounts WHERE name = ?1 AND kind = 'user'", [&name], |r| r.get(0))
        .map_err(|_| anyhow::anyhow!("no such user: {name}"))?;
    let tok = auth::new_token(&c, uid, "cli", &role)?;
    println!("{tok}");
    Ok(())
}

