//! `/api/...` routes, compatible with the Hugging Face Hub HTTP API as used by `huggingface_hub`.

pub mod commit;
pub mod community;
pub mod files;
pub mod lfs;
pub mod repos;
pub mod users;

use crate::error::{ApiError, ApiResult};
use crate::routes::{json, Req};
use crate::util::type_from_plural;
use axum::http::Method;
use axum::response::Response;
use serde_json::json;

pub async fn dispatch(r: &mut Req, s: &[&str]) -> ApiResult<Response> {
    let m = r.method.clone();
    match (m, s) {
        (Method::GET, ["whoami-v2"]) | (Method::GET, ["whoami"]) => users::whoami(r).await,
        (Method::POST, ["repos", "create"]) => repos::create(r).await,
        (Method::DELETE, ["repos", "delete"]) => repos::delete(r).await,
        (Method::POST, ["repos", "move"]) => repos::move_repo(r).await,
        (Method::POST, ["validate-yaml"]) => repos::validate_yaml(r).await,
        (Method::GET, [t @ ("models" | "datasets" | "spaces")]) => repos::list(r, type_from_plural(t).unwrap()).await,
        (Method::GET, ["models-tags-by-type" | "datasets-tags-by-type"]) => Ok(json(json!({}))),
        (Method::GET, ["spaces", "hardware"]) => Ok(json(json!([]))),
        (_, ["users", u, rest @ ..]) => users::user_route(r, u, rest).await,
        (_, ["organizations", o, rest @ ..]) => users::org_route(r, o, rest).await,
        (_, ["settings", "tokens", rest @ ..]) => users::tokens_route(r, rest).await,
        (_, [t @ ("models" | "datasets" | "spaces"), rest @ ..]) if !rest.is_empty() => {
            let rtype = type_from_plural(t).unwrap();
            // repo ids are `owner/name`, or a bare `name` for the caller's namespace
            let (ns, name, tail) = if rest.len() >= 3 && is_repo_action(rest[2]) {
                (rest[0].to_string(), rest[1].to_string(), &rest[2..])
            } else if rest.len() >= 2 && is_repo_action(rest[1]) {
                (String::new(), rest[0].to_string(), &rest[1..])
            } else if rest.len() == 1 {
                (String::new(), rest[0].to_string(), &[][..])
            } else {
                (rest[0].to_string(), rest[1].to_string(), &rest[2..])
            };
            repo_api(r, rtype, &ns, &name, tail).await
        }
        _ => Err(ApiError::not_found("Sorry, we can't find the page you are looking for.")),
    }
}

pub const REPO_ACTIONS: &[&str] = &[
    "revision", "tree", "paths-info", "preupload", "commit", "refs", "commits", "compare", "branch", "tag",
    "settings", "super-squash", "auth-check", "like", "likers", "lfs-files", "discussions", "xet-read-token",
    "xet-write-token",
];

pub fn is_repo_action(seg: &str) -> bool {
    REPO_ACTIONS.contains(&seg)
}

async fn repo_api(r: &mut Req, rtype: &str, ns: &str, name: &str, rest: &[&str]) -> ApiResult<Response> {
    let m = r.method.clone();
    match (m, rest) {
        (Method::GET, []) => repos::info(r, rtype, ns, name, None).await,
        (Method::GET, ["revision", rev @ ..]) => repos::info(r, rtype, ns, name, Some(&rev.join("/"))).await,
        (Method::GET, ["tree", rev, path @ ..]) => files::tree(r, rtype, ns, name, rev, &path.join("/")).await,
        (Method::POST, ["paths-info", rev @ ..]) => files::paths_info(r, rtype, ns, name, &rev.join("/")).await,
        (Method::POST, ["preupload", rev @ ..]) => commit::preupload(r, rtype, ns, name, &rev.join("/")).await,
        (Method::POST, ["commit", rev @ ..]) => commit::commit(r, rtype, ns, name, &rev.join("/")).await,
        (Method::GET, ["refs"]) => repos::refs(r, rtype, ns, name).await,
        (Method::GET, ["commits", rev @ ..]) => repos::commits(r, rtype, ns, name, &rev.join("/")).await,
        (Method::GET, ["compare", spec]) => repos::compare(r, rtype, ns, name, spec).await,
        (Method::POST, ["branch", b @ ..]) => repos::create_branch(r, rtype, ns, name, &b.join("/")).await,
        (Method::DELETE, ["branch", b @ ..]) => repos::delete_branch(r, rtype, ns, name, &b.join("/")).await,
        (Method::POST, ["tag", rev @ ..]) => repos::create_tag(r, rtype, ns, name, &rev.join("/")).await,
        (Method::DELETE, ["tag", t @ ..]) => repos::delete_tag(r, rtype, ns, name, &t.join("/")).await,
        (Method::PUT, ["settings"]) => repos::settings(r, rtype, ns, name).await,
        (Method::POST, ["super-squash", b @ ..]) => repos::super_squash(r, rtype, ns, name, &b.join("/")).await,
        (Method::GET, ["auth-check"]) => repos::auth_check(r, rtype, ns, name).await,
        (Method::POST, ["like"]) => users::like(r, rtype, ns, name, true).await,
        (Method::DELETE, ["like"]) => users::like(r, rtype, ns, name, false).await,
        (Method::GET, ["likers"]) => users::likers(r, rtype, ns, name).await,
        (Method::GET, ["lfs-files"]) => lfs::list_lfs_files(r, rtype, ns, name).await,
        (_, ["discussions", drest @ ..]) => community::route(r, rtype, ns, name, drest).await,
        _ => Err(ApiError::not_found("Sorry, we can't find the page you are looking for.")),
    }
}
