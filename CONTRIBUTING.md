# Contributing

Thanks for your interest! A few notes that will save you time:

## Layout

- `src/` — the Rust server (axum + rusqlite + minijinja). See "Architecture" in the README.
- `templates/`, `static/` — server-rendered UI. Templates are registered in `src/web/assets.rs`; add yours to `TEMPLATES` or it won't exist at runtime.
- `python/` — the `hugginghub` client shim.
- `tests/` — end-to-end tests that run against a live server.

## Rules of thumb

- Templates receive `serde_json::Value` contexts — never pass a struct.
  `RepoRow` in particular has methods like `web_path()` that are not fields;
  pass `web::repo_card(&repo)` instead.
- Repo URLs are routed on raw path segments in `routes.rs`, not axum wildcards.
  When you add a repo-scoped page, update `is_web_repo_action`.
- Run the three test suites against a running instance before opening a PR:
  `tests/e2e_hub.py`, `tests/e2e_git.sh`, `tests/e2e_web.sh`.

## Code style

- `cargo fmt` and fix `cargo clippy` warnings that your change introduces.
- Commit messages: say *why* as well as *what*.
