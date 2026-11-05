#!/usr/bin/env python3
"""Seed demo repos so the UI has realistic content to show."""
import sys

EP = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1"
TOKEN = sys.argv[2] if len(sys.argv) > 2 else ""
sys.path.insert(0, "python")
import hugginghub  # noqa: E402

hugginghub.connect(EP, token=TOKEN)
from huggingface_hub import HfApi  # noqa: E402

api = HfApi()

READMES = {
    "admin/Qwen2.5-Demo-0.5B-Instruct": """---
license: apache-2.0
language:
- en
- de
library_name: transformers
pipeline_tag: text-generation
tags:
- chat
- instruct
- demo
base_model: Qwen/Qwen2.5-0.5B
datasets:
- tatsu-lab/alpaca
---

# Qwen2.5-Demo-0.5B-Instruct

This is a **demonstration model card** rendered by HuggingHub, the self-hosted
Hugging Face Hub. It shows how model cards, tags and files look.

## Model Details

| Property | Value |
| --- | --- |
| Base model | Qwen2.5-0.5B |
| Parameters | 0.5B |
| Context | 32k tokens |
| License | Apache-2.0 |

## Usage

```python
import hugginghub
hugginghub.connect("http://localhost:8080")

from transformers import AutoModelForCausalLM, AutoTokenizer
model = AutoModelForCausalLM.from_pretrained("admin/Qwen2.5-Demo-0.5B-Instruct")
tokenizer = AutoTokenizer.from_pretrained("admin/Qwen2.5-Demo-0.5B-Instruct")
```

## Training

Trained on a small instruction set for demo purposes.

- Stage 1: supervised fine-tuning
- Stage 2: preference optimisation

## Evaluation

| Task | Score |
| --- | --- |
| MMLU | 42.1 |
| HellaSwag | 61.4 |
""",
    "admin/alpaca-demo": """---
license: cc-by-4.0
task_categories:
- text-generation
language:
- en
size_categories:
- n<1K
---

# Alpaca demo subset

A tiny instruction dataset used by the demo model. Columns: `instruction`, `output`.

## Files

- `train.csv` -- 1 example
""",
    "admin/vision-demo": """---
title: Vision Demo
emoji: 🖼️
colorFrom: indigo
colorTo: pink
sdk: gradio
pinned: false
---

# Vision Demo

A demo Space card shown by HuggingHub. Spaces here are git repositories with a
card; running the app is up to you.
""",
}

FILES = {
    "admin/Qwen2.5-Demo-0.5B-Instruct": {
        "config.json": b'{\n  "architectures": ["Qwen2ForCausalLM"],\n  "model_type": "qwen2",\n  "hidden_size": 896,\n  "num_hidden_layers": 24,\n  "num_attention_heads": 14,\n  "vocab_size": 151936\n}\n',
        "generation_config.json": b'{\n  "temperature": 0.7,\n  "top_p": 0.8,\n  "max_new_tokens": 512\n}\n',
        "tokenizer_config.json": b'{\n  "bos_token": "<|endoftext|>",\n  "chat_template": "{% for message in messages %}{{ message[role] }}{% endfor %}"\n}\n',
    },
    "admin/alpaca-demo": {
        "train.csv": b"instruction,output\nWrite a haiku,A calm lake at dawn\n",
    },
    "admin/vision-demo": {
        "app.py": b"import gradio as gr\n\ndef greet(name):\n    return f\"Hello {name}!\"\n\ngr.Interface(greet, \"text\", \"text\").launch()\n",
        "requirements.txt": b"gradio\n",
    },
}

for repo_id, readme in READMES.items():
    rtype = "dataset" if "alpaca" in repo_id else ("space" if "vision" in repo_id else "model")
    kwargs = {"repo_type": rtype}
    create_kwargs = dict(kwargs)
    if rtype == "space":
        create_kwargs["space_sdk"] = "gradio"
    api.create_repo(repo_id, exist_ok=True, **create_kwargs)
    api.upload_file(path_or_fileobj=readme.encode(), path_in_repo="README.md", repo_id=repo_id, **kwargs)
    for path, data in FILES[repo_id].items():
        api.upload_file(path_or_fileobj=data, path_in_repo=path, repo_id=repo_id, **kwargs)

# a static space that actually runs inside HuggingHub
STATIC_INDEX = b"""<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>static-hello</title>
<link rel="stylesheet" href="style.css"></head>
<body><h1>Hello from a static Space</h1>
<p>Served straight out of the repo by HuggingHub.</p>
<div id="msg"></div>
<script src="app.js"></script>
</body></html>
"""
api.create_repo("admin/static-hello", repo_type="space", space_sdk="static", exist_ok=True)
api.upload_file(path_or_fileobj=b"---\nsdk: static\n---\n\n# static-hello\n\nA static space served by HuggingHub at `/spaces/admin/static-hello/app`.\n",
                path_in_repo="README.md", repo_id="admin/static-hello", repo_type="space")
api.upload_file(path_or_fileobj=STATIC_INDEX, path_in_repo="index.html", repo_id="admin/static-hello", repo_type="space")
api.upload_file(path_or_fileobj=b"body{font-family:system-ui;margin:3rem}\nh1{color:#2563eb}\n",
                path_in_repo="style.css", repo_id="admin/static-hello", repo_type="space")
api.upload_file(path_or_fileobj=b"document.getElementById('msg').textContent='JS is running: '+new Date().toISOString();\n",
                path_in_repo="app.js", repo_id="admin/static-hello", repo_type="space")

# a discussion and a pull request so the community tab has content
try:
    api.create_discussion("admin/Qwen2.5-Demo-0.5B-Instruct", "Tokenizer files for the German subset?",
                          description="Do we have a German tokenizer variant planned?")
except Exception as e:
    print("discussion:", e)

print("seeded")
