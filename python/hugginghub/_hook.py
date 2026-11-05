"""The actual hooking logic: environment variables plus live patching of an
already-imported `huggingface_hub`."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

CONFIG_PATH = Path(
    os.environ.get("HUGGINGHUB_CONFIG", Path.home() / ".config" / "hugginghub" / "config.json")
)

DEFAULT_HUB = "https://huggingface.co"


def _read_config() -> dict:
    try:
        return json.loads(CONFIG_PATH.read_text())
    except Exception:
        return {}


def _write_config(cfg: dict) -> None:
    CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    CONFIG_PATH.write_text(json.dumps(cfg, indent=2))
    try:
        os.chmod(CONFIG_PATH, 0o600)
    except OSError:
        pass


def current_endpoint() -> str:
    """The endpoint huggingface_hub would use right now."""
    cfg = _read_config()
    return (
        os.environ.get("HF_ENDPOINT")
        or cfg.get("endpoint")
        or DEFAULT_HUB
    ).rstrip("/")


def _cache_dir_for(endpoint: str) -> str:
    host = endpoint.split("://", 1)[-1].replace("/", "_").replace(":", "_")
    base = os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))
    return str(Path(base) / "hugginghub" / host)


def _patch_loaded(endpoint: str, token: str | None, isolate_cache: bool) -> None:
    """Patch modules of an already-imported huggingface_hub."""
    hf = sys.modules.get("huggingface_hub")
    if hf is None:
        return
    consts = sys.modules.get("huggingface_hub.constants")
    if consts is not None:
        consts.ENDPOINT = endpoint
        consts.HUGGINGFACE_CO_URL_TEMPLATE = endpoint + "/{repo_id}/resolve/{revision}/{filename}"
        consts.HF_HUB_DISABLE_XET = True
        host = endpoint.split("://", 1)[-1].split("/")[0].lower()
        try:
            consts.HF_URL_HOSTS = frozenset(set(consts.HF_URL_HOSTS) | {host})
        except Exception:
            pass
        if isolate_cache:
            cache = _cache_dir_for(endpoint)
            consts.HF_HUB_CACHE = cache
            os.environ["HF_HUB_CACHE"] = cache
    # the module-level HfApi() captured the endpoint at import time
    api_mod = sys.modules.get("huggingface_hub.hf_api")
    if api_mod is not None:
        for attr in ("api",):
            obj = getattr(api_mod, attr, None)
            if obj is not None and hasattr(obj, "endpoint"):
                obj.endpoint = endpoint
        if token:
            try:
                from huggingface_hub.utils import _auth  # noqa: F401
            except Exception:
                pass
    # file_download imports the template by value
    dl = sys.modules.get("huggingface_hub.file_download")
    if dl is not None and hasattr(dl, "HUGGINGFACE_CO_URL_TEMPLATE"):
        dl.HUGGINGFACE_CO_URL_TEMPLATE = endpoint + "/{repo_id}/resolve/{revision}/{filename}"


def connect(
    endpoint: str,
    token: str | None = None,
    *,
    isolate_cache: bool = True,
    persist: bool = False,
) -> str:
    """Point huggingface_hub (and anything built on it) at `endpoint`.

    Call this *before* importing huggingface_hub for a fully clean setup; calling
    it afterwards also works (the loaded modules are patched in place).
    """
    endpoint = endpoint.rstrip("/")
    os.environ["HF_ENDPOINT"] = endpoint
    os.environ["HF_HUB_DISABLE_XET"] = "1"  # HuggingHub serves plain LFS
    if token:
        os.environ["HF_TOKEN"] = token
    if isolate_cache:
        cache = _cache_dir_for(endpoint)
        os.environ["HF_HUB_CACHE"] = cache
    _patch_loaded(endpoint, token, isolate_cache)
    if persist:
        cfg = _read_config()
        cfg["endpoint"] = endpoint
        if token:
            cfg["token"] = token
        _write_config(cfg)
    return endpoint


def disconnect() -> None:
    os.environ.pop("HF_ENDPOINT", None)
    os.environ.pop("HF_HUB_DISABLE_XET", None)
    os.environ.pop("HF_HUB_CACHE", None)
    os.environ["HF_ENDPOINT"] = DEFAULT_HUB
    _patch_loaded(DEFAULT_HUB, None, False)


def _site_packages() -> list[Path]:
    import site

    out = []
    try:
        out.extend(Path(p) for p in site.getsitepackages())
    except Exception:
        pass
    user = site.getusersitepackages()
    if user:
        out.append(Path(user))
    return [p for p in out if p.is_dir()]


def install_hook() -> Path:
    """Install a .pth file so every Python process auto-connects to the saved endpoint."""
    if not _read_config().get("endpoint"):
        raise RuntimeError("Run `hugginghub.connect(endpoint, persist=True)` first, or set HUGGINGHUB_CONFIG")
    for sp in _site_packages():
        if os.access(sp, os.W_OK):
            p = sp / "hugginghub_autohook.pth"
            p.write_text("import hugginghub._autohook\n")
            return p
    raise RuntimeError("No writable site-packages directory found (try sudo)")


def uninstall_hook() -> list[Path]:
    removed = []
    for sp in _site_packages():
        p = sp / "hugginghub_autohook.pth"
        if p.exists():
            try:
                p.unlink()
                removed.append(p)
            except OSError:
                pass
    return removed
