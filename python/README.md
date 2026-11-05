# hugginghub (Python client shim)

Points `huggingface_hub` -- and therefore `transformers`, `datasets`, `diffusers`,
`accelerate` and the `hf` CLI -- at your own HuggingHub server.

```python
import hugginghub
hugginghub.connect("http://my-server:8080", token="hf_...")

from huggingface_hub import HfApi, snapshot_download
HfApi().whoami()
snapshot_download("admin/my-model")
```

What `connect()` does:

- sets `HF_ENDPOINT` (and `HF_TOKEN` if given)
- sets `HF_HUB_DISABLE_XET=1` -- HuggingHub stores large files as Git LFS objects
- gives the instance its own download cache (`~/.cache/hugginghub/<host>`) so repos
  with the same id as public Hub repos never collide
- patches an already-imported `huggingface_hub` in place (endpoint, URL template,
  the module-level `HfApi()` instance, cache path)

## Machine-wide hook

```bash
hugginghub connect http://my-server:8080 --token hf_...
hugginghub install-hook     # writes a .pth file into site-packages
```

After that every Python process on the machine talks to your server without any
code change. `hugginghub uninstall-hook` removes it, and
`HUGGINGHUB_NO_AUTOHOOK=1` disables it for a single process.

## Without the shim

The server speaks the Hub HTTP API, so plain environment variables are enough:

```bash
export HF_ENDPOINT=http://my-server:8080
export HF_TOKEN=hf_...
export HF_HUB_DISABLE_XET=1   # optional; avoids Xet for uploads
```
