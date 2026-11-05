"""Loaded by the .pth hook at interpreter start-up."""

import os

from ._hook import _read_config, connect

if not os.environ.get("HUGGINGHUB_NO_AUTOHOOK"):
    cfg = _read_config()
    if cfg.get("endpoint"):
        connect(cfg["endpoint"], cfg.get("token"))
