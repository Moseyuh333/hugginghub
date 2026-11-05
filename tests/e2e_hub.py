#!/usr/bin/env python3
"""End-to-end test of a HuggingHub server using the real huggingface_hub client.

Usage: python tests/e2e_hub.py http://host:port hf_token
"""
import io
import os
import time

STAMP = str(int(time.time()))
import sys
import traceback

ENDPOINT = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("HH_ENDPOINT", "http://127.0.0.1:8080")
TOKEN = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("HF_TOKEN", "")

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
import hugginghub  # noqa: E402

hugginghub.connect(ENDPOINT, token=TOKEN)

import huggingface_hub  # noqa: E402
from huggingface_hub import HfApi, hf_hub_download, snapshot_download  # noqa: E402
from huggingface_hub import CommitOperationAdd, CommitOperationDelete  # noqa: E402

api = HfApi(token=TOKEN)
REPO = "admin/e2e-model"
PASS, FAIL = [], []


def check(name, fn):
    try:
        fn()
        PASS.append(name)
        print(f"  ok   {name}")
    except Exception as e:
        FAIL.append((name, e))
        print(f"  FAIL {name}: {type(e).__name__}: {e}")
        traceback.print_exc(limit=3)


def t_whoami():
    me = api.whoami()
    assert me["name"] == "admin", me


def t_create_repo():
    url = api.create_repo(REPO, exist_ok=True)
    assert str(url).startswith(ENDPOINT), url


def t_upload_file():
    out = api.upload_file(path_or_fileobj=io.BytesIO(b'{"architectures": ["TestModel"]}'), path_in_repo="config.json", repo_id=REPO)
    assert out is not None


def t_upload_text():
    api.upload_file(path_or_fileobj=b"# E2E\n\nA test model.\n", path_in_repo="README.md", repo_id=REPO,
                    commit_message="add card")
    info = api.model_info(REPO, files_metadata=True)
    assert any(s.rfilename == "README.md" for s in info.siblings), info.siblings


def t_upload_lfs():
    blob = os.urandom(12 * 1024 * 1024)  # 12MB > 10MB threshold -> LFS
    api.upload_file(path_or_fileobj=io.BytesIO(blob), path_in_repo="weights.bin", repo_id=REPO)
    info = api.model_info(REPO, files_metadata=True)
    sib = [s for s in info.siblings if s.rfilename == "weights.bin"]
    assert sib and sib[0].lfs is not None, "expected an LFS sibling"
    assert sib[0].size == len(blob), sib[0].size
    global BLOB_HASH
    import hashlib
    BLOB_HASH = hashlib.sha256(blob).hexdigest()
    assert sib[0].lfs.sha256 == BLOB_HASH, (sib[0].lfs.sha256, BLOB_HASH)


def t_download_lfs():
    import hashlib
    p = hf_hub_download(REPO, "weights.bin", force_download=True)
    data = open(p, "rb").read()
    assert hashlib.sha256(data).hexdigest() == BLOB_HASH, "content mismatch"


def t_download_small():
    p = hf_hub_download(REPO, "config.json", force_download=True)
    assert b"architectures" in open(p, "rb").read()


def t_snapshot():
    d = snapshot_download(REPO, force_download=True)
    names = sorted(os.listdir(d))
    assert "config.json" in names and "weights.bin" in names, names


def t_upload_folder(tmp="/tmp/hh-e2e-folder"):
    os.makedirs(tmp, exist_ok=True)
    for i in range(3):
        open(os.path.join(tmp, f"part{i}.txt"), "w").write(f"part {i}\n")
    api.upload_folder(repo_id=REPO, folder_path=tmp, commit_message="folder upload")
    files = api.list_repo_files(REPO)
    assert "part0.txt" in files and "part2.txt" in files, files


def t_tree():
    entries = list(api.list_repo_tree(REPO, recursive=True))
    names = {e.path for e in entries}
    assert "config.json" in names and "weights.bin" in names, names


def t_paths_info():
    info = api.get_paths_info(REPO, ["config.json", "nope.txt"])
    assert len(info) == 1, info


def t_refs():
    refs = api.list_repo_refs(REPO)
    assert any(b.name == "main" for b in refs.branches), refs


def t_branch():
    api.create_branch(REPO, branch="dev", exist_ok=True)
    refs = api.list_repo_refs(REPO)
    assert any(b.name == "dev" for b in refs.branches), [b.name for b in refs.branches]


def t_tag():
    api.create_tag(REPO, tag="v1", exist_ok=True)
    refs = api.list_repo_refs(REPO)
    assert any(t.name == "v1" for t in refs.tags)


