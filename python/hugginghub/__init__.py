"""hugginghub -- point `huggingface_hub` at your own HuggingHub server.

    import hugginghub
    hugginghub.connect("http://my-server:8080", token="hf_...")

    from huggingface_hub import HfApi
    HfApi().whoami()

or, without importing anything first:

    HF_ENDPOINT=http://my-server:8080 HF_TOKEN=hf_... python train.py

`hugginghub install-hook` drops a `.pth` file into site-packages so every
Python process on the machine picks up the configured endpoint automatically
(config stored in ~/.config/hugginghub/config.json or $HUGGINGHUB_CONFIG).
"""

from ._hook import (
    connect,
    current_endpoint,
    disconnect,
    install_hook,
    uninstall_hook,
)

__all__ = ["connect", "disconnect", "current_endpoint", "install_hook", "uninstall_hook"]
__version__ = "0.1.0"
