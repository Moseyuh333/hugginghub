<div align="center">

# 🤗 HuggingHub

**Your own Hugging Face Hub.** Self-hosted, source-available, and wire-compatible with `huggingface_hub`.

Models · Datasets · Spaces — with cards, files, git history, discussions and pull requests.
No billing. No quotas. No account on someone else's machine.

</div>

---

HuggingHub is a single Rust binary that reimplements the Hugging Face Hub HTTP API, so everything that talks to `huggingface.co` — `huggingface_hub`, `transformers`, `datasets`, `diffusers`, `accelerate`, the `hf` CLI, `git`/`git-lfs` — can talk to *your* server instead, usually with a two-line change or one environment variable.

```python
import hugginghub
hugginghub.connect("http://my-server:8080", token="hf_...")

from huggingface_hub import HfApi, snapshot_download
api = HfApi()
api.create_repo("me/my-model", exist_ok=True)
api.upload_folder(repo_id="me/my-model", folder_path="./my-model")
snapshot_download("me/my-model")
```

...or with plain environment variables, no shim at all:

```bash
export HF_ENDPOINT=http://my-server:8080
export HF_TOKEN=hf_...
hf download me/my-model
```

## What it does

| Area | Support |
| --- | --- |
| Repos | model / dataset / space, public or private, rename, move, delete, gated flag |
| Files | NDJSON commit API, preupload (regular vs LFS), resolve with Range requests, snapshots, folder uploads |
| Large files | Git LFS: batch API, upload/verify, download, content-addressed dedup store |
| Git | smart HTTP clone/fetch/push (incl. `git-lfs`), branches, tags, `refs/pr/N` |
| Community | discussions, comments, edits/hides, open/close/merge, pull requests with diffs |
| Social | likes, followers, user and organization profiles, org membership |
| **Spaces** | `sdk: static` spaces **run** — the repo's files are served as a web app at `/spaces/ns/name/app/` and embedded on the space page; gradio/streamlit apps can be embedded from an external URL set in Settings |
| Web UI | landing page, listings, model/dataset/Space cards, file browser, blob view, commit history and diffs, discussions, settings, tokens |
| Admin | `/admin` panel: users (create, grant/revoke admin, reset passwords, delete), repositories, organizations, server info |
| Storage | local disk, or any S3-compatible bucket (AWS S3, R2, B2, Wasabi, MinIO, Ceph) with optional presigned redirects |

Deliberately **not** included: inference, a container runtime for gradio/streamlit
Spaces, jobs, papers, collections, billing, hardware.

## Run it

Requirements: Rust 1.75+, `git` (2.30+), and `git-lfs` if you want the CLI to
handle large files.

```bash
cargo build --release
export HH_DATA=/var/lib/hugginghub
./target/release/hugginghub create-user admin 'a-good-password'   # prints an API token
./target/release/hugginghub                                       # serves on 0.0.0.0:8080
```

The first user created is an administrator. Put it behind a reverse proxy (Caddy,
nginx) for TLS — the app derives URLs from the request's `Host` header, so
`X-Forwarded-Host`/`X-Forwarded-Proto` are respected automatically.

systemd unit: [`deploy/hugginghub.service`](deploy/hugginghub.service).

## Configuration

All settings are environment variables.

| Variable | Default | Meaning |
| --- | --- | --- |
| `HH_BIND` | `0.0.0.0:8080` | listen address |
| `HH_DATA` | `./data` | data dir (`hub.db`, `repos/`, `tmp/`, `avatars/`) |
| `HH_PUBLIC_URL` | derived from request | force an external base URL |
| `HH_SITE_NAME` | `HuggingHub` | name shown in the UI |
| `HH_OPEN_SIGNUP` | `true` | allow self sign-up |
| `HH_LFS_THRESHOLD` | `10485760` | files at or above this size go to LFS on preupload |
| `HH_STORAGE` | `local` | `local` or `s3` |
| `HH_GIT` | `git` | path to the git binary |

### S3-compatible storage

Large files (LFS objects) can live in a bucket instead of local disk. Git
repositories — pointers, text, history — always stay on local disk, which keeps
clones fast and the bucket simple.

