"""Command line helper: `hugginghub connect ...`, `hugginghub status`, `hugginghub install-hook`."""

from __future__ import annotations

import argparse
import json
import sys

from ._hook import CONFIG_PATH, _read_config, connect, current_endpoint, install_hook, uninstall_hook


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="hugginghub", description="Connect huggingface_hub to a HuggingHub server")
    sub = p.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("connect", help="save an endpoint (and token) as the default")
    c.add_argument("endpoint")
    c.add_argument("--token", default=None)
    c.add_argument("--no-isolate-cache", action="store_true", help="share the HF cache with huggingface.co")

    sub.add_parser("status", help="show the configured endpoint")
    sub.add_parser("install-hook", help="auto-connect in every Python process (.pth hook)")
    sub.add_parser("uninstall-hook", help="remove the .pth hook")

    a = p.parse_args(argv)
    if a.cmd == "connect":
        ep = connect(a.endpoint, a.token, isolate_cache=not a.no_isolate_cache, persist=True)
        print(f"saved endpoint {ep} to {CONFIG_PATH}")
        if a.token:
            print("token saved")
        return 0
    if a.cmd == "status":
        cfg = _read_config()
        print(json.dumps({"endpoint": current_endpoint(), "config": str(CONFIG_PATH), "saved": cfg}, indent=2))
        return 0
    if a.cmd == "install-hook":
        print(f"installed {install_hook()}")
        return 0
    if a.cmd == "uninstall-hook":
        removed = uninstall_hook()
        print("removed " + ", ".join(map(str, removed)) if removed else "no hook installed")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