def t_commits():
    commits = api.list_repo_commits(REPO)
    assert len(commits) >= 3, len(commits)
    assert commits[0].commit_id


def t_create_commit():
    out = api.create_commit(
        repo_id=REPO,
        operations=[
            CommitOperationAdd(path_in_repo="notes.md", path_or_fileobj=b"# Notes\n"),
            CommitOperationDelete(path_in_repo="part1.txt"),
        ],
        commit_message="add notes, drop part1",
    )
    files = api.list_repo_files(REPO)
    assert "notes.md" in files and "part1.txt" not in files, files
    assert out.oid


def t_dataset_repo():
    api.create_repo("admin/e2e-dataset", repo_type="dataset", exist_ok=True)
    api.upload_file(path_or_fileobj=b"a,b\n1,2\n", path_in_repo="data.csv", repo_id="admin/e2e-dataset", repo_type="dataset")
    info = api.dataset_info("admin/e2e-dataset")
    assert info.id == "admin/e2e-dataset", info.id
    p = hf_hub_download("admin/e2e-dataset", "data.csv", repo_type="dataset", force_download=True)
    assert open(p, "rb").read().startswith(b"a,b")


def t_list_models():
    models = list(api.list_models(author="admin"))
    assert any(m.id == REPO for m in models), [m.id for m in models]


def t_discussion():
    d = api.create_discussion(REPO, "Question about weights", description="Is weights.bin fp32?")
    api.comment_discussion(REPO, d.num, "I think so.")
    details = api.get_discussion_details(REPO, d.num)
    assert len(details.events) >= 2, details.events
    api.change_discussion_status(REPO, d.num, "closed", comment="answered")
    det2 = api.get_discussion_details(REPO, d.num)
    assert det2.status == "closed", det2.status
    lst = list(api.get_repo_discussions(REPO))
    assert any(x.num == d.num for x in lst)


def t_pr_flow():
    out = api.create_commit(
        repo_id=REPO,
        operations=[CommitOperationAdd(path_in_repo=f"pr-file-{STAMP}.txt", path_or_fileobj=b"hello from a PR\n")],
        commit_message="Add pr-file",
        create_pr=True,
    )
    assert out.pr_url, out
    prs = list(api.get_repo_discussions(REPO, discussion_type="pull_request"))
    assert prs, "no PRs listed"
    num = prs[0].num
    details = api.get_discussion_details(REPO, num, _async=False) if False else api.get_discussion_details(REPO, num)
    assert details.is_pull_request
    api.merge_pull_request(REPO, num, comment="merging")
    files = api.list_repo_files(REPO)
    assert f"pr-file-{STAMP}.txt" in files, files


def t_like():
    import urllib.request
    req = urllib.request.Request(f"{ENDPOINT}/api/models/{REPO}/like", method="POST",
                                 headers={"Authorization": f"Bearer {TOKEN}"})
    urllib.request.urlopen(req).read()
    likes = api.list_liked_repos("admin")
    assert any(m == REPO for m in likes.models), likes


def t_user_overview():
    u = api.get_user_overview("admin")
    assert u.username == "admin", u


def t_delete_file_repo():
    api.create_repo("admin/throwaway", exist_ok=True)
    api.delete_repo("admin/throwaway")
    try:
        api.model_info("admin/throwaway")
        raise AssertionError("repo should be gone")
    except Exception:
        pass


tests = [
    ("whoami", t_whoami),
    ("create_repo", t_create_repo),
    ("upload_file (small)", t_upload_file),
    ("upload README", t_upload_text),
    ("upload LFS (12MB)", t_upload_lfs),
    ("download LFS", t_download_lfs),
    ("download small", t_download_small),
    ("snapshot_download", t_snapshot),
    ("upload_folder", t_upload_folder),
    ("list_repo_tree", t_tree),
    ("get_paths_info", t_paths_info),
    ("list_repo_refs", t_refs),
    ("create_branch", t_branch),
    ("create_tag", t_tag),
    ("list_repo_commits", t_commits),
    ("create_commit + delete", t_create_commit),
    ("dataset repo", t_dataset_repo),
    ("list_models", t_list_models),
    ("discussions", t_discussion),
    ("pull request + merge", t_pr_flow),
    ("like", t_like),
    ("user overview", t_user_overview),
    ("delete_repo", t_delete_file_repo),
]

print(f"Testing {ENDPOINT} with huggingface_hub {huggingface_hub.__version__}")
for name, fn in tests:
    check(name, fn)

print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
for name, e in FAIL:
    print(f"  - {name}: {type(e).__name__}: {e}")
sys.exit(1 if FAIL else 0)