```bash
HH_STORAGE=s3
HH_S3_BUCKET=hugginghub
HH_S3_ENDPOINT=https://s3.eu-central-1.amazonaws.com   # or https://<account>.r2.cloudflarestorage.com
HH_S3_REGION=eu-central-1
HH_S3_ACCESS_KEY=...
HH_S3_SECRET_KEY=...
HH_S3_PREFIX=hugginghub/prod        # optional key prefix
HH_S3_PATH_STYLE=1                  # default 1 (MinIO, Ceph); set 0 for AWS virtual-hosted style
HH_S3_REDIRECT=0                    # 1 = hand clients presigned URLs instead of proxying bytes
```

Objects are stored as `lfs/<aa>/<bb>/<sha256>`, content-addressed, so identical
files across repos are stored once. Uploads use a single PUT up to 256 MB and
multipart above that. With `HH_S3_REDIRECT=1` downloads become `302` redirects to
presigned URLs, like the Hub's CDN redirects — the bucket must then be reachable
by clients.

## The Python shim

`python/` contains a small package that points `huggingface_hub` at your server:

```bash
pip install ./python
hugginghub connect http://my-server:8080 --token hf_...   # saved to ~/.config/hugginghub
hugginghub install-hook                                   # every Python process picks it up
```

`connect()` is **per process, not per file**: call it once at your program's
entry point and every module in that process uses your hub. For one shell
session use `export HF_ENDPOINT=...`; for the whole machine use
`hugginghub install-hook`. See [`python/README.md`](python/README.md) and the
built-in **How to use** page (`/docs` on your server).

## Testing

```bash
tests/e2e_hub.py    http://host:port hf_token   # 23 checks with the real huggingface_hub client
tests/e2e_git.sh    http://host:port hf_token   # clone/push, git-lfs, anonymous access, PR refs
tests/e2e_web.sh    http://host:port hf_token   # every web page renders, admin panel, app serving
tests/seed_demo.py  http://host:port hf_token   # demo repos + a runnable static space
```

## Architecture

```
src/
  main.rs        router, CLI (create-user, create-token, info)
  routes.rs      request plumbing; repo URLs are parsed from raw path segments
                 because ids contain '/' and revisions arrive percent-encoded
  api/           Hub-compatible HTTP API (repos, files, commit, lfs, community, users)
  githttp.rs     smart HTTP via `git http-backend` (CGI)
  git/           bare-repo plumbing: trees, commits through a throwaway index,
                 refs, merges, LFS pointers
  storage/       LFS object store: local disk or S3 (SigV4, multipart, presigning)
  web/           server-rendered UI (minijinja templates, embedded assets)
  db.rs          SQLite schema; models.rs permissions; auth.rs tokens and sessions
```

Design notes:

- Every repo is a bare git repository; the API builds commits with `read-tree` /
  `update-index` / `write-tree` / `commit-tree` and moves refs with a
  compare-and-swap, so the API and `git push` share one source of truth.
- Large files are stored as real LFS pointer blobs in git and as objects in the
  LFS store, exactly like the Hub.
- `huggingface_hub` uploads LFS content with an unauthenticated PUT, so the
  upload/download hrefs handed out by the batch API carry a short-lived ticket.
- Pull requests are discussions whose commits live on `refs/pr/N`; merging is a
  fast-forward when possible, otherwise a merge commit built with `merge-tree`.
- Static Spaces are served straight out of the repo at `/spaces/ns/name/app/`
  with proper MIME types and trailing-slash semantics; non-static Spaces embed
  an external `app_url`.

## Security notes

- Passwords are hashed with Argon2id; API tokens are `hf_`-prefixed random secrets stored as SHA-256 hashes and shown once.
- `HH_OPEN_SIGNUP=0` closes registration; admins can always create users from `/admin`.
- The default is HTTP on a single box — put TLS in front before sharing it.
- Don't ship real tokens in commits; the values in this repo are placeholders (`hf_...`).

## License

Licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE) — free for personal, research, educational, non-profit, and non-commercial use. For commercial licensing, contact [Empero](https://empero.org).

---

<div align="center">
<sub>Built by <a href="https://empero.org">Empero</a></sub>
</div>
